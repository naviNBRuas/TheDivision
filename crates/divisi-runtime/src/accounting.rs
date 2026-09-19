//! Minimal token/cost accounting (propose-only: no business data).
//!
//! Two tables:
//! - `usage_events`: one row per token-usage event, linked to an
//!   execution and trace via `execution_id`/`trace_id`.
//! - `usage_event_breakdown`: per-input/output/cache token
//!   breakdown for each usage event.

use anyhow::{Context as _, Result};
use rusqlite::{params, Connection};
use std::path::Path;
// The wire types (`UsageEvent`, `UsageEventBreakdown`, `Totals`,
// `AccountingQuery`, `AccountingResult`) already live in `divisi_protocol`
// -- this module had briefly redeclared its own field-identical copies,
// which is what produced the `E0308: accounting::X vs divisi_protocol::X`
// mismatch handlers.rs hit. Reusing the protocol crate's types directly
// keeps there being exactly one definition each side of the wire agrees on.
pub use divisi_protocol::{AccountingQuery, AccountingResult, Totals, UsageEvent, UsageEventBreakdown};

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS usage_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            execution_id TEXT NOT NULL,
            trace_id TEXT NOT NULL,
            agent TEXT NOT NULL,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            cache_tokens INTEGER NOT NULL DEFAULT 0,
            cost_usd REAL NOT NULL DEFAULT 0.0,
            occurred_at TEXT NOT NULL
        )",
        (),
    )?;
    // execution_id and trace_id are mandatory for every event;
    // for databases predating this migration (none yet, but
    // defensively) default them to empty strings rather than NULL.
    let cols: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('usage_events')")?
        .query_map([], |row| row.get::<_, String>("name"))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !cols.iter().any(|c| c == "execution_id") {
        conn.execute(
            "ALTER TABLE usage_events ADD COLUMN execution_id TEXT NOT NULL DEFAULT ''",
            (),
        )?;
    }
    if !cols.iter().any(|c| c == "trace_id") {
        conn.execute(
            "ALTER TABLE usage_events ADD COLUMN trace_id TEXT NOT NULL DEFAULT ''",
            (),
        )?;
    }

    conn.execute(
        "CREATE TABLE IF NOT EXISTS usage_event_breakdown (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            execution_id TEXT NOT NULL,
            trace_id TEXT NOT NULL,
            event_type TEXT NOT NULL,
            token_count INTEGER NOT NULL
        )",
        (),
    )?;
    Ok(())
}

pub fn record_usage_event(
    conn: &Connection,
    event: &UsageEvent,
    breakdowns: &[UsageEventBreakdown],
) -> Result<()> {
    conn.execute(
        "INSERT INTO usage_events
         (id, execution_id, trace_id, agent, provider, model, prompt_tokens, completion_tokens, cache_tokens, cost_usd, occurred_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            event.id,
            event.execution_id,
            event.trace_id,
            event.agent,
            event.provider,
            event.model,
            event.prompt_tokens,
            event.completion_tokens,
            event.cache_tokens,
            event.cost_usd,
            event.occurred_at,
        ],
    )?;
    for b in breakdowns {
        conn.execute(
            "INSERT INTO usage_event_breakdown
             (id, execution_id, trace_id, event_type, token_count)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![b.id, b.execution_id, b.trace_id, b.event_type, b.token_count],
        )?;
    }
    Ok(())
}

