//! The shared memory subsystem (spec section 9): SQLite-backed, scoped,
//! with provenance tracking (spec section 46/47) so memory can't silently
//! become an uncontrolled prompt-injection persistence layer.
//!
//! Phase 3 scope: structured storage + substring search (SQLite `LIKE`),
//! not semantic/embedding search — that needs a configurable embedding
//! provider (spec section 9's "pluggable storage architecture"), which is
//! future work once a provider abstraction exists (Phase 6). This is
//! honestly a smaller capability than the full spec describes, not a stub
//! pretending otherwise: `search` is substring matching, documented as such
//! in `divisi memory search`'s output.
//!
//! Nothing in divisi automatically promotes agent output into this store
//! yet — there is no orchestrator calling it (Phase 4). Every write is
//! explicit and tagged with the `MemorySource` the caller claims, exactly
//! as given; this module does not itself decide what's trustworthy.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use divisi_protocol::{MemoryEntry, MemoryScope, MemorySource};

fn scope_as_str(scope: MemoryScope) -> &'static str {
    match scope {
        MemoryScope::Working => "working",
        MemoryScope::Project => "project",
        MemoryScope::User => "user",
        MemoryScope::Agent => "agent",
        MemoryScope::Task => "task",
        MemoryScope::LongTerm => "long_term",
        MemoryScope::Knowledge => "knowledge",
    }
}

fn parse_scope(s: &str) -> Result<MemoryScope> {
    Ok(match s {
        "working" => MemoryScope::Working,
        "project" => MemoryScope::Project,
        "user" => MemoryScope::User,
        "agent" => MemoryScope::Agent,
        "task" => MemoryScope::Task,
        "long_term" => MemoryScope::LongTerm,
        "knowledge" => MemoryScope::Knowledge,
        other => anyhow::bail!("unknown memory scope: {other}"),
    })
}

fn source_as_str(source: MemorySource) -> &'static str {
    match source {
        MemorySource::UserInstruction => "user_instruction",
        MemorySource::AgentOutput => "agent_output",
        MemorySource::ToolOutput => "tool_output",
        MemorySource::ProjectContent => "project_content",
        MemorySource::ExternalContent => "external_content",
    }
}

fn parse_source(s: &str) -> Result<MemorySource> {
    Ok(match s {
        "user_instruction" => MemorySource::UserInstruction,
        "agent_output" => MemorySource::AgentOutput,
        "tool_output" => MemorySource::ToolOutput,
        "project_content" => MemorySource::ProjectContent,
        "external_content" => MemorySource::ExternalContent,
        other => anyhow::bail!("unknown memory source: {other}"),
    })
}

#[derive(Debug, Clone, Default)]
pub struct NewMemory {
    pub scope: Option<MemoryScope>,
    pub source: Option<MemorySource>,
    pub project: Option<String>,
    pub agent: Option<String>,
    pub task: Option<String>,
    pub title: String,
    pub content: String,
    pub confidence: Option<f64>,
    pub expires_in_seconds: Option<i64>,
}

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS memories (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            scope TEXT NOT NULL,
            source TEXT NOT NULL,
            project TEXT,
            agent TEXT,
            task TEXT,
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            confidence REAL NOT NULL DEFAULT 1.0,
            created_at TEXT NOT NULL,
            expires_at TEXT
        )",
        (),
    )?;
    Ok(())
}

pub fn store(conn: &Connection, entry: NewMemory) -> Result<i64> {
    let scope = entry.scope.unwrap_or(MemoryScope::Working);
    let source = entry.source.unwrap_or(MemorySource::UserInstruction);
    let confidence = entry.confidence.unwrap_or(1.0).clamp(0.0, 1.0);
    let created_at = chrono::Utc::now();
    let expires_at = entry
        .expires_in_seconds
        .map(|secs| (created_at + chrono::Duration::seconds(secs)).to_rfc3339());

    conn.execute(
        "INSERT INTO memories (scope, source, project, agent, task, title, content, confidence, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            scope_as_str(scope),
            source_as_str(source),
            entry.project,
            entry.agent,
            entry.task,
            entry.title,
            entry.content,
            confidence,
            created_at.to_rfc3339(),
            expires_at,
        ],
    )
    .context("inserting memory entry")?;
    Ok(conn.last_insert_rowid())
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<MemoryEntry>> {
    purge_expired(conn)?;
    conn.query_row("SELECT * FROM memories WHERE id = ?1", params![id], row_to_entry)
        .optional()
        .context("querying memory by id")
}

pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    let affected = conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
    Ok(affected > 0)
}

/// Substring search over title+content, most recent first. `scope`/
/// `project` are optional filters. See module docs: this is `LIKE`
/// matching, not semantic search.
pub fn search(conn: &Connection, query: &str, scope: Option<MemoryScope>, project: Option<&str>) -> Result<Vec<MemoryEntry>> {
    purge_expired(conn)?;
    let pattern = format!("%{query}%");
    let mut sql = String::from(
        "SELECT * FROM memories WHERE (title LIKE ?1 OR content LIKE ?1)",
    );
    let mut idx = 2;
    let mut scope_str = String::new();
    if let Some(s) = scope {
        scope_str = scope_as_str(s).to_string();
        sql.push_str(&format!(" AND scope = ?{idx}"));
        idx += 1;
    }
    if project.is_some() {
        sql.push_str(&format!(" AND project = ?{idx}"));
    }
    sql.push_str(" ORDER BY created_at DESC");

    let mut stmt = conn.prepare(&sql)?;
    let rows = match (scope, project) {
        (Some(_), Some(p)) => stmt.query_map(params![pattern, scope_str, p], row_to_entry)?,
        (Some(_), None) => stmt.query_map(params![pattern, scope_str], row_to_entry)?,
        (None, Some(p)) => stmt.query_map(params![pattern, p], row_to_entry)?,
        (None, None) => stmt.query_map(params![pattern], row_to_entry)?,
    };
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collecting memory search results")
}

