//! the deterministic scheduler (spec E27.02 §4): a single `tick` on goal
//! submitted / task finished / timer / daemon start. always in charge; the
//! LLM brain is only ever consulted for plan / supervise / integrate.
//!
//! this file is the pure core — `tick_pure` takes a graph + a fake
//! capacity map + a budget snapshot and returns an ordered list of
//! actions, with no db / subprocess / LLM. every §4 branch (ready-set,
//! per-agent + global caps, critical-path ordering, budget stop, retry,
//! integrator trigger) is exercised by the unit tests below. the db +
//! dispatch shell (reconcile, real `task::run_background` handoff,
//! `on_task_finished`) is plan Task 8.

use crate::coordinator::graph::{Effort, Node, NodeKind, NodeStatus, TaskGraph};
use crate::coordinator::routing::{self, CoordinatorConfig, PoolHealth, RoutingTable};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::time::Duration;

/// live concurrency picture, counted across ALL goals (spec §4.3).
#[derive(Debug, Clone, Default)]
pub struct Capacity {
    pub global_running: usize,
    pub per_agent_running: BTreeMap<String, usize>,
    /// `min(registry.max_concurrency, routing cap)` per agent; a missing
    /// entry means "no specific cap" and only the global limit applies.
    pub per_agent_cap: BTreeMap<String, usize>,
}

impl Capacity {
    fn agent_headroom(&self, agent: &str) -> usize {
        match self.per_agent_cap.get(agent) {
            Some(&cap) => cap.saturating_sub(self.per_agent_running.get(agent).copied().unwrap_or(0)),
            None => usize::MAX,
        }
    }
}

/// per-goal budget snapshot (spec §4.5 / §11.2): dispatch count + wall
/// clock, whichever trips first.
#[derive(Debug, Clone)]
pub struct GoalBudget {
    pub dispatches: u32,
    pub max_dispatches: u32,
    pub started_at: DateTime<Utc>,
    pub max_minutes: u32,
    pub now: DateTime<Utc>,
}

impl GoalBudget {
    /// the human-readable reason the goal should block, or `None` if there
    /// is still budget.
    pub fn exhausted(&self) -> Option<String> {
        if self.dispatches >= self.max_dispatches {
            return Some(format!(
                "dispatch budget spent: {} of {} node dispatches used",
                self.dispatches, self.max_dispatches
            ));
        }
        let elapsed_min = (self.now - self.started_at).num_minutes();
        if elapsed_min >= self.max_minutes as i64 {
            return Some(format!(
                "time budget spent: {elapsed_min} min elapsed of {} min cap",
                self.max_minutes
            ));
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TickAction {
    Dispatch {
        node_id: String,
        agent: String,
        effort: Effort,
        worktree: bool,
        max_steps: u32,
    },
    RunIntegrator,
    Block {
        reason: String,
    },
    Fail {
        reason: String,
    },
    Noop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// `attempts < 2`, transient failure → retry the node once, advancing
    /// to the next agent in its routing list (spec §4.6).
    RetrySameNextAgent,
    /// `attempts >= 2` or a semantic failure → hand to the supervisor
    /// (subject to the per-goal patch cap, enforced by the caller).
    Supervisor,
    /// nothing left to try for this node.
    GiveUp,
}

/// spec §4.6. `semantic_failure` = the task ran to completion but produced
/// a wrong / unusable result (as opposed to a crash / timeout / rate
/// limit), which the caller classifies from the task record.
pub fn retry_decision(attempts: u32, semantic_failure: bool) -> RetryDecision {
    if semantic_failure {
        return RetryDecision::Supervisor;
    }
    match attempts {
        0 | 1 => RetryDecision::RetrySameNextAgent,
        _ => RetryDecision::Supervisor,
    }
}

/// the pure scheduling decision for one goal (spec §4 steps 2–7). callers
/// pass the current graph, config, a capacity snapshot, the budget, the
/// routing table and pool health; they get back an ordered action list to
/// execute. no side effects.
pub fn tick_pure(
    graph: &TaskGraph,
    cfg: &CoordinatorConfig,
    cap: &Capacity,
    budget: &GoalBudget,
    table: &RoutingTable,
    health: &PoolHealth,
) -> Vec<TickAction> {
    // step 5 (checked first so a spent budget always wins over new work):
    // stop admitting, block the goal with the reason.
    if let Some(reason) = budget.exhausted() {
        // only block if there is still pending/ready work — a goal whose
        // nodes are all terminal should integrate, not block.
        if !graph.is_all_terminal() {
            return vec![TickAction::Block { reason }];
        }
    }

    // step 7: everything terminal → integrate (or fail if unrecoverable).
    if graph.is_all_terminal() {
        // a graph with a failed node that was never recovered is a failed
        // goal; the integrator decides recoverability, but if there is
        // nothing to integrate (all failed/blocked) short-circuit to Fail.
        let any_done = graph.nodes.iter().any(|n| n.status == NodeStatus::Done);
        if graph.has_failure() && !any_done {
            return vec![TickAction::Fail {
                reason: "every node failed; nothing to integrate".into(),
            }];
        }
        return vec![TickAction::RunIntegrator];
    }

    // step 2: ready-set. E28 spec §8: `ready_set_at` (not the unstamped
    // `ready_set`) so a node still benched/exhausted (`earliest_retry_at_ms`
    // in the future) doesn't get re-admitted every tick and spin.
    let mut ready: Vec<_> = graph.ready_set_at(budget.now.timestamp_millis());
    if ready.is_empty() {
        return vec![TickAction::Noop];
    }

    // step 4 ordering: critical path first (longest dependent chain), then
    // cheaper effort first, then node id for determinism.
    ready.sort_by(|a, b| {
        graph
            .critical_path_depth(&b.id)
            .cmp(&graph.critical_path_depth(&a.id))
            .then(effort_rank(a.effort).cmp(&effort_rank(b.effort)))
            .then(a.id.cmp(&b.id))
    });

    // step 3 + 4: admit up to global headroom and per-agent headroom.
    let global_headroom = cfg.max_parallel.saturating_sub(cap.global_running);
    if global_headroom == 0 {
        return vec![TickAction::Noop];
    }

    let mut actions = Vec::new();
    let mut admitted = 0usize;
    // track admissions made in THIS tick so two ready nodes routed to the
    // same capped agent don't both get admitted past its cap.
    let mut this_tick_per_agent: BTreeMap<String, usize> = BTreeMap::new();

    for node in ready {
        if admitted >= global_headroom {
            break;
        }
        let agent = match node.agent.is_empty() {
            false => node.agent.clone(),
            true => match routing::select_agent_with_prefer_pool(table, node.kind, node.effort, health, cfg.prefer_pool) {
                Some(a) => a,
                None => continue, // no agent available for this kind right now
            },
        };
        let used_here = this_tick_per_agent.get(&agent).copied().unwrap_or(0);
        if cap.agent_headroom(&agent).saturating_sub(used_here) == 0 {
            continue; // this agent is at capacity; leave the node ready
        }
        actions.push(TickAction::Dispatch {
            node_id: node.id.clone(),
            agent: agent.clone(),
            effort: node.effort,
            worktree: node.worktree,
            max_steps: table.max_steps(node.effort),
        });
        *this_tick_per_agent.entry(agent).or_insert(0) += 1;
        admitted += 1;
    }

    if actions.is_empty() {
        actions.push(TickAction::Noop);
    }
    actions
}

fn effort_rank(e: Effort) -> u8 {
    match e {
        Effort::Quick => 0,
        Effort::Standard => 1,
        Effort::Deep => 2,
    }
}

// ------------------------------------------------------- db + dispatch shell

use crate::context::Context;
use crate::coordinator::events::{self, EventKind};
use crate::coordinator::goal::{self, Goal};
use crate::coordinator::graph::GoalStatus;
use anyhow::{Context as _, Result};
use rusqlite::Connection;

/// how a `Dispatch` action turns into a real running task. the production
/// impl hands off to `task::run_background`; tests inject a fake that just
/// records a terminal `tasks` row, so `tick`'s db bookkeeping is testable
/// without spawning an agent.
pub trait Dispatcher {
    fn dispatch(&self, opts: crate::task::OwnedRunTaskOptions) -> Result<i64>;
}

pub struct RealDispatcher<'a> {
    pub ctx: &'a Context,
    pub registry: crate::registry::TaskRegistry,
}

impl Dispatcher for RealDispatcher<'_> {
    fn dispatch(&self, opts: crate::task::OwnedRunTaskOptions) -> Result<i64> {
        Ok(crate::task::run_background(self.ctx, opts, self.registry.clone())?.id)
    }
}

fn timeout_for(effort: Effort) -> Duration {
    match effort {
        Effort::Quick => Duration::from_secs(180),
        Effort::Standard => Duration::from_secs(420),
        Effort::Deep => Duration::from_secs(900),
    }
}

/// spec §4.1: on daemon start (and periodically), any coordinator node
/// left `running` whose backing `tasks` row is no longer live is
/// reconciled — `done` if the task actually completed; otherwise it was
/// interrupted (killed mid-run by a daemon restart/crash, not a real
/// agent failure), so it goes through the same `retry_decision` an
/// ordinary crash/timeout would: bounced back to `pending` with
/// `attempts + 1` while retries remain, `failed` only once exhausted.
/// Live-verification finding: this used to jump straight to `failed`
/// unconditionally, permanently dooming every node downstream of one that
/// happened to be running when the daemon restarted — `single goal
/// resume` re-ticks pending nodes but never revives a `failed` one, so
/// the goal stayed `running` forever with no dispatchable work. Returns
/// the number of nodes touched. This is the coordinator-node analogue of
/// `task::reconcile_orphaned_tasks`.
pub fn reconcile(conn: &Connection) -> Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT goal_id, id, task_id, attempts FROM graph_nodes WHERE status = 'running'",
    )?;
    let rows: Vec<(String, String, Option<i64>, u32)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut touched = 0;
    for (goal_id, node_id, task_id, attempts) in rows {
        let task_status: Option<String> = match task_id {
            Some(tid) => conn
                .query_row("SELECT status FROM tasks WHERE id = ?1", [tid], |r| r.get(0))
                .ok(),
            None => None,
        };
        match task_status.as_deref() {
            Some("running") | Some("created") => continue, // genuinely still live
            Some("completed") => {
                goal::update_node(conn, &goal_id, &node_id, NodeStatus::Done, None, None, None)?;
            }
            _ => match retry_decision(attempts, false) {
                RetryDecision::RetrySameNextAgent => {
                    goal::update_node(conn, &goal_id, &node_id, NodeStatus::Pending, None, None, Some(attempts + 1))?;
                    if let Ok(Some(g)) = goal::get(conn, &goal_id) {
                        let _ = events::append(
                            conn,
                            &g.session_id,
                            Some(&goal_id),
                            events::EventKind::NodeFailed,
                            &format!("{node_id}: interrupted, retry {} scheduled", attempts + 1),
                        );
                    }
                }
                RetryDecision::Supervisor | RetryDecision::GiveUp => {
                    goal::update_node(conn, &goal_id, &node_id, NodeStatus::Failed, None, None, None)?;
                }
            },
        }
        touched += 1;
    }
    if touched > 0 {
        tracing::warn!(count = touched, "reconciled interrupted coordinator nodes");
    }
    Ok(touched)
}

