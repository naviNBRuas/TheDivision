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
    run_step(conn, report, Category::Infra, "stale_worktrees", || stale_worktrees(ctx, conn, cfg));
    run_step(conn, report, Category::Infra, "dead_agent_binaries", || dead_agent_binaries(ctx));
    run_step(conn, report, Category::Infra, "cooldown_probe", || cooldown_probe(conn));
    run_step(conn, report, Category::Infra, "agent_recovery", || agent_recovery(ctx, conn));
    Ok(())
}

/// Most agents probed in one pass (each probe is a real call and can take a minute).
const RECOVERY_PROBES_PER_PASS: usize = 2;

/// Brings an agent back by itself once its quota reset time (or backoff) has passed: a tiny real call
/// confirms it works (then it is simply routable again, strikes cleared) or benches it again with a longer
/// wait. This is what makes grok, agy, codex, claude and the rest rejoin routing without anyone noticing
/// they left.
fn agent_recovery(ctx: &Context, conn: &Connection) -> Result<String> {
    let due = crate::agent_cooldown::needing_verification(conn, chrono::Utc::now())?;
    if due.is_empty() {
        return Ok("no agent is waiting to be re-verified".into());
    }
    let mut back = Vec::new();
    let mut still = Vec::new();
    for name in due.iter().take(RECOVERY_PROBES_PER_PASS) {
        let row = crate::agent_auth::probe_one(ctx, conn, name, false);
        match row.category.as_str() {
            "authed" | "no_auth_needed" => back.push(name.clone()),
            // exhausted / needs_login were re-benched by the probe itself; anything else is benched here
            "exhausted" | "needs_login" => still.push(name.clone()),
            other => {
                crate::agent_cooldown::rebench(conn, name, &format!("recovery probe: {other}: {}", row.evidence));
                still.push(name.clone());
            }
        }
    }
    Ok(format!("back in routing: [{}]; still unavailable: [{}]; waiting for a later pass: {}", back.join(", "), still.join(", "), due.len().saturating_sub(RECOVERY_PROBES_PER_PASS)))
}

/// Removes `state/worktrees/task-<id>` worktrees whose task is no longer
/// running and has been idle for at least `worktree_retention_hours`,
/// provided the worktree has no uncommitted changes. Each one is a full
/// checkout, so without this they pile up without bound. Scans the
/// directory rather than `tasks.worktree_path`, which is NULL for most
/// rows. Dirty worktrees, ones git no longer recognises, and the
/// `single/task-*` branches are all left alone.
fn stale_worktrees(ctx: &Context, conn: &Connection, cfg: &SelfHealConfig) -> Result<String> {
    if cfg.worktree_retention_hours == 0 {
        return Ok("disabled (worktree_retention_hours = 0)".into());
    }
    let root = ctx.dirs.state_dir().join("worktrees");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok("no worktrees dir -- nothing to sweep".into());
    };
    let now = chrono::Utc::now();
    let retention = std::time::Duration::from_secs(cfg.worktree_retention_hours as u64 * 3600);
    let (mut removed, mut dirty, mut unmanaged, mut branches) = (0usize, 0usize, 0usize, 0usize);
    for entry in entries.filter_map(|e| e.ok()) {
        let wt = entry.path();
        let name = wt.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        let parsed_at = |s: &str| s.parse::<chrono::DateTime<chrono::Utc>>().ok().map(std::time::SystemTime::from);
        let last_active = if let Some(goal_id) = name.strip_prefix("goal-") {
            // A goal's shared worktree (`scheduler::goal_workdir`) lives as long as the goal does.
            match crate::coordinator::goal::get(conn, goal_id).ok().flatten() {
                Some(g) if !matches!(g.status, crate::coordinator::graph::GoalStatus::Done | crate::coordinator::graph::GoalStatus::Failed | crate::coordinator::graph::GoalStatus::Cancelled) => continue,
                Some(g) => parsed_at(&g.updated_at),
                None => None,
            }
        } else if let Some(id) = name.strip_prefix("task-").and_then(|n| n.parse::<i64>().ok()) {
            match crate::task::get(conn, id).ok().flatten() {
                Some(t) if matches!(t.status, divisi_protocol::TaskStatus::Created | divisi_protocol::TaskStatus::Running) => continue,
                Some(t) => parsed_at(&t.updated_at),
                None => None,
            }
        } else {
            continue;
        }
        .or_else(|| entry.metadata().and_then(|m| m.modified()).ok());
        let idle = last_active.and_then(|t| std::time::SystemTime::from(now).duration_since(t).ok()).unwrap_or_default();
        if idle < retention {
            continue;
        }
        let git = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(&wt).args(args).output();
        let is_clean = match git(&["status", "--porcelain"]) {
            Ok(o) if o.status.success() => o.stdout.is_empty(),
            _ => {
                unmanaged += 1;
                continue;
            }
        };
        if !is_clean {
            dirty += 1;
            continue;
        }
        let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .ok()
            .filter(|o| o.status.success())
            .map(|o| std::path::PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
        let Some(repo) = common.as_deref().and_then(|c| c.parent()) else { continue };
        let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"])
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|b| b.starts_with("single/task-") || b.starts_with("divisi/goal-"));
        if divisi_core::worktree::remove(repo, &wt, false).is_ok() {
            removed += 1;
            // The task branch goes too once its work is in the checked-out branch; `-d` refuses an
            // unmerged one, so unreviewed work is never lost. 170 of these had piled up by 2026-09-23.
            if let Some(b) = branch {
                let ok = std::process::Command::new("git").arg("-C").arg(repo).args(["branch", "-d", &b]).output();
                if ok.is_ok_and(|o| o.status.success()) {
                    branches += 1;
                }
            }
        }
    }
    Ok(format!("{removed} removed ({branches} merged branches deleted), {dirty} kept (uncommitted changes), {unmanaged} not recognised by git (left alone)"))
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

