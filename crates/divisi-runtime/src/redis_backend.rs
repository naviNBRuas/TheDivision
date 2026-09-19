//! Optional Redis-backed working memory (spec section 9's "pluggable
//! storage architecture"): fast, ephemeral, TTL-capable key/value state
//! that's genuinely useful as a shared scratchpad *while* multiple agent
//! processes are actively running concurrently — a role SQLite's
//! request-scoped connections aren't well suited to (no pub/sub, no TTL
//! eviction, higher latency for hot short-lived state).
//!
//! Entirely optional: only active when a Redis URL is configured (see
//! `resolve_url`). No agent or command requires Redis to function —
//! everything else in SingleCLI's memory subsystem works without it. This
//! module was built against, and its tests run against, a real local
//! Redis instance (`docker run redis:7-alpine`) during development, not
//! just written to the client library's API without verification — but
//! CI/other machines won't have that container, so tests skip cleanly
//! (not fail) when no Redis is reachable.

use anyhow::{Context, Result};
use redis::Commands;

/// `SINGLE_REDIS_URL`, e.g. `redis://127.0.0.1:6379`. Returns `None` (not
/// an error) when unset — Redis is opt-in.
pub fn resolve_url() -> Option<String> {
    std::env::var("SINGLE_REDIS_URL").ok()
}

fn connect(url: &str) -> Result<redis::Connection> {
    let client = redis::Client::open(url).with_context(|| format!("parsing redis url {url}"))?;
    client.get_connection().with_context(|| format!("connecting to redis at {url}"))
}

pub fn ping(url: &str) -> Result<()> {
    let mut conn = connect(url)?;
    let _: String = redis::cmd("PING").query(&mut conn).context("PING failed")?;
    Ok(())
}

pub fn set(url: &str, key: &str, value: &str, ttl_secs: Option<u64>) -> Result<()> {
    let mut conn = connect(url)?;
    match ttl_secs {
        Some(ttl) => conn.set_ex::<_, _, ()>(key, value, ttl).context("SET EX failed"),
        None => conn.set::<_, _, ()>(key, value).context("SET failed"),
    }
}

pub fn get(url: &str, key: &str) -> Result<Option<String>> {
    let mut conn = connect(url)?;
    conn.get(key).context("GET failed")
}

pub fn delete(url: &str, key: &str) -> Result<bool> {
    let mut conn = connect(url)?;
    let removed: i64 = conn.del(key).context("DEL failed")?;
    Ok(removed > 0)
}

/// Uses `KEYS`, which is fine for the small local working-memory
/// namespaces this is meant for — not appropriate at production Redis
/// scale, but SingleCLI's usage here is a single-user local dev tool, not
/// a shared multi-tenant cache.
pub fn list_keys(url: &str, pattern: &str) -> Result<Vec<String>> {
    let mut conn = connect(url)?;
    let mut keys: Vec<String> = conn.keys(pattern).context("KEYS failed")?;
    keys.sort();
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The URL this project's own Redis integration was developed and
    /// tested against (`docker run -d -p 16379:6379 redis:7-alpine`).
    /// Tests skip cleanly if nothing answers there instead of failing —
    /// most environments running `cargo test` won't have this container.
    const TEST_URL: &str = "redis://127.0.0.1:16379";

    fn skip_if_unavailable() -> Option<()> {
        if ping(TEST_URL).is_err() {
            eprintln!("skipping redis_backend test: no redis reachable at {TEST_URL}");
            return None;
        }
        Some(())
    }

    #[test]
    fn set_get_delete_round_trip_against_real_redis() {
        let Some(()) = skip_if_unavailable() else { return };
        let key = "singlecli:test:roundtrip";
        set(TEST_URL, key, "hello", None).unwrap();
        assert_eq!(get(TEST_URL, key).unwrap().as_deref(), Some("hello"));
        assert!(delete(TEST_URL, key).unwrap());
        assert_eq!(get(TEST_URL, key).unwrap(), None);
    }

    #[test]
    fn ttl_expires_the_key() {
        let Some(()) = skip_if_unavailable() else { return };
        let key = "singlecli:test:ttl";
        set(TEST_URL, key, "temp", Some(1)).unwrap();
        assert_eq!(get(TEST_URL, key).unwrap().as_deref(), Some("temp"));
        std::thread::sleep(std::time::Duration::from_millis(1200));
        assert_eq!(get(TEST_URL, key).unwrap(), None);
    }

    #[test]
    fn list_keys_matches_pattern() {
        let Some(()) = skip_if_unavailable() else { return };
        set(TEST_URL, "singlecli:test:list:a", "1", None).unwrap();
        set(TEST_URL, "singlecli:test:list:b", "2", None).unwrap();
        let keys = list_keys(TEST_URL, "singlecli:test:list:*").unwrap();
        assert_eq!(keys, vec!["singlecli:test:list:a", "singlecli:test:list:b"]);
        delete(TEST_URL, "singlecli:test:list:a").unwrap();
        delete(TEST_URL, "singlecli:test:list:b").unwrap();
    }

    #[test]
    fn resolve_url_reads_env_var() {
        std::env::set_var("SINGLE_REDIS_URL", "redis://example.invalid:6379");
        assert_eq!(resolve_url().as_deref(), Some("redis://example.invalid:6379"));
        std::env::remove_var("SINGLE_REDIS_URL");
        assert_eq!(resolve_url(), None);
    }

    #[test]
    fn connecting_to_a_bad_url_errors_cleanly_not_panics() {
        assert!(get("redis://127.0.0.1:1", "x").is_err());
    }
}