/// counts running coordinator nodes across every goal, per agent and
/// globally, and derives per-agent caps from the registry's
/// `max_concurrency`.
fn build_capacity(conn: &Connection, ctx: &Context) -> Result<Capacity> {
    let mut stmt = conn.prepare("SELECT agent, COUNT(*) FROM graph_nodes WHERE status = 'running' GROUP BY agent")?;
    let per_agent_running: BTreeMap<String, usize> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize)))?
        .collect::<rusqlite::Result<_>>()?;
    let global_running = per_agent_running.values().sum();
    let per_agent_cap = ctx
        .registry
        .iter()
        .filter_map(|a| a.max_concurrency.map(|c| (a.name.clone(), c as usize)))
        .collect();
    Ok(Capacity { global_running, per_agent_running, per_agent_cap })
}

fn goal_budget(goal: &Goal, cfg: &CoordinatorConfig) -> GoalBudget {
    GoalBudget {
        dispatches: goal.dispatches,
        max_dispatches: if goal.max_dispatches > 0 { goal.max_dispatches } else { cfg.max_dispatches_per_goal },
        started_at: goal.created_at.parse().unwrap_or_else(|_| Utc::now()),
        max_minutes: if goal.max_minutes > 0 { goal.max_minutes } else { cfg.max_goal_minutes },
        now: Utc::now(),
    }
}

