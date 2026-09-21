//! `goals` + `graph_nodes` tables (spec §3.2 / §3.3). one goal = one unit
//! of "do this", submitted into a session. the `TaskGraph` lives both as
//! `goals.plan_json` (human-readable mirror) and as `graph_nodes` rows
//! (authoritative for status, so the scheduler can query ready-sets with
//! SQL and cross-session status reads stay cheap).

use crate::coordinator::graph::{Effort, GoalMode, GoalStatus, Node, NodeKind, NodeStatus, TaskGraph};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone)]
pub struct Goal {
    pub id: String,
    pub session_id: String,
    pub text: String,
    pub mode: GoalMode,
    pub status: GoalStatus,
    pub max_dispatches: u32,
    pub max_minutes: u32,
    pub dispatches: u32,
    pub supervisor_patches: u32,
    pub plan_json: String,
    pub result_summary: Option<String>,
    pub blocked_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// E28 spec §8: which providers/pools are spent — set alongside
    /// `WaitingOnCapacity`, cleared on resume.
    pub capacity_reason: Option<String>,
    /// E28 spec §8: unix-ms ETA for the earliest-recovering candidate;
    /// paired with `capacity_reason`.
    pub earliest_retry_at_ms: Option<i64>,
    /// E28 spec §8: how many times this goal has entered
    /// `WaitingOnCapacity` — checked against `max_capacity_waits_per_goal`
    /// (or `capacity_budget_override`) before finally giving up to `Blocked`.
    pub capacity_waits: u32,
    /// E28 spec §8: `divisi goal amend <id> capacity-budget=N` override of
    /// `CoordinatorConfig::max_capacity_waits_per_goal` for this goal only.
    pub capacity_budget_override: Option<u32>,
    /// `divisi goal amend <id> capacity-minutes=N` override of
    /// `CoordinatorConfig::max_capacity_wait_minutes` for this goal only.
    /// Live-verification finding: unlike `max_dispatches`/`max_minutes`,
    /// this wall-clock cap (measured from `created_at`, never reset) had
    /// no per-goal override at all -- an otherwise-healthy goal older than
    /// the global default (720 min / 12h) gets permanently `Blocked` the
    /// next time it hits a capacity wait, no matter how briefly, with no
    /// documented recovery path.
    pub capacity_wait_minutes_override: Option<u32>,
    /// E28 spec §9.2: last time a human touched this goal via `GoalAmend`
    /// — self-heal's coordinator category never edits a goal touched in
    /// the last hour.
    pub last_human_edit_at: Option<String>,
    /// E28 spec §9.2: how many times self-heal's coordinator category has
    /// re-evaluated a `Blocked` goal — bounded by `max_auto_reevals_per_goal`.
    pub auto_reevals: u32,
    /// opt-in only (`divisi goal amend <id> auto-merge=true`) — never set by
    /// default. `docs/architecture.md`'s "branches are never auto-merged;
    /// that stays a human decision" still holds: this is the human's
    /// upfront decision to allow it for this goal, not a bypass of it. The
    /// scheduler only acts on it once a `review`-kind node the merged
    /// node depends on has itself finished `Done` (a passing
    /// reviewer/verifier), and only via the existing
    /// `divisi_core::worktree::merge` — no separate merge mechanism.
    pub auto_merge: bool,
}

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS goals (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            text TEXT NOT NULL,
            mode TEXT NOT NULL DEFAULT 'auto',
            status TEXT NOT NULL DEFAULT 'planning',
            max_dispatches INTEGER NOT NULL DEFAULT 25,
            max_minutes INTEGER NOT NULL DEFAULT 60,
            dispatches INTEGER NOT NULL DEFAULT 0,
            supervisor_patches INTEGER NOT NULL DEFAULT 0,
            plan_json TEXT NOT NULL DEFAULT '{}',
            result_summary TEXT,
            blocked_reason TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
        (),
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS graph_nodes (
            goal_id TEXT NOT NULL,
            id TEXT NOT NULL,
            desc TEXT NOT NULL,
            kind TEXT NOT NULL,
            effort TEXT NOT NULL,
            agent TEXT NOT NULL DEFAULT '',
            depends_on TEXT NOT NULL DEFAULT '[]',
            status TEXT NOT NULL DEFAULT 'pending',
            task_id INTEGER,
            attempts INTEGER NOT NULL DEFAULT 0,
            worktree INTEGER NOT NULL DEFAULT 0,
            output_ref TEXT,
            PRIMARY KEY (goal_id, id)
        )",
        (),
    )?;
    // E28 spec §8/§12 (Part D, auto-continue): additive columns, via
    // `add_column_if_missing` per the plan (same helper `task.rs`'s
    // `tasks` table migrations use) — no rewrite of either table.
    crate::task::add_column_if_missing(conn, "goals", "capacity_reason", "TEXT")?;
    crate::task::add_column_if_missing(conn, "goals", "earliest_retry_at_ms", "INTEGER")?;
    crate::task::add_column_if_missing(conn, "goals", "capacity_waits", "INTEGER NOT NULL DEFAULT 0")?;
    crate::task::add_column_if_missing(conn, "goals", "capacity_budget_override", "INTEGER")?;
    crate::task::add_column_if_missing(conn, "goals", "capacity_wait_minutes_override", "INTEGER")?;
    crate::task::add_column_if_missing(conn, "graph_nodes", "earliest_retry_at_ms", "INTEGER")?;
    // E28 spec §9.2 (self-heal coordinator category): `last_human_edit_at`
    // is the hard-rule marker — the pass never touches a goal a human
    // `amend`ed in the last hour; `auto_reevals` bounds how many times the
    // pass re-evaluates one `Blocked` goal.
    crate::task::add_column_if_missing(conn, "goals", "last_human_edit_at", "TEXT")?;
    crate::task::add_column_if_missing(conn, "goals", "auto_reevals", "INTEGER NOT NULL DEFAULT 0")?;
    crate::task::add_column_if_missing(conn, "goals", "auto_merge", "INTEGER NOT NULL DEFAULT 0")?;
    Ok(())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[allow(clippy::too_many_arguments)]
