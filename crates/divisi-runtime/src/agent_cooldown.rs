//! Remembers *until when* an agent is unavailable, so routing stops sending it work until then
//! instead of retrying every 15 minutes for days, and brings it back by itself afterwards.
//!
//! - Out of quota and the agent says when it resets (`divisi_core::ratelimit::reset_time`): benched
//!   until exactly then.
//! - Out of quota with no time given: benched with a growing wait (30 min, 1 h, 2 h ... 12 h), one
//!   strike per failure; a successful run clears the strikes.
//! - Not logged in / auth rejected: benched for a long while, since retrying cannot help until a person
//!   logs in; the recovery probe notices when it starts working.
//! - Once a cooldown has passed, the self-heal pass probes the agent with a tiny real call
//!   (`needing_verification`) so it is confirmed, or benched again with a longer wait, within minutes.
//!
//! Provider-backed `divisi-*` agents are exempt: the pool keeps its own per-key ledger for them.

use anyhow::Result;
use chrono::{DateTime, Duration, Local, Utc};
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
    // Older databases lack these columns.
    crate::task::add_column_if_missing(conn, "agent_cooldowns", "strikes", "INTEGER NOT NULL DEFAULT 0")?;
    crate::task::add_column_if_missing(conn, "agent_cooldowns", "kind", "TEXT NOT NULL DEFAULT 'quota'")?;
    crate::task::add_column_if_missing(conn, "agent_cooldowns", "verified_at", "TEXT")?;
    // `stated` = the end time came from the agent itself or a named daily/monthly period, not a guess.
    crate::task::add_column_if_missing(conn, "agent_cooldowns", "stated", "INTEGER NOT NULL DEFAULT 0")?;
    crate::task::add_column_if_missing(conn, "agent_cooldowns", "probed_at", "TEXT")?;
    Ok(())
}

/// Wait after the `strikes`-th quota failure with no stated reset time: 30 min, doubling, capped at 12 h.
/// Wait for a bench with no stated reset: 15m, 30m, then 1h at most. Live finding (2026-09-24): it
/// grew to 12h, so transient provider errors ("3 failed runs in a row") kept opencode and nvidia
/// out for half a day; the recovery probe re-benches an agent that is really still down.
pub fn backoff(strikes: u32) -> Duration {
    Duration::minutes((15i64 << strikes.min(2)).min(60))
}

/// How long a not-logged-in agent stays benched between recovery probes.
pub const AUTH_BENCH: Duration = Duration::hours(6);

