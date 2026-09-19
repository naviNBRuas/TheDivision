//! Cooldown ladder + provenance — spec §6.2. The probe job that
//! re-validates `Heuristic` benches early is wired in Phase 3's client
//! integration; this task only implements the ladder, provenance, and the
//! `heuristic_probe_candidates` query it will use.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::time::Duration;

const TRANSIENT_MS: i64 = 90 * 1000;
const ESCALATION_LADDER_MS: [i64; 4] = [2 * 60 * 1000, 10 * 60 * 1000, 60 * 60 * 1000, 24 * 60 * 60 * 1000];
const HITS_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
const PAYMENT_OR_TIER_MS: i64 = 24 * 60 * 60 * 1000;
const AUTH_BENCHED_MS: i64 = 5 * 60 * 1000;
const LOCAL_ERROR_MS: i64 = 5 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    Heuristic,
    Authoritative,
    Credit,
    Tier,
}

impl Provenance {
    fn as_str(self) -> &'static str {
        match self {
            Provenance::Heuristic => "heuristic",
            Provenance::Authoritative => "authoritative",
            Provenance::Credit => "credit",
            Provenance::Tier => "tier",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum BenchKind {
    Transient,
    Escalated,
    PaymentRequired,
    TierGate,
    AuthBenched,
    Local,
    Authoritative(Duration),
}

pub fn cooldown_ceiling(conn: &Connection) -> Duration {
    let ms: i64 = conn
        .query_row("SELECT cooldown_ceiling_ms FROM pool_config WHERE id = 0", (), |row| row.get(0))
        .unwrap_or(24 * 60 * 60 * 1000);
    Duration::from_millis(ms as u64)
}

pub fn set_cooldown_ceiling(conn: &Connection, d: Duration) -> Result<()> {
    conn.execute("UPDATE pool_config SET cooldown_ceiling_ms = ?1 WHERE id = 0", params![d.as_millis() as i64])?;
    Ok(())
}

/// Applies one bench event, escalating the ladder over a rolling 24h hit
/// window. Returns the resulting `until_ms`.
pub fn bench(conn: &Connection, platform: &str, model: &str, key_id: &str, kind: BenchKind, now_ms: i64) -> Result<i64> {
    let (provenance, base_ms, enters_ladder) = match kind {
        BenchKind::Transient => (Provenance::Heuristic, TRANSIENT_MS, true),
        BenchKind::Escalated => (Provenance::Heuristic, 0, true), // base_ms computed from hits below
        BenchKind::PaymentRequired => (Provenance::Credit, PAYMENT_OR_TIER_MS, false),
        BenchKind::TierGate => (Provenance::Tier, PAYMENT_OR_TIER_MS, false),
        BenchKind::AuthBenched => (Provenance::Heuristic, AUTH_BENCHED_MS, false),
        BenchKind::Local => (Provenance::Heuristic, LOCAL_ERROR_MS, false),
        BenchKind::Authoritative(d) => (Provenance::Authoritative, d.as_millis() as i64, false),
    };

    let existing = load_row(conn, platform, model, key_id)?;
    let (mut hits, mut window_start) = match &existing {
        Some(row) if enters_ladder && now_ms - row.hits_window_start_ms < HITS_WINDOW_MS => (row.hits, row.hits_window_start_ms),
        _ => (0, now_ms),
    };

    let mut delay_ms = base_ms;
    if enters_ladder {
        hits += 1;
        // hits==1 -> Transient (90s); hits==2 -> ladder[0] (2m); hits==3 ->
        // ladder[1] (10m); ...; hits>=5 clamps to the last rung (1 day).
        let ladder_index = (hits - 2).clamp(0, ESCALATION_LADDER_MS.len() as i64 - 1);
        delay_ms = if hits <= 1 { TRANSIENT_MS } else { ESCALATION_LADDER_MS[ladder_index as usize] };
    } else if let Some(row) = &existing {
        // non-ladder benches (payment/tier/auth/local) don't touch hits.
        hits = row.hits;
        window_start = row.hits_window_start_ms;
    }

    // The operator ceiling caps the ladder + 402/403 benches, but never
    // shortens an Authoritative (provider-stated) time.
    let ceiling_ms = cooldown_ceiling(conn).as_millis() as i64;
    let capped_ms = if matches!(provenance, Provenance::Authoritative) { delay_ms } else { delay_ms.min(ceiling_ms) };

    let until_ms = now_ms + capped_ms;

    conn.execute(
        "INSERT INTO pool_cooldowns (platform, model, key_id, until_ms, provenance, hits, hits_window_start_ms, bench_start_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(platform, model, key_id) DO UPDATE SET
            until_ms = excluded.until_ms,
            provenance = excluded.provenance,
            hits = excluded.hits,
            hits_window_start_ms = excluded.hits_window_start_ms,
            bench_start_ms = excluded.bench_start_ms",
        params![platform, model, key_id, until_ms, provenance.as_str(), hits, window_start, now_ms],
    )?;

    Ok(until_ms)
}

pub fn is_benched(conn: &Connection, platform: &str, model: &str, key_id: &str, now_ms: i64) -> Result<Option<i64>> {
    let row = load_row(conn, platform, model, key_id)?;
    Ok(row.filter(|r| r.until_ms > now_ms).map(|r| r.until_ms))
}

/// Called on a success — resets the hit counter so a run of successes
/// de-escalates future benches back to `Transient`.
pub fn clear_hits(conn: &Connection, platform: &str, model: &str, key_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE pool_cooldowns SET hits = 0, hits_window_start_ms = 0 WHERE platform = ?1 AND model = ?2 AND key_id = ?3",
        params![platform, model, key_id],
    )?;
    Ok(())
}

pub fn clear(conn: &Connection, key_id: Option<&str>) -> Result<()> {
    match key_id {
        Some(key_id) => {
            conn.execute("DELETE FROM pool_cooldowns WHERE key_id = ?1", params![key_id])?;
        }
        None => {
            conn.execute("DELETE FROM pool_cooldowns", ())?;
        }
    }
    Ok(())
}

/// Candidates for the probe job (wired in Phase 3): `Heuristic`
/// provenance, past half the bench elapsed, with >60s remaining. Never
/// `Authoritative`/`Credit`/`Tier` — those are never probed (spec §6.2).
pub fn heuristic_probe_candidates(conn: &Connection, now_ms: i64) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT platform, model, key_id, until_ms, bench_start_ms FROM pool_cooldowns WHERE provenance = 'heuristic' AND until_ms > ?1",
    )?;
    let rows = stmt
        .query_map(params![now_ms], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(rows
        .into_iter()
        .filter_map(|(platform, model, key_id, until_ms, bench_start_ms)| {
            let remaining = until_ms - now_ms;
            let total = (until_ms - bench_start_ms).max(1);
            let half_elapsed = now_ms - bench_start_ms >= total / 2;
            if remaining > 60_000 && half_elapsed {
                Some((platform, model, key_id))
            } else {
                None
            }
        })
        .collect())
}