pub fn create(
    conn: &Connection,
    session_id: &str,
    text: &str,
    mode: GoalMode,
    max_dispatches: u32,
    max_minutes: u32,
) -> Result<Goal> {
    let id = super::short_id("goal");
    let ts = now();
    conn.execute(
        "INSERT INTO goals
         (id, session_id, text, mode, status, max_dispatches, max_minutes, dispatches,
          supervisor_patches, plan_json, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'planning', ?5, ?6, 0, 0, '{}', ?7, ?7)",
        params![id, session_id, text, mode.as_str(), max_dispatches, max_minutes, ts],
    )?;
    super::session::set_title_if_empty(conn, session_id, &truncate(text, 80))?;
    super::events::append(
        conn,
        session_id,
        Some(&id),
        super::events::EventKind::Message,
        &format!("goal submitted: {}", truncate(text, 200)),
    )?;
    get(conn, &id)?.context("goal disappeared right after insert")
}

fn row_to_goal(row: &rusqlite::Row) -> rusqlite::Result<Goal> {
    Ok(Goal {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        text: row.get("text")?,
        mode: GoalMode::parse(&row.get::<_, String>("mode")?).unwrap_or(GoalMode::Auto),
        status: GoalStatus::parse(&row.get::<_, String>("status")?).unwrap_or(GoalStatus::Planning),
        max_dispatches: row.get("max_dispatches")?,
        max_minutes: row.get("max_minutes")?,
        dispatches: row.get("dispatches")?,
        supervisor_patches: row.get("supervisor_patches")?,
        plan_json: row.get("plan_json")?,
        result_summary: row.get("result_summary")?,
        blocked_reason: row.get("blocked_reason")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        capacity_reason: row.get("capacity_reason")?,
        earliest_retry_at_ms: row.get("earliest_retry_at_ms")?,
        capacity_waits: row.get("capacity_waits")?,
        capacity_budget_override: row.get("capacity_budget_override")?,
        capacity_wait_minutes_override: row.get("capacity_wait_minutes_override")?,
        last_human_edit_at: row.get("last_human_edit_at")?,
        auto_reevals: row.get("auto_reevals")?,
        auto_merge: row.get::<_, i64>("auto_merge")? != 0,
    })
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Goal>> {
    Ok(conn.query_row("SELECT * FROM goals WHERE id = ?1", [id], row_to_goal).optional()?)
}

/// all goals, optionally scoped to one session, newest first.
pub fn list(conn: &Connection, session_id: Option<&str>) -> Result<Vec<Goal>> {
    let mut stmt = match session_id {
        Some(_) => conn.prepare("SELECT * FROM goals WHERE session_id = ?1 ORDER BY created_at DESC")?,
        None => conn.prepare("SELECT * FROM goals ORDER BY created_at DESC")?,
    };
    let rows = match session_id {
        Some(s) => stmt.query_map([s], row_to_goal)?,
        None => stmt.query_map([], row_to_goal)?,
    };
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// non-terminal goals — the scheduler tick iterates these. Includes
/// `waiting_on_capacity` (E28 spec §8) so a held goal keeps getting
/// ticked and re-admits its node once the retry stamp passes, instead of
/// going stale the way a genuinely terminal status would.
pub fn active(conn: &Connection) -> Result<Vec<Goal>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM goals WHERE status IN ('planning','running','queued','waiting_on_capacity') ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], row_to_goal)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// E29: normalized-token-overlap check against every active goal's
/// `text`, used at goal submission to detect "this is the same ask
/// already in flight" across sessions/prompts rather than starting a
/// duplicate. Not semantic/LLM-based — a fixed threshold on shared
/// lowercased word tokens (>2 chars, to drop stopword-length noise) is
/// the v1 cut; a documented follow-up if it proves too coarse.
pub fn find_overlapping(conn: &Connection, text: &str) -> Result<Option<Goal>> {
    fn tokens(s: &str) -> std::collections::HashSet<String> {
        s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 2).map(|w| w.to_string()).collect()
    }
    let want = tokens(text);
    if want.is_empty() {
        return Ok(None);
    }
    for g in active(conn)? {
        let have = tokens(&g.text);
        let shared = want.intersection(&have).count();
        // Live-verification finding (2026-09-12): dividing by
        // `min(want.len(), have.len())` meant a short new goal could
        // spuriously "overlap" a much longer, unrelated one purely by
        // reusing common boilerplate phrasing ("exhaust free-pool agents
        // before paid ones", "commit locally as you go...") -- being
        // short made the threshold easy to clear regardless of topic.
        // Confirmed live: three genuinely distinct epic-audit goals
        // (E08/E09/E25 quality/docs/vault-cleanup work) each got silently
        // folded into an unrelated, already-active goal instead of being
        // created, purely because they shared the same instructional
        // boilerplate. Jaccard similarity (shared / union) fixes this: a
        // short text can no longer inflate its match rate against a long
        // one just by being short, since the long text's unique tokens
        // now count against the ratio too. Still catches genuine
        // near-duplicates (two differently-worded phrasings of the same
        // short ask) since those share most of both sides' tokens either
        // way.
        let union = want.len() + have.len() - shared;
        if union > 0 && (shared as f64 / union as f64) >= 0.5 {
            return Ok(Some(g));
        }
    }
    Ok(None)
}

