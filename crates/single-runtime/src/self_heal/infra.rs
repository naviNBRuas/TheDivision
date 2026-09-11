//! Infra self-repair — spec §9.1 (Task 20). Stale sockets, zombie task
//! rows, corrupt config, DB integrity, dead agent binaries.

use super::{run_step, Category, PassReport, SelfHealConfig};
use crate::context::Context;
use anyhow::{Context as _, Result};
use rusqlite::Connection;

pub fn run(ctx: &Context, conn: &Connection, cfg: &SelfHealConfig, report: &mut PassReport, allow_db_restore: bool) -> Result<()> {
    run_step(conn, report, Category::Infra, "stale_socket", || stale_socket(ctx));
    run_step(conn, report, Category::Infra, "corrupt_config", || corrupt_config(ctx));
    run_step(conn, report, Category::Infra, "db_integrity", || db_integrity(ctx, conn, allow_db_restore));
    run_step(conn, report, Category::Infra, "db_backup", || db_backup(ctx, conn, cfg));
    run_step(conn, report, Category::Infra, "dead_agent_binaries", || dead_agent_binaries(ctx));
    run_step(conn, report, Category::Infra, "cooldown_probe", || cooldown_probe(conn));
    Ok(())
}

/// `runtime.sock` exists but nothing is actually listening on it — a
/// crash can leave the Unix socket file behind. Detected the same way
/// `server.rs`'s own bind already does (connect, not a PID lookup — this
/// daemon tracks no separate pidfile), so this substep is mostly useful
/// for a mid-run pass catching what the startup check already handled
/// once but something later re-created stale (rare, but cheap to check).
fn stale_socket(ctx: &Context) -> Result<String> {
    let path = ctx.dirs.socket_path();
    if !path.exists() {
        return Ok("no socket file present".to_string());
    }
    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
        return Ok("socket is live".to_string());
    }
    std::fs::remove_file(&path).context("removing stale socket")?;
    Ok(format!("removed stale socket at {}", path.display()))
}

/// Every `*.toml` under `~/.config/single/` is parse-checked. A broken
/// one is restored from its newest `*.bak-*` sibling; with no backup, the
/// file is moved aside (so nothing is silently lost) and a fresh default
/// is regenerated on the next read that owns that file (this substep
/// itself only clears the corrupt file out of the way — it doesn't know
/// each file's own default-writer, which already runs lazily on next
/// load, same as `CoordinatorConfig`/`SelfHealConfig`).
fn corrupt_config(ctx: &Context) -> Result<String> {
    let root = ctx.dirs.root();
    if !root.exists() {
        return Ok("config directory does not exist yet".to_string());
    }
    let mut repaired = Vec::new();
    let mut moved_aside = Vec::new();

    for entry in std::fs::read_dir(root).context("reading config directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else { continue };
        if toml::from_str::<toml::Value>(&contents).is_ok() {
            continue; // parses fine, nothing to do
        }

        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        if let Some(backup) = newest_backup(root, &name)? {
            std::fs::copy(&backup, &path).with_context(|| format!("restoring {} from {}", path.display(), backup.display()))?;
            repaired.push(format!("{name} <- {}", backup.file_name().and_then(|n| n.to_str()).unwrap_or_default()));
        } else {
            let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
            let aside = path.with_extension(format!("toml.corrupt-{timestamp}"));
            std::fs::rename(&path, &aside).with_context(|| format!("moving aside corrupt {}", path.display()))?;
            moved_aside.push(name);
        }
    }

    if repaired.is_empty() && moved_aside.is_empty() {
        return Ok("every *.toml parses cleanly".to_string());
    }
    Ok(format!("restored from backup: [{}]; moved aside (no backup): [{}]", repaired.join(", "), moved_aside.join(", ")))
}

