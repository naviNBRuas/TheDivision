//! Minimal SQLite-backed state: a log of runtime events (agent detected,
//! integration applied, setup run). Phase 1 doesn't yet have task/session
//! state to persist — this exists so the storage seam and schema
//! versioning approach are real from day one instead of retrofitted in
//! Phase 4 when task state shows up.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

/// Opens a connection to the shared state db. Sets WAL journal mode plus a
/// busy timeout — standard SQLite practice for a file multiple connections
/// write to concurrently (every `Request` handler opens its own connection
/// here, and v0.1.17's parallel orchestrate opens one per worker thread).
/// Without this, the default rollback-journal mode fails immediately with
/// "database is locked" on any real write contention instead of waiting;
/// WAL lets readers and a writer proceed together, and the busy timeout
/// makes a genuinely simultaneous writer retry instead of erroring.
pub fn open(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path).with_context(|| format!("opening {}", db_path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL").context("setting WAL journal mode")?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).context("setting busy timeout")?;
    ensure_events_schema(&conn)?;
    Ok(conn)
}

pub fn ensure_events_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            occurred_at TEXT NOT NULL,
            kind TEXT NOT NULL,
            detail TEXT NOT NULL
        )",
        (),
    )?;
    Ok(())
}

pub fn record_event(conn: &Connection, kind: &str, detail: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO events (occurred_at, kind, detail) VALUES (?1, ?2, ?3)",
        (chrono::Utc::now().to_rfc3339(), kind, detail),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_schema_and_records_events() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("state").join("divisi.db")).unwrap();
        record_event(&conn, "setup", "ran dry-run").unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM events", (), |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    /// Real proof the WAL + busy_timeout fix works, not just a comment
    /// claiming it does: 8 threads, each with its own connection to the
    /// same db file (exactly what parallel orchestrate does), all writing
    /// concurrently. Without WAL/busy_timeout this reliably hits "database
    /// is locked" under real contention.
    #[test]
    fn concurrent_connections_from_multiple_threads_dont_lock_each_other_out() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("state").join("divisi.db");
        open(&db_path).unwrap(); // create the file/schema once up front

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let db_path = db_path.clone();
                std::thread::spawn(move || {
                    let conn = open(&db_path).unwrap();
                    for j in 0..20 {
                        record_event(&conn, "concurrent_test", &format!("thread {i} write {j}")).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let conn = open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE kind = 'concurrent_test'", (), |row| row.get(0))
            .unwrap();
        assert_eq!(count, 8 * 20);
    }
}
