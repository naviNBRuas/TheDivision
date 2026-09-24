//! Learned preferences + pending approvals — the "answer for me using
//! memory and my preferences, unless it really doesn't know" layer on top
//! of `permissions.rs`'s static deny/ask/allow rules.
//!
//! When `permissions::evaluate` returns `Ask` (or a resource matches no
//! rule at all, which also means `Ask`), `evaluate_and_learn` checks here
//! for a previously *learned* decision before escalating to a human — a
//! pattern the user has approved/denied before, confidently enough to
//! auto-apply. No match means a real pending-approval record is created
//! instead of guessing.
//!
//! Lives in `divisi-core` (not `divisi-runtime`) specifically so both the
//! daemon and the `divisi-gateway` gateway (a separate process that already
//! reads `mcp.toml` directly rather than round-tripping through the
//! daemon socket — see `crates/divisi-gateway/src/gateway.rs`) can read/write
//! the same SQLite table directly, without needing IPC for every
//! permission check.
//!
//! **Scope, stated plainly**: this gates tool calls divisi itself
//! controls — right now, only `divisi-gateway`'s `invoke_mcp`. It does *not*
//! intercept an agent CLI's own mid-run permission prompts (e.g. Claude
//! Code asking to edit a file) — that would need per-agent research into
//! each CLI's own approval/hook mechanism (Claude Code has one; whether
//! others do is unconfirmed) and isn't built here. A task run either
//! trusts the agent's own judgment calls as it always has, or is gated at
//! the MCP-tool-call boundary — there's no third option yet.

use crate::permissions::{Decision, Rule};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS preferences (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            pattern TEXT NOT NULL UNIQUE,
            decision TEXT NOT NULL,
            confidence REAL NOT NULL,
            learned_from TEXT,
            created_at TEXT NOT NULL
        )",
        (),
    )?;
    // divisi's own MCP tools were `singlecli:<tool>` before the rename; carry learned verdicts over.
    conn.execute(
        "UPDATE OR IGNORE preferences SET pattern = 'divisi:' || substr(pattern, 11) WHERE pattern LIKE 'singlecli:%'",
        (),
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS approvals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            resource TEXT NOT NULL,
            context TEXT,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            resolved_at TEXT
        )",
        (),
    )?;
    Ok(())
}

fn decision_as_str(d: Decision) -> &'static str {
    match d {
        Decision::Deny => "deny",
        Decision::Ask => "ask",
        Decision::Allow => "allow",
    }
}

fn parse_decision(s: &str) -> Result<Decision> {
    Ok(match s {
        "deny" => Decision::Deny,
        "ask" => Decision::Ask,
        "allow" => Decision::Allow,
        other => anyhow::bail!("unknown decision: {other}"),
    })
}

/// Minimum confidence for a learned preference to auto-apply rather than
/// escalate — a low-confidence guess should still ask, not gamble.
pub const MIN_AUTO_APPLY_CONFIDENCE: f64 = 0.8;

pub struct Preference {
    pub id: i64,
    pub pattern: String,
    pub decision: Decision,
    pub confidence: f64,
    pub learned_from: Option<String>,
    pub created_at: String,
}

/// Records (or updates) a learned decision for `pattern`. One row per
/// exact pattern — re-learning the same pattern overwrites it.
pub fn learn(conn: &Connection, pattern: &str, decision: Decision, confidence: f64, learned_from: Option<&str>) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO preferences (pattern, decision, confidence, learned_from, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(pattern) DO UPDATE SET decision = excluded.decision, confidence = excluded.confidence, learned_from = excluded.learned_from, created_at = excluded.created_at",
        params![pattern, decision_as_str(decision), confidence.clamp(0.0, 1.0), learned_from, now],
    )
    .context("recording learned preference")?;
    Ok(())
}

pub fn list_preferences(conn: &Connection) -> Result<Vec<Preference>> {
    let mut stmt = conn.prepare("SELECT id, pattern, decision, confidence, learned_from, created_at FROM preferences ORDER BY pattern")?;
    let rows = stmt.query_map([], |row| {
        let decision_str: String = row.get(2)?;
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, decision_str, row.get::<_, f64>(3)?, row.get::<_, Option<String>>(4)?, row.get::<_, String>(5)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, pattern, decision_str, confidence, learned_from, created_at) = row?;
        out.push(Preference { id, pattern, decision: parse_decision(&decision_str)?, confidence, learned_from, created_at });
    }
    Ok(out)
}