pub fn list(conn: &Connection, scope: Option<MemoryScope>) -> Result<Vec<MemoryEntry>> {
    purge_expired(conn)?;
    let entries: Vec<MemoryEntry> = match scope {
        Some(s) => {
            let mut stmt = conn.prepare("SELECT * FROM memories WHERE scope = ?1 ORDER BY created_at DESC")?;
            let rows = stmt.query_map(params![scope_as_str(s)], row_to_entry)?.collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        }
        None => {
            let mut stmt = conn.prepare("SELECT * FROM memories ORDER BY created_at DESC")?;
            let rows = stmt.query_map([], row_to_entry)?.collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        }
    };
    Ok(entries)
}

/// Deletes anything past its `expires_at`. Called at the start of every
/// read so expiry is enforced without needing a background sweeper.
pub fn purge_expired(conn: &Connection) -> Result<usize> {
    let now = chrono::Utc::now().to_rfc3339();
    let affected = conn.execute("DELETE FROM memories WHERE expires_at IS NOT NULL AND expires_at < ?1", params![now])?;
    Ok(affected)
}

fn row_to_entry(row: &rusqlite::Row) -> rusqlite::Result<MemoryEntry> {
    let scope_str: String = row.get("scope")?;
    let source_str: String = row.get("source")?;
    Ok(MemoryEntry {
        id: row.get("id")?,
        scope: parse_scope(&scope_str).map_err(|e| rusqlite::Error::InvalidColumnType(0, e.to_string(), rusqlite::types::Type::Text))?,
        source: parse_source(&source_str).map_err(|e| rusqlite::Error::InvalidColumnType(0, e.to_string(), rusqlite::types::Type::Text))?,
        project: row.get("project")?,
        agent: row.get("agent")?,
        task: row.get("task")?,
        title: row.get("title")?,
        content: row.get("content")?,
        confidence: row.get("confidence")?,
        created_at: row.get("created_at")?,
        expires_at: row.get("expires_at")?,
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
    fn store_then_get_round_trips() {
        let conn = test_conn();
        let id = store(&conn, NewMemory {
            scope: Some(MemoryScope::Project),
            source: Some(MemorySource::UserInstruction),
            title: "prefers tabs".into(),
            content: "user prefers tabs over spaces in this repo".into(),
            ..Default::default()
        }).unwrap();
        let entry = get(&conn, id).unwrap().unwrap();
        assert_eq!(entry.scope, MemoryScope::Project);
        assert_eq!(entry.title, "prefers tabs");
        assert_eq!(entry.confidence, 1.0);
    }

    #[test]
    fn search_matches_substring_in_title_or_content() {
        let conn = test_conn();
        store(&conn, NewMemory { title: "auth bug".into(), content: "root cause was a token refresh race".into(), ..Default::default() }).unwrap();
        store(&conn, NewMemory { title: "unrelated".into(), content: "nothing to see here".into(), ..Default::default() }).unwrap();
        let results = search(&conn, "token refresh", None, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "auth bug");
    }

    #[test]
    fn search_respects_scope_filter() {
        let conn = test_conn();
        store(&conn, NewMemory { scope: Some(MemoryScope::User), title: "x".into(), content: "shared token".into(), ..Default::default() }).unwrap();
        store(&conn, NewMemory { scope: Some(MemoryScope::Project), title: "y".into(), content: "shared token".into(), ..Default::default() }).unwrap();
        let results = search(&conn, "shared", Some(MemoryScope::User), None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "x");
    }

    #[test]
    fn expired_entries_are_purged_on_read() {
        let conn = test_conn();
        let id = store(&conn, NewMemory { title: "temp".into(), content: "short-lived".into(), expires_in_seconds: Some(-1), ..Default::default() }).unwrap();
        assert!(get(&conn, id).unwrap().is_none());
    }

    #[test]
    fn delete_reports_whether_anything_was_removed() {
        let conn = test_conn();
        let id = store(&conn, NewMemory { title: "x".into(), content: "y".into(), ..Default::default() }).unwrap();
        assert!(delete(&conn, id).unwrap());
        assert!(!delete(&conn, id).unwrap());
    }

    #[test]
    fn list_filters_by_scope_and_orders_recent_first() {
        let conn = test_conn();
        store(&conn, NewMemory { scope: Some(MemoryScope::Knowledge), title: "a".into(), content: "1".into(), ..Default::default() }).unwrap();
        store(&conn, NewMemory { scope: Some(MemoryScope::Working), title: "b".into(), content: "2".into(), ..Default::default() }).unwrap();
        let knowledge = list(&conn, Some(MemoryScope::Knowledge)).unwrap();
        assert_eq!(knowledge.len(), 1);
        assert_eq!(knowledge[0].title, "a");
    }
}