/// runs one scheduling pass for every active goal, executing the pure
/// scheduler's decisions against the db and the given dispatcher. this is
/// the entrypoint the daemon timer, `GoalSubmit`, and `on_task_finished`
/// all call.
pub fn tick(
    ctx: &Context,
    conn: &mut Connection,
    cfg: &CoordinatorConfig,
    table: &RoutingTable,
    health: &PoolHealth,
    dispatcher: &dyn Dispatcher,
) -> Result<()> {
    // poll-based completion: the timer, not a callback, is what advances
    // the coordinator, so first settle every node whose backing task row
    // has gone terminal since the last tick.
    let finished: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT n.task_id FROM graph_nodes n JOIN tasks t ON t.id = n.task_id
             WHERE n.status = 'running' AND t.status IN ('completed','failed','cancelled')",
        )?;
        let ids = stmt.query_map([], |r| r.get::<_, i64>(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
        ids
    };
    for task_id in finished {
        settle_finished_node(ctx, conn, cfg, table, health, task_id)?;
    }

    for goal in goal::active(conn)? {
        // a goal still `planning` with no graph is the planner's job, not
        // the scheduler's — leave it (the handler kicks planning off).
        let graph = goal::load_graph(conn, &goal.id)?;
        if graph.nodes.is_empty() {
            continue;
        }
        if goal.status == GoalStatus::Planning {
            goal::set_status(conn, &goal.id, GoalStatus::Running)?;
        }

        let cap = build_capacity(conn, ctx)?;
        let budget = goal_budget(&goal, cfg);
        let actions = tick_pure(&graph, cfg, &cap, &budget, table, health);

        for action in actions {
            match action {
                TickAction::Noop => {}
                TickAction::Block { reason } => {
                    goal::set_blocked(conn, &goal.id, &reason)?;
                    events::append(conn, &goal.session_id, Some(&goal.id), EventKind::Blocked, &reason)?;
                }
                TickAction::Fail { reason } => {
                    goal::set_status(conn, &goal.id, GoalStatus::Failed)?;
                    events::append(conn, &goal.session_id, Some(&goal.id), EventKind::NodeFailed, &reason)?;
                }
                TickAction::RunIntegrator => {
                    run_integrator(ctx, conn, &goal, &graph, table, health)?;
                }
                TickAction::Dispatch { node_id, agent, effort, worktree, max_steps: _ } => {
                    let Some(node) = graph.find(&node_id) else { continue };
                    // E28 spec §8: a real re-admission out of
                    // `waiting_on_capacity` -- its retry stamp passed and
                    // `ready_set_at` let it back into this tick's ready
                    // set. Clear the hold and log the resume before the
                    // ordinary dispatch bookkeeping below.
                    if goal.status == GoalStatus::WaitingOnCapacity {
                        goal::clear_waiting_on_capacity(conn, &goal.id)?;
                        events::append(
                            conn,
                            &goal.session_id,
                            Some(&goal.id),
                            EventKind::CapacityResumed,
                            &format!("{node_id}: capacity window freed, resuming"),
                        )?;
                    }
                    let prompt = build_node_prompt(&graph, node);
                    let session_cwd = std::path::PathBuf::from(load_session_cwd(conn, &goal.session_id)?);
                    if !session_cwd.exists() {
                        // A session's cwd is resolved once at creation and
                        // reused verbatim on every tick; unlike a per-relay
                        // worktree (`prepare_shared_cwd`, recreated fresh
                        // each call) there's nothing here to regenerate --
                        // if it's gone (tmp cleanup, a manually deleted
                        // scratch dir, ...) every dispatch attempt would
                        // otherwise fail identically forever. Fail this
                        // node now with a clear reason instead of feeding
                        // the coordinator an infinite retry loop.
                        let reason = format!(
                            "{node_id}: session cwd {} no longer exists (stale/cleaned-up path)",
                            session_cwd.display()
                        );
                        goal::update_node(conn, &goal.id, &node_id, NodeStatus::Failed, None, None, None)?;
                        events::append(conn, &goal.session_id, Some(&goal.id), EventKind::NodeFailed, &reason)?;
                        continue;
                    }
                    let opts = crate::task::OwnedRunTaskOptions {
                        description: prompt,
                        agent: agent.clone(),
                        cwd: session_cwd,
                        use_worktree: worktree,
                        account: None,
                        real_home: false,
                        no_memory_context: false,
                        timeout: timeout_for(effort),
                        allow_fallback: true,
                        usage_json: cfg.usage_json_agents.iter().any(|a| a == &agent),
                        // `code` work nodes are the one graph-dispatched
                        // kind that needs real tool-calling — exclude
                        // aihorde-class free-pool providers (`no_tools`)
                        // the same way brain roles exclude them for JSON.
                        require_structured_output: node.kind == NodeKind::Code,
                    };
                    match dispatcher.dispatch(opts) {
                        Ok(task_id) => {
                            goal::update_node(conn, &goal.id, &node_id, NodeStatus::Running, Some(task_id), None, None)?;
                            goal::bump_dispatches(conn, &goal.id)?;
                            events::append(
                                conn,
                                &goal.session_id,
                                Some(&goal.id),
                                EventKind::NodeStarted,
                                &format!("{node_id} → {agent} (#{task_id})"),
                            )?;
                        }
                        Err(e) => {
                            goal::update_node(conn, &goal.id, &node_id, NodeStatus::Failed, None, None, None)?;
                            events::append(
                                conn,
                                &goal.session_id,
                                Some(&goal.id),
                                EventKind::NodeFailed,
                                &format!("{node_id}: dispatch failed: {e}"),
                            )?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// spec §4.6: called after a coordinator-backed task finishes. maps the
/// task row back to its node, records success / failure, applies the retry
/// decision, and re-ticks. a no-op when `task_id` is not a coordinator
/// node (so the handler can call it blindly after any task).
pub fn on_task_finished(
    ctx: &Context,
    conn: &mut Connection,
    cfg: &CoordinatorConfig,
    table: &RoutingTable,
    health: &PoolHealth,
    dispatcher: &dyn Dispatcher,
    task_id: i64,
) -> Result<()> {
    settle_finished_node(ctx, conn, cfg, table, health, task_id)?;
    tick(ctx, conn, cfg, table, health, dispatcher)
}

/// records the outcome of one finished coordinator-backed task against its
/// node and runs retry / supervisor / block, WITHOUT re-ticking. a no-op
/// when `task_id` is not a running coordinator node.
#[allow(clippy::too_many_arguments)]
fn settle_finished_node(
    ctx: &Context,
    conn: &mut Connection,
    cfg: &CoordinatorConfig,
    table: &RoutingTable,
    health: &PoolHealth,
    task_id: i64,
) -> Result<()> {
    let row: Option<(String, String, u32)> = conn
        .query_row(
            "SELECT goal_id, id, attempts FROM graph_nodes WHERE task_id = ?1 AND status = 'running'",
            [task_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let Some((goal_id, node_id, attempts)) = row else {
        return Ok(());
    };

    let task = crate::task::get(conn, task_id)?;
    let (completed, rate_limited, artifact) = match &task {
        Some(t) => (
            matches!(t.status, single_protocol::TaskStatus::Completed),
            t.rate_limited,
            t.artifact_path.clone(),
        ),
        None => (false, false, None),
    };
    let goal = goal::get(conn, &goal_id)?.context("goal vanished mid-run")?;

    // `careful` mode (`single loop`): re-dispatch the single node with the
    // previous output appended until the agent emits a lone `DONE` line or
    // the iteration cap (`max_dispatches`) is spent. Runs before the normal
    // success/failure handling.
    if goal.mode == crate::coordinator::graph::GoalMode::Careful {
        return settle_careful_node(conn, &goal, &node_id, artifact.as_deref());
    }

    if completed {
        goal::update_node(conn, &goal_id, &node_id, NodeStatus::Done, None, artifact.as_deref(), None)?;
        events::append(conn, &goal.session_id, Some(&goal_id), EventKind::NodeDone, &node_id)?;
        maybe_auto_merge(conn, &goal, &node_id)?;
    } else if rate_limited {
        // E28 spec §8 (Part D, auto-continue): a `pool_agent::Exhausted`
        // outcome (Task 14 mapped it onto this same `rate_limited` signal)
        // or a CLI agent's fallback chain fully rate-limited -- either way
        // the node isn't broken, capacity is just spent. Never falls
        // through to the supervisor/failed path below.
        handle_capacity_exhaustion(conn, &goal, &node_id, artifact.as_deref(), cfg)?;
    } else {
        let semantic = false; // crash/timeout, not a wrong result (rate-limit handled above)
        match retry_decision(attempts, semantic) {
            RetryDecision::RetrySameNextAgent => {
                // bounce the node back to pending with an incremented
                // attempt count; the next tick re-routes it (select_agent
                // walks past the now-known-bad agent via pool health).
                goal::update_node(
                    conn,
                    &goal_id,
                    &node_id,
                    NodeStatus::Pending,
                    None,
                    None,
                    Some(attempts + 1),
                )?;
                events::append(
                    conn,
                    &goal.session_id,
                    Some(&goal_id),
                    EventKind::NodeFailed,
                    &format!("{node_id}: retry {} scheduled", attempts + 1),
                )?;
            }
            _ => {
                goal::update_node(conn, &goal_id, &node_id, NodeStatus::Failed, None, artifact.as_deref(), None)?;
                events::append(
                    conn,
                    &goal.session_id,
                    Some(&goal_id),
                    EventKind::NodeFailed,
                    &format!("{node_id}: exhausted retries → supervisor/failed"),
                )?;
                run_supervisor_or_block(ctx, conn, &goal, &node_id, table, health)?;
            }
        }
    }

    Ok(())
}

/// E28 spec §8: a node's dispatch was rate-limited/exhausted. Bounded by
/// `max_capacity_waits_per_goal` (or a per-goal `capacity-budget=N`
/// override) and `max_capacity_wait_minutes` (wall clock since the goal
/// was created) — past either, the goal finally gives up to `Blocked`
/// rather than waiting forever.
fn handle_capacity_exhaustion(conn: &Connection, goal: &Goal, node_id: &str, artifact: Option<&str>, cfg: &CoordinatorConfig) -> Result<()> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let precise_recovery_ms = artifact.and_then(|p| std::fs::read_to_string(p).ok()).and_then(|text| parse_earliest_recovery_ms(&text));

    let max_waits = goal.capacity_budget_override.unwrap_or(cfg.max_capacity_waits_per_goal);
    let max_wait_minutes = goal.capacity_wait_minutes_override.unwrap_or(cfg.max_capacity_wait_minutes);
    let elapsed_minutes = (chrono::Utc::now() - goal.created_at.parse().unwrap_or_else(|_| Utc::now())).num_minutes();

    if goal.capacity_waits >= max_waits || elapsed_minutes >= max_wait_minutes as i64 {
        // Live-verification finding (2026-09-11/12): `elapsed_minutes` is
        // measured from `goal.created_at`, not from the goal's last
        // resume — so `single goal resume` on a goal that has simply been
        // sitting blocked for days re-trips this exact same wall-clock
        // check on the very next tick, before the pool ever gets a
        // chance to actually retry dispatch. The fix for a stuck-for-days
        // goal is `single goal amend <id> capacity-minutes=<N>` (a
        // *separate* field from `amend ... minutes=<N>`, which raises the
        // goal's overall time budget, not this capacity-wait one) — spent
        // real time confused between the two before finding
        // `capacity_wait_minutes_override` here, so the blocked reason
        // now says exactly which knob to turn instead of leaving that to
        // be rediscovered by reading this function's source.
        let reason = format!(
            "waited {:.1}h for capacity, still exhausted (raise via `single goal amend {} capacity-minutes=<N>`, not `minutes=`)",
            elapsed_minutes as f64 / 60.0,
            goal.id
        );
        goal::set_blocked(conn, &goal.id, &reason)?;
        events::append(conn, &goal.session_id, Some(&goal.id), EventKind::Blocked, &reason)?;
        return Ok(());
    }

    // A precise stamp (`earliest_recovery_ms` from the artifact) means
    // single-pool itself gave up after trying every provider it has --
    // there's nothing else to fail over to, so honor its ETA verbatim.
    // No precise stamp means a single CLI agent's own rate limit tripped
    // (grok, claude, codex, ...), not the whole pool -- retrying that same
    // pinned agent every cycle is exactly the wedge this function used to
    // cause (a node stayed bound to its dead agent forever, re-blocking
    // the goal on every self-heal reeval). Clear the pin instead so the
    // next tick's `select_agent` routes past the just-exhausted agent
    // (tracked via `PoolHealth::rate_limited`) onto the next available
    // agent/provider, with only a short buffer to avoid a tight retry
    // loop. Only once every candidate is genuinely exhausted does the
    // goal actually reach the `Blocked` branch above.
    let (earliest_recovery_ms, reason) = match precise_recovery_ms {
        Some(ms) => {
            let eta = chrono::DateTime::from_timestamp_millis(ms).map(|d| d.to_rfc3339()).unwrap_or_default();
            (ms, format!("{node_id}: every routable candidate exhausted, resumes ~{eta}"))
        }
        None => {
            goal::clear_node_agent_pin(conn, &goal.id, node_id)?;
            (now_ms + 15_000, format!("{node_id}: exhausted, rerouting to next available agent/provider"))
        }
    };

    goal::set_waiting_on_capacity(conn, &goal.id, &reason, earliest_recovery_ms)?;
    goal::stamp_node_retry(conn, &goal.id, node_id, Some(earliest_recovery_ms))?;
    events::append(conn, &goal.session_id, Some(&goal.id), EventKind::CapacityWait, &reason)?;
    Ok(())
}

/// Parses the `earliest recovery at <ms>` marker `pool_agent::run_as_task`
/// writes into a task's artifact on `Exhausted`. `None` for anything else
/// (a CLI agent's ordinary rate-limit text) -- the caller falls back to a
/// heuristic hold in that case.
fn parse_earliest_recovery_ms(text: &str) -> Option<i64> {
    let marker = "earliest recovery at ";
    let pos = text.find(marker)?;
    let rest = &text[pos + marker.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// One iteration step for a `careful` (`single loop`) goal. `prev_artifact`
/// is the just-finished task's artifact path.
fn settle_careful_node(
    conn: &mut Connection,
    goal: &Goal,
    node_id: &str,
    prev_artifact: Option<&str>,
) -> Result<()> {
    let output = prev_artifact
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let done = output
        .lines()
        .any(|l| l.trim() == crate::coordinator::CAREFUL_DONE_LINE);
    let iter = goal.dispatches; // one dispatch per iteration

    if done {
        goal::update_node(conn, &goal.id, node_id, NodeStatus::Done, None, prev_artifact, None)?;
        goal::set_summary(conn, &goal.id, &crate::orchestrate::truncate(&output, 2000))?;
        goal::set_status(conn, &goal.id, GoalStatus::Done)?;
        events::append(
            conn,
            &goal.session_id,
            Some(&goal.id),
            EventKind::Integrated,
            &format!("loop finished (DONE) after {iter} iteration(s)"),
        )?;
        return Ok(());
    }

    if iter >= goal.max_dispatches {
        goal::update_node(conn, &goal.id, node_id, NodeStatus::Done, None, prev_artifact, None)?;
        goal::set_summary(
            conn,
            &goal.id,
            &format!(
                "stopped after {iter} iteration(s) without a DONE. last output:\n{}",
                crate::orchestrate::truncate(&output, 2000)
            ),
        )?;
        goal::set_status(conn, &goal.id, GoalStatus::Done)?;
        events::append(
            conn,
            &goal.session_id,
            Some(&goal.id),
            EventKind::Integrated,
            &format!("loop stopped at the {iter}-iteration cap (no DONE)"),
        )?;
        return Ok(());
    }

    // next iteration: feed this step's output back in, reset the node.
    let next_prompt = format!(
        "{}\n\n--- previous step output ---\n{}\n\n--- continue ---\n\
         Do the next concrete piece of work toward the goal. When the goal is fully \
         complete, reply with a line containing only DONE.",
        goal.text,
        crate::orchestrate::truncate(&output, 4000),
    );
    goal::set_node_desc(conn, &goal.id, node_id, &next_prompt)?;
    goal::update_node(conn, &goal.id, node_id, NodeStatus::Pending, None, None, Some(iter + 1))?;
    events::append(
        conn,
        &goal.session_id,
        Some(&goal.id),
        EventKind::Message,
        &format!("iteration {}", iter + 1),
    )?;
    Ok(())
}

fn run_supervisor_or_block(
    ctx: &Context,
    conn: &mut Connection,
    goal: &Goal,
    failing_node_id: &str,
    table: &RoutingTable,
    health: &PoolHealth,
) -> Result<()> {
    let cfg = CoordinatorConfig::default();
    if goal.supervisor_patches >= cfg.max_supervisor_patches {
        let reason = format!(
            "tried {} supervisor fixes on this goal; need a decision. last failure at node {failing_node_id}",
            goal.supervisor_patches
        );
        goal::set_blocked(conn, &goal.id, &reason)?;
        events::append(conn, &goal.session_id, Some(&goal.id), EventKind::Blocked, &reason)?;
        return Ok(());
    }

    let mut graph = goal::load_graph(conn, &goal.id)?;
    let failing_output = graph
        .find(failing_node_id)
        .and_then(|n| n.output_ref.clone())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let cwd = std::path::PathBuf::from(load_session_cwd(conn, &goal.session_id)?);

    match crate::coordinator::brain::supervise(
        conn, ctx, &cwd, &graph, failing_node_id, &failing_output, table, health,
    ) {
        Ok(ops) => match graph.apply_patch(&ops) {
            Ok(()) => {
                goal::save_graph(conn, &goal.id, &graph)?;
                goal::bump_supervisor_patches(conn, &goal.id)?;
                events::append(
                    conn,
                    &goal.session_id,
                    Some(&goal.id),
                    EventKind::Supervisor,
                    &format!("applied {} patch op(s)", ops.len()),
                )?;
            }
            Err(e) => {
                let reason = format!("supervisor patch could not be applied: {e}");
                goal::set_status(conn, &goal.id, GoalStatus::Failed)?;
                events::append(conn, &goal.session_id, Some(&goal.id), EventKind::NodeFailed, &reason)?;
            }
        },
        Err(e) => {
            let reason = format!("supervisor produced no usable patch: {e}");
            goal::set_blocked(conn, &goal.id, &reason)?;
            events::append(conn, &goal.session_id, Some(&goal.id), EventKind::Blocked, &reason)?;
        }
    }
    Ok(())
}

fn run_integrator(
    ctx: &Context,
    conn: &mut Connection,
    goal: &Goal,
    graph: &TaskGraph,
    table: &RoutingTable,
    health: &PoolHealth,
) -> Result<()> {
    let cwd = std::path::PathBuf::from(load_session_cwd(conn, &goal.session_id)?);
    let outputs: Vec<(String, String)> = graph
        .nodes
        .iter()
        .map(|n| {
            let body = n
                .output_ref
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default();
            (n.id.clone(), body)
        })
        .collect();

    match crate::coordinator::brain::integrate(conn, ctx, &cwd, &goal.text, &outputs, table, health) {
        Ok(outcome) => {
            goal::set_summary(conn, &goal.id, &outcome.summary)?;
            let final_status = if outcome.unrecoverable { GoalStatus::Failed } else { GoalStatus::Done };
            goal::set_status(conn, &goal.id, final_status)?;
            events::append(
                conn,
                &goal.session_id,
                Some(&goal.id),
                EventKind::Integrated,
                &serde_json::to_string(&outcome).unwrap_or_else(|_| outcome.summary.clone()),
            )?;
        }
        Err(e) => {
            let reason = format!("integrator failed: {e}");
            goal::set_blocked(conn, &goal.id, &reason)?;
            events::append(conn, &goal.session_id, Some(&goal.id), EventKind::Blocked, &reason)?;
        }
    }
    Ok(())
}

/// feeds a node its dependencies' outputs and its siblings' current status
/// alongside its own description.
fn build_node_prompt(graph: &TaskGraph, node: &Node) -> String {
    let mut deps = String::new();
    for dep_id in &node.depends_on {
        if let Some(dep) = graph.find(dep_id) {
            if let Some(out) = dep.output_ref.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
                deps.push_str(&format!("\n--- output of {dep_id} ---\n{}\n", crate::orchestrate::truncate(&out, 2000)));
            }
        }
    }
    let mut prompt = node.desc.clone();
    if !deps.is_empty() {
        prompt.push_str(&format!("\n\nUPSTREAM RESULTS:{deps}"));
    }
    let siblings = graph.sibling_status(&node.id);
    if !siblings.is_empty() {
        let list: String = siblings.iter().map(|(id, status)| format!("\n- {id}: {}", status.as_str())).collect();
        prompt.push_str(&format!(
            "\n\nSIBLING NODE STATUS (read-only, for context — you cannot affect these):{list}"
        ));
    }
    prompt
}

/// opt-in auto-merge (goal-improvement #1): when a `review`-kind node
/// finishes `Done` (a passing reviewer/verifier — a `Failed` review never
/// reaches here, since only the `completed` branch of `settle_finished_node`
/// calls this) on a goal that set `auto_merge`, this used to merge every
/// worktree-backed dependency of that node straight in.
///
/// Live-verification finding (this goal's own review step): an upfront
/// `auto_merge` flag set when the goal was created doesn't give a human
/// visibility into the *actual diff* at the moment it lands, later, after
/// arbitrary other work has happened — a real contradiction of
/// `docs/architecture.md`'s "branches are never auto-merged; that stays a
/// human decision" invariant, not just an implementation nit. Fixed:
/// `auto_merge` now means "eligible to be offered a merge confirmation
/// once review passes", not "skip human review". This function requests
/// confirmation via `single_core::pending_merge` and stops — it never
/// calls `worktree::merge` itself. Only `single goal merge confirm`
/// (`Request::GoalMergeResolve`, `allow: true`) does, after a human has
/// been shown the real diff (`single goal merge show`, backed by
/// `worktree::diff`). A no-op for every other goal/node shape, so this
/// changes nothing unless a human explicitly opted in.
fn maybe_auto_merge(conn: &Connection, goal: &Goal, node_id: &str) -> Result<()> {
    if !goal.auto_merge {
        return Ok(());
    }
    let graph = goal::load_graph(conn, &goal.id)?;
    let Some(node) = graph.find(node_id) else { return Ok(()) };
    if node.kind != NodeKind::Review {
        return Ok(());
    }

    // Confirm this goal is actually a git repo before queueing anything —
    // a real branch to confirm requires a real repo root, even though
    // confirm-time (`GoalMergeResolve`) resolves it fresh rather than
    // trusting a value captured here.
    if single_core::project_context::resolve(std::path::Path::new(&load_session_cwd(conn, &goal.session_id)?)).repo_root.is_none() {
        return Ok(()); // not a git repo at all — nothing worktree-backed to merge
    }

    for dep_id in &node.depends_on {
        let Some(dep) = graph.find(dep_id) else { continue };
        if !dep.worktree || dep.status != NodeStatus::Done {
            continue;
        }
        let Some(task_id) = dep.task_id else { continue };
        let branch = format!("single/task-{task_id}");
        let pending_id = single_core::pending_merge::request(conn, &goal.id, &goal.session_id, node_id, dep_id, &branch)?;
        events::append(
            conn,
            &goal.session_id,
            Some(&goal.id),
            EventKind::MergeAwaitingConfirmation,
            &format!("{dep_id} ({branch}) awaiting human merge confirmation after {node_id} passed review — see `single goal merge show {pending_id}`"),
        )?;
    }
    Ok(())
}

fn load_session_cwd(conn: &Connection, session_id: &str) -> Result<String> {
    Ok(crate::coordinator::session::get(conn, session_id)?
        .map(|s| s.cwd)
        .unwrap_or_else(|| ".".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::graph::{Effort, Node, NodeKind, NodeStatus};

    fn cfg(max_parallel: usize) -> CoordinatorConfig {
        CoordinatorConfig { max_parallel, ..Default::default() }
    }

    fn budget_ok() -> GoalBudget {
        let now = Utc::now();
        GoalBudget { dispatches: 0, max_dispatches: 25, started_at: now, max_minutes: 60, now }
    }

    fn node(id: &str, deps: &[&str], effort: Effort, agent: &str) -> Node {
        Node {
            id: id.into(),
            desc: id.into(),
            kind: NodeKind::Code,
            effort,
            agent: agent.into(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            status: NodeStatus::Pending,
            task_id: None,
            attempts: 0,
            worktree: false,
            output_ref: None,
            earliest_retry_at_ms: None,
        }
    }

    fn caps(global_running: usize, agent_caps: &[(&str, usize)], agent_running: &[(&str, usize)]) -> Capacity {
        Capacity {
            global_running,
            per_agent_cap: agent_caps.iter().map(|(a, c)| (a.to_string(), *c)).collect(),
            per_agent_running: agent_running.iter().map(|(a, c)| (a.to_string(), *c)).collect(),
        }
    }

    fn dispatched_ids(actions: &[TickAction]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|a| match a {
                TickAction::Dispatch { node_id, .. } => Some(node_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn tick_pure_honors_coordinator_config_prefer_pool() {
        // Regression test: `select_agent_with_prefer_pool` existed but
        // `tick_pure` (the actual per-tick admission path) called plain
        // `select_agent` and silently ignored `cfg.prefer_pool` --
        // confirmed live against the real daemon before this fix (a
        // `prefer_pool = true` goal still routed to `opencode`).
        let mut n = node("s1", &[], Effort::Standard, "");
        n.kind = NodeKind::Code;
        let g = TaskGraph { nodes: vec![n] };
        // opencode detected+authed; single-pool needs no such entry (PoolHealth::usable's carve-out).
        let health = PoolHealth { detected_authed: ["opencode".to_string()].into_iter().collect(), rate_limited: Default::default() };

        let cfg_off = cfg(6);
        let a = tick_pure(&g, &cfg_off, &caps(0, &[], &[]), &budget_ok(), &RoutingTable::default(), &health);
        match &a[0] {
            TickAction::Dispatch { agent, .. } => assert_eq!(agent, "opencode"),
            other => panic!("expected Dispatch to opencode, got {other:?}"),
        }

        let cfg_on = CoordinatorConfig { prefer_pool: true, ..cfg(6) };
        let a2 = tick_pure(&g, &cfg_on, &caps(0, &[], &[]), &budget_ok(), &RoutingTable::default(), &health);
        match &a2[0] {
            TickAction::Dispatch { agent, .. } => assert_eq!(agent, "single-pool"),
            other => panic!("expected Dispatch to single-pool, got {other:?}"),
        }
    }

    #[test]
    fn admits_independent_nodes_up_to_global_cap() {
        let g = TaskGraph {
            nodes: vec![
                node("s1", &[], Effort::Standard, "grok"),
                node("s2", &[], Effort::Standard, "grok"),
                node("s3", &[], Effort::Standard, "grok"),
                node("s4", &[], Effort::Standard, "grok"),
            ],
        };
        let a = tick_pure(&g, &cfg(2), &caps(0, &[], &[]), &budget_ok(), &RoutingTable::default(), &PoolHealth::default());
        assert_eq!(dispatched_ids(&a).len(), 2);
    }

    #[test]
    fn respects_per_agent_cap() {
        let g = TaskGraph {
            nodes: vec![
                node("s1", &[], Effort::Standard, "opencode"),
                node("s2", &[], Effort::Standard, "opencode"),
                node("s3", &[], Effort::Standard, "opencode"),
            ],
        };
        let a = tick_pure(
            &g,
            &cfg(6),
            &caps(0, &[("opencode", 1)], &[]),
            &budget_ok(),
            &RoutingTable::default(),
            &PoolHealth::default(),
        );
        assert_eq!(dispatched_ids(&a), vec!["s1"]);
    }

    #[test]
    fn prefers_critical_path_then_cheaper_effort() {
        // s1 -> s2 -> s3 (depth 2), plus lone s4 (deep) and lone s5 (quick).
        let g = TaskGraph {
            nodes: vec![
                node("s1", &[], Effort::Standard, "grok"),
                node("s2", &["s1"], Effort::Standard, "grok"),
                node("s3", &["s2"], Effort::Standard, "grok"),
                node("s4", &[], Effort::Deep, "grok"),
                node("s5", &[], Effort::Quick, "grok"),
            ],
        };
        // global cap 1 → only the single most-preferred node dispatches.
        let a = tick_pure(&g, &cfg(1), &caps(0, &[], &[]), &budget_ok(), &RoutingTable::default(), &PoolHealth::default());
        assert_eq!(dispatched_ids(&a), vec!["s1"]); // critical path wins

        // remove the chain; now effort breaks the tie: s5 (quick) before s4 (deep).
        let g2 = TaskGraph { nodes: vec![node("s4", &[], Effort::Deep, "grok"), node("s5", &[], Effort::Quick, "grok")] };
        let a2 = tick_pure(&g2, &cfg(1), &caps(0, &[], &[]), &budget_ok(), &RoutingTable::default(), &PoolHealth::default());
        assert_eq!(dispatched_ids(&a2), vec!["s5"]);
    }

    #[test]
    fn queues_when_pool_saturated() {
        let g = TaskGraph { nodes: vec![node("s1", &[], Effort::Standard, "grok")] };
        let a = tick_pure(&g, &cfg(2), &caps(2, &[], &[]), &budget_ok(), &RoutingTable::default(), &PoolHealth::default());
        assert_eq!(a, vec![TickAction::Noop]);
    }

    #[test]
    fn tick_skips_node_whose_retry_stamp_is_in_the_future() {
        let mut n = node("s1", &[], Effort::Standard, "grok");
        let mut budget = budget_ok();
        n.earliest_retry_at_ms = Some(budget.now.timestamp_millis() + 60_000); // 1 min from now
        let g = TaskGraph { nodes: vec![n] };
        let a = tick_pure(&g, &cfg(6), &caps(0, &[], &[]), &budget, &RoutingTable::default(), &PoolHealth::default());
        assert_eq!(a, vec![TickAction::Noop]);

        // advance past the stamp -> re-admitted.
        budget.now += chrono::Duration::seconds(61);
        let a2 = tick_pure(&g, &cfg(6), &caps(0, &[], &[]), &budget, &RoutingTable::default(), &PoolHealth::default());
        assert_eq!(dispatched_ids(&a2), vec!["s1"]);
    }

    #[test]
    fn tick_readmits_node_once_retry_stamp_passes() {
        let mut n = node("s1", &[], Effort::Standard, "grok");
        let budget = budget_ok();
        n.earliest_retry_at_ms = Some(budget.now.timestamp_millis() - 1000); // already past
        let g = TaskGraph { nodes: vec![n] };
        let a = tick_pure(&g, &cfg(6), &caps(0, &[], &[]), &budget, &RoutingTable::default(), &PoolHealth::default());
        assert_eq!(dispatched_ids(&a), vec!["s1"]);
    }

    #[test]
    fn budget_dispatch_cap_blocks_the_goal() {
        let g = TaskGraph { nodes: vec![node("s1", &[], Effort::Standard, "grok")] };
        let now = Utc::now();
        let b = GoalBudget { dispatches: 25, max_dispatches: 25, started_at: now, max_minutes: 60, now };
        let a = tick_pure(&g, &cfg(6), &caps(0, &[], &[]), &b, &RoutingTable::default(), &PoolHealth::default());
        match &a[0] {
            TickAction::Block { reason } => assert!(reason.contains("dispatch budget")),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn budget_wallclock_cap_blocks_the_goal() {
        let g = TaskGraph { nodes: vec![node("s1", &[], Effort::Standard, "grok")] };
        let now = Utc::now();
        let b = GoalBudget {
            dispatches: 1,
            max_dispatches: 25,
            started_at: now - chrono::Duration::minutes(90),
            max_minutes: 60,
            now,
        };
        let a = tick_pure(&g, &cfg(6), &caps(0, &[], &[]), &b, &RoutingTable::default(), &PoolHealth::default());
        match &a[0] {
            TickAction::Block { reason } => assert!(reason.contains("time budget")),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn all_terminal_triggers_integrator() {
        let mut g = TaskGraph { nodes: vec![node("s1", &[], Effort::Standard, "grok")] };
        g.nodes[0].status = NodeStatus::Done;
        let a = tick_pure(&g, &cfg(6), &caps(0, &[], &[]), &budget_ok(), &RoutingTable::default(), &PoolHealth::default());
        assert_eq!(a, vec![TickAction::RunIntegrator]);
    }

    #[test]
    fn all_failed_and_nothing_done_fails_the_goal() {
        let mut g = TaskGraph { nodes: vec![node("s1", &[], Effort::Standard, "grok")] };
        g.nodes[0].status = NodeStatus::Failed;
        let a = tick_pure(&g, &cfg(6), &caps(0, &[], &[]), &budget_ok(), &RoutingTable::default(), &PoolHealth::default());
        assert!(matches!(a[0], TickAction::Fail { .. }));
    }

    #[test]
    fn retry_decision_advances_agent_then_escalates_to_supervisor() {
        assert_eq!(retry_decision(0, false), RetryDecision::RetrySameNextAgent);
        assert_eq!(retry_decision(1, false), RetryDecision::RetrySameNextAgent);
        assert_eq!(retry_decision(2, false), RetryDecision::Supervisor);
        assert_eq!(retry_decision(0, true), RetryDecision::Supervisor); // semantic failure jumps straight to supervisor
    }

    #[test]
    fn reconcile_retries_dead_task_with_attempts_left_and_marks_completed_as_done() {
        use crate::coordinator::goal;
        use crate::coordinator::graph::GoalMode;

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();

        let s = crate::coordinator::session::new_session(&conn, std::path::Path::new("/tmp/p")).unwrap();
        let g = goal::create(&conn, &s.id, "g", GoalMode::Auto, 25, 60).unwrap();
        let graph = TaskGraph {
            nodes: vec![
                node("s1", &[], Effort::Standard, "grok"),
                node("s2", &[], Effort::Standard, "grok"),
                node("s3", &[], Effort::Standard, "grok"),
            ],
        };
        goal::save_graph(&mut conn, &g.id, &graph).unwrap();

        // s1 -> a task row killed mid-run (e.g. daemon restart), attempts
        // still under the retry cap; s2 -> a task row still RUNNING;
        // s3 -> a task row that COMPLETED.
        for (nid, tid, status) in [("s1", 10i64, "failed"), ("s2", 11, "running"), ("s3", 12, "completed")] {
            conn.execute(
                "INSERT INTO tasks (id, description, agent, status, timed_out, created_at, updated_at, cwd, workspace_id)
                 VALUES (?1, 'x', 'grok', ?2, 0, '', '', '', '')",
                rusqlite::params![tid, status],
            )
            .unwrap();
            goal::update_node(&conn, &g.id, nid, NodeStatus::Running, Some(tid), None, None).unwrap();
        }

        let touched = reconcile(&conn).unwrap();
        assert_eq!(touched, 2); // s1 and s3 move; s2 stays running

        let reloaded = goal::load_graph(&conn, &g.id).unwrap();
        // interrupted, not genuinely broken -- bounced back to pending for
        // a retry instead of permanently failed.
        assert_eq!(reloaded.find("s1").unwrap().status, NodeStatus::Pending);
        assert_eq!(reloaded.find("s1").unwrap().attempts, 1);
        assert_eq!(reloaded.find("s2").unwrap().status, NodeStatus::Running);
        assert_eq!(reloaded.find("s3").unwrap().status, NodeStatus::Done);
    }

    #[test]
    fn reconcile_marks_dead_task_failed_once_retries_are_exhausted() {
        use crate::coordinator::goal;
        use crate::coordinator::graph::GoalMode;

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();

        let s = crate::coordinator::session::new_session(&conn, std::path::Path::new("/tmp/p")).unwrap();
        let g = goal::create(&conn, &s.id, "g", GoalMode::Auto, 25, 60).unwrap();
        let mut n = node("s1", &[], Effort::Standard, "grok");
        n.attempts = 2; // already exhausted RetrySameNextAgent's budget
        goal::save_graph(&mut conn, &g.id, &TaskGraph { nodes: vec![n] }).unwrap();

        conn.execute(
            "INSERT INTO tasks (id, description, agent, status, timed_out, created_at, updated_at, cwd, workspace_id)
             VALUES (10, 'x', 'grok', 'failed', 0, '', '', '', '')",
            [],
        )
        .unwrap();
        goal::update_node(&conn, &g.id, "s1", NodeStatus::Running, Some(10), None, None).unwrap();
        // update_node's attempts arg is only touched on an explicit Some(_);
        // set it directly to simulate a node already on its last attempt.
        conn.execute("UPDATE graph_nodes SET attempts = 2 WHERE goal_id = ?1 AND id = 's1'", [&g.id]).unwrap();

        let touched = reconcile(&conn).unwrap();
        assert_eq!(touched, 1);
        let reloaded = goal::load_graph(&conn, &g.id).unwrap();
        assert_eq!(reloaded.find("s1").unwrap().status, NodeStatus::Failed);
    }

    // ---- E28 spec §8: auto-continue on capacity exhaustion ----

    fn test_ctx(root: &std::path::Path) -> crate::context::Context {
        let dirs = single_core::SingleDirs::from_root(root.to_path_buf());
        dirs.ensure_created().unwrap();
        crate::context::Context { dirs, resolved: single_core::ResolvedConfig::default(), registry: single_core::builtin_registry() }
    }

    /// Sets up one goal with a single node whose backing task row is
    /// `failed` + `rate_limited = 1` (the same shape `task::execute`
    /// leaves after a `pool_agent::Exhausted` outcome, per Task 14), with
    /// `artifact_path` pointing at a file containing the exact "earliest
    /// recovery at <ms>" marker `pool_agent::run_as_task` writes.
    fn rate_limited_goal(conn: &mut rusqlite::Connection, dir: &std::path::Path, earliest_recovery_ms: Option<i64>) -> (crate::coordinator::goal::Goal, i64) {
        use crate::coordinator::{goal, graph::GoalMode};
        let s = crate::coordinator::session::new_session(conn, dir).unwrap();
        let g = goal::create(conn, &s.id, "g", GoalMode::Auto, 25, 60).unwrap();
        let graph = TaskGraph { nodes: vec![node("s1", &[], Effort::Standard, "single-pool")] };
        goal::save_graph(conn, &g.id, &graph).unwrap();

        let artifact_path = dir.join("artifact.txt");
        let body = match earliest_recovery_ms {
            Some(ms) => format!("single-pool: rate limited — every keyed provider is exhausted or benched, earliest recovery at {ms}"),
            None => "some CLI agent's rate-limit text with no parseable marker".to_string(),
        };
        std::fs::write(&artifact_path, &body).unwrap();

        let tid = 900i64;
        conn.execute(
            "INSERT INTO tasks (id, description, agent, status, timed_out, created_at, updated_at, cwd, workspace_id, rate_limited, artifact_path)
             VALUES (?1, 'x', 'single-pool', 'failed', 0, '', '', '', '', 1, ?2)",
            rusqlite::params![tid, artifact_path.display().to_string()],
        )
        .unwrap();
        goal::update_node(conn, &g.id, "s1", NodeStatus::Running, Some(tid), None, None).unwrap();
        (g, tid)
    }

    #[test]
    fn exhausted_dispatch_moves_node_to_pending_with_retry_stamp_and_goal_to_waiting_on_capacity() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();

        let earliest_ms = Utc::now().timestamp_millis() + 90_000;
        let (g, tid) = rate_limited_goal(&mut conn, tmp.path(), Some(earliest_ms));

        settle_finished_node(&ctx, &mut conn, &CoordinatorConfig::default(), &RoutingTable::default(), &PoolHealth::default(), tid).unwrap();

        let reloaded_goal = crate::coordinator::goal::get(&conn, &g.id).unwrap().unwrap();
        assert_eq!(reloaded_goal.status, crate::coordinator::graph::GoalStatus::WaitingOnCapacity);
        assert_eq!(reloaded_goal.earliest_retry_at_ms, Some(earliest_ms));
        assert_eq!(reloaded_goal.capacity_waits, 1);
        assert!(reloaded_goal.capacity_reason.unwrap().contains("s1"));

        let reloaded_graph = crate::coordinator::goal::load_graph(&conn, &g.id).unwrap();
        let n = reloaded_graph.find("s1").unwrap();
        assert_eq!(n.status, NodeStatus::Pending);
        assert_eq!(n.earliest_retry_at_ms, Some(earliest_ms));
    }

    #[test]
    fn exhausted_dispatch_falls_back_to_heuristic_hold_when_no_marker_present() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();

        let before = Utc::now().timestamp_millis();
        let (g, tid) = rate_limited_goal(&mut conn, tmp.path(), None); // a CLI agent, no marker
        settle_finished_node(&ctx, &mut conn, &CoordinatorConfig::default(), &RoutingTable::default(), &PoolHealth::default(), tid).unwrap();

        let reloaded_goal = crate::coordinator::goal::get(&conn, &g.id).unwrap().unwrap();
        assert_eq!(reloaded_goal.status, crate::coordinator::graph::GoalStatus::WaitingOnCapacity);
        // heuristic hold is ~5 minutes out, not authoritative -- just confirm it's in the future.
        assert!(reloaded_goal.earliest_retry_at_ms.unwrap() > before);
    }

    #[test]
    fn exhausted_dispatch_on_pinned_cli_agent_clears_pin_for_failover() {
        // Live-verification regression: a node pinned to a specific CLI
        // agent (e.g. `grok`) that goes rate-limited with no precise
        // pool-internal recovery marker used to keep retrying that same
        // dead agent forever — the pin was never cleared, so every
        // subsequent tick (and every self-heal `reeval_blocked_goals`
        // resume) just hit the same exhausted candidate again. It should
        // instead fail over to the next available agent/provider.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();

        use crate::coordinator::{goal, graph::GoalMode};
        let s = crate::coordinator::session::new_session(&conn, tmp.path()).unwrap();
        let g = goal::create(&conn, &s.id, "g", GoalMode::Auto, 25, 60).unwrap();
        let graph = TaskGraph { nodes: vec![node("s1", &[], Effort::Standard, "grok")] };
        goal::save_graph(&mut conn, &g.id, &graph).unwrap();

        let tid = 901i64;
        conn.execute(
            "INSERT INTO tasks (id, description, agent, status, timed_out, created_at, updated_at, cwd, workspace_id, rate_limited, artifact_path)
             VALUES (?1, 'x', 'grok', 'failed', 0, '', '', '', '', 1, NULL)",
            rusqlite::params![tid],
        )
        .unwrap();
        goal::update_node(&conn, &g.id, "s1", NodeStatus::Running, Some(tid), None, None).unwrap();

        settle_finished_node(&ctx, &mut conn, &CoordinatorConfig::default(), &RoutingTable::default(), &PoolHealth::default(), tid).unwrap();

        let reloaded_graph = crate::coordinator::goal::load_graph(&conn, &g.id).unwrap();
        let n = reloaded_graph.find("s1").unwrap();
        assert!(n.agent.is_empty(), "the pin should be cleared so the next tick routes to a different agent");
        assert_eq!(n.status, NodeStatus::Pending);
    }

    #[test]
    fn resume_budget_exhaustion_moves_goal_to_blocked_with_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();

        let (g, tid) = rate_limited_goal(&mut conn, tmp.path(), Some(Utc::now().timestamp_millis() + 1000));
        // A cfg with max_capacity_waits_per_goal = 0 -> the very first
        // exhaustion already exceeds budget -> Blocked, not WaitingOnCapacity.
        let tight_cfg = CoordinatorConfig { max_capacity_waits_per_goal: 0, ..Default::default() };
        settle_finished_node(&ctx, &mut conn, &tight_cfg, &RoutingTable::default(), &PoolHealth::default(), tid).unwrap();

        let reloaded_goal = crate::coordinator::goal::get(&conn, &g.id).unwrap().unwrap();
        assert_eq!(reloaded_goal.status, crate::coordinator::graph::GoalStatus::Blocked);
        assert!(reloaded_goal.blocked_reason.unwrap().contains("capacity"));
    }

    #[test]
    fn capacity_budget_amend_raises_the_per_goal_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();
        let (g, _tid) = rate_limited_goal(&mut conn, tmp.path(), Some(Utc::now().timestamp_millis()));

        assert_eq!(crate::coordinator::goal::get(&conn, &g.id).unwrap().unwrap().capacity_budget_override, None);
        crate::coordinator::goal::raise_capacity_budget(&conn, &g.id, 50).unwrap();
        assert_eq!(crate::coordinator::goal::get(&conn, &g.id).unwrap().unwrap().capacity_budget_override, Some(50));
    }

    #[test]
    fn build_node_prompt_folds_in_upstream_output_when_present() {
        let g = TaskGraph { nodes: vec![node("s1", &[], Effort::Standard, "grok"), node("s2", &["s1"], Effort::Standard, "grok")] };
        // no output_ref on s1 -> no UPSTREAM RESULTS section, but s1 still
        // shows up as a sibling for visibility.
        let p = build_node_prompt(&g, g.find("s2").unwrap());
        assert!(!p.contains("UPSTREAM RESULTS"));
        assert!(p.contains("SIBLING NODE STATUS"));
        assert!(p.contains("s1: pending"));
    }

    #[test]
    fn build_node_prompt_omits_sibling_section_when_alone() {
        let g = TaskGraph { nodes: vec![node("s1", &[], Effort::Standard, "grok")] };
        let p = build_node_prompt(&g, g.find("s1").unwrap());
        assert_eq!(p, "s1");
    }

    // ---- careful mode (`single loop`) ----

    fn careful_goal(conn: &mut rusqlite::Connection, max_iters: u32) -> crate::coordinator::goal::Goal {
        use crate::coordinator::{goal, graph::GoalMode, session};
        let s = session::new_session(conn, std::path::Path::new("/tmp/loop")).unwrap();
        let g = goal::create(conn, &s.id, "make the tests pass", GoalMode::Careful, max_iters, 60).unwrap();
        let graph = TaskGraph { nodes: vec![node("s1", &[], Effort::Standard, "grok")] };
        goal::save_graph(conn, &g.id, &graph).unwrap();
        goal::set_status(conn, &g.id, crate::coordinator::graph::GoalStatus::Running).unwrap();
        g
    }

    fn artifact_with(text: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        std::fs::write(&path, text).unwrap();
        (dir, path.display().to_string())
    }

    #[test]
    fn careful_done_line_finishes_the_goal() {
        use crate::coordinator::{goal, graph::GoalStatus};
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        let g = careful_goal(&mut conn, 6);
        goal::bump_dispatches(&conn, &g.id).unwrap(); // iteration 1 ran
        let g = goal::get(&conn, &g.id).unwrap().unwrap();

        let (_d, path) = artifact_with("did the work.\nDONE\n");
        settle_careful_node(&mut conn, &g, "s1", Some(&path)).unwrap();

        let after = goal::get(&conn, &g.id).unwrap().unwrap();
        assert_eq!(after.status, GoalStatus::Done);
        assert!(after.result_summary.unwrap().contains("did the work"));
        assert_eq!(goal::load_graph(&conn, &g.id).unwrap().find("s1").unwrap().status, NodeStatus::Done);
    }

    #[test]
    fn careful_without_done_redispatches_with_prior_output() {
        use crate::coordinator::goal;
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        let g = careful_goal(&mut conn, 6);
        goal::bump_dispatches(&conn, &g.id).unwrap(); // iteration 1
        let g = goal::get(&conn, &g.id).unwrap().unwrap();

        let (_d, path) = artifact_with("progress, not finished yet");
        settle_careful_node(&mut conn, &g, "s1", Some(&path)).unwrap();

        let node = goal::load_graph(&conn, &g.id).unwrap().find("s1").unwrap().clone();
        assert_eq!(node.status, NodeStatus::Pending); // ready for the next tick
        assert_eq!(node.attempts, 2);
        assert!(node.desc.contains("previous step output"));
        assert!(node.desc.contains("progress, not finished yet"));
        assert_eq!(goal::get(&conn, &g.id).unwrap().unwrap().status.as_str(), "running");
    }

    #[test]
    fn careful_stops_at_the_iteration_cap_without_done() {
        use crate::coordinator::{goal, graph::GoalStatus};
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        let g = careful_goal(&mut conn, 2);
        for _ in 0..2 {
            goal::bump_dispatches(&conn, &g.id).unwrap();
        }
        let g = goal::get(&conn, &g.id).unwrap().unwrap();

        let (_d, path) = artifact_with("still going");
        settle_careful_node(&mut conn, &g, "s1", Some(&path)).unwrap();

        let after = goal::get(&conn, &g.id).unwrap().unwrap();
        assert_eq!(after.status, GoalStatus::Done);
        assert!(after.result_summary.unwrap().contains("without a DONE"));
    }

    // ---- opt-in auto-merge ----

    fn init_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git").current_dir(repo.path()).args(args).status().unwrap();
            assert!(status.success(), "git {:?} failed", args);
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("README.md"), "hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "initial"]);
        repo
    }

    /// builds `code` (worktree, Done, real branch with one commit) ->
    /// `review` (Done) on a goal whose session cwd is `repo`, wiring up
    /// exactly what `maybe_auto_merge` reads.
    fn code_then_review_goal(conn: &mut rusqlite::Connection, repo: &std::path::Path, code_task_id: i64, auto_merge: bool) -> Goal {
        use crate::coordinator::{graph::GoalMode, session};
        let s = session::new_session(conn, repo).unwrap();
        let g = goal::create(conn, &s.id, "ship it", GoalMode::Auto, 25, 60).unwrap();
        if auto_merge {
            goal::set_auto_merge(conn, &g.id, true).unwrap();
        }
        let mut code = node("code", &[], Effort::Standard, "grok");
        code.kind = NodeKind::Code;
        code.worktree = true;
        code.status = NodeStatus::Done;
        code.task_id = Some(code_task_id);
        let mut review = node("review", &["code"], Effort::Standard, "grok");
        review.kind = NodeKind::Review;
        review.status = NodeStatus::Done;
        goal::save_graph(conn, &g.id, &TaskGraph { nodes: vec![code, review] }).unwrap();
        goal::get(conn, &g.id).unwrap().unwrap()
    }

    #[test]
    fn auto_merge_queues_a_confirmation_instead_of_merging_directly() {
        // Live-verification finding: an upfront `auto_merge` flag merging
        // immediately once review passes gives a human no chance to see
        // the actual diff at landing time — this must only ever queue a
        // `pending_merge` request, never touch the repo itself.
        let repo = init_repo();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();

        let task_id = crate::task::create_for_cwd(&conn, "do the work", "grok", repo.path()).unwrap();
        let branch = format!("single/task-{task_id}");
        let worktree_path = tempfile::tempdir().unwrap().path().join(format!("task-{task_id}"));
        single_core::worktree::add(repo.path(), &worktree_path, &branch).unwrap();
        std::fs::write(worktree_path.join("new-file.txt"), "from the worktree").unwrap();
        let run_in_worktree = |args: &[&str]| {
            let status = std::process::Command::new("git").current_dir(&worktree_path).args(args).status().unwrap();
            assert!(status.success(), "git {:?} failed", args);
        };
        run_in_worktree(&["add", "."]);
        run_in_worktree(&["commit", "-q", "-m", "add new-file"]);

        let g = code_then_review_goal(&mut conn, repo.path(), task_id, true);

        maybe_auto_merge(&conn, &g, "review").unwrap();

        assert!(!repo.path().join("new-file.txt").is_file(), "must not merge on its own — needs a human `merge confirm`");
        let events = events::for_goal(&conn, &g.id, 10).unwrap();
        assert!(events.iter().any(|e| e.kind == "merge_awaiting_confirmation" && e.body.contains(&branch)));
        let pending = single_core::pending_merge::list_pending(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].goal_id, g.id);
        assert_eq!(pending[0].branch, branch);

        // and confirming it is what actually performs the merge
        single_core::worktree::merge(repo.path(), &branch).unwrap();
        assert!(repo.path().join("new-file.txt").is_file());
    }

    #[test]
    fn auto_merge_is_a_noop_when_the_goal_never_opted_in() {
        let repo = init_repo();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();

        let task_id = crate::task::create_for_cwd(&conn, "do the work", "grok", repo.path()).unwrap();
        let branch = format!("single/task-{task_id}");
        let worktree_path = tempfile::tempdir().unwrap().path().join(format!("task-{task_id}"));
        single_core::worktree::add(repo.path(), &worktree_path, &branch).unwrap();
        std::fs::write(worktree_path.join("new-file.txt"), "from the worktree").unwrap();
        let run_in_worktree = |args: &[&str]| {
            let status = std::process::Command::new("git").current_dir(&worktree_path).args(args).status().unwrap();
            assert!(status.success(), "git {:?} failed", args);
        };
        run_in_worktree(&["add", "."]);
        run_in_worktree(&["commit", "-q", "-m", "add new-file"]);

        let g = code_then_review_goal(&mut conn, repo.path(), task_id, false);

        maybe_auto_merge(&conn, &g, "review").unwrap();

        assert!(!repo.path().join("new-file.txt").is_file());
        assert!(!events::for_goal(&conn, &g.id, 10).unwrap().iter().any(|e| e.kind == "merged"));
    }

    #[test]
    fn auto_merge_ignores_a_non_review_node_even_when_opted_in() {
        // settle_finished_node only calls maybe_auto_merge on the `completed`
        // branch, but the function itself must also refuse to act on a
        // `code`-kind node id -- it should never merge just because
        // something in the graph finished, only when a review passed.
        let repo = init_repo();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();

        let task_id = crate::task::create_for_cwd(&conn, "do the work", "grok", repo.path()).unwrap();
        let branch = format!("single/task-{task_id}");
        let worktree_path = tempfile::tempdir().unwrap().path().join(format!("task-{task_id}"));
        single_core::worktree::add(repo.path(), &worktree_path, &branch).unwrap();
        std::fs::write(worktree_path.join("new-file.txt"), "from the worktree").unwrap();
        let run_in_worktree = |args: &[&str]| {
            let status = std::process::Command::new("git").current_dir(&worktree_path).args(args).status().unwrap();
            assert!(status.success(), "git {:?} failed", args);
        };
        run_in_worktree(&["add", "."]);
        run_in_worktree(&["commit", "-q", "-m", "add new-file"]);

        let g = code_then_review_goal(&mut conn, repo.path(), task_id, true);

        // pass the "code" node id, not "review" -- must be a no-op
        maybe_auto_merge(&conn, &g, "code").unwrap();

        assert!(single_core::pending_merge::list_pending(&conn).unwrap().is_empty());
        assert!(!repo.path().join("new-file.txt").is_file());
    }

    #[test]
    fn auto_merge_skips_deps_that_are_not_worktree_backed_or_not_done() {
        // only a `Done`, worktree-backed dependency has a real branch to
        // confirm -- anything else must be silently skipped, not queued.
        let repo = init_repo();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();

        use crate::coordinator::{graph::GoalMode, session};
        let s = session::new_session(&conn, repo.path()).unwrap();
        let g = goal::create(&conn, &s.id, "ship it", GoalMode::Auto, 25, 60).unwrap();
        goal::set_auto_merge(&conn, &g.id, true).unwrap();

        let mut not_worktree = node("plain", &[], Effort::Standard, "grok");
        not_worktree.kind = NodeKind::Code;
        not_worktree.worktree = false;
        not_worktree.status = NodeStatus::Done;
        not_worktree.task_id = Some(1);

        let mut still_running = node("running", &[], Effort::Standard, "grok");
        still_running.kind = NodeKind::Code;
        still_running.worktree = true;
        still_running.status = NodeStatus::Running;
        still_running.task_id = Some(2);

        let mut review = node("review", &["plain", "running"], Effort::Standard, "grok");
        review.kind = NodeKind::Review;
        review.status = NodeStatus::Done;
        goal::save_graph(&mut conn, &g.id, &TaskGraph { nodes: vec![not_worktree, still_running, review] }).unwrap();
        let g = goal::get(&conn, &g.id).unwrap().unwrap();

        maybe_auto_merge(&conn, &g, "review").unwrap();

        assert!(single_core::pending_merge::list_pending(&conn).unwrap().is_empty());
        assert!(events::for_goal(&conn, &g.id, 10).unwrap().iter().all(|e| e.kind != "merge_awaiting_confirmation"));
    }

    #[test]
    fn auto_merge_is_a_noop_outside_a_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();

        use crate::coordinator::{graph::GoalMode, session};
        let s = session::new_session(&conn, tmp.path()).unwrap();
        let g = goal::create(&conn, &s.id, "ship it", GoalMode::Auto, 25, 60).unwrap();
        goal::set_auto_merge(&conn, &g.id, true).unwrap();

        let mut code = node("code", &[], Effort::Standard, "grok");
        code.kind = NodeKind::Code;
        code.worktree = true;
        code.status = NodeStatus::Done;
        code.task_id = Some(1);
        let mut review = node("review", &["code"], Effort::Standard, "grok");
        review.kind = NodeKind::Review;
        review.status = NodeStatus::Done;
        goal::save_graph(&mut conn, &g.id, &TaskGraph { nodes: vec![code, review] }).unwrap();
        let g = goal::get(&conn, &g.id).unwrap().unwrap();

        maybe_auto_merge(&conn, &g, "review").unwrap();

        assert!(single_core::pending_merge::list_pending(&conn).unwrap().is_empty());
    }
}
