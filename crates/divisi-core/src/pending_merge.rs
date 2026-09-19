//! Live merge-confirmation queue for the coordinator's opt-in auto-merge
//! improvement (`Goal.auto_merge`, `crate::worktree` `diff`/`merge`).
//!
//! `docs/architecture.md`: "branches are never auto-merged; that stays a
//! human decision." An upfront `auto_merge` flag set when a goal is
//! created doesn't give a human visibility into the *actual diff* at the
//! moment it lands — that's a real gap, not just a style nit (found by a
//! coordinator goal's own review step auditing this feature). This module
//! is the fix: `auto_merge` now means "eligible to be offered a merge
//! confirmation once review passes", not "skip human review". The
//! coordinator requests a confirmation here (see
//! `coordinator::scheduler::maybe_auto_merge`) instead of merging
//! directly; `single goal merge list/show/confirm/reject` is the human's
//! side of it, and only `confirm` ever calls `worktree::merge`.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS pending_merges (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            goal_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            review_node_id TEXT NOT NULL,
            dep_node_id TEXT NOT NULL,
            branch TEXT NOT NULL,
            status TEXT NOT NULL, -- pending | confirmed | rejected
            created_at TEXT NOT NULL,
            resolved_at TEXT
        )",
        (),
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingMerge {
    pub id: i64,
    pub goal_id: String,
    pub session_id: String,
    pub review_node_id: String,
    pub dep_node_id: String,
    pub branch: String,
    pub status: PendingMergeStatus,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingMergeStatus {
    Pending,
    Confirmed,
    Rejected,
}

impl PendingMergeStatus {
    fn as_str(self) -> &'static str {
        match self {
            PendingMergeStatus::Pending => "pending",
            PendingMergeStatus::Confirmed => "confirmed",
            PendingMergeStatus::Rejected => "rejected",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "confirmed" => PendingMergeStatus::Confirmed,
            "rejected" => PendingMergeStatus::Rejected,
            _ => PendingMergeStatus::Pending,
        }
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Records a merge awaiting human confirmation. Never merges anything
/// itself — see `worktree::diff`/`worktree::merge`, called only from the
/// `confirm` side (`handlers::Request::GoalMergeResolve`).
pub fn request(conn: &Connection, goal_id: &str, session_id: &str, review_node_id: &str, dep_node_id: &str, branch: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO pending_merges (goal_id, session_id, review_node_id, dep_node_id, branch, status, created_at, resolved_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, NULL)",
        params![goal_id, session_id, review_node_id, dep_node_id, branch, now()],
    )?;
    Ok(conn.last_insert_rowid())
}

fn row_to_pending_merge(row: &rusqlite::Row) -> rusqlite::Result<PendingMerge> {
    let status: String = row.get(6)?;
    Ok(PendingMerge {
        id: row.get(0)?,
        goal_id: row.get(1)?,
        session_id: row.get(2)?,
        review_node_id: row.get(3)?,
        dep_node_id: row.get(4)?,
        branch: row.get(5)?,
        status: PendingMergeStatus::parse(&status),
        created_at: row.get(7)?,
        resolved_at: row.get(8)?,
    })
}

const SELECT_COLS: &str = "id, goal_id, session_id, review_node_id, dep_node_id, branch, status, created_at, resolved_at";

pub fn get(conn: &Connection, id: i64) -> Result<Option<PendingMerge>> {
    Ok(conn
        .query_row(&format!("SELECT {SELECT_COLS} FROM pending_merges WHERE id = ?1"), params![id], row_to_pending_merge)
        .optional()?)
}

pub fn list_pending(conn: &Connection) -> Result<Vec<PendingMerge>> {
    let mut stmt = conn.prepare(&format!("SELECT {SELECT_COLS} FROM pending_merges WHERE status = 'pending' ORDER BY created_at"))?;
    let rows = stmt.query_map([], row_to_pending_merge)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Marks the record resolved. The caller (the `GoalMergeResolve` handler)
/// is responsible for actually calling `worktree::merge` when `allow` is
/// true — this function only tracks the human decision, same separation
/// `worktree::diff`/`merge` already keep.
pub fn resolve(conn: &Connection, id: i64, allow: bool) -> Result<PendingMerge> {
    let status = if allow { PendingMergeStatus::Confirmed } else { PendingMergeStatus::Rejected };
    conn.execute(
        "UPDATE pending_merges SET status = ?2, resolved_at = ?3 WHERE id = ?1 AND status = 'pending'",
        params![id, status.as_str(), now()],
    )?;
    get(conn, id)?.ok_or_else(|| anyhow::anyhow!("no such pending merge: {id}"))
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
    fn request_then_confirm_flows_through_pending_to_confirmed() {
        let conn = test_conn();
        let id = request(&conn, "g1", "s1", "review", "code", "single/task-1").unwrap();

        let pending = list_pending(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].status, PendingMergeStatus::Pending);

        let resolved = resolve(&conn, id, true).unwrap();
        assert_eq!(resolved.status, PendingMergeStatus::Confirmed);
        assert!(resolved.resolved_at.is_some());
        assert!(list_pending(&conn).unwrap().is_empty(), "confirmed merge should drop out of the pending list");
    }

    #[test]
    fn reject_leaves_it_rejected_not_pending() {
        let conn = test_conn();
        let id = request(&conn, "g1", "s1", "review", "code", "single/task-1").unwrap();
        let resolved = resolve(&conn, id, false).unwrap();
        assert_eq!(resolved.status, PendingMergeStatus::Rejected);
        assert!(list_pending(&conn).unwrap().is_empty());
    }

    #[test]
    fn resolving_an_already_resolved_merge_is_a_noop_not_a_double_apply() {
        let conn = test_conn();
        let id = request(&conn, "g1", "s1", "review", "code", "single/task-1").unwrap();
        resolve(&conn, id, true).unwrap();
        // a second resolve (e.g. a duplicate confirm click) must not flip
        // a confirmed decision to rejected or vice versa
        let second = resolve(&conn, id, false).unwrap();
        assert_eq!(second.status, PendingMergeStatus::Confirmed, "first decision must stick");
    }

    #[test]
    fn unknown_id_errors_instead_of_silently_succeeding() {
        let conn = test_conn();
        assert!(resolve(&conn, 999, true).is_err());
    }
}
