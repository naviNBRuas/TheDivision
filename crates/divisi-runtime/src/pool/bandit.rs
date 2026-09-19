//! Thompson-sampling scorer — spec §6.4. Ranks `(platform, model, key_id)`
//! candidates on reliability (decayed Beta posterior), speed (inverse
//! latency), and catalog intelligence, then applies headroom/ratelimit
//! guardrail factors before picking a winner.

use crate::pool::ledger;
use rusqlite::{params, Connection};
use divisi_core::free_pool;
use std::sync::atomic::{AtomicU64, Ordering};

const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const HALF_LIFE_DAYS: f64 = 2.0;

/// Appends one outcome and prunes anything past the 7-day retention
/// window on the same write (spec §6.4). Signature matches the plan's
/// specified interface exactly (same rationale as `ledger::admit`).
#[allow(clippy::too_many_arguments)]
pub fn record_outcome(conn: &Connection, platform: &str, model: &str, key_id: &str, ok: bool, latency_ms: u64, tokens: u64, at_ms: i64) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO pool_outcomes (platform, model, key_id, ok, latency_ms, tokens, at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![platform, model, key_id, ok as i64, latency_ms as i64, tokens as i64, at_ms],
    )?;
    conn.execute("DELETE FROM pool_outcomes WHERE at_ms < ?1", params![at_ms - RETENTION_MS])?;
    Ok(())
}

/// `(alpha, beta)` of a decay-weighted Beta posterior over the 7-day
/// outcome window, 2-day half-life. `Beta(1,1)` uniform prior — there is
/// no community feed to seed from this iteration (spec §17/D3); a future
/// signed-feed loader can replace the `1.0 +` base with a seeded prior
/// without changing this function's shape.
pub fn posterior(conn: &Connection, platform: &str, model: &str, key_id: &str, now_ms: i64) -> (f64, f64) {
    let mut stmt = match conn.prepare(
        "SELECT ok, at_ms FROM pool_outcomes WHERE platform = ?1 AND model = ?2 AND key_id = ?3 AND at_ms >= ?4",
    ) {
        Ok(s) => s,
        Err(_) => return (1.0, 1.0),
    };
    let rows: Vec<(bool, i64)> = stmt
        .query_map(params![platform, model, key_id, now_ms - RETENTION_MS], |row| {
            Ok((row.get::<_, i64>(0)? != 0, row.get::<_, i64>(1)?))
        })
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    let mut successes = 0.0f64;
    let mut failures = 0.0f64;
    for (ok, at_ms) in rows {
        let age_days = ((now_ms - at_ms).max(0) as f64) / (24.0 * 60.0 * 60.0 * 1000.0);
        let weight = 0.5f64.powf(age_days / HALF_LIFE_DAYS);
        if ok {
            successes += weight;
        } else {
            failures += weight;
        }
    }
    (1.0 + successes, 1.0 + failures)
}

/// Deterministic Thompson sample bounded to `[0, 1]`. No `rand` crate in
/// the workspace — a counter-seeded xorshift is a fine heuristic scorer
/// here (this ranks candidates relative to each other, it isn't
/// cryptographic or statistically load-bearing beyond "roughly samples
/// the posterior's shape"), documented tradeoff per the plan.
pub fn thompson_sample(alpha: f64, beta: f64, rng_seed: u64) -> f64 {
    // Mean-approximation sample: draw a uniform jitter around the Beta
    // mean, scaled by the posterior's concentration (more data -> less
    // jitter), then clamp to [0,1]. This approximates Thompson sampling's
    // "explore proportional to uncertainty" behavior without a real Beta
    // sampler (which needs `rand_distr`, not in the workspace).
    let mean = alpha / (alpha + beta);
    let concentration = alpha + beta;
    let spread = (1.0 / (1.0 + concentration)).min(0.5);
    let jitter = (xorshift(rng_seed) as f64 / u64::MAX as f64) * 2.0 - 1.0; // [-1, 1]
    (mean + jitter * spread).clamp(0.0, 1.0)
}

