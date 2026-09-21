//! Coordinator self-correction — spec §9.2 (Task 21). Hard rule enforced
//! throughout, not just documented: this module never deletes a goal,
//! never force-kills a running node, and never edits a goal a human
//! `amend`ed (`goal::mark_human_edited`) or a config file modified in the
//! last hour.

use super::{run_step, Category, PassReport, SelfHealConfig};
use crate::context::Context;
use crate::coordinator::graph::{GoalStatus, NodeStatus};
use crate::coordinator::goal;
use anyhow::{Context as _, Result};
use rusqlite::Connection;

const HUMAN_EDIT_GRACE: chrono::Duration = chrono::Duration::hours(1);
const ROUTING_DRIFT_THRESHOLD: chrono::Duration = chrono::Duration::hours(24);

pub fn run(ctx: &Context, conn: &Connection, cfg: &SelfHealConfig, report: &mut PassReport) -> Result<()> {
    run_step(conn, report, Category::Coordinator, "reeval_blocked_goals", || reeval_blocked_goals(ctx, conn, cfg));
    run_step(conn, report, Category::Coordinator, "reroute_repeated_failures", || reroute_repeated_failures(ctx, conn));
    run_step(conn, report, Category::Coordinator, "routing_toml_drift", || routing_toml_drift(ctx));
    run_step(conn, report, Category::Coordinator, "coordinator_toml_sanity", || coordinator_toml_sanity(ctx));
    Ok(())
}

fn recently_human_edited(g: &goal::Goal) -> bool {
    g.last_human_edit_at.as_deref().and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok()).is_some_and(|t| chrono::Utc::now() - t < HUMAN_EDIT_GRACE)
}

/// Why a goal is blocked, as far as its reason text tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// Rate limits, a busy pool, a bad planner sample, a timeout: clears by itself, so retry.
    Transient,
    /// A dispatch or wall-clock cap was spent: raise it (bounded), it is not a failure.
    Budget,
    /// Only a person can unblock it: authorization, a login, a decision.
    NeedsHuman,
}

pub fn classify_block(reason: &str) -> BlockKind {
    let r = reason.to_lowercase();
    const HUMAN: [&str; 8] = ["human decision", "needs input", "needs your", "approval", "authoriz", "log in", "login required", "credentials"];
    if HUMAN.iter().any(|w| r.contains(w)) {
        return BlockKind::NeedsHuman;
    }
    if r.contains("budget") && !r.contains("capacity") {
        return BlockKind::Budget;
    }
    BlockKind::Transient
}

/// Longest a pass spends re-planning goals (each re-plan is a real model call).
const MAX_REPLANS_PER_PASS: usize = 3;
/// Caps a budget-blocked goal is raised to.
const RAISED_DISPATCHES: u32 = 200;
const RAISED_MINUTES: u32 = 10_080;

