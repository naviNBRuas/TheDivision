//! Per-provider API-key storage for the E28 free-provider pool
//! (`free_pool::FreeProvider`) — distinct from both `providers.rs`'s
//! single shared key per provider and `provider_keys.rs`'s per-agent
//! labeled keys, because pool providers are keyed by `(platform, key_id)`
//! with no agent attribution at all (a pool key belongs to the pool
//! engine, not to any one agent). Reuses the same registry-row/keychain
//! split those two modules already establish: this table only ever holds
//! metadata, the actual key value lives in the OS keychain under
//! `secret_name(platform, key_id)`, set separately via
//! `divisi_core::secrets::SecretStore` by the caller (mirrors
//! `provider_keys.rs::add`'s doc comment).
//!
//! Storage is SQLite (the shared runtime db, opened by the caller via
//! `crate::state`-equivalent in `divisi-runtime`), not a TOML file —
//! `notes.rs` is the precedent for a `divisi-core` module owning a table
//! in that shared db via `ensure_schema(conn)` + plain rusqlite CRUD.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

/// `"pool-key:{platform}:{key_id}"` — a namespace distinct from
/// `providers.rs`'s `"provider:{name}"` and `provider_keys.rs`'s
/// `"provider-key:{provider}:{label}"`, so all three registries can
/// coexist in the OS keychain without ever colliding.
pub fn secret_name(platform: &str, key_id: &str) -> String {
    format!("pool-key:{platform}:{key_id}")
}

#[derive(Debug, Clone, PartialEq)]
pub struct PoolProviderKey {
    pub platform: String,
    pub key_id: String,
    pub secret_ref: String,
    pub added_at: String,
    pub last_validated_at: Option<String>,
    pub valid: bool,
    pub disabled: bool,
}

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS pool_provider_keys (
            platform TEXT NOT NULL,
            key_id TEXT NOT NULL,
            secret_ref TEXT NOT NULL,
            added_at TEXT NOT NULL,
            last_validated_at TEXT,
            valid INTEGER NOT NULL DEFAULT 0,
            disabled INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (platform, key_id)
        )",
        (),
    )?;
    Ok(())
}

/// Registers (or re-registers) one key's metadata row. The caller stores
/// the actual secret value separately via `secrets::SecretStore` under
/// `secret_name(platform, key_id)` — this only writes the registry row,
/// same separation `provider_keys.rs::add` documents.
pub fn add(conn: &Connection, platform: &str, key_id: &str) -> Result<()> {
    let secret_ref = secret_name(platform, key_id);
    let added_at = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO pool_provider_keys (platform, key_id, secret_ref, added_at, last_validated_at, valid, disabled)
         VALUES (?1, ?2, ?3, ?4, NULL, 0, 0)
         ON CONFLICT(platform, key_id) DO UPDATE SET secret_ref = excluded.secret_ref",
        params![platform, key_id, secret_ref, added_at],
    )
    .context("inserting pool provider key")?;
    Ok(())
}

/// Picks a fresh `key_id` for a new key on a platform that already has
/// `existing` keys, so a second `divisi provider add-free` call for the
/// same platform (e.g. a key from a different account) adds real pool
/// capacity instead of overwriting the first key — see the live-
/// verification finding in `handlers.rs`'s `Request::ProviderAddFree`.
/// The very first key for a platform stays `"default"` (backward
/// compatible with every already-registered single-key platform); every
/// one after that is `key2`, `key3`, ... skipping any id already taken
/// (defensive — `existing` should never contain a gap in practice, but
/// this never picks a colliding id even if it does).
pub fn next_free_key_id(existing: &[PoolProviderKey]) -> String {
    if existing.is_empty() {
        return "default".to_string();
    }
    let mut n = existing.len() + 1;
    loop {
        let candidate = format!("key{n}");
        if !existing.iter().any(|k| k.key_id == candidate) {
            return candidate;
        }
        n += 1;
    }
}

pub fn list(conn: &Connection, platform: Option<&str>) -> Result<Vec<PoolProviderKey>> {
    let mut sql = String::from("SELECT platform, key_id, secret_ref, added_at, last_validated_at, valid, disabled FROM pool_provider_keys");
    if platform.is_some() {
        sql.push_str(" WHERE platform = ?1");
    }
    sql.push_str(" ORDER BY platform, key_id");

    let mut stmt = conn.prepare(&sql)?;
    let rows = if let Some(platform) = platform {
        stmt.query_map(params![platform], row_to_key)?.collect::<rusqlite::Result<Vec<_>>>()
    } else {
        stmt.query_map((), row_to_key)?.collect::<rusqlite::Result<Vec<_>>>()
    };
    rows.context("collecting pool provider keys")
}

