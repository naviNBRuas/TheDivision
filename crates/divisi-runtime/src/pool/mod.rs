//! E28 free-provider pool engine — quota ledger, cooldowns, pool inference
//! (spec §6). Lives in `divisi-runtime` (not `divisi-core`) because it owns
//! runtime-only state (SQLite tables in the shared daemon db, in-memory
//! leases) the way `coordinator/` does, unlike `divisi-core::free_pool`'s
//! pure vendored data.

pub mod backoff;
pub mod bandit;
pub mod client;
pub mod cooldown;
pub mod degrade;
pub mod handoff;
pub mod ledger;
pub mod models;
pub mod pools;

use anyhow::Result;
use rusqlite::Connection;

/// Creates every E28 pool table up front (Task 4's "schema first" call,
/// mirroring E27 Task 1's precedent) even though `cooldown`/`bandit` only
/// start writing to their tables in later phases — one migration point
/// avoids a second `ensure_*_schema` call site per phase.
pub fn ensure_pool_schema(conn: &Connection) -> Result<()> {
    divisi_core::pool_keys::ensure_schema(conn)?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS pool_key_notes (platform TEXT NOT NULL, key_id TEXT NOT NULL, note TEXT NOT NULL, at TEXT NOT NULL, PRIMARY KEY (platform, key_id))",
        (),
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS pool_usage (
            platform TEXT NOT NULL,
            model TEXT NOT NULL,
            key_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            amount INTEGER NOT NULL,
            at_ms INTEGER NOT NULL
        )",
        (),
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_pool_usage_lookup ON pool_usage (platform, model, key_id, at_ms)",
        (),
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS pool_leases (
            lease_id TEXT PRIMARY KEY,
            platform TEXT NOT NULL,
            model TEXT NOT NULL,
            key_id TEXT NOT NULL,
            est_tokens INTEGER NOT NULL,
            acquired_at_ms INTEGER NOT NULL
        )",
        (),
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS pool_cooldowns (
            platform TEXT NOT NULL,
            model TEXT NOT NULL,
            key_id TEXT NOT NULL,
            until_ms INTEGER NOT NULL,
            provenance TEXT NOT NULL,
            hits INTEGER NOT NULL DEFAULT 0,
            hits_window_start_ms INTEGER NOT NULL DEFAULT 0,
            bench_start_ms INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (platform, model, key_id)
        )",
        (),
    )?;

    // single-row config table — deliberately per-db (not a process-global
    // static) so operator overrides like the cooldown ceiling survive a
    // restart and, just as importantly, so unit tests using separate
    // in-memory connections never race each other over shared state.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS pool_config (
            id INTEGER PRIMARY KEY CHECK (id = 0),
            cooldown_ceiling_ms INTEGER NOT NULL DEFAULT 86400000
        )",
        (),
    )?;
    conn.execute("INSERT OR IGNORE INTO pool_config (id, cooldown_ceiling_ms) VALUES (0, 86400000)", ())?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS pool_outcomes (
            platform TEXT NOT NULL,
            model TEXT NOT NULL,
            key_id TEXT NOT NULL,
            ok INTEGER NOT NULL,
            latency_ms INTEGER NOT NULL,
            tokens INTEGER NOT NULL,
            at_ms INTEGER NOT NULL
        )",
        (),
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_pool_outcomes_lookup ON pool_outcomes (platform, model, key_id, at_ms)",
        (),
    )?;

    // self_heal_events is created in Phase 8 — signature kept stable now so
    // callers (`server.rs` startup) don't need a second migration wiring
    // point later; the table itself is added when self_heal.rs lands.

    Ok(())
}