/// Longest-matching-prefix lookup — same algorithm as
/// `permissions::evaluate`, over learned patterns instead of static
/// rules. Never returns a match below `min_confidence`.
pub fn lookup(conn: &Connection, resource: &str, min_confidence: f64) -> Result<Option<(Decision, f64)>> {
    let candidates = list_preferences(conn)?;
    let best = candidates
        .into_iter()
        .filter(|p| resource.starts_with(&p.pattern) && p.confidence >= min_confidence)
        .max_by_key(|p| p.pattern.len());
    Ok(best.map(|p| (p.decision, p.confidence)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalStatus {
    Pending,
    Allowed,
    Denied,
    /// A one-time `resolve(.., remember: false)` decision that has already
    /// been consumed by the retried MCP call it was meant for. Live-
    /// verification finding: before this status existed, `evaluate_and_learn`
    /// had no way to see that a plain `--allow` (no `--remember`) had just
    /// resolved this exact resource, so the immediate retry of the same
    /// `task_run`/`orchestrate_*` call re-escalated into a brand-new
    /// pending approval every time — `--allow` alone could never actually
    /// let a call through, only `--remember` (a standing preference) could.
    /// `Used` marks a one-time grant as spent so it can unblock exactly the
    /// one retry it was resolved for, without granting every future call
    /// the way a learned preference does.
    Used,
}

pub struct Approval {
    pub id: i64,
    pub resource: String,
    pub context: Option<String>,
    pub status: ApprovalStatus,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

fn status_as_str(s: ApprovalStatus) -> &'static str {
    match s {
        ApprovalStatus::Pending => "pending",
        ApprovalStatus::Allowed => "allowed",
        ApprovalStatus::Denied => "denied",
        ApprovalStatus::Used => "used",
    }
}

fn parse_status(s: &str) -> Result<ApprovalStatus> {
    Ok(match s {
        "pending" => ApprovalStatus::Pending,
        "allowed" => ApprovalStatus::Allowed,
        "denied" => ApprovalStatus::Denied,
        "used" => ApprovalStatus::Used,
        other => anyhow::bail!("unknown approval status: {other}"),
    })
}

pub fn request_approval(conn: &Connection, resource: &str, context: Option<&str>) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO approvals (resource, context, status, created_at, resolved_at) VALUES (?1, ?2, ?3, ?4, NULL)",
        params![resource, context, status_as_str(ApprovalStatus::Pending), now],
    )
    .context("recording pending approval")?;
    Ok(conn.last_insert_rowid())
}

pub fn get_approval(conn: &Connection, id: i64) -> Result<Option<Approval>> {
    conn.query_row("SELECT id, resource, context, status, created_at, resolved_at FROM approvals WHERE id = ?1", params![id], row_to_approval)
        .optional()
        .context("querying approval by id")
}

pub fn list_pending(conn: &Connection) -> Result<Vec<Approval>> {
    let mut stmt = conn.prepare("SELECT id, resource, context, status, created_at, resolved_at FROM approvals WHERE status = 'pending' ORDER BY created_at")?;
    let rows = stmt.query_map([], row_to_approval)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collecting pending approvals")
}

fn row_to_approval(row: &rusqlite::Row) -> rusqlite::Result<Approval> {
    let status_str: String = row.get(3)?;
    Ok(Approval {
        id: row.get(0)?,
        resource: row.get(1)?,
        context: row.get(2)?,
        status: parse_status(&status_str).map_err(|e| rusqlite::Error::InvalidColumnType(3, e.to_string(), rusqlite::types::Type::Text))?,
        created_at: row.get(4)?,
        resolved_at: row.get(5)?,
    })
}

/// Resolves a pending approval. When `remember` is set, also records the
/// decision as a learned preference for `resource` (confidence 1.0, since
/// it's a direct human decision, not a guess) — so the same resource
/// pattern doesn't ask twice. Errors if `id` doesn't exist or was already
/// resolved.
pub fn resolve(conn: &Connection, id: i64, allow: bool, remember: bool) -> Result<()> {
    let approval = get_approval(conn, id)?.with_context(|| format!("no approval with id {id}"))?;
    if approval.status != ApprovalStatus::Pending {
        anyhow::bail!("approval #{id} was already resolved");
    }
    let now = chrono::Utc::now().to_rfc3339();
    let status = if allow { ApprovalStatus::Allowed } else { ApprovalStatus::Denied };
    conn.execute("UPDATE approvals SET status = ?1, resolved_at = ?2 WHERE id = ?3", params![status_as_str(status), now, id])?;
    if remember {
        let decision = if allow { Decision::Allow } else { Decision::Deny };
        learn(conn, &approval.resource, decision, 1.0, Some("user_approval"))?;
    }
    Ok(())
}

/// Marks an already-resolved (allowed or denied) approval as spent, so it cannot grant or deny
/// the next request for the same resource. For callers that act on an answer immediately
/// instead of retrying. A pending approval is left untouched.
pub fn mark_used(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE approvals SET status = ?1 WHERE id = ?2 AND status IN ('allowed', 'denied')",
        params![status_as_str(ApprovalStatus::Used), id],
    )
    .context("marking approval as used")?;
    Ok(())
}