struct Row {
    until_ms: i64,
    hits: i64,
    hits_window_start_ms: i64,
}

fn load_row(conn: &Connection, platform: &str, model: &str, key_id: &str) -> Result<Option<Row>> {
    let row = conn
        .query_row(
            "SELECT until_ms, hits, hits_window_start_ms FROM pool_cooldowns WHERE platform = ?1 AND model = ?2 AND key_id = ?3",
            params![platform, model, key_id],
            |row| Ok(Row { until_ms: row.get(0)?, hits: row.get(1)?, hits_window_start_ms: row.get(2)? }),
        )
        .optional()?;
    Ok(row)
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

    #[test]
    fn ladder_escalates_over_scripted_hit_sequence() {
        let conn = test_conn();
        let mut now = 1_700_000_000_000;

        let until = bench(&conn, "p", "m", "k", BenchKind::Escalated, now).unwrap();
        assert_eq!(until - now, TRANSIENT_MS); // hit 1

        now += 1000;
        let until = bench(&conn, "p", "m", "k", BenchKind::Escalated, now).unwrap();
        assert_eq!(until - now, ESCALATION_LADDER_MS[0]); // hit 2 -> 2m

        now += 1000;
        let until = bench(&conn, "p", "m", "k", BenchKind::Escalated, now).unwrap();
        assert_eq!(until - now, ESCALATION_LADDER_MS[1]); // hit 3 -> 10m

        now += 1000;
        let until = bench(&conn, "p", "m", "k", BenchKind::Escalated, now).unwrap();
        assert_eq!(until - now, ESCALATION_LADDER_MS[2]); // hit 4 -> 1h

        now += 1000;
        let until = bench(&conn, "p", "m", "k", BenchKind::Escalated, now).unwrap();
        assert_eq!(until - now, ESCALATION_LADDER_MS[3]); // hit 5 -> 1day (ceiling)
    }

    #[test]
    fn success_clears_hit_counter() {
        let conn = test_conn();
        let now = 1_700_000_000_000;
        bench(&conn, "p", "m", "k", BenchKind::Escalated, now).unwrap();
        bench(&conn, "p", "m", "k", BenchKind::Escalated, now + 1000).unwrap();
        clear_hits(&conn, "p", "m", "k").unwrap();

        let until = bench(&conn, "p", "m", "k", BenchKind::Escalated, now + 2000).unwrap();
        assert_eq!(until - (now + 2000), TRANSIENT_MS); // back to hit 1
    }

    #[test]
    fn payment_required_benches_one_day_as_credit_provenance() {
        let conn = test_conn();
        let now = 1_700_000_000_000;
        let until = bench(&conn, "p", "m", "k", BenchKind::PaymentRequired, now).unwrap();
        assert_eq!(until - now, PAYMENT_OR_TIER_MS);
    }

    #[test]
    fn local_error_never_enters_ladder() {
        let conn = test_conn();
        let now = 1_700_000_000_000;
        bench(&conn, "p", "m", "k", BenchKind::Local, now).unwrap();
        let row = load_row(&conn, "p", "m", "k").unwrap().unwrap();
        assert_eq!(row.hits, 0);
    }

    #[test]
    fn heuristic_capped_at_operator_ceiling() {
        let conn = test_conn();
        set_cooldown_ceiling(&conn, Duration::from_secs(600)).unwrap(); // 10m ceiling
        let now = 1_700_000_000_000;
        // Drive hits up to the 1-day rung, which should be capped to 10m.
        for i in 0..5 {
            bench(&conn, "p", "m", "k", BenchKind::Escalated, now + i * 1000).unwrap();
        }
        let until = bench(&conn, "p", "m", "k", BenchKind::Escalated, now + 5000).unwrap();
        assert_eq!(until - (now + 5000), 600_000);
    }

    #[test]
    fn authoritative_never_shortened_by_ceiling() {
        let conn = test_conn();
        set_cooldown_ceiling(&conn, Duration::from_secs(60)).unwrap(); // tiny ceiling
        let now = 1_700_000_000_000;
        let until = bench(&conn, "p", "m", "k", BenchKind::Authoritative(Duration::from_secs(7200)), now).unwrap();
        assert_eq!(until - now, 7_200_000);
    }

    #[test]
    fn probe_candidates_only_heuristic_past_half_elapsed() {
        let conn = test_conn();
        let now = 1_700_000_000_000;
        bench(&conn, "p", "m", "k1", BenchKind::Transient, now).unwrap(); // until now+90s
        bench(&conn, "p", "m", "k2", BenchKind::PaymentRequired, now).unwrap(); // Credit provenance -> never

        // Not yet half elapsed (only 10s of a 90s bench) -> not a candidate.
        let candidates = heuristic_probe_candidates(&conn, now + 10_000).unwrap();
        assert!(candidates.is_empty());

        // Well past half elapsed (70s of 90s), >60s ceiling would already
        // have expired for a shorter bench, so scale up the check via a
        // longer escalated bench instead: re-bench k1 into a 10m rung.
        bench(&conn, "p", "m", "k1", BenchKind::Escalated, now + 1_000).unwrap();
        bench(&conn, "p", "m", "k1", BenchKind::Escalated, now + 2_000).unwrap();
        let until = bench(&conn, "p", "m", "k1", BenchKind::Escalated, now + 3_000).unwrap(); // -> 10m rung
        let total = until - (now + 3_000);
        let past_half = now + 3_000 + total / 2 + 1000;

        let candidates = heuristic_probe_candidates(&conn, past_half).unwrap();
        let ids: Vec<&str> = candidates.iter().map(|(_, _, k)| k.as_str()).collect();
        assert!(ids.contains(&"k1"));
        assert!(!ids.contains(&"k2"));
    }
}
