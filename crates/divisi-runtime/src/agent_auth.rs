//! The auth inventory: for every agent, whether it needs a login, has one, needs none, or is out
//! of quota, with the evidence. Nothing here is guessed. A category comes from a tiny real call
//! through the agent (`probe`), run the way the pool runs it (an isolated home), and, for agents
//! that answered, again from an *empty* home to find the ones that need no credentials at all.

use crate::context::Context;
use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use divisi_agent_sdk::backend::ExecBackend;
use divisi_protocol::AgentAuthRow;
use rusqlite::{params, Connection};
use std::collections::BTreeMap;
use std::time::Duration;

const PROBE_PROMPT: &str = "Reply with exactly the single word: ready";
const PROBE_TIMEOUT: Duration = Duration::from_secs(90);
/// How many agents are probed at once.
const PARALLEL: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    NoAuthNeeded,
    Authed,
    Exhausted,
    NeedsLogin,
    Unresponsive,
    Error,
    NotDispatchable,
    Provider,
    NotInstalled,
    Unverified,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::NoAuthNeeded => "no_auth_needed",
            Category::Authed => "authed",
            Category::Exhausted => "exhausted",
            Category::NeedsLogin => "needs_login",
            Category::Unresponsive => "unresponsive",
            Category::Error => "error",
            Category::NotDispatchable => "not_dispatchable",
            Category::Provider => "provider",
            Category::NotInstalled => "not_installed",
            Category::Unverified => "unverified",
        }
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("---")).unwrap_or("");
    line.chars().take(160).collect()
}

/// The first line of `text` that contains one of the `needles`, else its first line.
fn evidence_line(text: &str, matches: impl Fn(&str) -> bool) -> String {
    let hit = text.lines().map(str::trim).find(|l| !l.is_empty() && matches(l));
    hit.map(|l| l.chars().take(160).collect()).unwrap_or_else(|| first_line(text))
}

/// Turns the result of a probe call into a category, with the evidence and, for quota, the reset time.
pub fn classify(success: bool, timed_out: bool, stdout: &str, stderr: &str, now: DateTime<Local>) -> (Category, String, Option<DateTime<Utc>>) {
    if timed_out {
        return (Category::Unresponsive, "no answer within the probe timeout".into(), None);
    }
    if success && !stdout.trim().is_empty() {
        return (Category::Authed, format!("answered a real call: {}", first_line(stdout)), None);
    }
    let combined = format!("{stdout}\n{stderr}");
    if divisi_core::ratelimit::looks_like_rate_limit(&combined) {
        let until = divisi_core::ratelimit::reset_time(&combined, now);
        let why = evidence_line(&combined, divisi_core::ratelimit::looks_like_rate_limit);
        return (Category::Exhausted, why, until);
    }
    if divisi_core::ratelimit::looks_like_auth_failure(&combined) {
        return (Category::NeedsLogin, evidence_line(&combined, divisi_core::ratelimit::looks_like_auth_failure), None);
    }
    (Category::Error, first_line(&combined), None)
}

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS agent_auth_probe (
            agent TEXT PRIMARY KEY,
            category TEXT NOT NULL,
            evidence TEXT NOT NULL,
            checked_at TEXT NOT NULL,
            until TEXT
        )",
        (),
    )?;
    Ok(())
}

fn store(conn: &Connection, row: &AgentAuthRow) -> Result<()> {
    ensure_schema(conn)?;
    conn.execute(
        "INSERT INTO agent_auth_probe (agent, category, evidence, checked_at, until) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(agent) DO UPDATE SET category = excluded.category, evidence = excluded.evidence,
            checked_at = excluded.checked_at, until = excluded.until",
        params![row.agent, row.category, row.evidence, row.checked_at, row.until],
    )?;
    Ok(())
}

fn load(conn: &Connection) -> Result<BTreeMap<String, AgentAuthRow>> {
    ensure_schema(conn)?;
    let mut stmt = conn.prepare("SELECT agent, category, evidence, checked_at, until FROM agent_auth_probe")?;
    let rows = stmt.query_map([], |r| {
        Ok(AgentAuthRow { agent: r.get(0)?, category: r.get(1)?, evidence: r.get(2)?, checked_at: r.get(3)?, until: r.get(4)? })
    })?;
    let mut out = BTreeMap::new();
    for row in rows {
        let row = row?;
        out.insert(row.agent.clone(), row);
    }
    Ok(out)
}

/// A stored `exhausted` whose reset time has passed no longer describes the agent, and a live cooldown
/// turns any stored state into `exhausted`.
fn overlay_cooldown(mut row: AgentAuthRow, cooling: &BTreeMap<String, DateTime<Utc>>, now: DateTime<Utc>) -> AgentAuthRow {
    if let Some(until) = cooling.get(&row.agent).filter(|u| **u > now) {
        row.category = Category::Exhausted.as_str().into();
        row.until = Some(until.to_rfc3339());
        if !row.evidence.contains("cooling down") {
            row.evidence = format!("cooling down until {} ({})", until.with_timezone(&Local).format("%b %-d %H:%M"), row.evidence);
        }
    } else if row.category == Category::Exhausted.as_str() && row.until.as_deref().and_then(|u| DateTime::parse_from_rfc3339(u).ok()).is_some_and(|u| u.with_timezone(&Utc) <= now) {
        row.category = Category::Unverified.as_str().into();
        row.evidence = "its quota reset time has passed; probe again to confirm".into();
        row.until = None;
    }
    row
}