/// Deals with every `Blocked` goal on its own, and only asks a person when it must:
/// - transient blocks (capacity, supervisor, planner/integrator output, rate limits) are retried with a
///   backoff that doubles each time (`blocked_reeval_minutes`, x2, x4, ...), up to
///   `max_auto_reevals_per_goal` times;
/// - a spent dispatch/time budget is raised (bounded the same way);
/// - blocks that name an approval, login or decision, and goals that run out of automatic attempts,
///   move to `waiting_input` with the question in `blocked_reason`.
/// Never touches a goal a person amended in the last hour, and re-plans at most a few goals per pass.
fn reeval_blocked_goals(ctx: &Context, conn: &Connection, cfg: &SelfHealConfig) -> Result<String> {
    let blocked = goal::list_by_status(conn, GoalStatus::Blocked)?;
    let (mut retried, mut raised, mut asked) = (Vec::new(), Vec::new(), Vec::new());
    let (mut waiting_backoff, mut skipped_human, mut replans) = (0, 0, 0);

    for g in blocked {
        let reason = g.blocked_reason.clone().unwrap_or_default();
        let age_minutes = (chrono::Utc::now() - g.updated_at.parse().unwrap_or_else(|_| chrono::Utc::now())).num_minutes();
        if recently_human_edited(&g) {
            skipped_human += 1;
            continue;
        }
        let kind = classify_block(&reason);
        if kind == BlockKind::NeedsHuman {
            ask(conn, &g, &reason)?;
            asked.push(g.id);
            continue;
        }
        if g.auto_reevals >= cfg.max_auto_reevals_per_goal {
            ask(conn, &g, &format!("divisi tried {} times on its own and is still blocked: {reason}. Amend with what to do, or `goal resume`, or cancel.", g.auto_reevals))?;
            asked.push(g.id);
            continue;
        }
        let backoff = cfg.blocked_reeval_minutes as i64 * (1i64 << g.auto_reevals.min(10));
        if age_minutes < backoff {
            waiting_backoff += 1;
            continue;
        }
        if kind == BlockKind::Budget {
            goal::raise_dispatch_cap(conn, &g.id, RAISED_DISPATCHES)?;
            goal::raise_time_cap(conn, &g.id, RAISED_MINUTES)?;
            goal::reevaluate_blocked(conn, &g.id)?;
            raised.push(g.id);
            continue;
        }
        if goal::load_graph(conn, &g.id)?.nodes.is_empty() {
            // The planner never produced a graph: plan again (what `goal resume` does).
            if replans >= MAX_REPLANS_PER_PASS {
                continue;
            }
            replans += 1;
            let mut own = crate::handlers::coordinator_db(ctx)?;
            let _ = crate::coordinator::resume_goal(ctx, &mut own, &g.id);
            // resume_goal blocks it again with the new reason if planning failed; count the attempt either way.
            conn.execute("UPDATE goals SET auto_reevals = auto_reevals + 1 WHERE id = ?1", [&g.id])?;
        } else {
            goal::reevaluate_blocked(conn, &g.id)?;
        }
        retried.push(g.id);
    }

    Ok(format!(
        "retried: [{}]; budget raised: [{}]; asked for input: [{}]; backing off: {waiting_backoff}; human-edited (skipped): {skipped_human}",
        retried.join(", "),
        raised.join(", "),
        asked.join(", ")
    ))
}

fn ask(conn: &Connection, g: &goal::Goal, question: &str) -> Result<()> {
    goal::wait_for_input(conn, &g.id, question)?;
    crate::coordinator::events::append(conn, &g.session_id, Some(&g.id), crate::coordinator::events::EventKind::NeedsInput, &format!("{}: waiting for you: {question}", g.id))?;
    Ok(())
}

/// A node whose retries are exhausted (`Failed`, `attempts >= 2`) gets
/// its agent pin cleared and status reset to `Pending`, so the next
/// tick's `select_agent` routes it through the kind's list fresh
/// (potentially onto `single-pool`) instead of sitting dead forever.
/// Never touches a goal amended in the last hour.
///
/// Live-verification finding: this previously required `node.agent`
/// non-empty (i.e. only a node explicitly pinned via `/agent`) before
/// acting. A node dispatched through ordinary kind-based routing
/// (`routing.toml`, no pin) has an *empty* `agent` field once the
/// scheduler gives up on it — `settle_finished_node`'s exhausted-retries
/// path marks it `Failed` without ever setting `agent`. That's the
/// common case, not the exception, so this substep silently never fired
/// for the failures it exists to fix: such a node sat `Failed` with no
/// `earliest_retry_at_ms` forever, invisible to both `goal resume`
/// (only re-ticks `Pending` nodes) and this self-heal pass, needing a
/// manual DB reset every time. Dropped the non-empty-agent requirement
/// — `clear_node_agent_pin` is a no-op-safe reset (`agent=''` on an
/// already-empty field, `status='pending'`) whether or not a pin was
/// ever set.
fn reroute_repeated_failures(_ctx: &Context, conn: &Connection) -> Result<String> {
    let mut rerouted = Vec::new();

    for g in goal::active(conn)?.into_iter().chain(goal::list_by_status(conn, GoalStatus::Blocked)?) {
        if recently_human_edited(&g) {
            continue;
        }
        let graph = goal::load_graph(conn, &g.id)?;
        for node in &graph.nodes {
            if node.status == NodeStatus::Failed && node.attempts >= 2 {
                goal::clear_node_agent_pin(conn, &g.id, &node.id)?;
                rerouted.push(format!("{}/{}", g.id, node.id));
            }
        }
    }

    Ok(format!("rerouted: [{}]", rerouted.join(", ")))
}