/// Every `*.toml` under `~/.config/divisi/` is parse-checked. A broken
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

/// `PRAGMA quick_check` on a db file opened read-only and immutable, so
/// checking a backup never creates sidecars or touches its bytes.
fn is_sound_db(path: &std::path::Path) -> bool {
    use rusqlite::OpenFlags;
    let uri = format!("file:{}?immutable=1", path.display());
    Connection::open_with_flags(uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)
        .and_then(|c| c.query_row("PRAGMA quick_check", (), |r| r.get::<_, String>(0)))
        .is_ok_and(|r| r == "ok")
}

/// The newest `<name>.bak-<timestamp>` that itself passes `quick_check`.
fn newest_sound_backup(dir: &std::path::Path, original_name: &str) -> Result<Option<std::path::PathBuf>> {
    let prefix = format!("{original_name}.bak-");
    let mut candidates: Vec<std::path::PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix)))
        .collect();
    candidates.sort();
    Ok(candidates.into_iter().rev().find(|p| is_sound_db(p)))
}

/// `PRAGMA integrity_check` on the open connection. On failure, when
/// `allow_db_restore` is set, restores from the newest `divisi.db.bak-*`
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
    let db_name = db_path.file_name().and_then(|n| n.to_str()).unwrap_or("divisi.db");

    if !allow_db_restore {
        return Ok(format!(
            "integrity_check failed ({result}); NOT restoring (other connections may be live) — restart the daemon to trigger the startup pass, which will restore from the newest divisi.db.bak-* automatically"
        ));
    }

    // Live-verification finding (2026-09-23): the restore used to rename
    // the newest backup over `divisi.db` whether or not that backup was
    // itself sound, and left the live db's `-wal`/`-shm` sidecars in
    // place -- SQLite then replays that foreign WAL onto the restored
    // file, which is corruption by construction. Every hourly backup had
    // been copying the same damaged `coordinator_events` pages for a day,
    // so "newest" was never good enough. Now: the newest backup that
    // itself passes `quick_check`, written through SQLite's own backup
    // API into the open connection, so the pager and WAL stay coherent.
    if let Some(backup) = newest_sound_backup(db_dir, db_name)? {
        let mut target = Connection::open(&db_path).context("opening db for restore")?;
        target
            .restore(rusqlite::DatabaseName::Main, &backup, None::<fn(rusqlite::backup::Progress)>)
            .with_context(|| format!("restoring {} from {}", db_path.display(), backup.display()))?;
        return Ok(format!("integrity_check failed ({result}); restored from {}", backup.display()));
    }

    Ok(format!("integrity_check failed ({result}); no sound backup available — stop the daemon and rebuild with `sqlite3 divisi.db .recover`"))
}