fn row(agent: &str, category: Category, evidence: impl Into<String>, until: Option<DateTime<Utc>>) -> AgentAuthRow {
    AgentAuthRow {
        agent: agent.to_owned(),
        category: category.as_str().into(),
        evidence: evidence.into(),
        checked_at: Some(Utc::now().to_rfc3339()),
        until: until.map(|u| u.to_rfc3339()),
    }
}

/// A scratch directory that is a git repo, because several agents refuse to run outside one.
fn scratch_repo() -> Result<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    let _ = std::process::Command::new("git").args(["init", "-q"]).current_dir(dir.path()).status();
    Ok(dir)
}

/// Runs one agent through a real probe. `deep` also tries it from an empty home.
pub fn probe_one(ctx: &Context, conn: &Connection, name: &str, deep: bool) -> AgentAuthRow {
    // Provider proxies and the built-in pool agent have no login of their own: their credential is a
    // pool provider key, reported by the provider table.
    if name == "single-pool" || name == "single-agent" || name.starts_with("single-") {
        return row(name, Category::Provider, "dispatches through pool provider keys; see `divisi provider`", None);
    }
    let Some(adapter) = divisi_agent_sdk::adapters::for_agent_with_custom(name, &ctx.dirs.agents_dir(), &ctx.registry) else {
        return row(name, Category::Error, "no adapter for this agent", None);
    };
    if !adapter.discover().detected {
        return row(name, Category::NotInstalled, "binary not found", None);
    }
    if ctx.registry.iter().find(|a| a.name == name).is_some_and(|a| !a.capabilities.non_interactive_run) {
        return row(name, Category::NotDispatchable, "has no non-interactive mode, so the pool cannot run it headlessly", None);
    }
    let scratch = match scratch_repo() {
        Ok(s) => s,
        Err(e) => return row(name, Category::Error, format!("could not create a scratch directory: {e}"), None),
    };

    // 1. The way the pool runs it: an isolated home plus the provider keys divisi holds for it.
    let real = divisi_core::paths::real_home_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
    let home = match divisi_core::agent_home::ensure_bootstrapped(&ctx.dirs.homes_dir(), &real, name) {
        Ok(h) => h,
        Err(e) => return row(name, Category::Error, format!("could not prepare its isolated home: {e:#}"), None),
    };
    let env = divisi_core::provider_keys::resolve_env_for_agent(&ctx.dirs, name);
    let backend = ExecBackend::host_with_env(Some(&home), &env);
    let outcome = adapter.run_prompt(scratch.path(), PROBE_PROMPT, &backend, None, PROBE_TIMEOUT, None);
    let (category, evidence, until) = match &outcome {
        Ok(o) => classify(o.success, o.timed_out, &o.stdout, &o.stderr, Local::now()),
        Err(e) => (Category::Error, first_line(&format!("{e:#}")), None),
    };
    if category == Category::Exhausted {
        // Remember when it recovers so routing stops sending it work until then.
        if let (Some(until), Ok(o)) = (until, &outcome) {
            let _ = crate::agent_cooldown::record(conn, name, until, &format!("{}\n{}", o.stdout, o.stderr));
        }
    }
    if category != Category::Authed {
        return row(name, category, evidence, until);
    }

    // 2. Does it need any login at all? Try again from a home with nothing in it.
    let candidates = ["opencode", "kilocode"];
    if deep || candidates.contains(&name) {
        if let Ok(empty) = tempfile::tempdir() {
            let no_env = BTreeMap::new();
            let backend = ExecBackend::host_with_env(Some(empty.path()), &no_env);
            if let Ok(o) = adapter.run_prompt(scratch.path(), PROBE_PROMPT, &backend, None, PROBE_TIMEOUT, None) {
                if o.success && !o.stdout.trim().is_empty() {
                    return row(name, Category::NoAuthNeeded, format!("answered from an empty home with no credentials: {}", first_line(&o.stdout)), None);
                }
            }
        }
    }
    row(name, Category::Authed, evidence, None)
}