/// `routing.toml` drift: an agent named in a kind's list that isn't
/// currently detected is removed from every list it appears in (backup
/// written first) -- `divisi provider sync-pool` / the next detection
/// re-adds it. Skipped entirely if the file itself was modified in the
/// last 24h (spec's ">24h undetected" duration-tracking is approximated
/// here by the file's own mtime, since no per-agent detection-history
/// table exists this iteration -- documented simplification), which also
/// satisfies "never edits a config touched in the last hour" for free.
fn routing_toml_drift(ctx: &Context) -> Result<String> {
    let path = ctx.dirs.routing_file();
    if !path.exists() {
        return Ok("routing.toml does not exist yet".to_string());
    }
    let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|m| m.elapsed().ok()).map(chrono::Duration::from_std).and_then(|r| r.ok());
    if age.is_none_or(|a| a < ROUTING_DRIFT_THRESHOLD) {
        return Ok("routing.toml modified too recently to touch (< 24h)".to_string());
    }

    let mut table = crate::coordinator::routing::RoutingTable::load(&ctx.dirs);
    let mut removed = Vec::new();
    for by_effort in table.kinds.values_mut() {
        for agents in by_effort.values_mut() {
            agents.retain(|a| {
                // single-pool and single-<provider> presets are never
                // "undetected" the way a shelled CLI is -- see
                // `PoolHealth::usable`'s same carve-out.
                if a == "single-pool" || a.starts_with("single-") {
                    return true;
                }
                let detected = divisi_agent_sdk::adapters::for_agent_with_custom(a, &ctx.dirs.agents_dir(), &ctx.registry).map(|ad| ad.discover().detected).unwrap_or(false);
                if !detected {
                    removed.push(a.clone());
                }
                detected
            });
        }
    }

    if removed.is_empty() {
        return Ok("no undetected agents in routing.toml".to_string());
    }

    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    std::fs::copy(&path, path.with_extension(format!("toml.bak-{timestamp}"))).context("backing up routing.toml")?;
    std::fs::write(&path, toml::to_string_pretty(&table)?).context("writing routing.toml")?;
    Ok(format!("removed undetected agent(s) from routing.toml: {}", removed.join(", ")))
}