fn strikes_of(conn: &Connection, agent: &str) -> u32 {
    conn.query_row("SELECT strikes FROM agent_cooldowns WHERE agent = ?1", [agent], |r| r.get::<_, u32>(0)).unwrap_or(0)
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

/// `divisi-pool` is rate limited per key by the pool, not per agent. The other `divisi-*` names are
/// fixed provider+model wrappers with no pool behind them -- exempting them too (as this once did) let
/// one EOL'd model and two daily-capped keys fail 500+ dispatches in a day without ever being benched.
fn exempt(agent: &str) -> bool {
    divisi_core::agent_names::is_pool(agent)
}

/// How long an agent whose model the provider has retired (410 Gone, "end of life", unknown model)
/// stays out of routing: retrying cannot fix it, a config change can, and `divisi agent` clears it.
const GONE_BENCH: Duration = Duration::days(7);

fn looks_like_model_gone(output: &str) -> bool {
    let lower = output.to_lowercase();
    lower.contains("410 gone")
        || lower.contains("end of life")
        || lower.contains("model_not_found")
        || lower.contains("model not found")
        // Waiting never fixes a spent paid balance either; adding credits does.
        || lower.contains("402 payment required")
        || lower.contains("out of credits")
}

/// Benches `agent` for a failed run's `output`: until the stated reset, else with a growing wait for a
/// quota failure, else for `AUTH_BENCH` for a login failure. Returns whether a cooldown was recorded.
pub fn note(conn: &Connection, agent: &str, output: &str) -> bool {
    if exempt(agent) || ensure_schema(conn).is_err() {
        return false;
    }
    let now = Utc::now();
    // The agent's own word on when it recovers beats any guess, shorter or longer.
    if let Some(until) = divisi_core::ratelimit::reset_time(output, Local::now()) {
        return record_stated(conn, agent, until, output, "quota").is_ok();
    }
    if looks_like_model_gone(output) {
        return record_with(conn, agent, now + GONE_BENCH, output, strikes_of(conn, agent), "gone").is_ok();
    }
    if divisi_core::ratelimit::looks_like_rate_limit(output) {
        // A daily or monthly allowance comes back when its period resets, not after a guessed backoff.
        if let Some(until) = period_reset(output, now) {
            return record_stated(conn, agent, until, output, "quota").is_ok();
        }
        let strikes = strikes_of(conn, agent);
        return record_with(conn, agent, now + backoff(strikes), output, strikes + 1, "quota").is_ok();
    }
    if divisi_core::ratelimit::looks_like_unavailable(output) {
        return record_with(conn, agent, now + AUTH_BENCH, output, strikes_of(conn, agent), "auth").is_ok();
    }
    false
}

/// When a quota that names its period ("daily free allocation", "free-models-per-day", "monthly
/// included credits") resets: the next UTC midnight, or the first of next month.
fn period_reset(output: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    use chrono::{Datelike, TimeZone};
    let l = output.to_lowercase();
    if ["monthly", "per month", "this month"].iter().any(|k| l.contains(k)) {
        let (y, m) = if now.month() == 12 { (now.year() + 1, 1) } else { (now.year(), now.month() + 1) };
        return Utc.with_ymd_and_hms(y, m, 1, 0, 5, 0).single();
    }
    if ["daily", "per day", "per-day", "a day", "today", "rpd", "24 hours", "24h"].iter().any(|k| l.contains(k)) {
        let tomorrow = now.date_naive().succ_opt()?;
        return Utc.from_local_datetime(&tomorrow.and_hms_opt(0, 5, 0)?).single();
    }
    None
}

/// A bench whose end the agent (or its quota period) stated: replaces whatever was there.
fn record_stated(conn: &Connection, agent: &str, until: DateTime<Utc>, reason: &str, kind: &str) -> Result<()> {
    let reason: String = reason.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(200).collect();
    conn.execute(
        "INSERT INTO agent_cooldowns (agent, until, reason, noted_at, kind, stated) VALUES (?1, ?2, ?3, ?4, ?5, 1)
         ON CONFLICT(agent) DO UPDATE SET until = excluded.until, reason = excluded.reason, noted_at = excluded.noted_at,
            kind = excluded.kind, stated = 1, verified_at = NULL",
        params![agent, until.to_rfc3339(), reason, Utc::now().to_rfc3339(), kind],
    )?;
    Ok(())
}

/// Minutes between early probes of a guessed bench, by kind: an agent can recover long before a
/// guessed backoff or a 7-day "gone" bench ends, and a bench should last no longer than needed.
fn early_probe_minutes(kind: &str) -> i64 {
    match kind {
        "gone" => 6 * 60,
        "auth" => 60,
        // shorter than the first 15-minute guessed bench, so a recovered agent returns early
        _ => 10,
    }
}

/// Agents still inside a guessed bench whose last check is older than `early_probe_minutes`.
pub fn early_probe_due(conn: &Connection, now: DateTime<Utc>) -> Result<Vec<String>> {
    ensure_schema(conn)?;
    let mut stmt = conn.prepare("SELECT agent, until, kind, noted_at, probed_at FROM agent_cooldowns WHERE stated = 0")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, Option<String>>(4)?))
    })?;
    let parse = |t: &str| DateTime::parse_from_rfc3339(t).ok().map(|t| t.with_timezone(&Utc));
    let mut out = Vec::new();
    for row in rows {
        let (agent, until, kind, noted, probed) = row?;
        let Some(until) = parse(&until) else { continue };
        let last = probed.as_deref().and_then(parse).into_iter().chain(parse(&noted)).max();
        if until > now && last.is_none_or(|l| now - l >= Duration::minutes(early_probe_minutes(&kind))) {
            out.push(agent);
        }
    }
    Ok(out)
}

/// A snapshot of one bench, so an early probe that finds the agent still unavailable leaves the
/// bench exactly as it was instead of escalating it.
pub fn snapshot(conn: &Connection, agent: &str) -> Option<(String, String, u32, String)> {
    conn.query_row("SELECT until, reason, strikes, kind FROM agent_cooldowns WHERE agent = ?1", [agent], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })
    .ok()
}