/// What a caller should actually do about `resource`, per the full
/// pipeline: static `permissions.toml` rules, then learned preferences,
/// then an unconsumed one-time approval, then (only if none of those has
/// an answer) a real pending-approval record — never a silent guess.
pub enum Verdict {
    Allow,
    Deny,
    /// Approval id — the caller should surface this to the user (`single
    /// approval list`/the TUI) rather than block indefinitely; MCP tool
    /// calls in particular can't block a synchronous request forever.
    PendingApproval(i64),
}

/// The most recently resolved-but-unconsumed one-time (`--allow`/`--deny`
/// without `--remember`) approval for `resource`, if any, immediately
/// marked `Used` so it can only unblock one retry. Exact-string match only
/// — unlike `lookup`'s prefix match for learned preferences, a one-time
/// grant is scoped to the specific resource it was raised for, not a
/// pattern.
fn take_unconsumed_resolution(conn: &Connection, resource: &str) -> Result<Option<Decision>> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, status FROM approvals
             WHERE resource = ?1 AND status IN ('allowed', 'denied')
             ORDER BY resolved_at DESC LIMIT 1",
            params![resource],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("looking up unconsumed one-time approval")?;
    let Some((id, status_str)) = row else { return Ok(None) };
    conn.execute("UPDATE approvals SET status = ?1 WHERE id = ?2", params![status_as_str(ApprovalStatus::Used), id])
        .context("marking one-time approval as used")?;
    Ok(Some(match parse_status(&status_str)? {
        ApprovalStatus::Allowed => Decision::Allow,
        ApprovalStatus::Denied => Decision::Deny,
        ApprovalStatus::Pending | ApprovalStatus::Used => unreachable!("query filters to allowed/denied only"),
    }))
}