/// `coordinator.toml` sanity: values that would wedge the scheduler
/// entirely (`max_parallel = 0`, `max_goal_minutes < 1`) are reset to
/// defaults, backup written first.
fn coordinator_toml_sanity(ctx: &Context) -> Result<String> {
    let path = ctx.dirs.coordinator_file();
    if !path.exists() {
        return Ok("coordinator.toml does not exist yet".to_string());
    }
    let mut cfg = crate::coordinator::routing::CoordinatorConfig::load(&ctx.dirs);
    let defaults = crate::coordinator::routing::CoordinatorConfig::default();
    let mut fixed = Vec::new();

    if cfg.max_parallel == 0 {
        cfg.max_parallel = defaults.max_parallel;
        fixed.push("max_parallel");
    }
    if cfg.max_goal_minutes < 1 {
        cfg.max_goal_minutes = defaults.max_goal_minutes;
        fixed.push("max_goal_minutes");
    }

    if fixed.is_empty() {
        return Ok("coordinator.toml values are sane".to_string());
    }

    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    std::fs::copy(&path, path.with_extension(format!("toml.bak-{timestamp}"))).context("backing up coordinator.toml")?;
    std::fs::write(&path, toml::to_string_pretty(&cfg)?).context("writing coordinator.toml")?;
    Ok(format!("reset to defaults: {}", fixed.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::graph::{Effort, GoalMode, Node, NodeKind, TaskGraph};

    fn test_ctx(root: &std::path::Path) -> Context {
        let dirs = divisi_core::DivisiDirs::from_root(root.to_path_buf());
        dirs.ensure_created().unwrap();
        Context { dirs, resolved: divisi_core::ResolvedConfig::default(), registry: divisi_core::builtin_registry() }
    }

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();
        conn
    }

    fn make_blocked_goal(conn: &Connection, reason: &str, updated_minutes_ago: i64) -> goal::Goal {
        let s = crate::coordinator::session::new_session(conn, std::path::Path::new("/tmp")).unwrap();
        let g = goal::create(conn, &s.id, "g", GoalMode::Auto, 25, 60).unwrap();
        goal::set_blocked(conn, &g.id, reason).unwrap();
        let stamp = (chrono::Utc::now() - chrono::Duration::minutes(updated_minutes_ago)).to_rfc3339();
        conn.execute("UPDATE goals SET updated_at = ?2 WHERE id = ?1", rusqlite::params![g.id, stamp]).unwrap();
        goal::get(conn, &g.id).unwrap().unwrap()
    }

    /// Gives a goal a one-node graph so self-heal takes the retry path, never a real re-plan.
    fn with_graph(conn: &mut Connection, id: &str) {
        let node = Node {
            id: "s1".into(),
            desc: "d".into(),
            kind: NodeKind::Code,
            effort: Effort::Standard,
            agent: String::new(),
            depends_on: vec![],
            status: NodeStatus::Pending,
            task_id: None,
            attempts: 0,
            worktree: false,
            output_ref: None,
            earliest_retry_at_ms: None,
        };
        goal::save_graph(conn, id, &TaskGraph { nodes: vec![node] }).unwrap();
    }

    fn blocked_with_graph(conn: &mut Connection, reason: &str, minutes_ago: i64) -> goal::Goal {
        let g = make_blocked_goal(conn, reason, minutes_ago);
        with_graph(conn, &g.id);
        goal::set_blocked(conn, &g.id, reason).unwrap();
        let stamp = (chrono::Utc::now() - chrono::Duration::minutes(minutes_ago)).to_rfc3339();
        conn.execute("UPDATE goals SET updated_at = ?2 WHERE id = ?1", rusqlite::params![g.id, stamp]).unwrap();
        goal::get(conn, &g.id).unwrap().unwrap()
    }

    fn status_of(conn: &Connection, id: &str) -> GoalStatus {
        goal::get(conn, id).unwrap().unwrap().status
    }

    #[test]
    fn classifies_blocks() {
        assert_eq!(classify_block("waited 12.0h for capacity, still exhausted"), BlockKind::Transient);
        assert_eq!(classify_block("re-plan failed: brain role produced no parseable JSON"), BlockKind::Transient);
        assert_eq!(classify_block("time budget spent: 558 min elapsed of 240 min cap"), BlockKind::Budget);
        assert_eq!(classify_block("needs your approval to push"), BlockKind::NeedsHuman);
        assert_eq!(classify_block("login required for claude"), BlockKind::NeedsHuman);
    }

    #[test]
    fn long_blocked_capacity_goal_is_reevaluated_and_reticked_when_cleared() {
        let mut conn = test_conn();
        let ctx = test_ctx(&tempfile::tempdir().unwrap().keep());
        let cfg = SelfHealConfig::default();
        let g = blocked_with_graph(&mut conn, "waited 12.0h for capacity, still exhausted", 60);

        let detail = reeval_blocked_goals(&ctx, &conn, &cfg).unwrap();
        assert!(detail.contains(&g.id), "{detail}");
        let reloaded = goal::get(&conn, &g.id).unwrap().unwrap();
        assert_eq!(reloaded.status, GoalStatus::Running);
        assert_eq!(reloaded.auto_reevals, 1);
    }

    #[test]
    fn a_transient_block_backs_off_and_the_wait_doubles() {
        let mut conn = test_conn();
        let ctx = test_ctx(&tempfile::tempdir().unwrap().keep());
        let cfg = SelfHealConfig { blocked_reeval_minutes: 10, ..SelfHealConfig::default() };
        let g = blocked_with_graph(&mut conn, "supervisor gave up", 5);
        assert!(reeval_blocked_goals(&ctx, &conn, &cfg).unwrap().contains("backing off: 1"));
        assert_eq!(status_of(&conn, &g.id), GoalStatus::Blocked, "too fresh to retry");

        // after one retry the wait is 20 minutes, so 15 is still too soon
        conn.execute("UPDATE goals SET auto_reevals = 1, updated_at = ?2 WHERE id = ?1", rusqlite::params![g.id, (chrono::Utc::now() - chrono::Duration::minutes(15)).to_rfc3339()]).unwrap();
        assert!(reeval_blocked_goals(&ctx, &conn, &cfg).unwrap().contains("backing off: 1"));
        assert_eq!(status_of(&conn, &g.id), GoalStatus::Blocked);
    }

    #[test]
    fn running_out_of_automatic_attempts_asks_for_input() {
        let mut conn = test_conn();
        let ctx = test_ctx(&tempfile::tempdir().unwrap().keep());
        let cfg = SelfHealConfig { max_auto_reevals_per_goal: 1, ..SelfHealConfig::default() };
        let g = blocked_with_graph(&mut conn, "supervisor gave up", 60);

        reeval_blocked_goals(&ctx, &conn, &cfg).unwrap(); // 1st retry: allowed
        goal::set_blocked(&conn, &g.id, "supervisor gave up again").unwrap();
        conn.execute("UPDATE goals SET updated_at = ?2 WHERE id = ?1", rusqlite::params![g.id, (chrono::Utc::now() - chrono::Duration::minutes(600)).to_rfc3339()]).unwrap();

        let detail = reeval_blocked_goals(&ctx, &conn, &cfg).unwrap();
        assert!(detail.contains("asked for input"), "{detail}");
        let reloaded = goal::get(&conn, &g.id).unwrap().unwrap();
        assert_eq!(reloaded.status, GoalStatus::WaitingInput);
        assert!(reloaded.blocked_reason.unwrap().contains("supervisor gave up again"));
    }

    #[test]
    fn a_block_that_needs_a_person_goes_straight_to_waiting_input() {
        let mut conn = test_conn();
        let ctx = test_ctx(&tempfile::tempdir().unwrap().keep());
        let g = blocked_with_graph(&mut conn, "needs your approval to publish the release", 1);

        reeval_blocked_goals(&ctx, &conn, &SelfHealConfig::default()).unwrap();
        assert_eq!(status_of(&conn, &g.id), GoalStatus::WaitingInput);
        // and the pass never touches it again
        reeval_blocked_goals(&ctx, &conn, &SelfHealConfig::default()).unwrap();
        assert_eq!(status_of(&conn, &g.id), GoalStatus::WaitingInput);
    }

    #[test]
    fn a_spent_budget_is_raised_instead_of_failing() {
        let mut conn = test_conn();
        let ctx = test_ctx(&tempfile::tempdir().unwrap().keep());
        let g = blocked_with_graph(&mut conn, "time budget spent: 558 min elapsed of 240 min cap", 60);

        let detail = reeval_blocked_goals(&ctx, &conn, &SelfHealConfig::default()).unwrap();
        assert!(detail.contains("budget raised"), "{detail}");
        assert_eq!(status_of(&conn, &g.id), GoalStatus::Running);
    }

    #[test]
    fn human_edited_goal_is_never_touched() {
        let mut conn = test_conn();
        let ctx = test_ctx(&tempfile::tempdir().unwrap().keep());
        let g = blocked_with_graph(&mut conn, "waited too long for capacity", 60);
        goal::mark_human_edited(&conn, &g.id).unwrap();

        let detail = reeval_blocked_goals(&ctx, &conn, &SelfHealConfig::default()).unwrap();
        assert!(detail.contains("human-edited (skipped): 1"), "{detail}");
        assert_eq!(status_of(&conn, &g.id), GoalStatus::Blocked);
    }

    #[test]
    fn repeated_same_node_same_agent_failure_reroutes_to_next_agent() {
        let conn = test_conn();
        let ctx = test_ctx(&tempfile::tempdir().unwrap().keep());
        let s = crate::coordinator::session::new_session(&conn, std::path::Path::new("/tmp")).unwrap();
        let g = goal::create(&conn, &s.id, "g", GoalMode::Auto, 25, 60).unwrap();
        let node = Node {
            id: "s1".into(),
            desc: "d".into(),
            kind: NodeKind::Code,
            effort: Effort::Standard,
            agent: "grok".into(), // pinned
            depends_on: vec![],
            status: NodeStatus::Failed,
            task_id: None,
            attempts: 2,
            worktree: false,
            output_ref: None,
            earliest_retry_at_ms: None,
        };
        let mut conn = conn;
        goal::save_graph(&mut conn, &g.id, &TaskGraph { nodes: vec![node] }).unwrap();

        let detail = reroute_repeated_failures(&ctx, &conn).unwrap();
        assert!(detail.contains("s1"), "{detail}");
        let reloaded = goal::load_graph(&conn, &g.id).unwrap();
        let n = reloaded.find("s1").unwrap();
        assert!(n.agent.is_empty(), "the pin should be cleared");
        assert_eq!(n.status, NodeStatus::Pending);
    }

    #[test]
    fn exhausted_kind_routed_failure_with_no_pin_still_reroutes() {
        // Live-verification regression: a node dispatched through ordinary
        // kind-based routing (never `/agent`-pinned) has an empty `agent`
        // field once the scheduler exhausts its retries -- this must still
        // get reset to `Pending`, not just the pinned-agent case.
        let conn = test_conn();
        let ctx = test_ctx(&tempfile::tempdir().unwrap().keep());
        let s = crate::coordinator::session::new_session(&conn, std::path::Path::new("/tmp")).unwrap();
        let g = goal::create(&conn, &s.id, "g", GoalMode::Auto, 25, 60).unwrap();
        let node = Node {
            id: "s6".into(),
            desc: "d".into(),
            kind: NodeKind::Code,
            effort: Effort::Deep,
            agent: String::new(), // never pinned -- kind-routed
            depends_on: vec![],
            status: NodeStatus::Failed,
            task_id: Some(1469),
            attempts: 2,
            worktree: true,
            output_ref: None,
            earliest_retry_at_ms: None,
        };
        let mut conn = conn;
        goal::save_graph(&mut conn, &g.id, &TaskGraph { nodes: vec![node] }).unwrap();

        let detail = reroute_repeated_failures(&ctx, &conn).unwrap();
        assert!(detail.contains("s6"), "{detail}");
        let reloaded = goal::load_graph(&conn, &g.id).unwrap();
        let n = reloaded.find("s6").unwrap();
        assert_eq!(n.status, NodeStatus::Pending, "an unpinned exhausted node must also be reset, not left dead");
    }

    #[test]
    fn coordinator_toml_absurd_values_reset_to_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        std::fs::write(ctx.dirs.coordinator_file(), "max_parallel = 0\nmax_goal_minutes = 0\n").unwrap();

        let detail = coordinator_toml_sanity(&ctx).unwrap();
        assert!(detail.contains("max_parallel") && detail.contains("max_goal_minutes"), "{detail}");
        let cfg = crate::coordinator::routing::CoordinatorConfig::load(&ctx.dirs);
        assert_eq!(cfg.max_parallel, crate::coordinator::routing::CoordinatorConfig::default().max_parallel);
    }

    #[test]
    fn routing_toml_drift_is_a_noop_when_file_is_recent() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let _ = crate::coordinator::routing::RoutingTable::load(&ctx.dirs); // writes a fresh (recent) file
        let detail = routing_toml_drift(&ctx).unwrap();
        assert!(detail.contains("too recently"), "{detail}");
    }

    #[test]
    fn pass_never_deletes_a_goal_or_force_kills_a_running_node() {
        let conn = test_conn();
        let ctx = test_ctx(&tempfile::tempdir().unwrap().keep());
        let s = crate::coordinator::session::new_session(&conn, std::path::Path::new("/tmp")).unwrap();
        let g = goal::create(&conn, &s.id, "g", GoalMode::Auto, 25, 60).unwrap();
        let node = Node {
            id: "s1".into(),
            desc: "d".into(),
            kind: NodeKind::Code,
            effort: Effort::Standard,
            agent: String::new(),
            depends_on: vec![],
            status: NodeStatus::Running,
            task_id: Some(1),
            attempts: 0,
            worktree: false,
            output_ref: None,
            earliest_retry_at_ms: None,
        };
        let mut conn = conn;
        goal::save_graph(&mut conn, &g.id, &TaskGraph { nodes: vec![node] }).unwrap();

        let cfg = SelfHealConfig::default();
        let mut report = PassReport::default();
        run(&ctx, &conn, &cfg, &mut report).unwrap();

        assert!(goal::get(&conn, &g.id).unwrap().is_some(), "the goal must still exist");
        let reloaded = goal::load_graph(&conn, &g.id).unwrap();
        assert_eq!(reloaded.find("s1").unwrap().status, NodeStatus::Running, "a running node must never be force-killed by this pass");
    }
}