pub fn query_usage_events(conn: &Connection, q: &AccountingQuery) -> Result<AccountingResult> {
    let mut where_clauses = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(ref eid) = q.execution_id {
        where_clauses.push("execution_id = ?");
        params.push(Box::new(eid));
    }
    if let Some(ref tid) = q.trace_id {
        where_clauses.push("trace_id = ?");
        params.push(Box::new(tid));
    }
    if let Some(ref a) = q.agent {
        where_clauses.push("agent = ?");
        params.push(Box::new(a));
    }
    if let Some(ref p) = q.provider {
        where_clauses.push("provider = ?");
        params.push(Box::new(p));
    }

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };

    let mut stmt = conn.prepare(&format!(
        "SELECT id, execution_id, trace_id, agent, provider, model, prompt_tokens, completion_tokens, cache_tokens, cost_usd, occurred_at
         FROM usage_events {where_sql} ORDER BY occurred_at DESC"
    ))?;

    let events = stmt.query_map(rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())), |row| {
        Ok(UsageEvent {
            id: row.get("id")?,
            execution_id: row.get("execution_id")?,
            trace_id: row.get("trace_id")?,
            agent: row.get("agent")?,
            provider: row.get("provider")?,
            model: row.get("model")?,
            prompt_tokens: row.get("prompt_tokens")?,
            completion_tokens: row.get("completion_tokens")?,
            cache_tokens: row.get("cache_tokens")?,
            cost_usd: row.get("cost_usd")?,
            occurred_at: row.get("occurred_at")?,
        })
    })?
    .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut b_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut b_where = String::new();
    if let Some(ref eid) = q.execution_id {
        b_where.push_str("execution_id = ?");
        b_params.push(Box::new(eid));
    }
    if let Some(ref tid) = q.trace_id {
        if !b_where.is_empty() {
            b_where.push_str(" AND ");
        }
        b_where.push_str("trace_id = ?");
        b_params.push(Box::new(tid));
    }
    if let Some(ref et) = q.event_type {
        if !b_where.is_empty() {
            b_where.push_str(" AND ");
        }
        b_where.push_str("event_type = ?");
        b_params.push(Box::new(et));
    }

    let b_sql = if b_where.is_empty() {
        String::new()
    } else {
        format!("WHERE {b_where}")
    };

    let mut b_stmt = conn.prepare(&format!(
        "SELECT id, execution_id, trace_id, event_type, token_count
         FROM usage_event_breakdown {b_sql} ORDER BY id"
    ))?;

    let breakdowns = b_stmt.query_map(rusqlite::params_from_iter(b_params.iter().map(|p| p.as_ref())), |row| {
        Ok(UsageEventBreakdown {
            id: row.get("id")?,
            execution_id: row.get("execution_id")?,
            trace_id: row.get("trace_id")?,
            event_type: row.get("event_type")?,
            token_count: row.get("token_count")?,
        })
    })?
    .collect::<rusqlite::Result<Vec<_>>>()?;

    let totals = Totals {
        prompt_tokens: events.iter().map(|e| e.prompt_tokens).sum(),
        completion_tokens: events.iter().map(|e| e.completion_tokens).sum(),
        cache_tokens: events.iter().map(|e| e.cache_tokens).sum(),
        total_tokens: events
            .iter()
            .map(|e| e.prompt_tokens + e.completion_tokens + e.cache_tokens)
            .sum(),
        cost_usd: events.iter().map(|e| e.cost_usd).sum(),
    };

    Ok(AccountingResult {
        events,
        breakdowns,
        totals,
    })
}

