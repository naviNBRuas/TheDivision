//! 4-D quota ledger (RPM/RPD/TPM/TPD admission) — spec §6.1.
//!
//! Windows: RPM/TPM are 60s sliding; RPD/TPD reset at the next UTC
//! midnight (providers reset at midnight, not on a 24h rolling clock —
//! confirmed by spec §6.1, not assumed).

use anyhow::Result;
use chrono::{TimeZone, Utc};
use rusqlite::{params, Connection};
use divisi_core::free_pool::Limits;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub type LeaseId = String;

/// Backstop: a lease older than this is treated as abandoned by admission
/// math even if `release_lease` was never called (e.g. the task crashed
/// mid-dispatch). Checked lazily on the next admission call, not a timer.
const LEASE_BACKSTOP_MS: i64 = 2 * 60 * 1000;

/// Degraded-DB fallback cap — bounded so a stuck SQLite path can't leak
/// memory across the life of the process (spec §6.1).
const MEMORY_FALLBACK_CAP: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    Rpm,
    Rpd,
    Tpm,
    Tpd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageKind {
    Request,
    Tokens,
}

impl UsageKind {
    fn as_str(self) -> &'static str {
        match self {
            UsageKind::Request => "request",
            UsageKind::Tokens => "tokens",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitResult {
    Ok,
    Denied { window: WindowKind, retry_after_ms: i64 },
}

struct UsageEvent {
    platform: String,
    model: String,
    key_id: String,
    kind: UsageKind,
    amount: u64,
    at_ms: i64,
}

/// Process-wide fallback used only when a `pool_usage` write fails —
/// admission math still works for the life of the process even if SQLite
/// is unwritable (spec §6.1 "degraded-DB fallback").
static MEMORY_FALLBACK: Mutex<Vec<UsageEvent>> = Mutex::new(Vec::new());

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

/// RPD/TPD reset at the next UTC midnight after `now_ms`, not on a 24h
/// rolling window.
pub fn next_utc_midnight_ms(now_ms: i64) -> i64 {
    let dt = Utc.timestamp_millis_opt(now_ms).unwrap();
    let next_day = dt.date_naive().succ_opt().unwrap();
    Utc.from_utc_datetime(&next_day.and_hms_opt(0, 0, 0).unwrap()).timestamp_millis()
}

fn window_start_ms(window: WindowKind, now_ms: i64) -> i64 {
    match window {
        WindowKind::Rpm | WindowKind::Tpm => now_ms - 60_000,
        WindowKind::Rpd | WindowKind::Tpd => {
            // start of "today" in UTC = previous midnight
            let next_midnight = next_utc_midnight_ms(now_ms - 1);
            next_midnight - 24 * 60 * 60 * 1000
        }
    }
}

fn window_kind_for(kind: UsageKind, window: WindowKind) -> bool {
    matches!(
        (kind, window),
        (UsageKind::Request, WindowKind::Rpm)
            | (UsageKind::Request, WindowKind::Rpd)
            | (UsageKind::Tokens, WindowKind::Tpm)
            | (UsageKind::Tokens, WindowKind::Tpd)
    )
}

fn recorded_sum(
    conn: &Connection,
    platform: &str,
    model: &str,
    key_id: &str,
    kind: UsageKind,
    since_ms: i64,
) -> Result<u64> {
    let sum: Option<i64> = conn
        .query_row(
            "SELECT SUM(amount) FROM pool_usage WHERE platform = ?1 AND model = ?2 AND key_id = ?3 AND kind = ?4 AND at_ms >= ?5",
            params![platform, model, key_id, kind.as_str(), since_ms],
            |row| row.get(0),
        )
        .unwrap_or(None);
    Ok(sum.unwrap_or(0).max(0) as u64)
}

fn recorded_sum_fallback(platform: &str, model: &str, key_id: &str, kind: UsageKind, since_ms: i64) -> u64 {
    let events = MEMORY_FALLBACK.lock().unwrap();
    events
        .iter()
        .filter(|e| e.platform == platform && e.model == model && e.key_id == key_id && e.kind == kind && e.at_ms >= since_ms)
        .map(|e| e.amount)
        .sum()
}

/// Sums recorded `Request` usage across every model for one
/// `(platform, key_id)` since `since_ms` — used by `divisi provider
/// key-status`'s headroom column, which has no single-model granularity
/// to filter on (spec §17's model-selection seam: each provider is one
/// nominal model this iteration).
pub fn recorded_requests_since(conn: &Connection, platform: &str, key_id: &str, since_ms: i64) -> Result<u64> {
    let sum: Option<i64> = conn
        .query_row(
            "SELECT SUM(amount) FROM pool_usage WHERE platform = ?1 AND key_id = ?2 AND kind = 'request' AND at_ms >= ?3",
            params![platform, key_id, since_ms],
            |row| row.get(0),
        )
        .unwrap_or(None);
    Ok(sum.unwrap_or(0).max(0) as u64)
}

/// Records one usage event. Falls back to the in-memory ledger if the
/// SQLite write fails so admission math keeps working (spec §6.1).
pub fn record(conn: &Connection, platform: &str, model: &str, key_id: &str, kind: UsageKind, amount: u64, at_ms: i64) -> Result<()> {
    let res = conn.execute(
        "INSERT INTO pool_usage (platform, model, key_id, kind, amount, at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![platform, model, key_id, kind.as_str(), amount as i64, at_ms],
    );
    if res.is_err() {
        let mut events = MEMORY_FALLBACK.lock().unwrap();
        if events.len() >= MEMORY_FALLBACK_CAP {
            events.remove(0);
        }
        events.push(UsageEvent {
            platform: platform.to_string(),
            model: model.to_string(),
            key_id: key_id.to_string(),
            kind,
            amount,
            at_ms,
        });
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Lease {
    pub lease_id: LeaseId,
    pub platform: String,
    pub model: String,
    pub key_id: String,
    pub est_tokens: u64,
    pub acquired_at_ms: i64,
}

/// Records an in-flight lease so concurrent dispatches can't all read the
/// same unspent counter (spec §6.1). Persisted to `pool_leases` for
/// cross-restart visibility; the caller's in-memory `Vec<Lease>` (passed
/// into `admit`) is the fast path during normal operation.
pub fn acquire_lease(conn: &Connection, platform: &str, model: &str, key_id: &str, est_tokens: u64) -> Result<Lease> {
    let lease_id = uuid_v4();
    let acquired_at_ms = now_ms();
    conn.execute(
        "INSERT INTO pool_leases (lease_id, platform, model, key_id, est_tokens, acquired_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![lease_id, platform, model, key_id, est_tokens as i64, acquired_at_ms],
    )?;
    Ok(Lease { lease_id, platform: platform.to_string(), model: model.to_string(), key_id: key_id.to_string(), est_tokens, acquired_at_ms })
}

/// Idempotent — releasing a lease that's already gone (or never existed)
/// is a no-op, not an error, since a guard's `Drop` and an explicit
/// release can race harmlessly.
pub fn release_lease(conn: &Connection, lease_id: &str) -> Result<()> {
    conn.execute("DELETE FROM pool_leases WHERE lease_id = ?1", params![lease_id])?;
    Ok(())
}

/// Loads leases still within the 2-min backstop for one (platform, model,
/// key). Stale leases are dropped from the table lazily here rather than
/// on a timer.
pub fn live_leases(conn: &Connection, platform: &str, model: &str, key_id: &str) -> Result<Vec<Lease>> {
    let cutoff = now_ms() - LEASE_BACKSTOP_MS;
    conn.execute("DELETE FROM pool_leases WHERE acquired_at_ms < ?1", params![cutoff])?;

    let mut stmt = conn.prepare(
        "SELECT lease_id, platform, model, key_id, est_tokens, acquired_at_ms FROM pool_leases WHERE platform = ?1 AND model = ?2 AND key_id = ?3",
    )?;
    let rows = stmt
        .query_map(params![platform, model, key_id], |row| {
            Ok(Lease {
                lease_id: row.get(0)?,
                platform: row.get(1)?,
                model: row.get(2)?,
                key_id: row.get(3)?,
                est_tokens: row.get::<_, i64>(4)? as u64,
                acquired_at_ms: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn in_flight_for(leases: &[Lease], window: WindowKind) -> u64 {
    // requests windows count 1 per lease; token windows count est_tokens.
    match window {
        WindowKind::Rpm | WindowKind::Rpd => leases.len() as u64,
        WindowKind::Tpm | WindowKind::Tpd => leases.iter().map(|l| l.est_tokens).sum(),
    }
}

/// Admission check: for every window with `Some(limit)`, denies if
/// `recorded + in_flight + estimate >= limit`. A `None` limit skips that
/// window entirely (unknown-limit case — spec §6.1; the cooldown ceiling
/// covers the risk of an unbounded unknown limit, not this fn).
// Signature matches the plan's specified interface (spec §6.1) exactly;
// grouping the identifier triple into a struct would obscure the 1:1 match
// to `pool_usage`'s (platform, model, key_id) columns for no real gain.
#[allow(clippy::too_many_arguments)]
pub fn admit(conn: &Connection, leases: &[Lease], platform: &str, model: &str, key_id: &str, est_tokens: u64, limits: &Limits, now_ms: i64) -> AdmitResult {
    let windows: [(WindowKind, Option<u64>, UsageKind, u64); 4] = [
        (WindowKind::Rpm, limits.rpm.map(u64::from), UsageKind::Request, 1),
        (WindowKind::Rpd, limits.rpd.map(u64::from), UsageKind::Request, 1),
        (WindowKind::Tpm, limits.tpm.map(u64::from), UsageKind::Tokens, est_tokens),
        (WindowKind::Tpd, limits.tpd, UsageKind::Tokens, est_tokens),
    ];

    for (window, limit, kind, estimate) in windows {
        let Some(limit) = limit else { continue };
        if !window_kind_for(kind, window) {
            continue;
        }
        let since = window_start_ms(window, now_ms);
        let recorded = recorded_sum(conn, platform, model, key_id, kind, since).unwrap_or(0)
            + recorded_sum_fallback(platform, model, key_id, kind, since);
        let in_flight = in_flight_for(leases, window);
        if recorded + in_flight + estimate >= limit {
            let retry_after_ms = match window {
                WindowKind::Rpm | WindowKind::Tpm => 60_000 - (now_ms - since).max(0),
                WindowKind::Rpd | WindowKind::Tpd => next_utc_midnight_ms(now_ms) - now_ms,
            };
            return AdmitResult::Denied { window, retry_after_ms: retry_after_ms.max(0) };
        }
    }
    AdmitResult::Ok
}

fn uuid_v4() -> String {
    // no `uuid` crate in the workspace — a timestamp+random hex id is
    // sufficient for a lease key that only needs process-local uniqueness.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("lease-{}-{}", now_ms(), n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::ensure_pool_schema;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_pool_schema(&conn).unwrap();
        conn
    }

    fn limits(rpm: Option<u32>, rpd: Option<u32>, tpm: Option<u32>, tpd: Option<u64>) -> Limits {
        Limits { rpm, rpd, tpm, tpd }
    }

    #[test]
    fn admission_denies_when_recorded_plus_inflight_plus_estimate_exceeds_limit() {
        let conn = test_conn();
        let now = 1_700_000_000_000;
        record(&conn, "groq", "m1", "k1", UsageKind::Request, 8, now - 1000).unwrap();
        let lease = acquire_lease(&conn, "groq", "m1", "k1", 0).unwrap();
        let leases = vec![lease];
        let result = admit(&conn, &leases, "groq", "m1", "k1", 0, &limits(Some(10), None, None, None), now);
        assert_eq!(result, AdmitResult::Denied { window: WindowKind::Rpm, retry_after_ms: result_retry(&result) });
    }

    fn result_retry(r: &AdmitResult) -> i64 {
        match r {
            AdmitResult::Denied { retry_after_ms, .. } => *retry_after_ms,
            AdmitResult::Ok => panic!("expected denied"),
        }
    }

    #[test]
    fn admission_allows_when_under_limit() {
        let conn = test_conn();
        let now = 1_700_000_000_000;
        record(&conn, "groq", "m1", "k1", UsageKind::Request, 2, now - 1000).unwrap();
        let result = admit(&conn, &[], "groq", "m1", "k1", 0, &limits(Some(10), None, None, None), now);
        assert_eq!(result, AdmitResult::Ok);
    }

    #[test]
    fn admission_skips_unset_limit_windows() {
        let conn = test_conn();
        let now = 1_700_000_000_000;
        // Absurdly high recorded usage but no limit set anywhere -> Ok.
        record(&conn, "groq", "m1", "k1", UsageKind::Request, 1_000_000, now - 1000).unwrap();
        record(&conn, "groq", "m1", "k1", UsageKind::Tokens, 1_000_000, now - 1000).unwrap();
        let result = admit(&conn, &[], "groq", "m1", "k1", 100, &limits(None, None, None, None), now);
        assert_eq!(result, AdmitResult::Ok);
    }

    #[test]
    fn rpd_window_resets_at_utc_midnight_not_24h_rolling() {
        let conn = test_conn();
        // 2026-09-07T23:00:00Z
        let before_midnight = Utc.with_ymd_and_hms(2026, 9, 7, 23, 0, 0).unwrap().timestamp_millis();
        record(&conn, "groq", "m1", "k1", UsageKind::Request, 9, before_midnight).unwrap();

        // 2026-09-08T01:00:00Z is only 2h later but past UTC midnight -> window reset.
        let after_midnight = Utc.with_ymd_and_hms(2026, 9, 8, 1, 0, 0).unwrap().timestamp_millis();
        let result = admit(&conn, &[], "groq", "m1", "k1", 0, &limits(None, Some(10), None, None), after_midnight);
        assert_eq!(result, AdmitResult::Ok);
    }

    #[test]
    fn rpd_window_still_denies_within_same_utc_day() {
        let conn = test_conn();
        let morning = Utc.with_ymd_and_hms(2026, 9, 7, 1, 0, 0).unwrap().timestamp_millis();
        record(&conn, "groq", "m1", "k1", UsageKind::Request, 9, morning).unwrap();
        let evening = Utc.with_ymd_and_hms(2026, 9, 7, 23, 0, 0).unwrap().timestamp_millis();
        let result = admit(&conn, &[], "groq", "m1", "k1", 0, &limits(None, Some(10), None, None), evening);
        assert!(matches!(result, AdmitResult::Denied { window: WindowKind::Rpd, .. }));
    }

    #[test]
    fn lease_acquire_release_is_idempotent() {
        let conn = test_conn();
        let lease = acquire_lease(&conn, "groq", "m1", "k1", 100).unwrap();
        assert_eq!(live_leases(&conn, "groq", "m1", "k1").unwrap().len(), 1);
        release_lease(&conn, &lease.lease_id).unwrap();
        assert_eq!(live_leases(&conn, "groq", "m1", "k1").unwrap().len(), 0);
        // second release of the same id is a no-op, not an error.
        release_lease(&conn, &lease.lease_id).unwrap();
    }

    #[test]
    fn stale_lease_backstop_expires_after_2_minutes() {
        let conn = test_conn();
        let old_ms = now_ms() - LEASE_BACKSTOP_MS - 1000;
        conn.execute(
            "INSERT INTO pool_leases (lease_id, platform, model, key_id, est_tokens, acquired_at_ms) VALUES ('stale', 'groq', 'm1', 'k1', 0, ?1)",
            params![old_ms],
        )
        .unwrap();
        assert_eq!(live_leases(&conn, "groq", "m1", "k1").unwrap().len(), 0);
    }

    #[test]
    fn degraded_db_fallback_keeps_admitting_from_memory() {
        let conn = Connection::open_in_memory().unwrap();
        // Schema deliberately NOT created -> every pool_usage write fails,
        // forcing the in-memory fallback path.
        let now = now_ms();
        record(&conn, "fallback-test-platform", "m1", "k1", UsageKind::Request, 9, now - 1000).unwrap();
        let result = admit(&conn, &[], "fallback-test-platform", "m1", "k1", 0, &limits(Some(10), None, None, None), now);
        assert!(matches!(result, AdmitResult::Denied { window: WindowKind::Rpm, .. }));
    }
}
