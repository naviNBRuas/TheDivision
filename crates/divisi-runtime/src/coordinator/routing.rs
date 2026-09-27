//! routing table (`~/.config/divisi/routing.toml`, spec E27.02 §5.4) and
//! `coordinator.toml` config.
//!
//! every `kind` — work kinds (`code`/`test`/`research`/`review`/`docs`/
//! `infra`) and the brain kinds (`plan`/`supervise`/`integrate`) — has a
//! capability-ranked agent list per `effort`. the list is a *preference
//! order*, not a hardcode: `select_agent` walks it and returns the first
//! agent that is detected, authed, and not currently rate-limited, then
//! dispatch-time fallback (inside `task::run`) continues down the chain.
//! an empty / exhausted list falls back to a global default order.
//!
//! reasoning is routed exactly like work (spec §11.1) — nothing is pinned,
//! so the coordinator never stalls on one agent's rate limit, including
//! for its own planning / supervising / integrating.

use crate::coordinator::graph::{Effort, NodeKind};
use serde::{Deserialize, Serialize};
use divisi_core::DivisiDirs;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CoordinatorConfig {
    /// global concurrent-node ceiling across all goals (spec §4.3).
    pub max_parallel: usize,
    /// scheduler tick-timer interval in the daemon.
    pub tick_interval_secs: u64,
    /// per-goal cap on `node_started` events before the goal blocks (§4.5).
    pub max_dispatches_per_goal: u32,
    /// per-goal wall-clock cap in minutes (§4.5).
    pub max_goal_minutes: u32,
    /// per-goal supervisor-patch cap; the 6th trigger blocks instead (§5.2).
    pub max_supervisor_patches: u32,
    /// agents to dispatch in structured-output mode so their runs report
    /// real token counts (E27.03) — currently only `claude` has a verified
    /// usage envelope. Empty by default; other agents' node token counts
    /// are parse-or-estimated.
    #[serde(default)]
    pub usage_json_agents: Vec<String>,
    /// E28 spec §7: when true, `divisi-pool` is tried first for every
    /// kind unless that kind's own list already names it explicitly
    /// (that explicit placement always wins — this only supplies a
    /// default when the kind is silent on the pool). Off by default so
    /// upgrading to 0.11.0 doesn't change any existing goal's routing.
    #[serde(default)]
    pub prefer_pool: bool,
    /// E28 spec §8: per-goal cap on `capacity_waits` before a
    /// `waiting_on_capacity` goal finally gives up to `Blocked`.
    /// Overridable per goal via `divisi goal amend <id> capacity-budget=N`.
    #[serde(default = "default_max_capacity_waits_per_goal")]
    pub max_capacity_waits_per_goal: u32,
    /// E28 spec §8: per-goal wall-clock cap (minutes, since the goal's
    /// `created_at`) on how long it may spend in capacity waits before
    /// giving up to `Blocked`, independent of `max_capacity_waits_per_goal`.
    #[serde(default = "default_max_capacity_wait_minutes")]
    pub max_capacity_wait_minutes: u32,
    /// Concurrent nodes allowed on each provider-backed `divisi-<provider>` agent that has no cap of its
    /// own. Free tiers rate limit per key, so a few at a time beats a burst of 429s.
    #[serde(default = "default_provider_agent_concurrency")]
    pub provider_agent_concurrency: usize,
    /// Concurrent nodes allowed on `divisi-pool` itself (it fans out over many providers internally).
    #[serde(default = "default_pool_concurrency")]
    pub pool_concurrency: usize,
    /// Per-agent overrides, e.g. `divisi-google = 4` under `[agent_concurrency]`. Wins over the defaults above.
    #[serde(default)]
    pub agent_concurrency: std::collections::BTreeMap<String, usize>,
    /// Agents kept out of every dispatch, e.g. one whose quota is reserved for the user's own use.
    /// Treated like a benched agent: routing skips it and a node already pinned to it is re-routed.
    #[serde(default)]
    pub disabled_agents: Vec<String>,
    /// Repo roots whose goals work in the checkout itself instead of a goal worktree (for example a
    /// docs/queue repo whose new files another process must see immediately). Everything else gets a
    /// private worktree per goal, see `scheduler::goal_workdir`.
    #[serde(default)]
    pub shared_checkouts: Vec<String>,
    /// Start no new parts while the machine has less available memory than this (MiB); 0 turns it off.
    #[serde(default = "default_min_available_memory_mb")]
    pub min_available_memory_mb: u64,
}

