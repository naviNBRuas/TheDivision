//! Self-heal / self-fix — spec §9 (E28 Part E). A **pass** runs on daemon
//! start, every `self_heal_interval_secs` (default 300), and on `single
//! doctor --fix`. Every autonomous mutation writes a `self_heal_events`
//! row and is gated by a per-category toggle in
//! `~/.config/divisi/self_heal.toml` (all `true` by default).
//!
//! Three categories, each independently toggleable and each wrapped in
//! `catch_unwind` per sub-step (spec: "one failure can't wedge the rest"):
//! `infra` (Task 20), `coordinator` (Task 21), `agent` (Task 22).

use crate::context::Context;
use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use divisi_core::DivisiDirs;
use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Infra,
    Coordinator,
    Agent,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Infra => "infra",
            Category::Coordinator => "coordinator",
            Category::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Categories {
    pub infra: bool,
    pub coordinator: bool,
    pub agent: bool,
}

impl Default for Categories {
    fn default() -> Self {
        // `agent` defaults OFF, unlike the other two — confirmed live
        // (not just per the spec's own safety note) that unattended real
        // installs (`bootstrap::run_one(.., dry_run: false)`, no `--yes`
        // confirmation from anyone) run on a real production daemon and
        // wedged it: `DoctorGuard` held indefinitely with no subprocess
        // visible in `ps`, everything else on the box otherwise healthy.
        // Root cause not fully isolated before this default was flipped
        // (see `agent::install_missing_agents`'s doc comment for the
        // follow-up); until it is, this category needs an explicit
        // opt-in (`self_heal.toml`'s `[categories] agent = true`), not
        // opt-out.
        Categories { infra: true, coordinator: true, agent: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SelfHealConfig {
    pub categories: Categories,
    pub self_heal_interval_secs: u64,
    pub db_backup_interval_secs: u64,
    /// Coordinator category (Task 21): re-evaluate a goal `Blocked` for
    /// longer than this many minutes with a capacity/supervisor reason.
    pub blocked_reeval_minutes: u32,
    /// Coordinator category: bounds how many times one goal gets
    /// auto-reevaluated, so a goal that keeps re-blocking doesn't spin
    /// forever.
    pub max_auto_reevals_per_goal: u32,
    /// Agent category (Task 22): a pool provider key failing validation
    /// for longer than this is auto-disabled.
    pub provider_key_grace_hours: u32,
    /// Infra category: a finished task's git worktree (a full checkout,
    /// hundreds of MB each) is removed once its task has been terminal for
    /// this many hours and the worktree has no uncommitted changes. The
    /// `single/task-*` branch is kept. `0` disables the sweep.
    pub worktree_retention_hours: u32,
}

impl Default for SelfHealConfig {
    fn default() -> Self {
        SelfHealConfig {
            categories: Categories::default(),
            self_heal_interval_secs: 300,
            db_backup_interval_secs: 3600,
            blocked_reeval_minutes: 30,
            max_auto_reevals_per_goal: 3,
            provider_key_grace_hours: 24,
            worktree_retention_hours: 24,
        }
    }
}

impl SelfHealConfig {
    /// Lazy-write-on-first-use, same discipline as `coordinator.toml`
    /// (`CoordinatorConfig::load`) — a malformed file falls back to
    /// defaults rather than failing every pass.
    pub fn load(dirs: &DivisiDirs) -> Self {
        let path = self_heal_file(dirs);
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => {
                let cfg = Self::default();
                if let Ok(s) = toml::to_string_pretty(&cfg) {
                    let _ = std::fs::create_dir_all(dirs.root());
                    let _ = std::fs::write(&path, s);
                }
                cfg
            }
        }
    }

    /// `single self-heal disable <category>` — persists the toggle.
    pub fn disable_category(dirs: &DivisiDirs, category: Category) -> Result<()> {
        let mut cfg = Self::load(dirs);
        match category {
            Category::Infra => cfg.categories.infra = false,
            Category::Coordinator => cfg.categories.coordinator = false,
            Category::Agent => cfg.categories.agent = false,
        }
        let path = self_heal_file(dirs);
        std::fs::create_dir_all(dirs.root())?;
        std::fs::write(&path, toml::to_string_pretty(&cfg)?)?;
        Ok(())
    }

    fn enabled(&self, category: Category) -> bool {
        match category {
            Category::Infra => self.categories.infra,
            Category::Coordinator => self.categories.coordinator,
            Category::Agent => self.categories.agent,
        }
    }
}

fn self_heal_file(dirs: &DivisiDirs) -> std::path::PathBuf {
    dirs.root().join("self_heal.toml")
}

#[derive(Debug, Clone)]
pub struct HealAction {
    pub category: Category,
    pub action: String,
    pub detail: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PassReport {
    pub actions: Vec<HealAction>,
}

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS self_heal_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            at TEXT NOT NULL,
            category TEXT NOT NULL,
            action TEXT NOT NULL,
            detail TEXT NOT NULL,
            ok INTEGER NOT NULL
        )",
        (),
    )?;
    Ok(())
}

