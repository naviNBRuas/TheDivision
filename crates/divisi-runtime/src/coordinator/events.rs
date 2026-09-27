//! `coordinator_events` (spec §3.5): an append-only progress log the
//! messenger (`divisi acp`) tails. every scheduler decision and node
//! transition writes one row here; `GoalStatus` / `SessionEvents` read
//! them back. the session transcript is just this log filtered by session.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Plan,
    NodeStarted,
    NodeOutput,
    NodeDone,
    NodeFailed,
    Supervisor,
    Queued,
    Blocked,
    Integrated,
    Budget,
    Message,
    /// E28 spec §8: a goal's only routable candidates are exhausted/benched;
    /// body carries the reason + ETA (`goal::set_waiting_on_capacity`).
    CapacityWait,
    /// E28 spec §8: a `waiting_on_capacity` goal's retry stamp passed and a
    /// node was re-dispatched.
    CapacityResumed,
    /// E28 spec §10 (Part F): `resume_interrupted` (daemon start) or
    /// `divisi goal resume` (manual) picked this goal back up.
    SessionResumed,
    /// opt-in auto-merge (`goal.auto_merge`): a `review`-kind node came
    /// back `Done` and a human then `divisi goal merge confirm`ed the
    /// resulting `divisi_core::pending_merge` record, which called
    /// `divisi_core::worktree::merge`.
    Merged,
    /// A confirmed merge attempt and `divisi_core::worktree::merge`
    /// returned an error (e.g. a real conflict) — surfaced, never silently
    /// dropped; the goal itself still finishes since the review passed.
    MergeFailed,
    /// opt-in auto-merge (`goal.auto_merge`): a `review`-kind node came
    /// back `Done` and `scheduler::maybe_auto_merge` recorded a
    /// `divisi_core::pending_merge` request for a dependency's worktree
    /// branch instead of merging it — never merges on its own; a human
    /// must `divisi goal merge confirm` it first. See
    /// `docs/architecture.md`'s "branches are never auto-merged" invariant.
    MergeAwaitingConfirmation,
    /// A message you typed into the shared conversation (`{text, surface}`).
    ChatUser,
    /// divisi's reply (`{text, intent, goal_ids, degraded}`).
    ChatAssistant,
    /// A risky action waiting for your yes or no (`{approval_id, summary, action, expires_at}`).
    ChatConfirm,
    /// How a confirmation ended (`{approval_id, outcome}`: approved, denied or expired).
    ChatResult,
    /// divisi ran out of automatic options on a goal and is waiting for you (body: the question).
    NeedsInput,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Plan => "plan",
            EventKind::NodeStarted => "node_started",
            EventKind::NodeOutput => "node_output",
            EventKind::NodeDone => "node_done",
            EventKind::NodeFailed => "node_failed",
            EventKind::Supervisor => "supervisor",
            EventKind::Queued => "queued",
            EventKind::Blocked => "blocked",
            EventKind::Integrated => "integrated",
            EventKind::Budget => "budget",
            EventKind::Message => "message",
            EventKind::CapacityWait => "capacity_wait",
            EventKind::CapacityResumed => "capacity_resumed",
            EventKind::SessionResumed => "session_resumed",
            EventKind::Merged => "merged",
            EventKind::MergeFailed => "merge_failed",
            EventKind::MergeAwaitingConfirmation => "merge_awaiting_confirmation",
            EventKind::ChatUser => "chat_user",
            EventKind::ChatAssistant => "chat_assistant",
            EventKind::ChatConfirm => "chat_confirm",
            EventKind::ChatResult => "chat_result",
            EventKind::NeedsInput => "needs_input",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub session_id: String,
    pub goal_id: Option<String>,
    pub ts: String,
    pub kind: String,
    pub body: String,
}

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS coordinator_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            goal_id TEXT,
            ts TEXT NOT NULL,
            kind TEXT NOT NULL,
            body TEXT NOT NULL DEFAULT ''
        )",
        (),
    )?;
    // Hot lookups (the scheduler tick queries these per goal every few seconds); without them each was a full scan.
    conn.execute("CREATE INDEX IF NOT EXISTS idx_coordinator_events_goal ON coordinator_events (goal_id, id)", ())?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_coordinator_events_session_kind ON coordinator_events (session_id, kind)", ())?;

    Ok(())
}

/// appends one event, returns its new autoincrement id.
pub fn append(
    conn: &Connection,
    session_id: &str,
    goal_id: Option<&str>,
    kind: EventKind,
    body: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO coordinator_events (session_id, goal_id, ts, kind, body)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![session_id, goal_id, chrono::Utc::now().to_rfc3339(), kind.as_str(), body],
    )?;
    Ok(conn.last_insert_rowid())
}

fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<Event> {
    Ok(Event {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        goal_id: row.get("goal_id")?,
        ts: row.get("ts")?,
        kind: row.get("kind")?,
        body: row.get("body")?,
    })
}

/// events for a session with `id > since_event_id`, oldest first — the
/// messenger long-polls this with the last id it saw.
pub fn since(conn: &Connection, session_id: &str, since_event_id: i64) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM coordinator_events
         WHERE session_id = ?1 AND id > ?2 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![session_id, since_event_id], row_to_event)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// the most recent `limit` events for a goal, returned oldest-first (so a
/// `GoalStatus` reader can render them top-to-bottom).
pub fn for_goal(conn: &Connection, goal_id: &str, limit: usize) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM (
            SELECT * FROM coordinator_events WHERE goal_id = ?1 ORDER BY id DESC LIMIT ?2
         ) ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![goal_id, limit as i64], row_to_event)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_then_since_returns_new_events_only() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();

        append(&conn, "sess_x", None, EventKind::Message, "one").unwrap();
        let second = append(&conn, "sess_x", Some("goal_1"), EventKind::Plan, "two").unwrap();
        append(&conn, "sess_x", Some("goal_1"), EventKind::NodeStarted, "three").unwrap();
        // a different session must not leak in
        append(&conn, "sess_y", None, EventKind::Message, "other").unwrap();

        assert_eq!(since(&conn, "sess_x", 0).unwrap().len(), 3);
        let tail = since(&conn, "sess_x", second).unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].body, "three");

        let g = for_goal(&conn, "goal_1", 10).unwrap();
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].body, "two"); // oldest-first
    }

    #[test]
    fn chat_kinds_are_stored_and_read_back_in_order() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        for (kind, body) in [
            (EventKind::ChatUser, r#"{"text":"how is the pool","surface":"tui"}"#),
            (EventKind::ChatAssistant, r#"{"text":"all healthy"}"#),
            (EventKind::ChatConfirm, r#"{"approval_id":7}"#),
            (EventKind::ChatResult, r#"{"approval_id":7,"outcome":"approved"}"#),
        ] {
            append(&conn, "sess_main", None, kind, body).unwrap();
        }
        let got: Vec<String> = since(&conn, "sess_main", 0).unwrap().into_iter().map(|e| e.kind).collect();
        assert_eq!(got, ["chat_user", "chat_assistant", "chat_confirm", "chat_result"]);
    }
}