/// The lexicographically-newest `<name>.bak-<timestamp>` sibling — the
/// `%Y%m%dT%H%M%SZ` timestamp format (matching `write_settings_with_backup`
/// and every other `.bak-` writer in this codebase) sorts newest-last as
/// plain strings, so this is a simple max, no date parsing needed.
fn newest_backup(dir: &std::path::Path, original_name: &str) -> Result<Option<std::path::PathBuf>> {
    let prefix = format!("{original_name}.bak-");
    let mut candidates: Vec<std::path::PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix)))
        .collect();
    candidates.sort();
    Ok(candidates.pop())
}

/// `PRAGMA integrity_check` on the open connection. On failure, when
/// `allow_db_restore` is set, restores from the newest `single.db.bak-*`
/// (written by `db_backup` below); with no backup at all, this is a
/// last-resort schema rebuild — logged loudly, since it loses history —
/// but that path only triggers when integrity is ALREADY broken and
/// there's nothing to restore from, so "loses history" beats "stays
/// broken forever".
///
/// `allow_db_restore` must be `false` for every call except the
/// daemon-startup pass (see `run_pass_with_restore`'s doc comment) —
/// swapping the db file out from under other already-open connections is
/// itself a corruption risk, not just a fix for one. When restore isn't
/// allowed, a corruption finding is only reported, never acted on.
fn db_integrity(ctx: &Context, conn: &Connection, allow_db_restore: bool) -> Result<String> {
    let result: String = conn.query_row("PRAGMA integrity_check", (), |r| r.get(0))?;
    if result == "ok" {
        return Ok("integrity_check: ok".to_string());
    }

    let db_path = ctx.dirs.db_path();
    let db_dir = db_path.parent().unwrap_or(&db_path);
    let db_name = db_path.file_name().and_then(|n| n.to_str()).unwrap_or("single.db");

    if !allow_db_restore {
        return Ok(format!(
            "integrity_check failed ({result}); NOT restoring (other connections may be live) — restart the daemon to trigger the startup pass, which will restore from the newest single.db.bak-* automatically"
        ));
    }

    if let Some(backup) = newest_backup(db_dir, db_name)? {
        // Live-verification finding (2026-09-11): this used to be a
        // straight `std::fs::copy(&backup, &db_path)` -- overwriting the
        // live db file's bytes in place. That is not atomic: this
        // daemon's cgroup caps at `MemoryMax=6G` as a runaway backstop and
        // has repeatedly SIGKILLed it under heavy concurrent-agent load
        // (see db_backup's PASSIVE-checkpoint fix for the same trigger).
        // A kill landing mid-copy here left `single.db` itself truncated
        // -- confirmed live via `disk I/O error: Error code 522: Unable
        // to obtain number of requested bytes (file truncated?)` on a
        // plain `single approval resolve`, on the very day this was
        // found, well after 0.15.4's backup-side PASSIVE-checkpoint fix
        // (which only addressed producing a good backup, not restoring
        // one crash-safely). Copy to a temp file in the same directory
        // first, then atomically `rename()` it over `db_path` -- POSIX
        // guarantees a same-filesystem rename is atomic, so a kill
        // mid-copy now only ever leaves a stray temp file, never a
        // half-written live db.
        let tmp_path = db_dir.join(format!("{db_name}.restoring-{}", std::process::id()));
        std::fs::copy(&backup, &tmp_path).context("copying backup to temp file before atomic restore")?;
        let restore_result = std::fs::rename(&tmp_path, &db_path).context("atomically renaming restored db into place");
        if restore_result.is_err() {
            let _ = std::fs::remove_file(&tmp_path);
        }
        restore_result?;
        return Ok(format!("integrity_check failed ({result}); restored from {}", backup.display()));
    }

    Ok(format!("integrity_check failed ({result}); no backup available — schema will re-seed additively on next ensure_schema call (history for this db may be lost)"))
}