/// Live-verification finding (2026-09-14): the newest-backup interval
/// gate below (`db_backup_interval_secs`, default 3600s) turned out not
/// to reliably prevent frequent writes — this daemon restarts and
/// receives self-heal triggers (`handlers.rs`'s per-request pass, on top
/// of the `self_heal_interval_secs` ticker) often enough that in
/// practice backups landed roughly every 5 minutes for days, silently
/// accumulating 1203 files / 14GB and eventually filling the disk to the
/// point SQLite couldn't even open the live db in WAL mode anymore
/// ("disk I/O error: Error code 522: Unable to obtain number of
/// requested bytes (file truncated?)"). Rather than fully chase the
/// interval gate's race (a plausible contributor: `elapsed()` on a
/// backup's mtime returns `Err` on any clock non-monotonicity, which the
/// gate below silently treats as "not recent enough" and writes anyway),
/// this count-based retention is a hard backstop: no matter how often
/// the gate above fires, disk usage from this backup family stays
/// bounded.
const DB_BACKUP_RETENTION_COUNT: usize = 20;

/// Deletes every `<db_name>.bak-<timestamp>` under `db_dir` beyond the
/// newest [`DB_BACKUP_RETENTION_COUNT`]. Best-effort: a failed delete is
/// skipped rather than aborting the rest (a stray extra backup is far
/// cheaper than a self-heal pass erroring out over housekeeping).
fn prune_old_backups(db_dir: &std::path::Path, db_name: &str) -> Result<usize> {
    let prefix = format!("{db_name}.bak-");
    let mut candidates: Vec<std::path::PathBuf> = std::fs::read_dir(db_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix)))
        .collect();
    // Same newest-last lexicographic sort as `newest_backup` — the
    // `%Y%m%dT%H%M%SZ` timestamp format sorts correctly as plain strings.
    candidates.sort();
    let excess = candidates.len().saturating_sub(DB_BACKUP_RETENTION_COUNT);
    let mut removed = 0;
    for old in &candidates[..excess] {
        if std::fs::remove_file(old).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Periodic `divisi.db.bak-<timestamp>` writer, gated by
/// `db_backup_interval_secs` — writes a fresh copy only when the newest
/// existing backup is older than the interval (or there isn't one yet),
/// so this doesn't churn a full-db copy on every single pass. Also prunes
/// down to [`DB_BACKUP_RETENTION_COUNT`] on every pass regardless of
/// whether this call wrote a new one, so retention self-heals even if the
/// interval gate above has already let extras accumulate.
fn db_backup(ctx: &Context, conn: &Connection, cfg: &SelfHealConfig) -> Result<String> {
    let db_path = ctx.dirs.db_path();
    if !db_path.exists() {
        return Ok("no db file yet".to_string());
    }
    let db_dir = db_path.parent().unwrap_or(&db_path);
    let db_name = db_path.file_name().and_then(|n| n.to_str()).unwrap_or("divisi.db");
    let pruned = prune_old_backups(db_dir, db_name).unwrap_or(0);

    if let Some(existing) = newest_backup(db_dir, db_name)? {
        match std::fs::metadata(&existing).and_then(|meta| meta.modified()).and_then(|m| m.elapsed().map_err(std::io::Error::other)) {
            Ok(age) if age.as_secs() < cfg.db_backup_interval_secs => {
                return Ok(format!(
                    "last backup {}s old, interval is {}s -- skipped ({pruned} pruned)",
                    age.as_secs(),
                    cfg.db_backup_interval_secs
                ));
            }
            // A backup exists but its age can't be determined (clock
            // non-monotonicity, filesystem quirk) -- treat as "recent
            // enough" rather than writing unconditionally, closing the
            // likely source of the every-~5-minutes cadence above.
            Err(_) => return Ok(format!("could not determine last backup's age -- assuming recent, skipped ({pruned} pruned)")),
            Ok(_) => {}
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
    // concurrent-agent load, including the same night `divisi.db` was
    // twice found corrupt / "file is not a database"), the main db file
    // can be left genuinely malformed, not just stale. PASSIVE mode is
    // crash-safe: it never truncates or blocks, does as much of the
    // checkpoint as it safely can given concurrent readers, and simply
    // leaves later frames in the WAL rather than risking the main file --
    // a slightly-stale backup beats a corrupt one.
    //
    // Live-verification finding (2026-09-23): a plain `fs::copy` of the
    // main file, even after a PASSIVE checkpoint, is not a consistent
    // snapshot (frames still in the WAL, a checkpoint landing mid-copy),
    // and nothing checked the copy -- a day of hourly backups all carried
    // the live db's damaged pages and rotated every good one out.
    // `VACUUM INTO` writes a transactionally consistent copy through
    // SQLite itself; it is only kept if it passes `quick_check`, so
    // retention can never fill up with bad snapshots.
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup_path = db_dir.join(format!("{db_name}.bak-{timestamp}"));
    let staging = db_dir.join(format!("{db_name}.backing-up-{}", std::process::id()));
    let _ = std::fs::remove_file(&staging);
    let written = conn
        .execute("VACUUM INTO ?1", [staging.to_string_lossy().as_ref()])
        .context("writing db backup with VACUUM INTO");
    if let Err(e) = written {
        let _ = std::fs::remove_file(&staging);
        return Err(e);
    }
    if !is_sound_db(&staging) {
        let _ = std::fs::remove_file(&staging);
        anyhow::bail!("fresh backup failed quick_check -- live db is damaged, kept the existing backups ({pruned} pruned)");
    }
    std::fs::rename(&staging, &backup_path).context("moving db backup into place")?;
    let pruned = pruned + prune_old_backups(db_dir, db_name).unwrap_or(0);
    Ok(format!("wrote {} ({pruned} pruned)", backup_path.display()))
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
            divisi_agent_sdk::adapters::for_agent_with_custom(&a.name, &ctx.dirs.agents_dir(), &ctx.registry)
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
        let Some(provider) = divisi_core::free_pool::by_id(&platform) else { continue };
        let Some(validate_path) = provider.quirks.validate_url else {
            left += 1;
            continue; // no cheap probe endpoint for this provider -- can't tell without spending a real request.
        };
        use divisi_core::secrets::{SecretStore, SecretTool};
        let secret_name = divisi_core::pool_keys::secret_name(&platform, &key_id);
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
        let dirs = divisi_core::DivisiDirs::from_root(root.to_path_buf());
        dirs.ensure_created().unwrap();
        Context { dirs, resolved: divisi_core::ResolvedConfig::default(), registry: divisi_core::builtin_registry() }
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
    fn an_agent_whose_cooldown_passed_is_probed_and_rebenched_if_it_still_cannot_work() {
        let conn = Connection::open_in_memory().unwrap();
        let ctx = test_ctx(&tempfile::tempdir().unwrap().keep());
        // an agent name the registry has no adapter for: the probe cannot succeed, so it must go back on the bench
        crate::agent_cooldown::record(&conn, "no-such-agent", chrono::Utc::now() - chrono::Duration::minutes(1), "usage limit").unwrap();
        let detail = agent_recovery(&ctx, &conn).unwrap();
        assert!(detail.contains("still unavailable: [no-such-agent]"), "{detail}");
        assert!(crate::agent_cooldown::active(&conn, chrono::Utc::now()).unwrap().contains_key("no-such-agent"));
        // nothing due now
        assert!(agent_recovery(&ctx, &conn).unwrap().contains("no agent is waiting"));
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
    fn db_integrity_restore_skips_a_damaged_newer_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let db_path = ctx.dirs.db_path();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("CREATE TABLE t (x INTEGER)", ()).unwrap();
        for i in 0..2000 {
            conn.execute("INSERT INTO t VALUES (?1)", [i]).unwrap();
        }
        drop(conn);
        let name = db_path.file_name().unwrap().to_str().unwrap().to_string();
        std::fs::copy(&db_path, db_path.with_file_name(format!("{name}.bak-20260101T000000Z"))).unwrap();

        let mut bytes = std::fs::read(&db_path).unwrap();
        for b in bytes.iter_mut().skip(4096) {
            *b ^= 0xff;
        }
        std::fs::write(&db_path, &bytes).unwrap();
        // The newest backup carries the same damage -- the live incident.
        std::fs::write(db_path.with_file_name(format!("{name}.bak-20260102T000000Z")), &bytes).unwrap();

        let conn = Connection::open(&db_path).unwrap();
        let detail = db_integrity(&ctx, &conn, true).unwrap();
        assert!(detail.contains("20260101T000000Z"), "{detail}");
        drop(conn);

        let restored = Connection::open(&db_path).unwrap();
        let count: i64 = restored.query_row("SELECT COUNT(*) FROM t", (), |r| r.get(0)).unwrap();
        assert_eq!(count, 2000);
    }

    #[test]
    fn db_backup_refuses_to_keep_a_damaged_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let db_path = ctx.dirs.db_path();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("CREATE TABLE t (x TEXT)", ()).unwrap();
        for i in 0..2000 {
            conn.execute("INSERT INTO t VALUES (?1)", [format!("row {i}")]).unwrap();
        }
        drop(conn);
        let mut bytes = std::fs::read(&db_path).unwrap();
        for b in bytes.iter_mut().skip(4096) {
            *b ^= 0xff;
        }
        std::fs::write(&db_path, &bytes).unwrap();

        let conn = Connection::open(&db_path).unwrap();
        let cfg = crate::self_heal::SelfHealConfig { db_backup_interval_secs: 0, ..crate::self_heal::SelfHealConfig::load(&ctx.dirs) };
        assert!(db_backup(&ctx, &conn, &cfg).is_err());
        let backups = std::fs::read_dir(db_path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".bak-") || e.file_name().to_string_lossy().contains(".backing-up-"))
            .count();
        assert_eq!(backups, 0, "a damaged snapshot must never land in retention");
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
        assert_eq!(report.actions.len(), 8, "expected all 8 infra substeps to run: {report:?}");
        assert!(report.actions.iter().all(|a| a.ok), "a clean tempdir/fresh db should have nothing to repair: {report:?}");

        let events = crate::self_heal::recent_events(&conn, 20).unwrap();
        assert_eq!(events.len(), 8);
    }

    #[test]
    fn stale_worktrees_removes_clean_old_ones_and_keeps_dirty_ones() {
        let git = |dir: &std::path::Path, args: &[&str]| {
            let o = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(&tmp.path().join("cfg"));
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "init"]);
        let conn = test_conn();
        let root = ctx.dirs.state_dir().join("worktrees");
        let mut paths = Vec::new();
        for (id, name) in [(1, "clean"), (2, "dirty"), (3, "ahead")] {
            let wt = root.join(format!("task-{id}"));
            divisi_core::worktree::add(&repo, &wt, &format!("single/task-{name}")).unwrap();
            if name == "dirty" {
                std::fs::write(wt.join("scratch.txt"), "uncommitted").unwrap();
            }
            if name == "ahead" {
                git(&wt, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "unmerged work"]);
            }
            paths.push(wt);
        }
        let mut cfg = SelfHealConfig::default();
        // Freshly created worktrees are inside the default 24h window.
        let msg = stale_worktrees(&ctx, &conn, &cfg).unwrap();
        assert!(msg.starts_with("0 removed (0 merged branches deleted), 0 kept"), "{msg}");
        assert!(paths.iter().all(|p| p.exists()));

        cfg.worktree_retention_hours = 1;
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
        for p in &paths {
            std::fs::File::open(p).unwrap().set_modified(old).unwrap();
        }
        let msg = stale_worktrees(&ctx, &conn, &cfg).unwrap();
        assert!(msg.starts_with("2 removed (1 merged branches deleted), 1 kept"), "{msg}");
        assert!(!paths[0].exists(), "clean old worktree should be gone");
        assert!(paths[1].exists(), "dirty worktree must survive");
        let has = |b: &str| std::process::Command::new("git").arg("-C").arg(&repo).args(["rev-parse", "--verify", "-q", b]).output().unwrap().status.success();
        assert!(!has("single/task-clean"), "a merged task branch goes with its worktree");
        assert!(has("single/task-ahead"), "an unmerged task branch is never deleted");
    }

    #[test]
    fn db_backup_prunes_down_to_retention_count_regardless_of_interval_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let conn = Connection::open(ctx.dirs.db_path()).unwrap();
        conn.execute("CREATE TABLE t (x INTEGER)", ()).unwrap();
        drop(conn);
        let conn = Connection::open(ctx.dirs.db_path()).unwrap();

        let db_path = ctx.dirs.db_path();
        let db_dir = db_path.parent().unwrap();
        let db_name = db_path.file_name().unwrap().to_str().unwrap();

        // Seed far more backups than the retention limit, all old enough
        // that the interval gate would (correctly) want to write a fresh
        // one too -- regression coverage for the live incident: 1203
        // backups accumulated with no pruning at all.
        for i in 0..(DB_BACKUP_RETENTION_COUNT + 15) {
            std::fs::copy(&db_path, db_dir.join(format!("{db_name}.bak-202601{i:02}T000000Z"))).unwrap();
        }

        let cfg = crate::self_heal::SelfHealConfig { db_backup_interval_secs: 0, ..crate::self_heal::SelfHealConfig::load(&ctx.dirs) };
        let detail = db_backup(&ctx, &conn, &cfg).unwrap();
        assert!(detail.contains("wrote"), "{detail}");

        let remaining: Vec<_> = std::fs::read_dir(db_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(&format!("{db_name}.bak-")))
            .collect();
        assert_eq!(remaining.len(), DB_BACKUP_RETENTION_COUNT, "backups beyond the retention limit must be pruned: {remaining:?}");
    }
}
