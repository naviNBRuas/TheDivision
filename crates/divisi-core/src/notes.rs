//! Agent-to-agent inbox (spec: "communicate with each other when
//! needed"). Not a live event stream — nothing here holds a persistent
//! connection open — but it now has two real delivery paths:
//!
//! - **Start-of-run delivery**: `divisi-runtime::task::run`'s prompt
//!   preamble reads unread notes addressed to the agent at the start of
//!   every task and marks them read (see that module's
//!   `build_context_preamble`). Works for every agent, no cooperation from
//!   the agent CLI needed.
//! - **Live, mid-session (v0.1.17)**: `divisi-gateway`'s gateway exposes real
//!   `notes_leave`/`notes_read` tools an agent can call itself, at any
//!   point during its own run, if it has the gateway configured as an MCP
//!   server — the most honest version of "live" messaging achievable
//!   here, since it's a real tool call during a real run rather than
//!   simulated inter-process signaling (see `divisi-gateway::gateway`'s
//!   module doc for why bidirectional process-level IPC isn't real for
//!   these agent CLIs).
//!
//! Lives in `divisi-core` (not `divisi-runtime`, where it originated)
//! specifically so `divisi-gateway` — a separate binary that doesn't depend on
//! `divisi-runtime` — can use it directly, the same reason
//! `divisi-core::preferences`/`permissions` live here instead of the
//! runtime crate (see `divisi-gateway::gateway::check_permission` for the
//! established precedent this follows).

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use divisi_protocol::AgentNote;

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS agent_notes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project TEXT,
            from_agent TEXT NOT NULL,
            to_agent TEXT,
            topic TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL,
            read_at TEXT
        )",
        (),
    )?;
    Ok(())
}

pub fn leave(
    conn: &Connection,
    project: Option<String>,
    from_agent: &str,
    to_agent: Option<&str>,
    topic: &str,
    content: &str,
) -> Result<i64> {
    let created_at = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO agent_notes (project, from_agent, to_agent, topic, content, created_at, read_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
        params![project, from_agent, to_agent, topic, content, created_at],
    )
    .context("inserting agent note")?;
    Ok(conn.last_insert_rowid())
}

/// Notes addressed to `to_agent` in `project` — including broadcast notes
/// (`to_agent IS NULL` in storage) for that project — newest first.
/// `project: None` matches only broadcast/unscoped notes with no project,
/// not "all projects"; callers wanting a specific project's inbox should
/// always pass one when they have it.
pub fn inbox(conn: &Connection, project: Option<&str>, to_agent: &str, unread_only: bool) -> Result<Vec<AgentNote>> {
    let mut sql = String::from(
        "SELECT * FROM agent_notes WHERE (to_agent = ?1 OR to_agent IS NULL) AND project IS ?2",
    );
    if unread_only {
        sql.push_str(" AND read_at IS NULL");
    }
    sql.push_str(" ORDER BY created_at DESC");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![to_agent, project], row_to_note)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collecting agent notes")
}

pub fn mark_read(conn: &Connection, id: i64) -> Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let affected = conn.execute("UPDATE agent_notes SET read_at = ?1 WHERE id = ?2 AND read_at IS NULL", params![now, id])?;
    Ok(affected > 0)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<AgentNote>> {
    conn.query_row("SELECT * FROM agent_notes WHERE id = ?1", params![id], row_to_note).optional().context("querying note by id")
}

fn row_to_note(row: &rusqlite::Row) -> rusqlite::Result<AgentNote> {
    Ok(AgentNote {
        id: row.get("id")?,
        project: row.get("project")?,
        from_agent: row.get("from_agent")?,
        to_agent: row.get("to_agent")?,
        topic: row.get("topic")?,
        content: row.get("content")?,
        created_at: row.get("created_at")?,
        read_at: row.get("read_at")?,
    })
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
    fn leave_then_inbox_round_trips() {
        let conn = test_conn();
        leave(&conn, Some("proj-a".into()), "claude", Some("codex"), "auth bug", "root cause is token refresh").unwrap();

        let inbox = inbox(&conn, Some("proj-a"), "codex", false).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from_agent, "claude");
        assert_eq!(inbox[0].topic, "auth bug");
        assert!(inbox[0].read_at.is_none());
    }

    #[test]
    fn inbox_is_scoped_by_recipient_and_project() {
        let conn = test_conn();
        leave(&conn, Some("proj-a".into()), "claude", Some("codex"), "t1", "for codex in proj-a").unwrap();
        leave(&conn, Some("proj-a".into()), "claude", Some("agy"), "t2", "for agy in proj-a").unwrap();
        leave(&conn, Some("proj-b".into()), "claude", Some("codex"), "t3", "for codex in proj-b").unwrap();

        let codex_a = inbox(&conn, Some("proj-a"), "codex", false).unwrap();
        assert_eq!(codex_a.len(), 1);
        assert_eq!(codex_a[0].topic, "t1");
    }

    #[test]
    fn broadcast_notes_with_no_to_agent_reach_every_recipient_in_the_project() {
        let conn = test_conn();
        leave(&conn, Some("proj-a".into()), "claude", None, "heads up", "watch out for flaky test X").unwrap();

        assert_eq!(inbox(&conn, Some("proj-a"), "codex", false).unwrap().len(), 1);
        assert_eq!(inbox(&conn, Some("proj-a"), "agy", false).unwrap().len(), 1);
    }

    #[test]
    fn unread_only_filters_and_mark_read_is_idempotent() {
        let conn = test_conn();
        let id = leave(&conn, None, "claude", Some("codex"), "t", "c").unwrap();

        assert_eq!(inbox(&conn, None, "codex", true).unwrap().len(), 1);
        assert!(mark_read(&conn, id).unwrap());
        assert!(!mark_read(&conn, id).unwrap());
        assert_eq!(inbox(&conn, None, "codex", true).unwrap().len(), 0);
        assert_eq!(inbox(&conn, None, "codex", false).unwrap().len(), 1);
    }
}