/// All goals currently in one status — used by `resume_interrupted`
/// (E28 spec §10) to find `Paused` goals, which `active()` deliberately
/// excludes (a paused goal doesn't get ticked until something explicitly
/// resumes it).
pub fn list_by_status(conn: &Connection, status: GoalStatus) -> Result<Vec<Goal>> {
    let mut stmt = conn.prepare("SELECT * FROM goals WHERE status = ?1 ORDER BY created_at ASC")?;
    let rows = stmt.query_map([status.as_str()], row_to_goal)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// E28 spec §10: a clean `divisi daemon stop` marks every currently
/// active goal `Paused` (instead of leaving it `Running`, which
/// `scheduler::reconcile`'s PID check would otherwise mistake for a
/// crash) -- returns how many were touched.
pub fn pause_all_active(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE goals SET status = 'paused', updated_at = ?1 WHERE status IN ('planning','running','queued','waiting_on_capacity')",
        params![now()],
    )?)
}

/// E28 spec §10: full reset back to `running` for a goal a human (or
/// `resume_interrupted`) judges recoverable -- clears every hold reason
/// (`blocked_reason`, capacity bookkeeping) so stale state from before
/// the pause/block can't linger and confuse the next tick.
pub fn resume_status(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE goals SET status = 'running', blocked_reason = NULL, capacity_reason = NULL, earliest_retry_at_ms = NULL, updated_at = ?2
         WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

/// Companion to `resume_status` for a manually-triggered `divisi goal
/// resume`: a human overriding a `Blocked` goal wants its nodes retried
/// now, not held to a stale capacity stamp from before the block. NOT
/// called by the automatic `resume_interrupted` path, which deliberately
/// leaves real cooldown stamps alone (a provider's recovery time doesn't
/// reset just because the daemon restarted).
pub fn clear_node_retry_stamps(conn: &Connection, goal_id: &str) -> Result<()> {
    conn.execute("UPDATE graph_nodes SET earliest_retry_at_ms = NULL WHERE goal_id = ?1", params![goal_id])?;
    Ok(())
}

/// E28 spec §9.2: stamped whenever `GoalAmend` processes a real edit —
/// the marker self-heal's coordinator category checks before touching
/// this goal.
pub fn mark_human_edited(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("UPDATE goals SET last_human_edit_at = ?2 WHERE id = ?1", params![id, now()])?;
    Ok(())
}

/// E28 spec §9.2: the self-heal pass's own re-evaluation of a `Blocked`
/// goal — distinct from `resume_status` (the human/`resume_interrupted`
/// path): this one also increments `auto_reevals` so the pass can bound
/// how many times it retries the same goal.
pub fn reevaluate_blocked(conn: &Connection, id: &str) -> Result<u32> {
    conn.execute(
        "UPDATE goals SET status = 'running', blocked_reason = NULL, auto_reevals = auto_reevals + 1, updated_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(conn.query_row("SELECT auto_reevals FROM goals WHERE id = ?1", [id], |r| r.get(0))?)
}

/// E28 spec §9.2: clears a node's pinned `agent` and resets it to
/// `Pending` — used when a node keeps failing on the same explicitly-
/// pinned agent, so the next tick's `select_agent` routes it fresh
/// (walking past the known-bad agent, potentially onto `single-pool`)
/// instead of retrying the same pin forever.
pub fn clear_node_agent_pin(conn: &Connection, goal_id: &str, node_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE graph_nodes SET agent = '', status = 'pending' WHERE goal_id = ?1 AND id = ?2",
        params![goal_id, node_id],
    )?;
    Ok(())
}

pub fn set_status(conn: &Connection, id: &str, status: GoalStatus) -> Result<()> {
    conn.execute(
        "UPDATE goals SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, status.as_str(), now()],
    )?;
    Ok(())
}

pub fn set_blocked(conn: &Connection, id: &str, reason: &str) -> Result<()> {
    conn.execute(
        "UPDATE goals SET status = 'blocked', blocked_reason = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, reason, now()],
    )?;
    Ok(())
}

/// Gives a goal that ran out of capacity waits a fresh allowance: zero waits used, and a wait window
/// that ends `extra_minutes` from now. Self-heal calls this before retrying a capacity block, otherwise
/// the goal re-blocks on its very next dispatch.
pub fn reset_capacity_allowance(conn: &Connection, id: &str, extra_minutes: u32) -> Result<()> {
    let created: String = conn.query_row("SELECT created_at FROM goals WHERE id = ?1", [id], |r| r.get(0))?;
    let elapsed = (chrono::Utc::now() - created.parse().unwrap_or_else(|_| chrono::Utc::now())).num_minutes().max(0) as u32;
    conn.execute(
        "UPDATE goals SET capacity_waits = 0, capacity_wait_minutes_override = ?2, earliest_retry_at_ms = NULL, capacity_reason = NULL WHERE id = ?1",
        params![id, elapsed + extra_minutes],
    )?;
    Ok(())
}

/// Parks a goal until a person answers: `reason` is the question shown in `goal list`/`goal status`.
/// Never ticked (not in `active`), never retried by self-heal; `goal amend` with an answer, or
/// `goal resume`, brings it back.
pub fn wait_for_input(conn: &Connection, id: &str, question: &str) -> Result<()> {
    conn.execute(
        "UPDATE goals SET status = 'waiting_input', blocked_reason = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, question, now()],
    )?;
    Ok(())
}

/// E28 spec §8: moves a goal into `waiting_on_capacity`, recording why and
/// the earliest recovery time, and bumps the `capacity_waits` counter the
/// resume budget is checked against. Never touches node status — the
/// caller (`scheduler::settle_finished_node`) stamps the specific node
/// via `stamp_node_retry` in the same operation.
pub fn set_waiting_on_capacity(conn: &Connection, id: &str, reason: &str, earliest_retry_at_ms: i64) -> Result<()> {
    conn.execute(
        "UPDATE goals SET status = 'waiting_on_capacity', capacity_reason = ?2, earliest_retry_at_ms = ?3,
                          capacity_waits = capacity_waits + 1, updated_at = ?4
         WHERE id = ?1",
        params![id, reason, earliest_retry_at_ms, now()],
    )?;
    Ok(())
}

/// E28 spec §8: clears the capacity-wait bookkeeping and moves the goal
/// back to `running` — called when a stamped node's retry time passes and
/// the scheduler actually re-dispatches it (`capacity_resumed`).
pub fn clear_waiting_on_capacity(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE goals SET status = 'running', capacity_reason = NULL, earliest_retry_at_ms = NULL, updated_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

/// E28 spec §8: `divisi goal amend <id> capacity-budget=N` — raises this
/// goal's own `max_capacity_waits_per_goal` override (reuses the existing
/// `budget=N` amend-text parsing precedent, extended to a second key).
pub fn raise_capacity_budget(conn: &Connection, id: &str, new_budget: u32) -> Result<()> {
    conn.execute(
        "UPDATE goals SET capacity_budget_override = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, new_budget, now()],
    )?;
    Ok(())
}

/// `divisi goal amend <id> auto-merge=true|false` — the human's opt-in
/// switch (default off) for `scheduler::maybe_auto_merge`.
pub fn set_auto_merge(conn: &Connection, id: &str, enabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE goals SET auto_merge = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, enabled as i64, now()],
    )?;
    Ok(())
}