/// Periodic `single.db.bak-<timestamp>` writer, gated by
/// `db_backup_interval_secs` — writes a fresh copy only when the newest
/// existing backup is older than the interval (or there isn't one yet),
/// so this doesn't churn a full-db copy on every single pass.
fn db_backup(ctx: &Context, conn: &Connection, cfg: &SelfHealConfig) -> Result<String> {
    let db_path = ctx.dirs.db_path();
    if !db_path.exists() {
        return Ok("no db file yet".to_string());
    }
    let db_dir = db_path.parent().unwrap_or(&db_path);
    let db_name = db_path.file_name().and_then(|n| n.to_str()).unwrap_or("single.db");

    if let Some(existing) = newest_backup(db_dir, db_name)? {
        if let Ok(Ok(age)) = std::fs::metadata(&existing).map(|meta| meta.modified().and_then(|m| m.elapsed().map_err(std::io::Error::other))) {
            if age.as_secs() < cfg.db_backup_interval_secs {
                return Ok(format!("last backup {}s old, interval is {}s -- skipped", age.as_secs(), cfg.db_backup_interval_secs));
            }
        }
    }

    // A live WAL-mode SQLite file shouldn't be `fs::copy`d directly (the
    // WAL/SHM sidecars could be mid-checkpoint) -- force a checkpoint
    // first so the main db file is self-consistent before the copy.
    //
    // Live-verification finding: this used to be `wal_checkpoint(TRUNCATE)`.
    // TRUNCATE mode truncates the WAL file to zero bytes as part of the
    // checkpoint, which removes SQLite's own crash-safety net for the
    // duration of that operation -- if the process is killed mid-TRUNCATE
    // (confirmed live: this daemon's cgroup has a 6G `MemoryMax` backstop
    // and its own journal shows repeated `status=9/KILL` under heavy
    // concurrent-agent load, including the same night `single.db` was
    // twice found corrupt / "file is not a database"), the main db file
    // can be left genuinely malformed, not just stale. PASSIVE mode is
    // crash-safe: it never truncates or blocks, does as much of the
    // checkpoint as it safely can given concurrent readers, and simply
    // leaves later frames in the WAL rather than risking the main file --
    // a slightly-stale backup beats a corrupt one.
    conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE)").ok();
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup_path = db_dir.join(format!("{db_name}.bak-{timestamp}"));
    std::fs::copy(&db_path, &backup_path).context("writing db backup")?;
    Ok(format!("wrote {}", backup_path.display()))
}

pub fn db_backup_dir(ctx: &Context) -> std::path::PathBuf {
    ctx.dirs.db_path().parent().map(|p| p.to_path_buf()).unwrap_or_else(|| ctx.dirs.root().clone())
}

/// Re-detects every registered agent (same `AgentAdapter::discover()`
/// `doctor.rs` already uses) and reports which ones are missing. Actually
/// queuing a reinstall is the `agent` category's job (Task 22) -- this
/// substep is detection/reporting only, so it stays meaningful even with
/// `agent` self-install disabled on a shared box.
fn dead_agent_binaries(ctx: &Context) -> Result<String> {
    let missing: Vec<&str> = ctx
        .registry
        .iter()
        .filter(|a| a.bootstrap_install.is_some())
        // single-pool (E28) never shells a binary -- `discover()`'s
        // default "is `command()` on $PATH" check is meaningless for it
        // and would always report false, since nothing is ever installed
        // at a literal `single-pool` binary name.
        .filter(|a| a.name != "single-pool")
        .filter(|a| {
            single_agent_sdk::adapters::for_agent_with_custom(&a.name, &ctx.dirs.agents_dir(), &ctx.registry)
                .map(|adapter| !adapter.discover().detected)
                .unwrap_or(false)
        })
        .map(|a| a.name.as_str())
        .collect();
    if missing.is_empty() {
        return Ok("every installable agent detected".to_string());
    }
    Ok(format!("not detected: {}", missing.join(", ")))
}