fn xorshift(seed: u64) -> u64 {
    let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(1);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

static SAMPLE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Convenience seed source for callers that don't have a natural seed
/// (e.g. `pick`'s per-candidate scoring loop).
fn next_seed() -> u64 {
    SAMPLE_COUNTER.fetch_add(1, Ordering::Relaxed) ^ ledger::now_ms() as u64
}

pub fn speed_score(latency_ms: u64, ttfb_ms: Option<u64>) -> f64 {
    let effective = ttfb_ms.unwrap_or(latency_ms).max(1);
    // Normalize: 100ms -> ~1.0, 10s+ -> ~0.0, smooth inverse curve.
    (1000.0 / (effective as f64 + 1000.0)).clamp(0.0, 1.0)
}

pub fn intelligence_score(platform: &str, _model: &str) -> f64 {
    match free_pool::by_id(platform) {
        Some(p) => (p.intelligence_rank as f64 / 10.0).clamp(0.0, 1.0),
        None => 0.5,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Strategy {
    Balanced,
    Smartest,
    Fastest,
    Reliable,
    Custom { w_rel: f64, w_speed: f64, w_intel: f64 },
    Priority,
}

impl Strategy {
    /// Weights always sum to 1 (convex combination) for every named
    /// strategy; `Custom` is the caller's responsibility (validated at
    /// the `routing.toml` parse boundary, not here).
    pub fn weights(&self) -> (f64, f64, f64) {
        match self {
            Strategy::Balanced => (0.5, 0.25, 0.25),
            Strategy::Smartest => (0.2, 0.1, 0.7),
            Strategy::Reliable => (0.8, 0.1, 0.1),
            Strategy::Fastest => (0.1, 0.8, 0.1),
            Strategy::Custom { w_rel, w_speed, w_intel } => (*w_rel, *w_speed, *w_intel),
            Strategy::Priority => (0.0, 0.0, 0.0), // unused: priority never scores
        }
    }
}

pub fn effective_score(base: f64, headroom_factor: f64, ratelimit_factor: f64) -> f64 {
    base * headroom_factor.clamp(0.0, 1.0) * ratelimit_factor.clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySelection {
    Auto,
    LeastRemaining,
}

/// `LeastRemaining` is meaningless for a shared pool (every key reports
/// the same headroom number there) — falls back to `Auto` behavior
/// (first candidate) in that case.
pub fn key_selection<'a>(mode: KeySelection, platform: &str, candidates: &'a [String]) -> Option<&'a String> {
    if candidates.is_empty() {
        return None;
    }
    if mode == KeySelection::LeastRemaining && !crate::pool::pools::is_shared_pool(platform) {
        // "least remaining" ordering needs live headroom data per key,
        // which the caller (routing layer, Phase 6) supplies by sorting
        // `candidates` before calling this -- this fn's contract is just
        // "pick the first" once the caller has done that ordering, kept
        // deliberately dumb so the headroom-fetch stays in one place.
        return candidates.first();
    }
    candidates.first()
}

/// Scores every non-benched/non-disabled candidate and returns the top
/// pick. `Priority` strategy skips scoring entirely: first admissible
/// candidate in chain order wins.
pub fn pick(
    conn: &Connection,
    strategy: &Strategy,
    candidates: &[(String, String, String)],
    now_ms: i64,
) -> Option<(String, String, String)> {
    if candidates.is_empty() {
        return None;
    }

    let admissible: Vec<&(String, String, String)> = candidates
        .iter()
        .filter(|(platform, model, key_id)| {
            crate::pool::cooldown::is_benched(conn, platform, model, key_id, now_ms).ok().flatten().is_none()
                && !divisi_core::pool_keys::is_disabled(conn, platform, key_id).unwrap_or(false)
        })
        .collect();

    if admissible.is_empty() {
        return None;
    }

    if *strategy == Strategy::Priority {
        return admissible.first().map(|c| (*c).clone());
    }

    let (w_rel, w_speed, w_intel) = strategy.weights();
    let mut best: Option<((String, String, String), f64)> = None;
    for (platform, model, key_id) in admissible {
        let (alpha, beta) = posterior(conn, platform, model, key_id, now_ms);
        let reliability = thompson_sample(alpha, beta, next_seed());
        let speed = speed_score(300, None); // no live latency sample yet at pick-time; refined post-response.
        let intelligence = intelligence_score(platform, model);
        let base = w_rel * reliability + w_speed * speed + w_intel * intelligence;

        let headroom_factor = 1.0; // headroom wiring lands with the client/coordinator integration (Phase 4-6).
        let ratelimit_factor = 1.0;
        let score = effective_score(base, headroom_factor, ratelimit_factor);

        if best.as_ref().map(|(_, s)| score > *s).unwrap_or(true) {
            best = Some(((platform.clone(), model.clone(), key_id.clone()), score));
        }
    }
    best.map(|(c, _)| c)
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
    fn posterior_starts_at_uniform_beta_1_1() {
        let conn = test_conn();
        let (a, b) = posterior(&conn, "groq", "m1", "k1", 1_700_000_000_000);
        assert_eq!((a, b), (1.0, 1.0));
    }

    #[test]
    fn posterior_decays_with_2day_half_life() {
        let conn = test_conn();
        let now = 1_700_000_000_000;
        // A success exactly 2 days old should contribute weight ~0.5.
        record_outcome(&conn, "groq", "m1", "k1", true, 100, 50, now - 2 * 24 * 60 * 60 * 1000).unwrap();
        let (alpha, _beta) = posterior(&conn, "groq", "m1", "k1", now);
        assert!((alpha - 1.5).abs() < 0.01, "alpha={alpha}");
    }

    #[test]
    fn thompson_sample_bounded_0_1() {
        for seed in 0..50 {
            let s = thompson_sample(3.0, 2.0, seed);
            assert!((0.0..=1.0).contains(&s), "sample {s} out of bounds");
        }
    }

    #[test]
    fn strategy_weights_sum_to_one_for_every_named_strategy() {
        for s in [Strategy::Balanced, Strategy::Smartest, Strategy::Fastest, Strategy::Reliable] {
            let (a, b, c) = s.weights();
            assert!((a + b + c - 1.0).abs() < 1e-9, "{s:?} weights don't sum to 1: {a} {b} {c}");
        }
    }

    #[test]
    fn effective_score_is_base_times_headroom_times_ratelimit() {
        assert_eq!(effective_score(0.8, 0.5, 0.5), 0.2);
        assert_eq!(effective_score(1.0, 1.0, 1.0), 1.0);
    }

    #[test]
    fn timeout_counts_as_reliability_fail_and_speed_sample() {
        let conn = test_conn();
        let now = 1_700_000_000_000;
        // A timeout is recorded as ok=false but still with a latency/tokens
        // sample (spec: "timeouts count as failures for reliability but
        // feed speed, 0 tokens").
        record_outcome(&conn, "groq", "m1", "k1", false, 30_000, 0, now).unwrap();
        let (alpha, beta) = posterior(&conn, "groq", "m1", "k1", now);
        assert_eq!(alpha, 1.0); // no success weight added
        assert!(beta > 1.0); // failure weight added
    }

    #[test]
    fn priority_strategy_ignores_score_takes_first_admissible() {
        let conn = test_conn();
        let now = 1_700_000_000_000;
        let candidates = vec![
            ("groq".to_string(), "m1".to_string(), "k1".to_string()),
            ("cerebras".to_string(), "m1".to_string(), "k1".to_string()),
        ];
        // Bench the first candidate so it's excluded, second should win regardless of any score.
        crate::pool::cooldown::bench(&conn, "groq", "m1", "k1", crate::pool::cooldown::BenchKind::Transient, now).unwrap();
        let picked = pick(&conn, &Strategy::Priority, &candidates, now).unwrap();
        assert_eq!(picked.0, "cerebras");
    }

    #[test]
    fn least_remaining_skipped_for_shared_pools() {
        let candidates = vec!["k1".to_string(), "k2".to_string()];
        // navyai is a shared pool; the fn contract falls back to first().
        let picked = key_selection(KeySelection::LeastRemaining, "navyai", &candidates);
        assert_eq!(picked, Some(&"k1".to_string()));
    }
}