/// Records real evidence of whether a key works — either a real dispatch's
/// `AuthFailed`/success, or an explicit `divisi provider validate` probe.
/// Live-verification finding (2026-09-12): `valid = false` here used to be
/// purely informational — `candidates_from_keys` only ever filtered on
/// `disabled`, never `valid`, so a key already confirmed bad (e.g.
/// `kilo`/`xkiro`, both failing real `validate` probes) kept getting
/// handed to the bandit forever, wasting real dispatch attempts on a key
/// with actual evidence against it. `valid = false` now also disables the
/// key — `candidates_from_keys` already excludes disabled keys, so this
/// is the one place that needed to change, not the filter. `valid = true`
/// never disables (a key can only go from good evidence to bad, not the
/// reverse, without an explicit `enable`).
pub fn mark_validated(conn: &Connection, platform: &str, key_id: &str, valid: bool) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        // A key that answers is back in the pool; one that fails is only marked invalid. Its pool
        // cooldown keeps it out for a while and the next validation decides again -- a failed probe
        // used to disable a key for good, which left 18 working keys switched off (2026-09-24).
        "UPDATE pool_provider_keys SET last_validated_at = ?1, valid = ?2, disabled = CASE WHEN ?2 = 1 THEN 0 ELSE disabled END WHERE platform = ?3 AND key_id = ?4",
        params![now, valid as i64, platform, key_id],
    )
    .context("marking pool provider key validated")?;
    Ok(())
}

pub fn disable(conn: &Connection, platform: &str, key_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE pool_provider_keys SET disabled = 1 WHERE platform = ?1 AND key_id = ?2",
        params![platform, key_id],
    )
    .context("disabling pool provider key")?;
    Ok(())
}

/// Re-enables a key `mark_validated`/`disable` turned off — e.g. after
/// registering a fresh, working key value under the same `key_id`
/// (`divisi provider add-free ... --key-id <existing>` for rotation), or
/// a human judging an old failure no longer applies. Does not touch
/// `valid`/`last_validated_at` — the next real dispatch or `validate`
/// probe updates those on their own.
pub fn enable(conn: &Connection, platform: &str, key_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE pool_provider_keys SET disabled = 0 WHERE platform = ?1 AND key_id = ?2",
        params![platform, key_id],
    )
    .context("enabling pool provider key")?;
    Ok(())
}

pub fn is_disabled(conn: &Connection, platform: &str, key_id: &str) -> Result<bool> {
    let disabled: Option<i64> = conn
        .query_row(
            "SELECT disabled FROM pool_provider_keys WHERE platform = ?1 AND key_id = ?2",
            params![platform, key_id],
            |row| row.get(0),
        )
        .optional()
        .context("querying pool provider key disabled state")?;
    Ok(disabled.unwrap_or(0) != 0)
}

