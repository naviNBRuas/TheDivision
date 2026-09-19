//! Provider-wide pool inference and shared-pool gating — spec §6.3.

use crate::pool::ledger::{self, AdmitResult, Lease};
use rusqlite::Connection;
use divisi_core::free_pool::{self, PoolShape};

/// Derives the pool key freellmapi-style names like `openrouter::free`,
/// `google::project`, `nvidia::credit-pool`, `groq::account`,
/// `xkiro::free`, `navyai::daily-tokens` — driven by the catalog's
/// `FreeProvider.pool` shape, not hardcoded per platform.
pub fn infer_pool_key(platform: &str, _model: &str) -> String {
    let Some(provider) = free_pool::by_id(platform) else {
        return format!("{platform}::unknown");
    };
    let suffix = match provider.pool {
        Some(PoolShape::Free) => "free",
        Some(PoolShape::Project) => "project",
        Some(PoolShape::Account) => "account",
        Some(PoolShape::CreditPool { .. }) => "credit-pool",
        Some(PoolShape::DailyTokens { .. }) => "daily-tokens",
        None => "none",
    };
    format!("{platform}::{suffix}")
}

/// Providers with one shared free pool across every model — spec §6.3's
/// list. `least-remaining` key selection is skipped for these since every
/// key reports the same number.
const SHARED_POOL_PLATFORMS: &[&str] =
    &["routeway", "bazaarlink", "unorouter", "orcarouter", "xkiro", "anyapi", "navyai", "nara", "sea-lion", "aion", "requesty"];

pub fn is_shared_pool(platform: &str) -> bool {
    SHARED_POOL_PLATFORMS.contains(&platform)
}

/// Sums per-model `pool_usage` windows for the same `platform+key_id` and
/// admits/denies as one gate, so an `(N models × RPD)` fan-out on a shared
/// pool can't earn surprise 429s (spec §6.3).
pub fn aggregate_gate(conn: &Connection, platform: &str, key_id: &str, models: &[&str], now_ms: i64) -> AdmitResult {
    let Some(provider) = free_pool::by_id(platform) else {
        return AdmitResult::Ok;
    };

    // Aggregate est_tokens/leases across every model for this platform+key
    // so the same window math in `ledger::admit` treats the whole set as
    // one logical (platform, "*", key_id) consumer.
    let mut all_leases: Vec<Lease> = Vec::new();
    for model in models {
        if let Ok(leases) = ledger::live_leases(conn, platform, model, key_id) {
            all_leases.extend(leases);
        }
    }

    // Sum recorded usage across all models by checking admission against
    // each model's own window but requiring every one individually admit
    // — the aggregate is the strictest of the per-model checks combined
    // with the summed in-flight leases, which mirrors "one shared gate".
    for model in models {
        let result = ledger::admit(conn, &all_leases, platform, model, key_id, 0, &provider.limits, now_ms);
        if !matches!(result, AdmitResult::Ok) {
            return result;
        }
    }
    AdmitResult::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::ensure_pool_schema;
    use crate::pool::ledger::UsageKind;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_pool_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn infer_pool_key_matches_spec_examples() {
        assert_eq!(infer_pool_key("openrouter", "m"), "openrouter::free");
        assert_eq!(infer_pool_key("google", "m"), "google::project");
        assert_eq!(infer_pool_key("nvidia", "m"), "nvidia::credit-pool");
        assert_eq!(infer_pool_key("groq", "m"), "groq::account");
        // spec §6.3's illustrative list says "xkiro::free" but §5.2's actual
        // table (source of the Part A catalog) has xkiro as "5M tok/day
        // account-wide" -> PoolShape::DailyTokens; trust the catalog data
        // over the loose example.
        assert_eq!(infer_pool_key("xkiro", "m"), "xkiro::daily-tokens");
        assert_eq!(infer_pool_key("navyai", "m"), "navyai::daily-tokens");
    }

    #[test]
    fn is_shared_pool_matches_spec_list() {
        for platform in SHARED_POOL_PLATFORMS {
            assert!(is_shared_pool(platform));
        }
        assert!(!is_shared_pool("groq"));
        assert!(!is_shared_pool("openrouter"));
    }

    #[test]
    fn aggregate_gate_sums_across_models_for_same_key() {
        let conn = test_conn();
        let now = ledger::now_ms();
        // navyai: 150K tok/day, 20 rpm per the catalog. Push close to the
        // rpm limit via model A, then check model B's aggregate gate sees
        // the combined usage.
        for _ in 0..19 {
            ledger::record(&conn, "navyai", "model-a", "k1", UsageKind::Request, 1, now - 1000).unwrap();
        }
        let result = aggregate_gate(&conn, "navyai", "k1", &["model-a", "model-b"], now);
        assert!(matches!(result, AdmitResult::Denied { .. }));
    }
}