/// `divisi goal amend <id> capacity-minutes=N` — raises this goal's
/// `max_capacity_wait_minutes` override and re-opens a goal blocked on
/// having aged past the global default. Also clears `blocked_reason` and
/// re-opens `Blocked` -> `Running` the same way `raise_dispatch_cap` does,
/// since this cap can be the sole reason a goal is stuck.
pub fn raise_capacity_wait_minutes(conn: &Connection, id: &str, new_cap: u32) -> Result<()> {
    conn.execute(
        "UPDATE goals SET capacity_wait_minutes_override = ?2,
                          status = CASE WHEN status = 'blocked' THEN 'running' ELSE status END,
                          blocked_reason = NULL, updated_at = ?3
         WHERE id = ?1",
        params![id, new_cap, now()],
    )?;
    Ok(())
}

/// E28 spec §8: stamps (or clears, when `None`) one node's
/// `earliest_retry_at_ms` and sets it back to `Pending` so the next tick's
/// `ready_set_at` re-evaluates it once the stamp passes. A plain `SET`
/// (not `update_node`'s `COALESCE`) since clearing the stamp on eventual
/// success is a real requirement, not just "leave it alone".
pub fn stamp_node_retry(conn: &Connection, goal_id: &str, node_id: &str, earliest_retry_at_ms: Option<i64>) -> Result<()> {
    conn.execute(
        "UPDATE graph_nodes SET status = 'pending', earliest_retry_at_ms = ?3 WHERE goal_id = ?1 AND id = ?2",
        params![goal_id, node_id, earliest_retry_at_ms],
    )?;
    Ok(())
}