fn row_to_key(row: &rusqlite::Row) -> rusqlite::Result<PoolProviderKey> {
    Ok(PoolProviderKey {
        platform: row.get("platform")?,
        key_id: row.get("key_id")?,
        secret_ref: row.get("secret_ref")?,
        added_at: row.get("added_at")?,
        last_validated_at: row.get("last_validated_at")?,
        valid: row.get::<_, i64>("valid")? != 0,
        disabled: row.get::<_, i64>("disabled")? != 0,
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
    fn add_list_mark_validated_roundtrip() {
        let conn = test_conn();
        add(&conn, "groq", "default").unwrap();

        let keys = list(&conn, Some("groq")).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].secret_ref, "pool-key:groq:default");
        assert!(!keys[0].valid);
        assert!(keys[0].last_validated_at.is_none());

        mark_validated(&conn, "groq", "default", true).unwrap();
        let keys = list(&conn, Some("groq")).unwrap();
        assert!(keys[0].valid);
        assert!(keys[0].last_validated_at.is_some());
    }

    #[test]
    fn mark_validated_false_also_disables_the_key() {
        // Regression test: candidates_from_keys only ever filtered on
        // `disabled`, never `valid` -- a key confirmed bad by real
        // evidence (an auth failure, or an explicit validate probe) kept
        // getting handed to the bandit forever.
        let conn = test_conn();
        add(&conn, "kilo", "default").unwrap();
        assert!(!is_disabled(&conn, "kilo", "default").unwrap());

        mark_validated(&conn, "kilo", "default", false).unwrap();
        let keys = list(&conn, Some("kilo")).unwrap();
        assert!(!keys[0].valid);
        assert!(!keys[0].disabled, "a failed validation is timed (see candidates_from_keys), never a permanent disable");
    }

    #[test]
    fn mark_validated_true_never_disables() {
        let conn = test_conn();
        add(&conn, "groq", "default").unwrap();
        mark_validated(&conn, "groq", "default", true).unwrap();
        assert!(!is_disabled(&conn, "groq", "default").unwrap());
    }

    #[test]
    fn enable_reverses_a_disable_from_bad_evidence() {
        let conn = test_conn();
        add(&conn, "kilo", "default").unwrap();
        disable(&conn, "kilo", "default").unwrap();
        assert!(is_disabled(&conn, "kilo", "default").unwrap());

        enable(&conn, "kilo", "default").unwrap();
        assert!(!is_disabled(&conn, "kilo", "default").unwrap());
    }

    #[test]
    fn secret_name_uses_the_pool_key_namespace() {
        assert_eq!(secret_name("groq", "default"), "pool-key:groq:default");
    }

    #[test]
    fn add_is_idempotent_by_platform_and_key_id() {
        let conn = test_conn();
        add(&conn, "groq", "default").unwrap();
        add(&conn, "groq", "default").unwrap();
        assert_eq!(list(&conn, Some("groq")).unwrap().len(), 1);
    }

    #[test]
    fn next_free_key_id_starts_at_default_then_grows() {
        assert_eq!(next_free_key_id(&[]), "default");
    }

    #[test]
    fn next_free_key_id_never_reuses_default_once_taken() {
        // Live-verification finding: adding a second key for a platform
        // that already has one must not overwrite it -- next_free_key_id
        // must pick something other than "default" once one exists.
        let conn = test_conn();
        add(&conn, "groq", "default").unwrap();
        let existing = list(&conn, Some("groq")).unwrap();
        let picked = next_free_key_id(&existing);
        assert_ne!(picked, "default");
        assert_eq!(picked, "key2");
    }

    #[test]
    fn next_free_key_id_skips_ids_already_taken() {
        let conn = test_conn();
        add(&conn, "groq", "default").unwrap();
        add(&conn, "groq", "key2").unwrap();
        add(&conn, "groq", "key3").unwrap();
        let existing = list(&conn, Some("groq")).unwrap();
        let picked = next_free_key_id(&existing);
        assert_eq!(picked, "key4");
        assert!(!existing.iter().any(|k| k.key_id == picked), "must not collide with an existing key_id");
    }

    #[test]
    fn adding_a_second_free_pool_key_grows_the_pool_instead_of_overwriting() {
        // End-to-end regression for the actual bug: two real add-free
        // calls (simulated via add() + next_free_key_id, same shape the
        // handler uses) for the same platform must leave TWO keys, not
        // one overwritten key.
        let conn = test_conn();
        add(&conn, "groq", &next_free_key_id(&list(&conn, Some("groq")).unwrap())).unwrap();
        add(&conn, "groq", &next_free_key_id(&list(&conn, Some("groq")).unwrap())).unwrap();
        let keys = list(&conn, Some("groq")).unwrap();
        assert_eq!(keys.len(), 2, "second add-free call must add a key, not overwrite the first");
    }

    #[test]
    fn list_with_no_platform_returns_every_key() {
        let conn = test_conn();
        add(&conn, "groq", "default").unwrap();
        add(&conn, "nvidia", "default").unwrap();
        assert_eq!(list(&conn, None).unwrap().len(), 2);
    }

    #[test]
    fn disable_and_is_disabled_round_trip() {
        let conn = test_conn();
        add(&conn, "groq", "default").unwrap();
        assert!(!is_disabled(&conn, "groq", "default").unwrap());
        disable(&conn, "groq", "default").unwrap();
        assert!(is_disabled(&conn, "groq", "default").unwrap());
    }

    #[test]
    fn is_disabled_is_false_for_an_unknown_key() {
        let conn = test_conn();
        assert!(!is_disabled(&conn, "no-such-platform", "default").unwrap());
    }
}
