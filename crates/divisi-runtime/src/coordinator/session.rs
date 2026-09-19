//! `sessions` table: one conversation thread (spec §3.1). one per Zed panel
//! thread; also created by `divisi session new`. the transcript is derived
//! from `coordinator_events` filtered by session, not stored here.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub cwd: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub status: String,
}

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            cwd TEXT NOT NULL,
            title TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'active'
        )",
        (),
    )?;
    Ok(())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub fn new_session(conn: &Connection, cwd: &Path) -> Result<Session> {
    let id = super::short_id("sess");
    let ts = now();
    let cwd = cwd.display().to_string();
    conn.execute(
        "INSERT INTO sessions (id, cwd, title, created_at, updated_at, status)
         VALUES (?1, ?2, '', ?3, ?3, 'active')",
        params![id, cwd, ts],
    )?;
    Ok(Session { id, cwd, title: String::new(), created_at: ts.clone(), updated_at: ts, status: "active".into() })
}

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get("id")?,
        cwd: row.get("cwd")?,
        title: row.get("title")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        status: row.get("status")?,
    })
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Session>> {
    Ok(conn
        .query_row("SELECT * FROM sessions WHERE id = ?1", [id], row_to_session)
        .optional()?)
}

/// newest first.
pub fn list(conn: &Connection) -> Result<Vec<Session>> {
    let mut stmt = conn.prepare("SELECT * FROM sessions ORDER BY created_at DESC")?;
    let rows = stmt.query_map([], row_to_session)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn close(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET status = 'closed', updated_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

/// sets the session title only if it is still empty — called by
/// `goal::create` with the first goal's truncated text, so a session shows
/// something useful in `divisi session list` without a title ever being
/// asked for explicitly.
pub fn set_title_if_empty(conn: &Connection, id: &str, title: &str) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET title = ?2, updated_at = ?3 WHERE id = ?1 AND title = ''",
        params![id, title, now()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_list_get_close_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();

        let s = new_session(&conn, Path::new("/tmp/proj")).unwrap();
        assert_eq!(s.cwd, "/tmp/proj");
        assert_eq!(s.status, "active");

        let fetched = get(&conn, &s.id).unwrap().unwrap();
        assert_eq!(fetched, s);

        set_title_if_empty(&conn, &s.id, "do the thing").unwrap();
        set_title_if_empty(&conn, &s.id, "should not overwrite").unwrap();
        assert_eq!(get(&conn, &s.id).unwrap().unwrap().title, "do the thing");

        assert_eq!(list(&conn).unwrap().len(), 1);

        close(&conn, &s.id).unwrap();
        assert_eq!(get(&conn, &s.id).unwrap().unwrap().status, "closed");
    }
}

/// Title of the shared conversation that the TUI, the notch and `divisi chat` open by default.
pub const MAIN_TITLE: &str = "chat:main";

pub fn set_title(conn: &Connection, id: &str, title: &str) -> Result<()> {
    conn.execute("UPDATE sessions SET title = ?2, updated_at = ?3 WHERE id = ?1", params![id, title, now()])?;
    Ok(())
}

/// The shared conversation: the oldest active session titled `MAIN_TITLE`, created on first use.
/// A closed one is replaced rather than reopened.
pub fn main_thread(conn: &Connection) -> Result<Session> {
    let existing = conn
        .query_row(
            "SELECT * FROM sessions WHERE title = ?1 AND status = 'active' ORDER BY created_at LIMIT 1",
            [MAIN_TITLE],
            row_to_session,
        )
        .optional()?;
    if let Some(s) = existing {
        return Ok(s);
    }
    let cwd = divisi_core::paths::real_home_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
    let mut s = new_session(conn, &cwd)?;
    set_title(conn, &s.id, MAIN_TITLE)?;
    s.title = MAIN_TITLE.into();
    Ok(s)
}

#[cfg(test)]
mod main_thread_tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn the_main_thread_is_created_once_and_reused() {
        let conn = db();
        let first = main_thread(&conn).unwrap();
        let second = main_thread(&conn).unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(first.title, MAIN_TITLE);
        assert_eq!(list(&conn).unwrap().len(), 1);
    }

    #[test]
    fn a_closed_main_thread_is_replaced() {
        let conn = db();
        let first = main_thread(&conn).unwrap();
        close(&conn, &first.id).unwrap();
        let second = main_thread(&conn).unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(second.title, MAIN_TITLE);
    }

    #[test]
    fn ordinary_sessions_are_not_mistaken_for_the_main_thread() {
        let conn = db();
        let other = new_session(&conn, std::path::Path::new("/tmp")).unwrap();
        let main = main_thread(&conn).unwrap();
        assert_ne!(other.id, main.id);
    }
}