fn default_min_available_memory_mb() -> u64 {
    2048
}

fn default_provider_agent_concurrency() -> usize {
    2
}
fn default_pool_concurrency() -> usize {
    8
}

fn default_max_capacity_waits_per_goal() -> u32 {
    20
}
fn default_max_capacity_wait_minutes() -> u32 {
    720
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            max_parallel: 6,
            tick_interval_secs: 5,
            max_dispatches_per_goal: 25,
            max_goal_minutes: 60,
            max_supervisor_patches: 5,
            usage_json_agents: Vec::new(),
            prefer_pool: false,
            max_capacity_waits_per_goal: default_max_capacity_waits_per_goal(),
            max_capacity_wait_minutes: default_max_capacity_wait_minutes(),
            provider_agent_concurrency: default_provider_agent_concurrency(),
            pool_concurrency: default_pool_concurrency(),
            agent_concurrency: Default::default(),
            disabled_agents: Vec::new(),
            shared_checkouts: Vec::new(),
            min_available_memory_mb: default_min_available_memory_mb(),
        }
    }
}

impl CoordinatorConfig {
    /// reads `coordinator.toml`, writing the defaults to disk if it is
    /// absent. a malformed file falls back to defaults rather than failing
    /// every tick (same tolerance as the rest of divisi's config).
    pub fn load(dirs: &DivisiDirs) -> Self {
        let path = dirs.coordinator_file();
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str::<Self>(&s)
                .map(|mut c| {
                    c.disabled_agents = divisi_core::agent_names::canonical_list(&c.disabled_agents);
                    c
                })
                .unwrap_or_else(|e| {
                    tracing::warn!(path = %path.display(), error = %e, "coordinator.toml does not parse; using defaults");
                    Self::default()
                }),
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
}

/// `kind -> effort -> [agent]`, plus the `effort -> max_steps` knobs and
/// the global fallback order.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingTable {
    /// outer key: kind (`code`, `plan`, …); inner key: effort
    /// (`quick`/`standard`/`deep`).
    pub kinds: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    /// effort -> agent max-steps.
    pub effort_max_steps: BTreeMap<String, u32>,
    /// used when a kind's list is empty or every entry is unavailable.
    pub fallback_default: Vec<String>,
}

impl Default for RoutingTable {
    fn default() -> Self {
        // spec §5.4 seed. deliberately references agents this user's pool
        // actually has (grok / opencode / divisi-openrouter / divisi-nvidia
        // / divisi-gemini / claude).
        let k = |pairs: &[(&str, &[&str])]| -> BTreeMap<String, Vec<String>> {
            pairs
                .iter()
                .map(|(e, list)| (e.to_string(), list.iter().map(|s| s.to_string()).collect()))
                .collect()
        };
        let mut kinds = BTreeMap::new();
        kinds.insert(
            "code".into(),
            k(&[
                ("quick", &["divisi-openrouter", "divisi-nvidia", "opencode"]),
                ("standard", &["opencode", "grok", "divisi-openrouter"]),
                ("deep", &["grok", "claude", "opencode"]),
            ]),
        );
        kinds.insert("research".into(), k(&[("standard", &["grok", "divisi-gemini"])]));
        kinds.insert("test".into(), k(&[("standard", &["opencode", "divisi-openrouter"])]));
        kinds.insert("review".into(), k(&[("standard", &["grok", "claude", "opencode"])]));
        kinds.insert("docs".into(), k(&[("standard", &["opencode", "divisi-openrouter"])]));
        kinds.insert("infra".into(), k(&[("standard", &["opencode", "grok"])]));
        kinds.insert(
            "plan".into(),
            k(&[("standard", &["grok", "claude", "opencode", "divisi-openrouter"])]),
        );
        kinds.insert("supervise".into(), k(&[("standard", &["grok", "claude", "opencode"])]));
        kinds.insert("integrate".into(), k(&[("standard", &["grok", "opencode", "claude"])]));

        let effort_max_steps =
            [("quick", 8u32), ("standard", 20), ("deep", 40)].into_iter().map(|(e, n)| (e.to_string(), n)).collect();

        Self {
            kinds,
            effort_max_steps,
            fallback_default: ["grok", "opencode", "divisi-openrouter", "divisi-gemini", "divisi-nvidia", "claude"]
                .into_iter()
                .map(String::from)
                .collect(),
        }
    }
}

