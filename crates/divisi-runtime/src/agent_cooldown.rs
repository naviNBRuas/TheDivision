//! Remembers *until when* an agent said it is out of quota, so routing stops sending it work
//! until then instead of retrying every 15 minutes for days. The time is read from the agent's
//! own message (`divisi_core::ratelimit::reset_time`); a message with no time records nothing and
//! the old short exclusion window still applies.

use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use rusqlite::{params, Connection};
use std::collections::BTreeMap;

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS agent_cooldowns (
            agent TEXT PRIMARY KEY,
            until TEXT NOT NULL,
            reason TEXT NOT NULL,
            noted_at TEXT NOT NULL
        )",
        (),
    )?;
    Ok(())
}

/// Records that `agent` is unavailable until `until`. A later time replaces an earlier one; an
/// earlier or equal time never shortens an existing cooldown.
pub fn record(conn: &Connection, agent: &str, until: DateTime<Utc>, reason: &str) -> Result<()> {
    ensure_schema(conn)?;
    let reason: String = reason.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(200).collect();
    conn.execute(
        "INSERT INTO agent_cooldowns (agent, until, reason, noted_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(agent) DO UPDATE SET
            until = CASE WHEN excluded.until > until THEN excluded.until ELSE until END,
            reason = CASE WHEN excluded.until > until THEN excluded.reason ELSE reason END,
            noted_at = excluded.noted_at",
        params![agent, until.to_rfc3339(), reason, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

/// Reads a reset time out of a failed run's output and records it. Returns whether one was found.
pub fn note(conn: &Connection, agent: &str, output: &str) -> bool {
    match divisi_core::ratelimit::reset_time(output, Local::now()) {
        Some(until) => record(conn, agent, until, output).is_ok(),
        None => false,
    }
}

/// Agents whose cooldown has not passed yet, with when it ends.
pub fn active(conn: &Connection, now: DateTime<Utc>) -> Result<BTreeMap<String, DateTime<Utc>>> {
    ensure_schema(conn)?;
    let mut stmt = conn.prepare("SELECT agent, until FROM agent_cooldowns")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (agent, until) = row?;
        if let Ok(until) = DateTime::parse_from_rfc3339(&until) {
            let until = until.with_timezone(&Utc);
            if until > now {
                out.insert(agent, until);
            }
        }
    }
    Ok(out)
}

/// Forgets an agent's cooldown (for when a parsed time turns out wrong).
pub fn clear(conn: &Connection, agent: &str) -> Result<bool> {
    ensure_schema(conn)?;
    Ok(conn.execute("DELETE FROM agent_cooldowns WHERE agent = ?1", [agent])? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn db() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn a_recorded_cooldown_is_active_until_it_passes() {
        let conn = db();
        let now = Utc::now();
        record(&conn, "codex", now + Duration::hours(30), "usage limit").unwrap();
        record(&conn, "kiro", now - Duration::hours(1), "old").unwrap();
        let active = active(&conn, now).unwrap();
        assert_eq!(active.keys().collect::<Vec<_>>(), ["codex"], "an expired cooldown is not active");
        assert!(active["codex"] > now + Duration::hours(29));
    }

    #[test]
    fn a_later_time_extends_but_an_earlier_one_never_shortens() {
        let conn = db();
        let now = Utc::now();
        record(&conn, "codex", now + Duration::hours(10), "first").unwrap();
        record(&conn, "codex", now + Duration::hours(2), "shorter").unwrap();
        assert!(active(&conn, now).unwrap()["codex"] > now + Duration::hours(9), "shortened by an earlier report");
        record(&conn, "codex", now + Duration::hours(48), "longer").unwrap();
        assert!(active(&conn, now).unwrap()["codex"] > now + Duration::hours(47));
    }

    #[test]
    fn note_reads_the_time_from_the_agents_own_words() {
        let conn = db();
        let tomorrow = (Local::now() + Duration::days(2)).format("%b %-d, %Y 1:35 AM").to_string();
        // day suffixes like "22nd" are how codex writes it; the parser accepts them and plain days alike
        assert!(note(&conn, "codex", &format!("ERROR: You've hit your usage limit. try again at {tomorrow}")));
        assert!(active(&conn, Utc::now()).unwrap().contains_key("codex"));
        assert!(!note(&conn, "grok", "Rate limit exceeded"), "no time in the message records nothing");
        assert!(!active(&conn, Utc::now()).unwrap().contains_key("grok"));
    }

    #[test]
    fn clearing_forgets_it() {
        let conn = db();
        record(&conn, "codex", Utc::now() + Duration::hours(5), "x").unwrap();
        assert!(clear(&conn, "codex").unwrap());
        assert!(!clear(&conn, "codex").unwrap(), "clearing twice reports nothing was there");
        assert!(active(&conn, Utc::now()).unwrap().is_empty());
    }

    #[test]
    fn routing_excludes_an_agent_that_is_cooling_down_even_with_no_recent_failure() {
        use crate::coordinator::routing::PoolHealth;
        let conn = db();
        crate::task::ensure_schema(&conn).unwrap();
        let registry = divisi_core::builtin_registry();
        assert!(!PoolHealth::probe(&registry, &conn).rate_limited.contains("codex"), "healthy before any cooldown");
        record(&conn, "codex", Utc::now() + Duration::hours(30), "usage limit, try again in 30 hours").unwrap();
        let health = PoolHealth::probe(&registry, &conn);
        assert!(health.rate_limited.contains("codex"), "a 30-hour cooldown must keep codex out of routing");
        assert!(!health.rate_limited.contains("claude"), "other agents are unaffected");
        clear(&conn, "codex").unwrap();
        assert!(!PoolHealth::probe(&registry, &conn).rate_limited.contains("codex"), "cleared means routable again");
    }
}