fn log_event(conn: &Connection, category: Category, action: &str, detail: &str, ok: bool) -> Result<()> {
    conn.execute(
        "INSERT INTO self_heal_events (at, category, action, detail, ok) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![chrono::Utc::now().to_rfc3339(), category.as_str(), action, detail, ok as i64],
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct SelfHealEventRow {
    pub at: String,
    pub category: String,
    pub action: String,
    pub detail: String,
    pub ok: bool,
}

pub fn recent_events(conn: &Connection, limit: u32) -> Result<Vec<SelfHealEventRow>> {
    let mut stmt = conn.prepare("SELECT at, category, action, detail, ok FROM self_heal_events ORDER BY id DESC LIMIT ?1")?;
    let rows = stmt.query_map(rusqlite::params![limit], |r| {
        Ok(SelfHealEventRow {
            at: r.get(0)?,
            category: r.get(1)?,
            action: r.get(2)?,
            detail: r.get(3)?,
            ok: r.get::<_, i64>(4)? != 0,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Runs every enabled category (or just `category_filter` if given).
/// `catch_unwind`s each sub-step individually — spec: "one failure can't
/// wedge the rest" — so a panicking sub-step still lets every other one
/// (in this category and any other) run.
pub fn run_pass(ctx: &Context, conn: &Connection, category_filter: Option<Category>) -> Result<PassReport> {
    run_pass_with_restore(ctx, conn, category_filter, false)
}

/// Same as `run_pass`, but `allow_db_restore` also controls whether
/// `infra::db_integrity` is allowed to swap the live db file out from
/// under the daemon on a corruption finding.
///
/// Live-verification finding: `db_integrity`'s restore-from-backup did a
/// raw `fs::copy` over `divisi.db` on ANY corruption finding, including
/// from the periodic self-heal tick and `doctor --fix` -- both of which
/// run while the daemon's other request-handling threads may hold their
/// own open `Connection`s to that same file. Swapping the file out from
/// under those still-open connections is exactly the kind of concurrent,
/// uncoordinated file-level mutation that corrupts SQLite further (this
/// was very likely a real contributor to recurring "database disk image
/// is malformed" corruption observed live, on top of whatever originally
/// caused it). Restore-on-corruption is now only allowed during the
/// daemon-startup pass, before the socket is opened and no other
/// connections exist yet; every later pass (periodic tick, `doctor
/// --fix`) only detects and reports corruption so a human can restart
/// the daemon (which re-runs this same startup pass) to actually fix it.
pub fn run_pass_with_restore(ctx: &Context, conn: &Connection, category_filter: Option<Category>, allow_db_restore: bool) -> Result<PassReport> {
    ensure_schema(conn)?;
    let cfg = SelfHealConfig::load(&ctx.dirs);
    let mut report = PassReport::default();

    let wants = |c: Category| category_filter.map(|f| f == c).unwrap_or(true) && cfg.enabled(c);

    if wants(Category::Infra) {
        infra::run(ctx, conn, &cfg, &mut report, allow_db_restore)?;
    }
    if wants(Category::Coordinator) {
        coordinator::run(ctx, conn, &cfg, &mut report)?;
    }
    if wants(Category::Agent) {
        agent::run(ctx, conn, &cfg, &mut report)?;
    }

    Ok(report)
}

/// Runs `step` inside `catch_unwind`, recording exactly one
/// `self_heal_events` row either way, and appending to `report`. A panic
/// is turned into a logged failure, never propagated — this is the "one
/// substep panic does not wedge the rest" guarantee.
fn run_step(conn: &Connection, report: &mut PassReport, category: Category, action: &str, step: impl FnOnce() -> Result<String>) {
    let result = catch_unwind(AssertUnwindSafe(step));
    let (ok, detail) = match result {
        Ok(Ok(detail)) => (true, detail),
        Ok(Err(e)) => (false, format!("{e:#}")),
        Err(panic) => {
            let msg = panic.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| panic.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic (no message)".to_string());
            (false, format!("panicked: {msg}"))
        }
    };
    let _ = log_event(conn, category, action, &detail, ok);
    report.actions.push(HealAction { category, action: action.to_string(), detail, ok });
}

mod infra;
pub use infra::db_backup_dir;

mod coordinator;
mod agent;

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx(root: &std::path::Path) -> Context {
        let dirs = DivisiDirs::from_root(root.to_path_buf());
        dirs.ensure_created().unwrap();
        Context { dirs, resolved: divisi_core::ResolvedConfig::default(), registry: divisi_core::builtin_registry() }
    }

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn every_action_writes_a_self_heal_events_row() {
        let conn = test_conn();
        let mut report = PassReport::default();
        run_step(&conn, &mut report, Category::Infra, "test_ok", || Ok("fine".to_string()));
        run_step(&conn, &mut report, Category::Infra, "test_fail", || anyhow::bail!("nope"));

        let events = recent_events(&conn, 10).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|e| e.action == "test_ok" && e.ok));
        assert!(events.iter().any(|e| e.action == "test_fail" && !e.ok));
    }

    #[test]
    fn one_substep_panic_does_not_wedge_the_rest() {
        let conn = test_conn();
        let mut report = PassReport::default();
        run_step(&conn, &mut report, Category::Infra, "will_panic", || panic!("boom"));
        run_step(&conn, &mut report, Category::Infra, "runs_anyway", || Ok("survived".to_string()));

        assert_eq!(report.actions.len(), 2);
        assert!(!report.actions[0].ok);
        assert!(report.actions[0].detail.contains("panicked"));
        assert!(report.actions[1].ok);
    }

    #[test]
    fn disabled_category_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let conn = test_conn();
        SelfHealConfig::disable_category(&ctx.dirs, Category::Infra).unwrap();

        let report = run_pass(&ctx, &conn, Some(Category::Infra)).unwrap();
        assert!(report.actions.is_empty(), "a disabled category must run nothing at all");
    }

    #[test]
    fn config_loads_defaults_and_persists_on_first_use() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = DivisiDirs::from_root(tmp.path().to_path_buf());
        dirs.ensure_created().unwrap();
        let cfg = SelfHealConfig::load(&dirs);
        assert!(cfg.categories.infra && cfg.categories.coordinator);
        // `agent` defaults off -- see `Categories::default()`'s doc comment.
        assert!(!cfg.categories.agent);
        assert!(self_heal_file(&dirs).exists());
    }
}