pub fn set_summary(conn: &Connection, id: &str, summary: &str) -> Result<()> {
    conn.execute(
        "UPDATE goals SET result_summary = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, summary, now()],
    )?;
    Ok(())
}

/// raise the per-goal dispatch cap (spec §4.5, via `GoalAmend`) and re-open
/// a blocked goal so the next tick re-evaluates it.
pub fn raise_dispatch_cap(conn: &Connection, id: &str, new_cap: u32) -> Result<()> {
    conn.execute(
        "UPDATE goals SET max_dispatches = ?2,
                          status = CASE WHEN status = 'blocked' THEN 'running' ELSE status END,
                          blocked_reason = NULL, updated_at = ?3
         WHERE id = ?1",
        params![id, new_cap, now()],
    )?;
    Ok(())
}

/// `divisi goal amend <id> minutes=N` — raises the per-goal wall-clock cap
/// (`max_goal_minutes`, spec §4.5) and re-opens a goal blocked on it.
/// `budget=N` alone can't recover this: a goal blocked on elapsed wall
/// time re-blocks immediately on the next tick if only its dispatch cap
/// moved, since `now() - created_at` already exceeds the unchanged
/// `max_minutes`.
pub fn raise_time_cap(conn: &Connection, id: &str, new_cap: u32) -> Result<()> {
    conn.execute(
        "UPDATE goals SET max_minutes = ?2,
                          status = CASE WHEN status = 'blocked' THEN 'running' ELSE status END,
                          blocked_reason = NULL, updated_at = ?3
         WHERE id = ?1",
        params![id, new_cap, now()],
    )?;
    Ok(())
}