pub fn restore_after_probe(conn: &Connection, agent: &str, snap: &(String, String, u32, String)) {
    let _ = conn.execute(
        "UPDATE agent_cooldowns SET until = CASE WHEN stated = 1 THEN until ELSE ?2 END, reason = CASE WHEN stated = 1 THEN reason ELSE ?3 END,
            strikes = ?4, kind = CASE WHEN stated = 1 THEN kind ELSE ?5 END, probed_at = ?6 WHERE agent = ?1",
        params![agent, snap.0, snap.1, snap.2, snap.3, Utc::now().to_rfc3339()],
    );
}

fn record_with(conn: &Connection, agent: &str, until: DateTime<Utc>, reason: &str, strikes: u32, kind: &str) -> Result<()> {
    record(conn, agent, until, reason)?;
    conn.execute("UPDATE agent_cooldowns SET strikes = ?2, kind = ?3, verified_at = NULL WHERE agent = ?1", params![agent, strikes, kind])?;
    Ok(())
}

/// A run succeeded: forget the strikes so the next quota failure starts from the short wait again.
pub fn succeeded(conn: &Connection, agent: &str) {
    if exempt(agent) || ensure_schema(conn).is_err() {
        return;
    }
    let _ = conn.execute("UPDATE agent_cooldowns SET strikes = 0, verified_at = ?2 WHERE agent = ?1", params![agent, Utc::now().to_rfc3339()]);
}

/// Agents whose cooldown has passed but that nothing has confirmed working since: the recovery probe's worklist.
pub fn needing_verification(conn: &Connection, now: DateTime<Utc>) -> Result<Vec<String>> {
    ensure_schema(conn)?;
    let mut stmt = conn.prepare("SELECT agent, until, verified_at FROM agent_cooldowns")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (agent, until, verified) = row?;
        let Ok(until) = DateTime::parse_from_rfc3339(&until).map(|t| t.with_timezone(&Utc)) else { continue };
        let confirmed = verified.and_then(|v| DateTime::parse_from_rfc3339(&v).ok()).is_some_and(|v| v.with_timezone(&Utc) >= until);
        if until <= now && !confirmed {
            out.push(agent);
        }
    }
    Ok(out)
}

/// How many finished runs in a row must fail (with no quota or login message) before the agent is benched.
const FAILURE_STREAK: usize = 3;