pub fn open(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path).with_context(|| format!("opening {}", db_path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    ensure_schema(&conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("accounting.db")).unwrap();
        conn
    }

    #[test]
    fn schema_creates_both_tables() {
        let conn = temp_conn();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('usage_events','usage_event_breakdown')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn execution_id_and_trace_id_columns_exist() {
        let conn = temp_conn();
        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('usage_events')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>("name"))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(cols.contains(&"execution_id".to_string()));
        assert!(cols.contains(&"trace_id".to_string()));
    }

    #[test]
    fn record_and_query_usage_events() {
        let conn = temp_conn();
        let event = UsageEvent {
            id: 1,
            execution_id: "exec-1".into(),
            trace_id: "trace-1".into(),
            agent: "claude".into(),
            provider: "anthropic".into(),
            model: "claude-sonnet-4".into(),
            prompt_tokens: 100,
            completion_tokens: 200,
            cache_tokens: 50,
            cost_usd: 0.005,
            occurred_at: "2026-09-17T12:00:00Z".into(),
        };
        let breakdowns = vec![
            UsageEventBreakdown {
                id: 1,
                execution_id: "exec-1".into(),
                trace_id: "trace-1".into(),
                event_type: "input".into(),
                token_count: 100,
            },
            UsageEventBreakdown {
                id: 2,
                execution_id: "exec-1".into(),
                trace_id: "trace-1".into(),
                event_type: "output".into(),
                token_count: 200,
            },
        ];

        record_usage_event(&conn, &event, &breakdowns).unwrap();

        let result = query_usage_events(&conn, &AccountingQuery::default()).unwrap();
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].execution_id, "exec-1");
        assert_eq!(result.breakdowns.len(), 2);
        assert_eq!(result.totals.prompt_tokens, 100);
        assert_eq!(result.totals.completion_tokens, 200);
        assert_eq!(result.totals.cache_tokens, 50);
        assert_eq!(result.totals.total_tokens, 350);
        assert_eq!(result.totals.cost_usd, 0.005);
    }

    #[test]
    fn query_filters_by_execution_id() {
        let conn = temp_conn();
        let e1 = UsageEvent {
            id: 1,
            execution_id: "exec-1".into(),
            trace_id: "trace-1".into(),
            agent: "claude".into(),
            provider: "anthropic".into(),
            model: "m1".into(),
            prompt_tokens: 10,
            completion_tokens: 20,
            cache_tokens: 0,
            cost_usd: 0.001,
            occurred_at: "2026-09-17T12:00:00Z".into(),
        };
        let e2 = UsageEvent {
            id: 2,
            execution_id: "exec-2".into(),
            trace_id: "trace-2".into(),
            agent: "codex".into(),
            provider: "openai".into(),
            model: "m2".into(),
            prompt_tokens: 30,
            completion_tokens: 40,
            cache_tokens: 0,
            cost_usd: 0.002,
            occurred_at: "2026-09-17T13:00:00Z".into(),
        };
        record_usage_event(&conn, &e1, &[]).unwrap();
        record_usage_event(&conn, &e2, &[]).unwrap();

        let result = query_usage_events(
            &conn,
            &AccountingQuery {
                execution_id: Some("exec-1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].execution_id, "exec-1");
    }

    #[test]
    fn query_filters_by_event_type_on_breakdown() {
        let conn = temp_conn();
        let event = UsageEvent {
            id: 1,
            execution_id: "exec-1".into(),
            trace_id: "trace-1".into(),
            agent: "claude".into(),
            provider: "anthropic".into(),
            model: "m1".into(),
            prompt_tokens: 100,
            completion_tokens: 200,
            cache_tokens: 50,
            cost_usd: 0.005,
            occurred_at: "2026-09-17T12:00:00Z".into(),
        };
        let breakdowns = vec![
            UsageEventBreakdown {
                id: 1,
                execution_id: "exec-1".into(),
                trace_id: "trace-1".into(),
                event_type: "input".into(),
                token_count: 100,
            },
            UsageEventBreakdown {
                id: 2,
                execution_id: "exec-1".into(),
                trace_id: "trace-1".into(),
                event_type: "output".into(),
                token_count: 200,
            },
            UsageEventBreakdown {
                id: 3,
                execution_id: "exec-1".into(),
                trace_id: "trace-1".into(),
                event_type: "cache".into(),
                token_count: 50,
            },
        ];
        record_usage_event(&conn, &event, &breakdowns).unwrap();

        let result = query_usage_events(
            &conn,
            &AccountingQuery {
                event_type: Some("input".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.breakdowns.len(), 1);
        assert_eq!(result.breakdowns[0].event_type, "input");
        assert_eq!(result.breakdowns[0].token_count, 100);
    }
}