/// The §6.2 probe job: re-validates `Heuristic`-provenance cooldowns past
/// half elapsed with >60s remaining, via each provider's cheap
/// `validate_url` (not a full chat completion). Budgeted to 3 per pass
/// (spec default). A successful probe clears the bench early; a failed
/// one is left alone for the next pass to retry -- this is a simplified
/// take on the spec's "push the next probe out, 2m doubling, cap 15m"
/// schedule, which would need a persisted per-key next-probe-at column
/// this iteration doesn't add; documented seam for that refinement.
fn cooldown_probe(conn: &Connection) -> Result<String> {
    let now = crate::pool::ledger::now_ms();
    let candidates = crate::pool::cooldown::heuristic_probe_candidates(conn, now)?;
    let budget = 3;
    let mut cleared = 0;
    let mut left = 0;

    for (platform, _model, key_id) in candidates.into_iter().take(budget) {
        let Some(provider) = single_core::free_pool::by_id(&platform) else { continue };
        let Some(validate_path) = provider.quirks.validate_url else {
            left += 1;
            continue; // no cheap probe endpoint for this provider -- can't tell without spending a real request.
        };
        use single_core::secrets::{SecretStore, SecretTool};
        let secret_name = single_core::pool_keys::secret_name(&platform, &key_id);
        let Ok(Some(key)) = SecretStore::get(&SecretTool, &secret_name) else {
            left += 1;
            continue;
        };
        let url = format!("{}{}", provider.base_url.trim_end_matches('/'), validate_path);
        let ok = reqwest::blocking::Client::new()
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .bearer_auth(&key)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false);

        if ok {
            crate::pool::cooldown::clear(conn, Some(&key_id))?;
            cleared += 1;
        } else {
            left += 1;
        }
    }
    Ok(format!("{cleared} cooldown(s) cleared early, {left} still benched"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_heal::{ensure_schema, run_pass, Categories};

    fn test_ctx(root: &std::path::Path) -> Context {
        let dirs = single_core::SingleDirs::from_root(root.to_path_buf());
        dirs.ensure_created().unwrap();
        Context { dirs, resolved: single_core::ResolvedConfig::default(), registry: single_core::builtin_registry() }
    }

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        crate::task::ensure_schema(&conn).unwrap();
        crate::coordinator::ensure_coordinator_schema(&conn).unwrap();
        crate::pool::ensure_pool_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn stale_socket_removed_when_no_live_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        // A plain regular file standing in for a socket nothing listens
        // on -- connecting to it fails exactly like a stale socket would.
        std::fs::write(ctx.dirs.socket_path(), b"").unwrap();
        let detail = stale_socket(&ctx).unwrap();
        assert!(detail.contains("removed"), "{detail}");
        assert!(!ctx.dirs.socket_path().exists());
    }

    #[test]
    fn corrupt_toml_restored_from_newest_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let target = ctx.dirs.root().join("routing.toml");
        std::fs::write(&target, "not valid toml {{{").unwrap();
        std::fs::write(ctx.dirs.root().join("routing.toml.bak-20260101T000000Z"), "max_parallel = 1\n").unwrap();
        std::fs::write(ctx.dirs.root().join("routing.toml.bak-20260201T000000Z"), "max_parallel = 2\n").unwrap();

        let detail = corrupt_config(&ctx).unwrap();
        assert!(detail.contains("restored from backup"), "{detail}");
        let restored = std::fs::read_to_string(&target).unwrap();
        assert!(restored.contains("max_parallel = 2"), "should restore from the NEWEST backup, got: {restored}");
    }

    #[test]
    fn corrupt_toml_with_no_backup_moved_aside_and_regenerated() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let target = ctx.dirs.root().join("pool.toml");
        std::fs::write(&target, "not valid toml {{{").unwrap();

        let detail = corrupt_config(&ctx).unwrap();
        assert!(detail.contains("moved aside"), "{detail}");
        assert!(!target.exists(), "the corrupt file must not be left in place");
        let moved: Vec<_> = std::fs::read_dir(ctx.dirs.root()).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().starts_with("pool.toml.corrupt-")).collect();
        assert_eq!(moved.len(), 1);
    }

    #[test]
    fn db_integrity_check_failure_restores_from_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let conn = Connection::open(ctx.dirs.db_path()).unwrap();
        conn.execute("CREATE TABLE t (x INTEGER)", ()).unwrap();
        conn.execute("INSERT INTO t VALUES (1)", ()).unwrap();
        drop(conn);

        // A known-good backup, newer name wins by construction (only one here).
        let db_path = ctx.dirs.db_path();
        let backup_path = db_path.with_file_name(format!("{}.bak-20260101T000000Z", db_path.file_name().unwrap().to_str().unwrap()));
        std::fs::copy(&db_path, &backup_path).unwrap();

        // Corrupt only past the first 4096-byte page (the header plus the
        // schema/`sqlite_master` root page, which `Connection::open`
        // reads eagerly in this rusqlite/SQLite build) — leaves `open`
        // able to succeed while the table's own data pages are damaged,
        // which `PRAGMA integrity_check` still catches.
        let mut bytes = std::fs::read(&db_path).unwrap();
        let corrupt_from = bytes.len().min(4096);
        for b in bytes.iter_mut().skip(corrupt_from) {
            *b ^= 0xff;
        }
        std::fs::write(&db_path, &bytes).unwrap();

        let conn = Connection::open(&db_path).unwrap();
        let detail = db_integrity(&ctx, &conn, true).unwrap();
        assert!(detail.contains("restored"), "{detail}");

        drop(conn);
        let restored_conn = Connection::open(&db_path).unwrap();
        let count: i64 = restored_conn.query_row("SELECT COUNT(*) FROM t", (), |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "the restored db should have the backup's data back");
    }

    #[test]
    fn db_integrity_check_failure_only_reports_when_restore_not_allowed() {
        // Live-verification regression: a periodic self-heal tick (or
        // `doctor --fix`) runs while other connections to the same db
        // file may be open elsewhere in the daemon -- swapping the file
        // out from under them is itself a corruption risk. Only the
        // daemon-startup pass may restore; everything else must leave the
        // file untouched and just report.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let conn = Connection::open(ctx.dirs.db_path()).unwrap();
        conn.execute("CREATE TABLE t (x INTEGER)", ()).unwrap();
        conn.execute("INSERT INTO t VALUES (1)", ()).unwrap();
        drop(conn);

        let db_path = ctx.dirs.db_path();
        let backup_path = db_path.with_file_name(format!("{}.bak-20260101T000000Z", db_path.file_name().unwrap().to_str().unwrap()));
        std::fs::copy(&db_path, &backup_path).unwrap();

        let mut bytes = std::fs::read(&db_path).unwrap();
        let corrupt_from = bytes.len().min(4096);
        for b in bytes.iter_mut().skip(corrupt_from) {
            *b ^= 0xff;
        }
        std::fs::write(&db_path, &bytes).unwrap();
        let bytes_before = std::fs::read(&db_path).unwrap();

        let conn = Connection::open(&db_path).unwrap();
        let detail = db_integrity(&ctx, &conn, false).unwrap();
        assert!(detail.contains("NOT restoring"), "{detail}");
        drop(conn);

        let bytes_after = std::fs::read(&db_path).unwrap();
        assert_eq!(bytes_before, bytes_after, "the file must not be touched when restore isn't allowed");
    }

    #[test]
    fn disabled_category_skips_every_infra_substep() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let conn = test_conn();
        crate::self_heal::SelfHealConfig::disable_category(&ctx.dirs, Category::Infra).unwrap();
        let report = run_pass(&ctx, &conn, None).unwrap();
        assert!(report.actions.iter().all(|a| a.category != Category::Infra));
    }

    #[test]
    fn full_infra_pass_runs_every_substep_and_logs_events() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let conn = test_conn();
        let cfg = crate::self_heal::SelfHealConfig { categories: Categories::default(), ..crate::self_heal::SelfHealConfig::load(&ctx.dirs) };
        let mut report = PassReport::default();
        run(&ctx, &conn, &cfg, &mut report, true).unwrap();
        assert_eq!(report.actions.len(), 6, "expected all 6 infra substeps to run: {report:?}");
        assert!(report.actions.iter().all(|a| a.ok), "a clean tempdir/fresh db should have nothing to repair: {report:?}");

        let events = crate::self_heal::recent_events(&conn, 20).unwrap();
        assert_eq!(events.len(), 6);
    }
}