pub fn evaluate_and_learn(rules: &[Rule], conn: &Connection, resource: &str, context: Option<&str>) -> Result<Verdict> {
    match crate::permissions::evaluate(rules, resource) {
        Decision::Allow => return Ok(Verdict::Allow),
        Decision::Deny => return Ok(Verdict::Deny),
        Decision::Ask => {}
    }
    if let Some((decision, _confidence)) = lookup(conn, resource, MIN_AUTO_APPLY_CONFIDENCE)? {
        match decision {
            Decision::Allow => return Ok(Verdict::Allow),
            Decision::Deny => return Ok(Verdict::Deny),
            Decision::Ask => {} // stored but not confident enough to act on; fall through to escalate
        }
    }
    if let Some(decision) = take_unconsumed_resolution(conn, resource)? {
        return Ok(match decision {
            Decision::Allow => Verdict::Allow,
            Decision::Deny => Verdict::Deny,
            Decision::Ask => unreachable!("take_unconsumed_resolution never returns Ask"),
        });
    }
    Ok(Verdict::PendingApproval(request_approval(conn, resource, context)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn learn_then_lookup_round_trips_and_respects_confidence_floor() {
        let conn = test_conn();
        learn(&conn, "mcp:git", Decision::Allow, 0.9, Some("test")).unwrap();
        assert_eq!(lookup(&conn, "mcp:git:git_status", 0.8).unwrap(), Some((Decision::Allow, 0.9)));
        assert_eq!(lookup(&conn, "mcp:git:git_status", 0.95).unwrap(), None, "below the confidence floor must not match");
        assert_eq!(lookup(&conn, "mcp:unrelated", 0.0).unwrap(), None);
    }

    #[test]
    fn longest_matching_pattern_wins() {
        let conn = test_conn();
        learn(&conn, "mcp:", Decision::Ask, 1.0, None).unwrap();
        learn(&conn, "mcp:git:", Decision::Allow, 1.0, None).unwrap();
        let (decision, _) = lookup(&conn, "mcp:git:git_status", 0.0).unwrap().unwrap();
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn evaluate_and_learn_creates_a_real_pending_approval_when_nothing_confident_exists() {
        let conn = test_conn();
        let verdict = evaluate_and_learn(&[], &conn, "mcp:brand-new-server:do-something", Some("test context")).unwrap();
        let Verdict::PendingApproval(id) = verdict else { panic!("expected a pending approval") };
        let pending = list_pending(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].resource, "mcp:brand-new-server:do-something");
    }

    #[test]
    fn resolving_with_remember_prevents_the_same_resource_from_asking_twice() {
        let conn = test_conn();
        let Verdict::PendingApproval(id) = evaluate_and_learn(&[], &conn, "mcp:slack:post_message", None).unwrap() else {
            panic!("expected pending")
        };
        resolve(&conn, id, true, true).unwrap();

        // Second time, the learned preference should auto-allow instead of asking again.
        let verdict = evaluate_and_learn(&[], &conn, "mcp:slack:post_message", None).unwrap();
        assert!(matches!(verdict, Verdict::Allow));
    }

    #[test]
    fn resolving_without_remember_does_not_learn() {
        let conn = test_conn();
        let Verdict::PendingApproval(id) = evaluate_and_learn(&[], &conn, "mcp:once:do", None).unwrap() else { panic!("expected pending") };
        resolve(&conn, id, true, false).unwrap();
        assert!(list_preferences(&conn).unwrap().is_empty());
    }

    #[test]
    fn one_time_allow_unblocks_exactly_one_retry_then_asks_again() {
        let conn = test_conn();
        // First call: nothing known yet, escalate.
        let Verdict::PendingApproval(id) = evaluate_and_learn(&[], &conn, "divisi:task_run", None).unwrap() else {
            panic!("expected pending")
        };
        // Human resolves with a plain one-time --allow (no --remember).
        resolve(&conn, id, true, false).unwrap();

        // The retried call (same resource, no new args tracked) must now
        // go through — this is the exact `task_run` retry-after-approve
        // loop that used to re-escalate into a brand-new pending approval
        // every time because nothing ever consulted the resolved row.
        let verdict = evaluate_and_learn(&[], &conn, "divisi:task_run", None).unwrap();
        assert!(matches!(verdict, Verdict::Allow));

        // A one-time grant is exactly that — one time. A further retry
        // must escalate again, not silently keep allowing (that's what
        // `--remember` is for).
        let Verdict::PendingApproval(_) = evaluate_and_learn(&[], &conn, "divisi:task_run", None).unwrap() else {
            panic!("expected the grant to be spent and a fresh pending approval raised")
        };
    }

    #[test]
    fn one_time_deny_also_blocks_exactly_one_retry_then_asks_again() {
        let conn = test_conn();
        let Verdict::PendingApproval(id) = evaluate_and_learn(&[], &conn, "divisi:task_run", None).unwrap() else {
            panic!("expected pending")
        };
        resolve(&conn, id, false, false).unwrap();

        assert!(matches!(evaluate_and_learn(&[], &conn, "divisi:task_run", None).unwrap(), Verdict::Deny));
        let Verdict::PendingApproval(_) = evaluate_and_learn(&[], &conn, "divisi:task_run", None).unwrap() else {
            panic!("expected the grant to be spent and a fresh pending approval raised")
        };
    }

    #[test]
    fn resolving_twice_errors() {
        let conn = test_conn();
        let Verdict::PendingApproval(id) = evaluate_and_learn(&[], &conn, "mcp:x", None).unwrap() else { panic!("expected pending") };
        resolve(&conn, id, true, false).unwrap();
        assert!(resolve(&conn, id, true, false).is_err());
    }

    #[test]
    fn deny_rule_short_circuits_before_any_preference_lookup() {
        let conn = test_conn();
        // Even with a contradictory learned "allow", a static Deny rule must win.
        learn(&conn, "mcp:danger", Decision::Allow, 1.0, None).unwrap();
        let rules = vec![Rule { pattern: "mcp:danger".into(), decision: Decision::Deny }];
        assert!(matches!(evaluate_and_learn(&rules, &conn, "mcp:danger:do", None).unwrap(), Verdict::Deny));
    }

    #[test]
    fn mark_used_stops_a_resolved_approval_from_granting_the_next_request() {
        let conn = test_conn();
        // The user allows one chat action, which is executed on the spot.
        let id = request_approval(&conn, "chat:goal.cancel", Some("ctx")).unwrap();
        resolve(&conn, id, true, false).unwrap();
        mark_used(&conn, id).unwrap();
        assert_eq!(get_approval(&conn, id).unwrap().unwrap().status, ApprovalStatus::Used);
        // The next identical request must ask again instead of inheriting that one-time grant.
        let Verdict::PendingApproval(next) = evaluate_and_learn(&[], &conn, "chat:goal.cancel", None).unwrap() else {
            panic!("a spent approval must not auto-allow the next request");
        };
        assert_ne!(next, id);
    }

    #[test]
    fn without_mark_used_the_one_time_grant_leaks_to_the_next_request() {
        // Documents the behaviour mark_used exists to prevent.
        let conn = test_conn();
        let id = request_approval(&conn, "chat:goal.cancel", None).unwrap();
        resolve(&conn, id, true, false).unwrap();
        assert!(matches!(evaluate_and_learn(&[], &conn, "chat:goal.cancel", None).unwrap(), Verdict::Allow));
    }

    #[test]
    fn mark_used_leaves_pending_approvals_alone() {
        let conn = test_conn();
        let id = request_approval(&conn, "chat:config", None).unwrap();
        mark_used(&conn, id).unwrap();
        assert_eq!(get_approval(&conn, id).unwrap().unwrap().status, ApprovalStatus::Pending);
    }
}
