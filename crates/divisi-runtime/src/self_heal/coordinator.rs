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
    run_step(conn, report, Category::Coordinator, "reeval_blocked_goals", || reeval_blocked_goals(conn, cfg));
    run_step(conn, report, Category::Coordinator, "reroute_repeated_failures", || reroute_repeated_failures(ctx, conn));
    run_step(conn, report, Category::Coordinator, "routing_toml_drift", || routing_toml_drift(ctx));
    run_step(conn, report, Category::Coordinator, "coordinator_toml_sanity", || coordinator_toml_sanity(ctx));
    Ok(())
}

fn recently_human_edited(g: &goal::Goal) -> bool {
    g.last_human_edit_at.as_deref().and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok()).is_some_and(|t| chrono::Utc::now() - t < HUMAN_EDIT_GRACE)
}

/// A goal `Blocked` for longer than `blocked_reeval_minutes` with a
/// capacity/supervisor reason is moved back to `Running` so the next tick
/// re-evaluates it for real — "if the blocking condition cleared" is
/// exactly what that next tick's ready-set computation answers; there's
/// nothing this substep needs to precompute. Bounded by
/// `max_auto_reevals_per_goal`; never touches a goal amended in the last
/// hour.
fn reeval_blocked_goals(conn: &Connection, cfg: &SelfHealConfig) -> Result<String> {
    let blocked = goal::list_by_status(conn, GoalStatus::Blocked)?;
    let mut reevaluated = Vec::new();
    let mut skipped_bounded = 0;
    let mut skipped_human = 0;

    for g in blocked {
        let reason = g.blocked_reason.as_deref().unwrap_or("");
        if !(reason.contains("capacity") || reason.contains("supervisor")) {
            continue; // not this substep's concern -- a human decision, budget block, etc.
        }
        let blocked_age_minutes = (chrono::Utc::now() - g.updated_at.parse().unwrap_or_else(|_| chrono::Utc::now())).num_minutes();
        if blocked_age_minutes < cfg.blocked_reeval_minutes as i64 {
            continue; // not stale enough yet
        }
        if recently_human_edited(&g) {
            skipped_human += 1;
            continue;
        }
        if g.auto_reevals >= cfg.max_auto_reevals_per_goal {
            skipped_bounded += 1;
            continue;
        }
        goal::reevaluate_blocked(conn, &g.id)?;
        reevaluated.push(g.id);
    }

    Ok(format!("reevaluated: [{}]; bounded-out: {skipped_bounded}; human-edited (skipped): {skipped_human}", reevaluated.join(", ")))
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

    #[test]
    fn long_blocked_capacity_goal_is_reevaluated_and_reticked_when_cleared() {
        let conn = test_conn();
        let cfg = SelfHealConfig::default();
        let g = make_blocked_goal(&conn, "waited 12.0h for capacity, still exhausted", 60);

        let detail = reeval_blocked_goals(&conn, &cfg).unwrap();
        assert!(detail.contains(&g.id), "{detail}");
        let reloaded = goal::get(&conn, &g.id).unwrap().unwrap();
        assert_eq!(reloaded.status, GoalStatus::Running);
        assert_eq!(reloaded.auto_reevals, 1);
    }

    #[test]
    fn reeval_bounded_by_max_auto_reevals() {
        let conn = test_conn();
        let cfg = SelfHealConfig { max_auto_reevals_per_goal: 1, ..SelfHealConfig::default() };
        let g = make_blocked_goal(&conn, "supervisor gave up", 60);

        reeval_blocked_goals(&conn, &cfg).unwrap(); // 1st reeval: allowed
        goal::set_blocked(&conn, &g.id, "supervisor gave up again").unwrap();
        conn.execute("UPDATE goals SET updated_at = ?2 WHERE id = ?1", rusqlite::params![g.id, (chrono::Utc::now() - chrono::Duration::minutes(60)).to_rfc3339()]).unwrap();

        let detail = reeval_blocked_goals(&conn, &cfg).unwrap(); // 2nd: bounded out
        assert!(detail.contains("bounded-out: 1"), "{detail}");
        assert_eq!(goal::get(&conn, &g.id).unwrap().unwrap().status, GoalStatus::Blocked);
    }

    #[test]
    fn human_edited_goal_is_never_touched() {
        let conn = test_conn();
        let cfg = SelfHealConfig::default();
        let g = make_blocked_goal(&conn, "waited too long for capacity", 60);
        goal::mark_human_edited(&conn, &g.id).unwrap();

        let detail = reeval_blocked_goals(&conn, &cfg).unwrap();
        assert!(detail.contains("human-edited (skipped): 1"), "{detail}");
        assert_eq!(goal::get(&conn, &g.id).unwrap().unwrap().status, GoalStatus::Blocked);
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