/// The inventory. With `probe`, every selected agent is probed (in parallel batches) and the result stored;
/// without it, the last stored probe is reported, with any live quota cooldown overlaid.
pub fn report(ctx: &Context, conn: &Connection, probe: bool, deep: bool, only: &[String]) -> Result<Vec<AgentAuthRow>> {
    ensure_schema(conn)?;
    let names: Vec<String> =
        ctx.registry.iter().map(|a| a.name.clone()).filter(|n| only.is_empty() || only.contains(n)).collect();

    if probe {
        for chunk in names.chunks(PARALLEL) {
            let rows: Vec<AgentAuthRow> = std::thread::scope(|scope| {
                let handles: Vec<_> = chunk
                    .iter()
                    .map(|name| {
                        scope.spawn(move || match crate::handlers::coordinator_db(ctx) {
                            Ok(own) => probe_one(ctx, &own, name, deep),
                            Err(e) => row(name, Category::Error, format!("no database: {e:#}"), None),
                        })
                    })
                    .collect();
                handles.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            for r in &rows {
                store(conn, r)?;
            }
        }
    }

    let stored = load(conn)?;
    let cooling = crate::agent_cooldown::active(conn, Utc::now())?;
    let now = Utc::now();
    let mut out = Vec::new();
    for name in &names {
        let base = stored.get(name).cloned().unwrap_or_else(|| {
            let installed = divisi_agent_sdk::adapters::for_agent_with_custom(name, &ctx.dirs.agents_dir(), &ctx.registry).is_some_and(|a| a.discover().detected);
            if name.starts_with("single-") {
                AgentAuthRow { agent: name.clone(), category: Category::Provider.as_str().into(), evidence: "dispatches through pool provider keys; see `divisi provider`".into(), checked_at: None, until: None }
            } else if installed {
                AgentAuthRow { agent: name.clone(), category: Category::Unverified.as_str().into(), evidence: "never probed; run `divisi agent auth --probe`".into(), checked_at: None, until: None }
            } else {
                AgentAuthRow { agent: name.clone(), category: Category::NotInstalled.as_str().into(), evidence: "binary not found".into(), checked_at: None, until: None }
            }
        });
        out.push(overlay_cooldown(base, &cooling, now));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as Dur, TimeZone};

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 20, 3, 30, 0).unwrap()
    }

    #[test]
    fn an_answer_means_authed() {
        let (c, why, until) = classify(true, false, "ready\n", "", now());
        assert_eq!(c, Category::Authed);
        assert!(why.contains("ready"));
        assert!(until.is_none());
    }

    #[test]
    fn codexs_quota_message_is_exhausted_with_its_reset_time() {
        let (c, why, until) = classify(false, false, "", "ERROR: You've hit your usage limit. Upgrade to Plus, or try again at Sep 22nd, 2026 1:35 AM.", now());
        assert_eq!(c, Category::Exhausted);
        assert!(why.contains("usage limit"), "{why}");
        assert_eq!(until, Some(Local.with_ymd_and_hms(2026, 9, 22, 1, 35, 0).unwrap().with_timezone(&Utc)));
    }

    #[test]
    fn a_hollow_success_with_a_quota_banner_is_exhausted_not_authed() {
        let (c, _, _) = classify(true, false, "", "Monthly request limit reached · limits reset on 10/01", now());
        assert_eq!(c, Category::Exhausted, "kiro exits 0 with nothing on stdout when it is out of quota");
    }

    #[test]
    fn login_problems_are_needs_login_and_named() {
        let (c, why, _) = classify(false, false, "", "Not logged in · Please run /login", now());
        assert_eq!(c, Category::NeedsLogin);
        assert!(why.contains("Not logged in"));
        assert_eq!(classify(false, false, "", "Error: No API key found. Set OPENAI_API_KEY", now()).0, Category::NeedsLogin);
    }

    #[test]
    fn a_timeout_is_unresponsive_and_anything_else_is_an_error() {
        assert_eq!(classify(false, true, "", "", now()).0, Category::Unresponsive);
        let (c, why, _) = classify(false, false, "", "\n\npanic: index out of bounds\nmore", now());
        assert_eq!((c, why.as_str()), (Category::Error, "panic: index out of bounds"));
    }

    fn stored(agent: &str, category: Category, until: Option<DateTime<Utc>>) -> AgentAuthRow {
        AgentAuthRow { agent: agent.into(), category: category.as_str().into(), evidence: "e".into(), checked_at: Some("t".into()), until: until.map(|u| u.to_rfc3339()) }
    }

    #[test]
    fn probes_are_stored_and_read_back() {
        let conn = Connection::open_in_memory().unwrap();
        store(&conn, &stored("claude", Category::Authed, None)).unwrap();
        store(&conn, &stored("claude", Category::NeedsLogin, None)).unwrap();
        let all = load(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all["claude"].category, "needs_login", "a newer probe replaces the older one");
    }

    #[test]
    fn a_live_cooldown_makes_any_stored_state_exhausted() {
        let n = Utc::now();
        let cooling = BTreeMap::from([("codex".to_string(), n + Dur::hours(30))]);
        let r = overlay_cooldown(stored("codex", Category::Authed, None), &cooling, n);
        assert_eq!(r.category, "exhausted");
        assert!(r.until.is_some() && r.evidence.contains("cooling down"), "{r:?}");
        let other = overlay_cooldown(stored("claude", Category::Authed, None), &cooling, n);
        assert_eq!(other.category, "authed", "other agents are untouched");
    }

    #[test]
    fn an_exhausted_agent_whose_reset_has_passed_is_no_longer_claimed_exhausted() {
        let n = Utc::now();
        let r = overlay_cooldown(stored("codex", Category::Exhausted, Some(n - Dur::hours(1))), &BTreeMap::new(), n);
        assert_eq!(r.category, "unverified");
        assert!(r.evidence.contains("reset time has passed"));
    }
}