impl RoutingTable {
    pub fn load(dirs: &DivisiDirs) -> Self {
        let path = dirs.routing_file();
        match std::fs::read_to_string(&path) {
                        // Live-verification finding (2026-09-23): a routing.toml without `[effort_max_steps]` failed
            // to parse and was silently replaced by the built-in table, so every hand edit was ignored
            // for days (`#[serde(default)]` on the struct fixes that). A file that still can't be parsed
            // is logged, and pre-rename names (`single-pool`, `single-<provider>`) keep routing.
            Ok(s) => toml::from_str::<Self>(&s)
                .map(|mut t| {
                    for lists in t.kinds.values_mut() {
                        for l in lists.values_mut() {
                            *l = divisi_core::agent_names::canonical_list(l);
                        }
                    }
                    t.fallback_default = divisi_core::agent_names::canonical_list(&t.fallback_default);
                    t
                })
                .unwrap_or_else(|e| {
                    tracing::warn!(path = %path.display(), error = %e, "routing.toml does not parse; using the built-in routing table");
                    Self::default()
                }),
            Err(_) => {
                let table = Self::default();
                if let Ok(s) = toml::to_string_pretty(&table) {
                    let _ = std::fs::create_dir_all(dirs.root());
                    let _ = std::fs::write(&path, s);
                }
                table
            }
        }
    }

    pub fn max_steps(&self, effort: Effort) -> u32 {
        self.effort_max_steps.get(effort.as_str()).copied().unwrap_or(20)
    }

    /// the ordered candidate list for `(kind, effort)`: the exact list,
    /// then the kind's `standard` list, then the global fallback. de-duped,
    /// order preserved.
    fn candidates(&self, kind: NodeKind, effort: Effort) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |list: Option<&Vec<String>>| {
            if let Some(list) = list {
                for a in list {
                    if !out.contains(a) {
                        out.push(a.clone());
                    }
                }
            }
        };
        let per_kind = self.kinds.get(kind.as_str());
        push(per_kind.and_then(|m| m.get(effort.as_str())));
        push(per_kind.and_then(|m| m.get("standard")));
        for a in &self.fallback_default {
            if !out.contains(a) {
                out.push(a.clone());
            }
        }
        out
    }
}

/// how many back-to-back failures (with zero completions in between) inside
/// the health-probe window earn an agent a spot in `PoolHealth::rate_limited`
/// even when none of those failures matched a known rate-limit/auth-failure
/// phrasing. Some real exhaustion never says so in words the agent's CLI
/// bothers to phrase consistently (`divisi-nvidia`'s flaky
/// "HTTP request to provider failed", `cursor`'s silent "exit code 1, no
/// output"), and a message-text allowlist can never cover every CLI's
/// wording — a streak of pure failures is itself strong enough evidence
/// that the agent is currently unusable, worth the same temporary skip.
const CONSECUTIVE_FAILURE_BREAKER_THRESHOLD: i64 = 3;

/// live pool state used to filter the routing candidates (spec §5.4).
#[derive(Debug, Clone, Default)]
pub struct PoolHealth {
    /// agents that are installed AND authenticated right now.
    pub detected_authed: BTreeSet<String>,
    /// agents currently excluded from routing: either a `tasks.
    /// rate_limited=1` row in the last N minutes (an explicit rate-limit/
    /// auth-failure signal), or `CONSECUTIVE_FAILURE_BREAKER_THRESHOLD`+
    /// back-to-back failures with no completion in between — same
    /// exclusion regardless of which tripped it, since the downstream
    /// remedy (skip this agent, try the next candidate) is identical.
    pub rate_limited: BTreeSet<String>,
}

impl PoolHealth {
    /// `divisi-pool` (E28) is never filtered here: it isn't a shelled
    /// binary, so "on $PATH" is meaningless, and it has its own admission
    /// engine (the ledger) instead of the `rate_limited` task-row signal
    /// this struct tracks for CLI agents.
    fn usable(&self, agent: &str) -> bool {
        divisi_core::agent_names::is_pool(agent) || (self.detected_authed.contains(agent) && !self.rate_limited.contains(agent))
    }