pub fn bump_dispatches(conn: &Connection, id: &str) -> Result<u32> {
    conn.execute(
        "UPDATE goals SET dispatches = dispatches + 1, updated_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(conn.query_row("SELECT dispatches FROM goals WHERE id = ?1", [id], |r| r.get(0))?)
}

pub fn bump_supervisor_patches(conn: &Connection, id: &str) -> Result<u32> {
    conn.execute(
        "UPDATE goals SET supervisor_patches = supervisor_patches + 1, updated_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(conn.query_row("SELECT supervisor_patches FROM goals WHERE id = ?1", [id], |r| r.get(0))?)
}

/// writes `plan_json` and replaces every `graph_nodes` row for this goal in
/// one transaction. delete-all-then-insert is fine here — a goal's graph is
/// small (2–8 nodes) and only the coordinator writes it.
pub fn save_graph(conn: &mut Connection, goal_id: &str, graph: &TaskGraph) -> Result<()> {
    let plan_json = serde_json::to_string(graph)?;
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE goals SET plan_json = ?2, updated_at = ?3 WHERE id = ?1",
        params![goal_id, plan_json, now()],
    )?;
    tx.execute("DELETE FROM graph_nodes WHERE goal_id = ?1", [goal_id])?;
    for n in &graph.nodes {
        tx.execute(
            "INSERT INTO graph_nodes
             (goal_id, id, desc, kind, effort, agent, depends_on, status, task_id, attempts, worktree, output_ref, earliest_retry_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                goal_id,
                n.id,
                n.desc,
                n.kind.as_str(),
                n.effort.as_str(),
                n.agent,
                serde_json::to_string(&n.depends_on)?,
                n.status.as_str(),
                n.task_id,
                n.attempts,
                n.worktree as i64,
                n.output_ref,
                n.earliest_retry_at_ms,
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn load_graph(conn: &Connection, goal_id: &str) -> Result<TaskGraph> {
    let mut stmt = conn.prepare(
        "SELECT id, desc, kind, effort, agent, depends_on, status, task_id, attempts, worktree, output_ref, earliest_retry_at_ms
         FROM graph_nodes WHERE goal_id = ?1 ORDER BY id ASC",
    )?;
    let nodes = stmt
        .query_map([goal_id], |row| {
            let depends_on: String = row.get("depends_on")?;
            Ok(Node {
                id: row.get("id")?,
                desc: row.get("desc")?,
                kind: NodeKind::parse(&row.get::<_, String>("kind")?).unwrap_or(NodeKind::Code),
                effort: Effort::parse(&row.get::<_, String>("effort")?).unwrap_or(Effort::Standard),
                agent: row.get("agent")?,
                depends_on: serde_json::from_str(&depends_on).unwrap_or_default(),
                status: NodeStatus::parse(&row.get::<_, String>("status")?).unwrap_or(NodeStatus::Pending),
                task_id: row.get("task_id")?,
                attempts: row.get("attempts")?,
                worktree: row.get::<_, i64>("worktree")? != 0,
                output_ref: row.get("output_ref")?,
                earliest_retry_at_ms: row.get("earliest_retry_at_ms")?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(TaskGraph { nodes })
}

/// updates one node's mutable fields in place. `None` leaves a field
/// untouched; `status` is always written.
pub fn update_node(
    conn: &Connection,
    goal_id: &str,
    node_id: &str,
    status: NodeStatus,
    task_id: Option<i64>,
    output_ref: Option<&str>,
    attempts: Option<u32>,
) -> Result<()> {
    conn.execute(
        "UPDATE graph_nodes SET
            status = ?3,
            task_id = COALESCE(?4, task_id),
            output_ref = COALESCE(?5, output_ref),
            attempts = COALESCE(?6, attempts)
         WHERE goal_id = ?1 AND id = ?2",
        params![goal_id, node_id, status.as_str(), task_id, output_ref, attempts],
    )?;
    Ok(())
}

/// Rewrites a node's prompt and clears its `task_id`. Used by `careful`
/// mode (`divisi loop`) to feed each iteration the previous output.
pub fn set_node_desc(conn: &Connection, goal_id: &str, node_id: &str, desc: &str) -> Result<()> {
    conn.execute(
        "UPDATE graph_nodes SET desc = ?3, task_id = NULL WHERE goal_id = ?1 AND id = ?2",
        params![goal_id, node_id, desc],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::graph::Node;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        conn
    }

    fn node(id: &str) -> Node {
        Node {
            id: id.into(),
            desc: format!("do {id}"),
            kind: NodeKind::Code,
            effort: Effort::Standard,
            agent: String::new(),
            depends_on: vec![],
            status: NodeStatus::Pending,
            task_id: None,
            attempts: 0,
            worktree: true,
            output_ref: None,
            earliest_retry_at_ms: None,
        }
    }

    #[test]
    fn a_goal_waiting_for_input_keeps_its_question_and_is_never_ticked() {
        let conn = Connection::open_in_memory().unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        let s = crate::coordinator::session::new_session(&conn, std::path::Path::new("/tmp")).unwrap();
        let g = create(&conn, &s.id, "g", GoalMode::Auto, 25, 60).unwrap();
        wait_for_input(&conn, &g.id, "approve the release?").unwrap();
        let got = get(&conn, &g.id).unwrap().unwrap();
        assert_eq!(got.status, GoalStatus::WaitingInput);
        assert_eq!(got.blocked_reason.as_deref(), Some("approve the release?"));
        assert!(active(&conn).unwrap().iter().all(|a| a.id != g.id));
        resume_status(&conn, &g.id).unwrap();
        assert_eq!(get(&conn, &g.id).unwrap().unwrap().status, GoalStatus::Running);
    }

    #[test]
    fn find_overlapping_matches_exact_duplicate_text() {
        let conn = mem();
        let s = super::super::session::new_session(&conn, std::path::Path::new("/tmp/p")).unwrap();
        let g = create(&conn, &s.id, "fix the login bug in auth.rs", GoalMode::Auto, 25, 60).unwrap();
        let found = find_overlapping(&conn, "fix the login bug in auth.rs").unwrap();
        assert_eq!(found.unwrap().id, g.id);
    }

    #[test]
    fn find_overlapping_matches_near_duplicate_phrasing() {
        let conn = mem();
        let s = super::super::session::new_session(&conn, std::path::Path::new("/tmp/p")).unwrap();
        create(&conn, &s.id, "please fix the login bug found in auth.rs today", GoalMode::Auto, 25, 60).unwrap();
        let found = find_overlapping(&conn, "fix login bug in auth.rs").unwrap();
        assert!(found.is_some());
    }

    #[test]
    fn find_overlapping_ignores_distinct_asks() {
        let conn = mem();
        let s = super::super::session::new_session(&conn, std::path::Path::new("/tmp/p")).unwrap();
        create(&conn, &s.id, "fix the login bug in auth.rs", GoalMode::Auto, 25, 60).unwrap();
        let found = find_overlapping(&conn, "add dark mode to the settings page").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn find_overlapping_does_not_match_a_short_text_sharing_only_boilerplate_with_a_long_one() {
        // Regression test: min-based overlap let a short, distinct ask
        // spuriously match a long, unrelated goal purely by sharing
        // common instructional phrasing -- three real epic goals with
        // different topics (quality audit, docs, vault cleanup) each got
        // silently folded into an unrelated already-active goal because
        // every submission this session reused the same boilerplate
        // ("exhaust free-pool agents before paid ones", "commit locally
        // as you go", "do not push to any remote").
        let conn = mem();
        let s = super::super::session::new_session(&conn, std::path::Path::new("/tmp/p")).unwrap();
        // Realistic lengths matter here: the shared boilerplate sentence
        // is a small fraction of each goal's real word count, same as
        // production submissions, not a toy-sized text where boilerplate
        // dominates the token set.
        let shared_boilerplate = "Exhaust free-pool agents before paid ones like claude or codex. Commit locally as you go with no AI or Co-Authored-By attribution, checking git log for this repo's own house commit style first. Do not push to any remote — a human reviews and pushes separately. If you hit a real decision only a human can make, write it down clearly in a HANDOFF-style note and move on to other work rather than blocking on it.";
        create(
            &conn,
            &s.id,
            &format!(
                "Make divisi self-healing as an ongoing standing concern, not a one-off audit: itself, single-pool (the free-provider dispatch engine), divisi-gateway, and divisi-lsp. Read the architecture doc's diagnostic sections first for already-diagnosed issues. Actually find the grok worktree-cwd spawn-failure root cause with real tracing, not guessing. Run the full test suite and clippy, fix real warnings you find. Verify divisi-gateway/divisi-lsp's lazy-spawn and idle-eviction work end to end against a real process. Audit the free-pool's ~44 providers for any more with the aihorde-class chat-prose problem. {shared_boilerplate}"
            ),
            GoalMode::Auto,
            25,
            60,
        )
        .unwrap();
        let found = find_overlapping(
            &conn,
            &format!(
                "Cross-cutting quality audit: pick one repo referenced from the quality/testing epic that has the weakest test or CI coverage, and actually improve it — add missing tests for real untested code paths, fix a flaky or broken CI gate, or wire up a missing check. Ship one concrete improvement with evidence it worked, not a survey. Write findings to that epic's own tracking doc. {shared_boilerplate}"
            ),
        )
        .unwrap();
        assert!(found.is_none(), "a distinct topic must not match just because it shares a boilerplate closing paragraph: {found:?}");
    }

    #[test]
    fn find_overlapping_ignores_terminal_goals() {
        let conn = mem();
        let s = super::super::session::new_session(&conn, std::path::Path::new("/tmp/p")).unwrap();
        let g = create(&conn, &s.id, "fix the login bug in auth.rs", GoalMode::Auto, 25, 60).unwrap();
        set_status(&conn, &g.id, GoalStatus::Done).unwrap();
        let found = find_overlapping(&conn, "fix the login bug in auth.rs").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn create_goal_persists_and_titles_session() {
        let mut conn = mem();
        let s = super::super::session::new_session(&conn, std::path::Path::new("/tmp/p")).unwrap();
        let g = create(&conn, &s.id, "wire up the widget", GoalMode::Auto, 25, 60).unwrap();
        assert_eq!(g.status, GoalStatus::Planning);
        assert_eq!(g.max_dispatches, 25);

        let title = super::super::session::get(&conn, &s.id).unwrap().unwrap().title;
        assert_eq!(title, "wire up the widget");

        let _ = &mut conn;
    }

    #[test]
    fn save_then_load_graph_roundtrips_node_status() {
        let mut conn = mem();
        let s = super::super::session::new_session(&conn, std::path::Path::new("/tmp/p")).unwrap();
        let g = create(&conn, &s.id, "g", GoalMode::Auto, 25, 60).unwrap();

        let graph = TaskGraph { nodes: vec![node("s1"), node("s2")] };
        save_graph(&mut conn, &g.id, &graph).unwrap();

        update_node(&conn, &g.id, "s1", NodeStatus::Done, Some(42), Some("/tmp/out"), Some(1)).unwrap();
        let loaded = load_graph(&conn, &g.id).unwrap();
        let s1 = loaded.find("s1").unwrap();
        assert_eq!(s1.status, NodeStatus::Done);
        assert_eq!(s1.task_id, Some(42));
        assert_eq!(s1.output_ref.as_deref(), Some("/tmp/out"));
        assert_eq!(loaded.find("s2").unwrap().status, NodeStatus::Pending);
    }

    #[test]
    fn bump_counters_increment_and_cap_raise_unblocks() {
        let conn = mem();
        let s = super::super::session::new_session(&conn, std::path::Path::new("/tmp/p")).unwrap();
        let g = create(&conn, &s.id, "g", GoalMode::Auto, 1, 60).unwrap();

        assert_eq!(bump_dispatches(&conn, &g.id).unwrap(), 1);
        assert_eq!(bump_supervisor_patches(&conn, &g.id).unwrap(), 1);

        set_blocked(&conn, &g.id, "hit the cap").unwrap();
        assert_eq!(get(&conn, &g.id).unwrap().unwrap().status, GoalStatus::Blocked);
        raise_dispatch_cap(&conn, &g.id, 10).unwrap();
        let g2 = get(&conn, &g.id).unwrap().unwrap();
        assert_eq!(g2.status, GoalStatus::Running);
        assert_eq!(g2.max_dispatches, 10);
        assert!(g2.blocked_reason.is_none());
    }

    #[test]
    fn raise_time_cap_unblocks_a_goal_blocked_on_elapsed_wall_time() {
        let conn = mem();
        let s = super::super::session::new_session(&conn, std::path::Path::new("/tmp/p")).unwrap();
        let g = create(&conn, &s.id, "g", GoalMode::Auto, 25, 60).unwrap();

        set_blocked(&conn, &g.id, "time budget spent: 64 min elapsed of 60 min cap").unwrap();
        assert_eq!(get(&conn, &g.id).unwrap().unwrap().status, GoalStatus::Blocked);

        raise_time_cap(&conn, &g.id, 120).unwrap();
        let g2 = get(&conn, &g.id).unwrap().unwrap();
        assert_eq!(g2.status, GoalStatus::Running);
        assert_eq!(g2.max_minutes, 120);
        assert!(g2.blocked_reason.is_none());
    }

    #[test]
    fn raise_capacity_wait_minutes_unblocks_a_goal_stuck_on_its_aged_out_capacity_cap() {
        let conn = mem();
        let s = super::super::session::new_session(&conn, std::path::Path::new("/tmp/p")).unwrap();
        let g = create(&conn, &s.id, "g", GoalMode::Auto, 25, 60).unwrap();
        assert!(g.capacity_wait_minutes_override.is_none());

        set_blocked(&conn, &g.id, "waited 12.6h for capacity, still exhausted").unwrap();
        assert_eq!(get(&conn, &g.id).unwrap().unwrap().status, GoalStatus::Blocked);

        raise_capacity_wait_minutes(&conn, &g.id, 2880).unwrap();
        let g2 = get(&conn, &g.id).unwrap().unwrap();
        assert_eq!(g2.status, GoalStatus::Running);
        assert_eq!(g2.capacity_wait_minutes_override, Some(2880));
        assert!(g2.blocked_reason.is_none());
    }
}