/// A run failed for a reason that is neither quota nor login (a backend error, a crash): if the agent's
/// last few finished runs all failed, bench it with the growing wait instead of feeding it more work.
/// One success anywhere in the streak keeps it in routing. Returns whether it was benched.
pub fn note_failure_streak(conn: &Connection, agent: &str) -> bool {
    if exempt(agent) || ensure_schema(conn).is_err() {
        return false;
    }
    let statuses: Vec<String> = conn
        .prepare("SELECT status FROM tasks WHERE agent = ?1 AND status IN ('failed','completed','cancelled') AND COALESCE(summary, '') NOT LIKE 'interrupted:%' ORDER BY id DESC LIMIT ?2")
        .and_then(|mut q| q.query_map(params![agent, FAILURE_STREAK as i64], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>())
        .unwrap_or_default();
    if statuses.len() < FAILURE_STREAK || !statuses.iter().all(|s| s == "failed") {
        return false;
    }
    rebench(conn, agent, &format!("{FAILURE_STREAK} failed runs in a row"));
    true
}

/// The recovery probe reached the agent but it still could not do the work (no stated reset time): bench
/// it again with the next, longer wait rather than probing it every pass.
pub fn rebench(conn: &Connection, agent: &str, why: &str) {
    if exempt(agent) || ensure_schema(conn).is_err() {
        return;
    }
    let strikes = strikes_of(conn, agent);
    let _ = record_with(conn, agent, Utc::now() + backoff(strikes), why, strikes + 1, "probe");
}

/// A probe that got no answer in time says nothing about quota or login, so it benches `agent` for
/// a flat `UNRESPONSIVE_BENCH` without adding a strike. Live finding (2026-09-24): opencode, the most
/// reliable agent, sat out 2.5h after one slow probe as timeouts escalated like real failures.
pub fn rebench_unresponsive(conn: &Connection, agent: &str, why: &str) {
    if exempt(agent) || ensure_schema(conn).is_err() {
        return;
    }
    let strikes = strikes_of(conn, agent);
    let _ = record_with(conn, agent, Utc::now() + UNRESPONSIVE_BENCH, why, strikes, "probe");
}

pub const UNRESPONSIVE_BENCH: Duration = Duration::minutes(20);

/// The probe found `agent` working: it is confirmed and its strikes are gone.
pub fn mark_verified(conn: &Connection, agent: &str) {
    succeeded(conn, agent);
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
    fn an_unresponsive_probe_benches_briefly_and_adds_no_strike() {
        let conn = db();
        ensure_schema(&conn).unwrap();
        let now = Utc::now();
        rebench_unresponsive(&conn, "opencode", "timeout");
        rebench_unresponsive(&conn, "opencode", "timeout");
        assert!(active(&conn, now).unwrap()["opencode"] <= now + UNRESPONSIVE_BENCH + Duration::minutes(1));
        assert_eq!(strikes_of(&conn, "opencode"), 0);
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
    }

    #[test]
    fn a_quota_failure_with_no_stated_time_backs_off_and_doubles() {
        let conn = db();
        assert!(note(&conn, "grok", "Error: You reached your free usage limit for now, try again later"));
        let first = active(&conn, Utc::now()).unwrap()["grok"] - Utc::now();
        assert!(first > Duration::minutes(14) && first <= Duration::minutes(15), "first strike waits 15 minutes, got {first}");
        // a second failure while still benched cannot shorten it, and the strike count advances
        assert!(note(&conn, "grok", "Error: You reached your free usage limit for now, try again later"));
        assert_eq!(strikes_of(&conn, "grok"), 2);
        assert_eq!(backoff(0), Duration::minutes(15));
        assert_eq!(backoff(1), Duration::minutes(30));
        assert_eq!(backoff(9), Duration::hours(1), "capped");
    }

    #[test]
    fn a_login_failure_benches_for_hours_and_a_success_clears_the_strikes() {
        let conn = db();
        assert!(note(&conn, "claude", "Not logged in · Please run /login"));
        let wait = active(&conn, Utc::now()).unwrap()["claude"] - Utc::now();
        assert!(wait > Duration::hours(5), "{wait}");
        note(&conn, "grok", "Rate limit exceeded, try again later");
        assert_eq!(strikes_of(&conn, "grok"), 1);
        succeeded(&conn, "grok");
        assert_eq!(strikes_of(&conn, "grok"), 0);
    }

    #[test]
    fn an_agent_whose_last_runs_all_failed_is_benched_and_one_success_prevents_it() {
        let conn = db();
        crate::task::ensure_schema(&conn).unwrap();
        let add = |agent: &str, status: &str| {
            conn.execute(
                "INSERT INTO tasks (agent, description, status, created_at, updated_at) VALUES (?1, 'd', ?2, ?3, ?3)",
                params![agent, status, Utc::now().to_rfc3339()],
            )
            .unwrap();
        };
        for _ in 0..2 {
            add("mistral-vibe", "failed");
        }
        assert!(!note_failure_streak(&conn, "mistral-vibe"), "two failures is not a streak");
        add("mistral-vibe", "failed");
        assert!(note_failure_streak(&conn, "mistral-vibe"));
        assert!(active(&conn, Utc::now()).unwrap().contains_key("mistral-vibe"));

        for s in ["failed", "completed", "failed"] {
            add("grok", s);
        }
        assert!(!note_failure_streak(&conn, "grok"), "a success inside the window keeps it in routing");
    }

    #[test]
    fn the_pool_agent_is_never_benched_here() {
        let conn = db();
        assert!(!note(&conn, "divisi-pool", "Provider error (429 Too Many Requests)"));
        assert!(active(&conn, Utc::now()).unwrap().is_empty());
    }

    #[test]
    fn a_fixed_provider_wrapper_is_benched_like_any_agent() {
        let conn = db();
        assert!(note(&conn, "divisi-openrouter", "Provider error (429 Too Many Requests): Rate limit exceeded: free-models-per-day"));
        assert!(active(&conn, Utc::now()).unwrap().contains_key("divisi-openrouter"));
    }

    #[test]
    fn runs_interrupted_by_a_daemon_restart_do_not_count_toward_a_failure_streak() {
        let conn = db();
        crate::task::ensure_schema(&conn).unwrap();
        for _ in 0..3 {
            conn.execute(
                "INSERT INTO tasks (agent, description, status, summary, created_at, updated_at) VALUES ('divisi-nvidia', 'd', 'failed', 'interrupted: divisid restarted while this task was in flight', ?1, ?1)",
                params![Utc::now().to_rfc3339()],
            )
            .unwrap();
        }
        assert!(!note_failure_streak(&conn, "divisi-nvidia"));
    }

    #[test]
    fn a_stated_reset_replaces_a_longer_guess_and_is_never_probed_early() {
        let conn = db();
        assert!(note(&conn, "grok", "Error: You reached your free usage limit for now, try again later"));
        for _ in 0..4 {
            note(&conn, "grok", "Error: You reached your free usage limit for now, try again later");
        }
        let guess = active(&conn, Utc::now()).unwrap()["grok"];
        let soon = (Local::now() + Duration::hours(1)).format("%b %-d, %Y %-I:%M %p").to_string();
        assert!(note(&conn, "grok", &format!("ERROR: You've hit your usage limit. try again at {soon}")));
        let until = active(&conn, Utc::now()).unwrap()["grok"];
        assert!(until < guess && until < Utc::now() + Duration::hours(2), "the stated time wins over the long guess: {until} vs {guess}");
        assert!(!early_probe_due(&conn, Utc::now() + Duration::minutes(30)).unwrap().contains(&"grok".to_string()));
    }

    #[test]
    fn a_guessed_bench_is_probed_early_and_a_failed_probe_leaves_it_as_it_was() {
        let conn = db();
        assert!(note(&conn, "agy", "RESOURCE_EXHAUSTED (code 429): exhausted your capacity"));
        assert!(early_probe_due(&conn, Utc::now()).unwrap().is_empty(), "not right away");
        let later = Utc::now() + Duration::minutes(11);
        assert_eq!(early_probe_due(&conn, later).unwrap(), vec!["agy".to_string()]);
        let snap = snapshot(&conn, "agy").unwrap();
        note(&conn, "agy", "RESOURCE_EXHAUSTED (code 429): exhausted your capacity");
        restore_after_probe(&conn, "agy", &snap);
        assert_eq!(snapshot(&conn, "agy").unwrap(), snap);
        assert!(early_probe_due(&conn, Utc::now() + Duration::minutes(5)).unwrap().is_empty(), "just probed");
    }

    #[test]
    fn a_daily_quota_is_benched_until_the_next_utc_midnight() {
        let conn = db();
        assert!(note(&conn, "divisi-cloudflare", "Provider error (429 Too Many Requests): you have used up your daily free allocation"));
        let until = active(&conn, Utc::now()).unwrap()["divisi-cloudflare"];
        assert_eq!(until.date_naive(), Utc::now().date_naive().succ_opt().unwrap());
    }

    #[test]
    fn an_agent_out_of_credits_is_benched_for_days() {
        let conn = db();
        assert!(note(&conn, "amp", "Error: Out of Credits Add credits to keep using Amp."));
        assert!(active(&conn, Utc::now()).unwrap()["amp"] > Utc::now() + Duration::days(6));
    }

    #[test]
    fn a_retired_model_is_benched_for_days_not_retried() {
        let conn = db();
        assert!(note(&conn, "divisi-nvidia", "Provider error (410 Gone): The model 'x' has reached its end of life"));
        let until = active(&conn, Utc::now()).unwrap()["divisi-nvidia"];
        assert!(until > Utc::now() + Duration::days(6));
    }

    #[test]
    fn an_expired_cooldown_is_probed_until_something_confirms_the_agent() {
        let conn = db();
        let now = Utc::now();
        record(&conn, "codex", now - Duration::minutes(5), "usage limit").unwrap();
        record(&conn, "cursor", now + Duration::hours(3), "usage limit").unwrap();
        assert_eq!(needing_verification(&conn, now).unwrap(), vec!["codex".to_string()], "only the one whose time has passed");
        mark_verified(&conn, "codex");
        assert!(needing_verification(&conn, now).unwrap().is_empty(), "confirmed working");
        // a fresh failure after that is benched and needs verifying again
        note(&conn, "codex", "You've hit your usage limit, try again later");
        assert!(needing_verification(&conn, now + Duration::hours(2)).unwrap().contains(&"codex".to_string()));
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