    /// builds a live snapshot cheaply — this is called on every scheduler
    /// tick and every `CoordinatorStatus`, so it must not shell out.
    /// `detected_authed` = the agent's command is on `$PATH` (an in-process
    /// check, microseconds for the whole registry); a real `--version` /
    /// auth probe here would reintroduce the E27.01 `agent list` stall
    /// (~24 s for 30 agents) and a genuine auth failure already surfaces as
    /// a dispatch failure the scheduler retries past. `rate_limited` = any
    /// `tasks` row flagged `rate_limited` in the last 15 minutes.
    pub fn probe(
        registry: &[divisi_core::registry::AgentDefinition],
        conn: &rusqlite::Connection,
    ) -> Self {
        // `capabilities.non_interactive_run == false` (e.g. codebuff) means
        // the coordinator's headless dispatch can never succeed against
        // this agent no matter how healthy it otherwise looks — exclude it
        // from routing entirely rather than let it get selected and burn a
        // guaranteed-failed dispatch (see `task::run`'s matching check,
        // which is the last-resort guard if routing is bypassed).
        let detected_authed = registry
            .iter()
            .filter(|a| command_on_path(&a.command) && a.capabilities.non_interactive_run)
            .map(|a| a.name.clone())
            .collect();

        let cutoff = (chrono::Utc::now() - chrono::Duration::minutes(15)).to_rfc3339();
        let mut rate_limited = conn
            .prepare("SELECT DISTINCT agent FROM tasks WHERE rate_limited = 1 AND updated_at >= ?1")
            .and_then(|mut stmt| {
                stmt.query_map([cutoff.clone()], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<BTreeSet<_>>>()
            })
            .unwrap_or_default();

        rate_limited.extend(agents_on_a_pure_failure_streak(conn, &cutoff));
        // An agent that said when it recovers stays out until then, not just for 15 minutes.
        if let Ok(cooling) = crate::agent_cooldown::active(conn, chrono::Utc::now()) {
            rate_limited.extend(cooling.into_keys());
        }

        Self { detected_authed, rate_limited }
    }

    /// Keeps `agents` out of routing for this snapshot (`CoordinatorConfig::disabled_agents`).
    pub fn disable(&mut self, agents: &[String]) {
        self.rate_limited.extend(agents.iter().filter(|a| !divisi_core::agent_names::is_pool(a)).cloned());
    }
}

/// See `CONSECUTIVE_FAILURE_BREAKER_THRESHOLD`'s doc comment: agents whose
/// most recent `updated_at` falls inside `cutoff` AND whose last
/// `CONSECUTIVE_FAILURE_BREAKER_THRESHOLD` tasks are all `failed`/
/// `cancelled` with no `completed` among them. Requiring the streak's most
/// recent entry to be within `cutoff` keeps this self-clearing exactly like
/// the rate-limit signal: an agent that failed a streak days ago but hasn't
/// been tried since isn't kept excluded forever, and one success anywhere
/// in the streak breaks it immediately.
fn agents_on_a_pure_failure_streak(conn: &rusqlite::Connection, cutoff: &str) -> BTreeSet<String> {
    let recent_agents: Vec<String> = conn
        .prepare("SELECT DISTINCT agent FROM tasks WHERE updated_at >= ?1")
        .and_then(|mut stmt| {
            stmt.query_map([cutoff], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();

    let mut breaker = BTreeSet::new();
    for agent in recent_agents {
        let statuses: Vec<String> = match conn
            .prepare("SELECT status FROM tasks WHERE agent = ?1 AND COALESCE(summary, '') NOT LIKE 'interrupted:%' ORDER BY id DESC LIMIT ?2")
            .and_then(|mut stmt| {
                stmt.query_map(rusqlite::params![agent, CONSECUTIVE_FAILURE_BREAKER_THRESHOLD], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            }) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let enough = statuses.len() as i64 >= CONSECUTIVE_FAILURE_BREAKER_THRESHOLD;
        let all_failures = statuses.iter().all(|s| s == "failed" || s == "cancelled");
        if enough && all_failures {
            breaker.insert(agent);
        }
    }
    breaker
}

/// true if `cmd` (a bare binary name, or an absolute path) resolves on the
/// current `$PATH`. no subprocess — just stat calls.
fn command_on_path(cmd: &str) -> bool {
    let p = std::path::Path::new(cmd);
    if p.is_absolute() {
        return p.is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let cand = dir.join(cmd);
                cand.is_file()
            })
        })
        .unwrap_or(false)
}

/// walks the `(kind, effort)` candidate list and returns the first agent
/// that is detected, authed, and not rate-limited. if none qualify but the
/// pool health map is empty (e.g. probing failed), returns the first raw
/// candidate so the coordinator degrades to "try anyway" rather than
/// stalling. `None` only when there are no candidates at all.
pub fn select_agent(
    table: &RoutingTable,
    kind: NodeKind,
    effort: Effort,
    health: &PoolHealth,
) -> Option<String> {
    select_agent_excluding(table, kind, effort, health, &[])
}

/// Same selection as `select_agent`, but skips any agent already in
/// `exclude` — for retrying a brain role (planner/supervisor/integrator)
/// against a *different* agent instead of resampling the one that just
/// produced unparseable output. `select_agent` is fully deterministic (no
/// randomness, first-usable-candidate wins), so simply calling it again
/// after a bad response returns the exact same agent every time; a stuck
/// agent stayed stuck for all of `BRAIN_JSON_RETRIES`' attempts before
/// this existed (live-verified 2026-09-12: a goal's supervisor role kept
/// re-selecting `grok` — the same agent with an open, undiagnosed
/// worktree-spawn bug — across all 3 attempts, every single time it was
/// asked to patch a failure).
pub fn select_agent_excluding(table: &RoutingTable, kind: NodeKind, effort: Effort, health: &PoolHealth, exclude: &[String]) -> Option<String> {
    let candidates = table.candidates(kind, effort);
    if let Some(a) = candidates.iter().find(|a| !exclude.iter().any(|e| e == *a) && health.usable(a)) {
        return Some(a.clone());
    }
    if health.detected_authed.is_empty() {
        return candidates.into_iter().find(|a| !exclude.iter().any(|e| e == a));
    }
    None
}

/// `select_agent`, but honors `CoordinatorConfig::prefer_pool` (E28 spec
/// §7): when set, `divisi-pool` is tried before everything else for
/// every kind — *unless* that kind's own exact `(kind, effort)` list
/// already names it explicitly, in which case that placement wins and
/// this is a no-op. `select_agent`'s own signature stays untouched so
/// every existing caller/test keeps working unchanged; this is strictly
/// additive.
pub fn select_agent_with_prefer_pool(table: &RoutingTable, kind: NodeKind, effort: Effort, health: &PoolHealth, prefer_pool: bool) -> Option<String> {
    if !prefer_pool {
        return select_agent(table, kind, effort, health);
    }
    let explicit_override = table.kinds.get(kind.as_str()).and_then(|m| m.get(effort.as_str())).is_some_and(|list| list.iter().any(|a| a == "divisi-pool"));
    if explicit_override {
        return select_agent(table, kind, effort, health);
    }
    Some("divisi-pool".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(authed: &[&str], limited: &[&str]) -> PoolHealth {
        PoolHealth {
            detected_authed: authed.iter().map(|s| s.to_string()).collect(),
            rate_limited: limited.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn select_agent_returns_divisi_pool_when_named_in_kind_list() {
        let mut t = RoutingTable::default();
        t.kinds.get_mut("code").unwrap().insert("standard".to_string(), vec!["divisi-pool".to_string(), "opencode".to_string()]);
        let h = health(&[], &[]); // divisi-pool needs no detected_authed entry
        assert_eq!(select_agent(&t, NodeKind::Code, Effort::Standard, &h), Some("divisi-pool".to_string()));
    }

    #[test]
    fn select_agent_excluding_skips_previously_tried_agents() {
        let mut t = RoutingTable::default();
        t.kinds.get_mut("code").unwrap().insert("standard".to_string(), vec!["grok".to_string(), "claude".to_string(), "codex".to_string()]);
        let h = health(&["grok", "claude", "codex"], &[]);

        assert_eq!(select_agent_excluding(&t, NodeKind::Code, Effort::Standard, &h, &[]), Some("grok".to_string()));
        assert_eq!(select_agent_excluding(&t, NodeKind::Code, Effort::Standard, &h, &["grok".to_string()]), Some("claude".to_string()));
        assert_eq!(
            select_agent_excluding(&t, NodeKind::Code, Effort::Standard, &h, &["grok".to_string(), "claude".to_string()]),
            Some("codex".to_string())
        );
    }

    #[test]
    fn select_agent_excluding_returns_none_once_every_candidate_is_excluded() {
        let mut t = RoutingTable::default();
        t.kinds.get_mut("code").unwrap().insert("standard".to_string(), vec!["grok".to_string()]);
        let h = health(&["grok"], &[]);
        assert_eq!(select_agent_excluding(&t, NodeKind::Code, Effort::Standard, &h, &["grok".to_string()]), None);
    }

    #[test]
    fn select_agent_is_select_agent_excluding_with_nothing_excluded() {
        let t = RoutingTable::default();
        let h = health(&["opencode"], &[]);
        assert_eq!(select_agent(&t, NodeKind::Code, Effort::Standard, &h), select_agent_excluding(&t, NodeKind::Code, Effort::Standard, &h, &[]));
    }

    #[test]
    fn prefer_pool_true_tries_divisi_pool_first_unless_kind_overrides() {
        let t = RoutingTable::default();
        let h = health(&["opencode", "grok"], &[]);
        // code/standard doesn't name divisi-pool -> prefer_pool wins.
        assert_eq!(select_agent_with_prefer_pool(&t, NodeKind::Code, Effort::Standard, &h, true), Some("divisi-pool".to_string()));
        // Without prefer_pool, ordinary routing applies.
        assert_eq!(select_agent_with_prefer_pool(&t, NodeKind::Code, Effort::Standard, &h, false), Some("opencode".to_string()));
    }

    #[test]
    fn prefer_pool_true_defers_to_kinds_own_explicit_divisi_pool_placement() {
        let mut t = RoutingTable::default();
        // code/standard explicitly puts divisi-pool second, after opencode.
        t.kinds.get_mut("code").unwrap().insert("standard".to_string(), vec!["opencode".to_string(), "divisi-pool".to_string()]);
        let h = health(&["opencode"], &[]);
        // Explicit placement wins: ordinary select_agent logic still
        // picks opencode first since it's usable and comes first in the
        // kind's own list.
        assert_eq!(select_agent_with_prefer_pool(&t, NodeKind::Code, Effort::Standard, &h, true), Some("opencode".to_string()));
    }

    #[test]
    fn divisi_pool_is_never_filtered_as_undetected() {
        let t = RoutingTable::default();
        let mut kinds = t.kinds.clone();
        kinds.get_mut("code").unwrap().insert("standard".to_string(), vec!["divisi-pool".to_string()]);
        let t = RoutingTable { kinds, ..t };
        // Empty health -- no agent detected_authed at all -- would
        // normally fall through to "pool health map is empty -> try the
        // first candidate anyway"; confirm divisi-pool is picked via the
        // *usable* path, not that degrade fallback, by also marking a
        // different agent authed (so detected_authed is non-empty and the
        // fallback branch does NOT apply) -- only `usable()`'s explicit
        // divisi-pool carve-out can explain the result then.
        let h = health(&["some-other-agent"], &[]);
        assert_eq!(select_agent(&t, NodeKind::Code, Effort::Standard, &h), Some("divisi-pool".to_string()));
    }

    #[test]
    fn select_agent_skips_rate_limited_and_unauthed() {
        let t = RoutingTable::default();
        // code/standard = [opencode, grok, divisi-openrouter]
        let h = health(&["grok", "divisi-openrouter"], &["grok"]);
        assert_eq!(
            select_agent(&t, NodeKind::Code, Effort::Standard, &h),
            Some("divisi-openrouter".to_string())
        );
    }

    #[test]
    fn select_agent_falls_back_to_default_when_kind_list_exhausted() {
        let t = RoutingTable::default();
        // nothing from code/quick or code/standard is usable; only a
        // fallback-order agent (divisi-gemini) is.
        let h = health(&["divisi-gemini"], &[]);
        assert_eq!(
            select_agent(&t, NodeKind::Code, Effort::Quick, &h),
            Some("divisi-gemini".to_string())
        );
    }

    #[test]
    fn select_agent_degrades_to_first_candidate_when_health_unknown() {
        let t = RoutingTable::default();
        let h = PoolHealth::default(); // probing produced nothing
        assert_eq!(
            select_agent(&t, NodeKind::Plan, Effort::Standard, &h),
            Some("grok".to_string())
        );
    }

    #[test]
    fn a_routing_file_without_effort_steps_keeps_its_own_lists() {
        let table: RoutingTable = toml::from_str("fallback_default = [\"opencode\"]\n[kinds.plan]\nstandard = [\"divisi-pool\"]\n").unwrap();
        assert_eq!(table.fallback_default, vec!["opencode".to_string()]);
        assert_eq!(table.kinds["plan"]["standard"], vec!["divisi-pool".to_string()]);
    }

    #[test]
    fn config_and_table_load_write_defaults_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = DivisiDirs::from_root(dir.path().to_path_buf());
        let cfg = CoordinatorConfig::load(&dirs);
        assert_eq!(cfg.max_parallel, 6);
        assert!(dirs.coordinator_file().exists());

        let t = RoutingTable::load(&dirs);
        assert_eq!(t.max_steps(Effort::Deep), 40);
        assert!(dirs.routing_file().exists());
    }
}
