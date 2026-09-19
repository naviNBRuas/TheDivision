//! Pure aggregation of the four coordinator/pool wire types into one
//! `NotchSnapshot` the UI renders. No iced, no I/O -- fully unit-testable.
//! See `docs/superpowers/plans/2026-09-17-e30-notch-hud.md` Phase 3 Task 5.

use serde::Serialize;
use divisi_protocol::{AgentInfo, AuthState, CoordinatorSnapshot, GoalSummary, PoolBenchedKey, PoolKeyStatusInfo, PoolStatusInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthTone {
    Healthy,
    Amber,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderTally {
    pub platform: String,
    pub key_count: usize,
    pub cooldown: String,
    pub headroom: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchRow {
    pub platform: String,
    pub model: String,
    pub key_id: String,
    pub remaining_secs: u64,
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GoalActivity {
    pub id: String,
    pub text: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentAuthDot {
    pub name: String,
    pub state: AuthState,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GoalRow {
    pub id: String,
    pub text: String,
    pub status: String,
    pub dispatches: u32,
    pub max_dispatches: u32,
    /// Why a blocked/waiting goal is held.
    pub note: Option<String>,
    pub eta: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentSlot {
    pub agent: String,
    pub running: usize,
    pub cap: Option<usize>,
    pub rate_limited: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentRow {
    pub name: String,
    pub detected: bool,
    pub version: Option<String>,
    /// `authed | needs_login | unverified | no_auth_needed | not_installed`
    pub class: String,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProblemKey {
    pub platform: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskRow {
    pub id: i64,
    pub agent: String,
    pub status: String,
    pub description: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderRow {
    pub platform: String,
    /// `key` or `keyless`.
    pub auth_kind: String,
    /// `authed | unverified | invalid | disabled | no_key | blocked | no_auth_needed`
    pub auth_state: String,
    pub key_count: u32,
    pub keys_valid: u32,
    pub keys_invalid: u32,
    pub keys_unvalidated: u32,
    pub keys_disabled: u32,
    pub can_validate: bool,
    pub reason: Option<String>,
    pub cooldown: String,
    pub requests_today: u64,
    pub rpd_limit: Option<u32>,
    pub rpm_limit: Option<u32>,
    pub tpm_limit: Option<u32>,
    pub tpd_limit: Option<u64>,
    /// A published daily limit exists, so usage can be shown against it.
    pub metered: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentUsage {
    pub agent: String,
    pub runs_24h: u64,
    pub runs_7d: u64,
    pub runs_total: u64,
    pub prompt_tokens_7d: u64,
    pub completion_tokens_7d: u64,
    pub estimated_runs_7d: u64,
    pub rate_limited_7d: u64,
    pub discarded_token_rows: u64,
    pub last_run_at: Option<String>,
}

/// Everything beyond the compact pill/card view -- consumed by the GNOME
/// extension's Goals / Pool / Agents tabs.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct NotchDetail {
    pub goals: Vec<GoalRow>,
    pub goals_running: usize,
    pub goals_queued: usize,
    pub goals_waiting: usize,
    pub goals_blocked: usize,
    pub max_parallel: usize,
    pub slots: Vec<AgentSlot>,
    pub agent_rows: Vec<AgentRow>,
    pub problem_keys: Vec<ProblemKey>,
    pub keys_unkeyed: usize,
    pub recent_tasks: Vec<TaskRow>,
    pub providers_full: Vec<ProviderRow>,
    pub agent_usage: Vec<AgentUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NotchSnapshot {
    pub tone: HealthTone,
    pub degraded: bool,
    pub healthy_ratio: f64,
    pub provider_count: usize,
    pub total_keys: usize,
    pub providers: Vec<ProviderTally>,
    pub benches: Vec<BenchRow>,
    pub activity: Vec<GoalActivity>,
    pub agents: Vec<AgentAuthDot>,
    pub any_goal_running: bool,
    pub detail: NotchDetail,
}

fn cooldown_is_benched(cooldown: &str) -> bool {
    cooldown.starts_with("benched")
}

/// Key count per platform = rows with `keyed == true` (spec assumption
/// 4) — `valid` isn't required for the compact tally, but a keyed+valid
/// row's cooldown/headroom is preferred when picking what to display for
/// a platform with mixed rows, since that's the row actually usable.
fn tally_providers(keys: &[PoolKeyStatusInfo]) -> Vec<ProviderTally> {
    use std::collections::BTreeMap;
    let mut by_platform: BTreeMap<String, Vec<&PoolKeyStatusInfo>> = BTreeMap::new();
    for k in keys.iter().filter(|k| k.keyed) {
        by_platform.entry(k.platform.clone()).or_default().push(k);
    }
    let mut out: Vec<ProviderTally> = by_platform
        .into_iter()
        .map(|(platform, rows)| {
            let representative = rows.iter().find(|r| r.valid).or_else(|| rows.first()).unwrap();
            ProviderTally {
                platform,
                key_count: rows.len(),
                cooldown: representative.cooldown.clone(),
                headroom: representative.headroom.clone(),
            }
        })
        .collect();
    out.sort_by(|a, b| b.key_count.cmp(&a.key_count).then_with(|| a.platform.cmp(&b.platform)));
    out
}

/// Up to 3 activity rows, running goals first, then waiting-on-capacity,
/// then queued -- blocked goals aren't "activity" (they need a human, not
/// a glance) and stay out of the notch's compact view.
fn pick_activity(coord: &CoordinatorSnapshot) -> Vec<GoalActivity> {
    fn to_activity(g: &GoalSummary) -> GoalActivity {
        GoalActivity { id: g.id.clone(), text: g.text.clone(), status: g.status.clone() }
    }
    coord
        .running_goals
        .iter()
        .chain(coord.waiting_goals.iter())
        .chain(coord.queued_goals.iter())
        .take(3)
        .map(to_activity)
        .collect()
}

/// First non-empty line, capped, so a multi-KB prompt doesn't bloat the
/// snapshot the shell extension re-reads every couple of seconds.
pub fn brief(text: &str, max_chars: usize) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if line.chars().count() <= max_chars {
        return line.to_string();
    }
    let cut: String = line.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{cut}…")
}

fn build_detail(keys: &[PoolKeyStatusInfo], coord: &CoordinatorSnapshot, agents: &[AgentInfo]) -> NotchDetail {
    let row = |g: &GoalSummary, note: Option<String>, eta: Option<String>| GoalRow {
        id: g.id.clone(),
        text: brief(&g.text, 160),
        status: g.status.clone(),
        dispatches: g.dispatches,
        max_dispatches: g.max_dispatches,
        note,
        eta,
    };
    let mut goals = Vec::new();
    goals.extend(coord.running_goals.iter().map(|g| row(g, None, None)));
    goals.extend(coord.waiting_goals.iter().map(|g| row(g, g.capacity_reason.clone(), g.capacity_eta.clone())));
    goals.extend(coord.queued_goals.iter().map(|g| row(g, None, None)));
    goals.extend(coord.blocked_goals.iter().map(|g| row(g, g.blocked_reason.clone(), None)));
    NotchDetail {
        goals,
        goals_running: coord.running_goals.len(),
        goals_queued: coord.queued_goals.len(),
        goals_waiting: coord.waiting_goals.len(),
        goals_blocked: coord.blocked_goals.len(),
        max_parallel: coord.max_parallel,
        slots: coord
            .pool
            .iter()
            .map(|p| AgentSlot { agent: p.agent.clone(), running: p.running, cap: p.cap, rate_limited: p.rate_limited })
            .collect(),
        agent_rows: agents
            .iter()
            .map(|a| {
                let c = divisi_core::auth_class::classify_agent(a, keys);
                AgentRow { name: a.name.clone(), detected: a.detected, version: a.version.clone(), class: c.class.into(), why: c.why }
            })
            .collect(),
        providers_full: keys
            .iter()
            .map(|k| ProviderRow {
                platform: k.platform.clone(),
                auth_kind: k.auth_kind.clone(),
                auth_state: k.auth_state.clone(),
                key_count: k.key_count,
                keys_valid: k.keys_valid,
                keys_invalid: k.keys_invalid,
                keys_unvalidated: k.keys_unvalidated,
                keys_disabled: k.keys_disabled,
                can_validate: k.can_validate,
                reason: k.disabled_reason.as_deref().map(|r| brief(r, 100)),
                cooldown: k.cooldown.clone(),
                requests_today: k.requests_today,
                rpd_limit: k.rpd_limit,
                rpm_limit: k.rpm_limit,
                tpm_limit: k.tpm_limit,
                tpd_limit: k.tpd_limit,
                metered: k.rpd_limit.is_some() || k.tpd_limit.is_some(),
            })
            .collect(),
        problem_keys: keys
            .iter()
            .filter(|k| k.keyed && k.disabled_reason.is_some())
            .map(|k| ProblemKey { platform: k.platform.clone(), reason: brief(k.disabled_reason.as_deref().unwrap_or_default(), 120) })
            .collect(),
        keys_unkeyed: keys.iter().filter(|k| !k.keyed).count(),
        recent_tasks: Vec::new(),
        agent_usage: Vec::new(),
    }
}

pub fn aggregate(pool: &PoolStatusInfo, keys: &[PoolKeyStatusInfo], coord: &CoordinatorSnapshot, agents: &[AgentInfo]) -> NotchSnapshot {
    let providers = tally_providers(keys);
    let total_keys = providers.iter().map(|p| p.key_count).sum();
    let benches: Vec<BenchRow> = pool
        .benched
        .iter()
        .map(|b: &PoolBenchedKey| BenchRow {
            platform: b.platform.clone(),
            model: b.model.clone(),
            key_id: b.key_id.clone(),
            remaining_secs: b.remaining_secs,
            provenance: b.provenance.clone(),
        })
        .collect();

    let tone = if pool.degraded {
        HealthTone::Degraded
    } else if !benches.is_empty() || keys.iter().any(|k| cooldown_is_benched(&k.cooldown)) {
        HealthTone::Amber
    } else {
        HealthTone::Healthy
    };

    NotchSnapshot {
        tone,
        degraded: pool.degraded,
        healthy_ratio: pool.healthy_ratio,
        provider_count: providers.len(),
        total_keys,
        providers,
        benches,
        activity: pick_activity(coord),
        agents: agents.iter().map(|a| AgentAuthDot { name: a.name.clone(), state: a.authenticated }).collect(),
        any_goal_running: !coord.running_goals.is_empty(),
        detail: build_detail(keys, coord, agents),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_coord() -> CoordinatorSnapshot {
        CoordinatorSnapshot {
            running_goals: vec![],
            queued_goals: vec![],
            blocked_goals: vec![],
            waiting_goals: vec![],
            pool: vec![],
            max_parallel: 6,
        }
    }

    fn key(platform: &str, keyed: bool, valid: bool) -> PoolKeyStatusInfo {
        PoolKeyStatusInfo {
            platform: platform.into(),
            keyed,
            valid,
            last_validated_at: None,
            disabled_reason: None,
            cooldown: "clear".into(),
            headroom: "40/50 rpd".into(),
            ..Default::default()
        }
    }

    fn goal(id: &str, status: &str) -> GoalSummary {
        GoalSummary {
            id: id.into(),
            session_id: "sess_1".into(),
            text: "do the thing".into(),
            mode: "auto".into(),
            status: status.into(),
            dispatches: 1,
            max_dispatches: 25,
            created_at: "2026-09-18T00:00:00Z".into(),
            capacity_reason: None,
            capacity_eta: None,
            blocked_reason: None,
        }
    }

    #[test]
    fn key_counts_group_keyed_rows_per_platform() {
        let pool = PoolStatusInfo { degraded: false, healthy_ratio: 1.0, benched: vec![] };
        let keys = vec![
            key("nvidia", true, true),
            key("nvidia", true, true),
            key("google", true, true),
            key("openai", false, false),
        ];
        let snap = aggregate(&pool, &keys, &empty_coord(), &[]);
        assert_eq!(snap.total_keys, 3);
        assert_eq!(snap.provider_count, 2);
        assert_eq!(snap.providers[0].platform, "nvidia");
        assert_eq!(snap.providers[0].key_count, 2);
        assert_eq!(snap.tone, HealthTone::Healthy);
    }

    #[test]
    fn degraded_forces_red_tone_even_without_benches() {
        let pool = PoolStatusInfo { degraded: true, healthy_ratio: 0.2, benched: vec![] };
        let snap = aggregate(&pool, &[], &empty_coord(), &[]);
        assert_eq!(snap.tone, HealthTone::Degraded);
    }

    #[test]
    fn bench_or_cooldown_yields_amber_when_not_degraded() {
        let pool = PoolStatusInfo {
            degraded: false,
            healthy_ratio: 0.9,
            benched: vec![PoolBenchedKey {
                platform: "openai".into(),
                model: "gpt".into(),
                key_id: "ab12".into(),
                remaining_secs: 42,
                provenance: "authoritative".into(),
            }],
        };
        let snap = aggregate(&pool, &[], &empty_coord(), &[]);
        assert_eq!(snap.tone, HealthTone::Amber);
        assert_eq!(snap.benches.len(), 1);
    }

    #[test]
    fn activity_prefers_running_then_waiting_then_queued() {
        let mut coord = empty_coord();
        coord.queued_goals = vec![goal("q1", "queued")];
        coord.waiting_goals = vec![goal("w1", "waiting_on_capacity")];
        coord.running_goals = vec![goal("r1", "running")];
        let pool = PoolStatusInfo { degraded: false, healthy_ratio: 1.0, benched: vec![] };
        let snap = aggregate(&pool, &[], &coord, &[]);
        assert_eq!(snap.activity.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), vec!["r1", "w1", "q1"]);
        assert!(snap.any_goal_running);
    }

    #[test]
    fn agent_auth_dots_preserve_auth_state() {
        let agents = vec![
            divisi_protocol::AgentInfo {
                name: "grok".into(),
                adapter: "grok".into(),
                command: "grok".into(),
                detected: true,
                version: None,
                install_method: divisi_protocol::InstallMethod::StandaloneBinary { detail: "test fixture".into() },
                bootstrap_install: None,
                unverified: false,
                home_requirement: Default::default(),
                max_concurrency: None,
                capabilities: Default::default(),
                config_paths: vec![],
                notes: None,
                authenticated: AuthState::Authenticated,
            },
        ];
        let pool = PoolStatusInfo { degraded: false, healthy_ratio: 1.0, benched: vec![] };
        let snap = aggregate(&pool, &[], &empty_coord(), &agents);
        assert_eq!(snap.agents.len(), 1);
        assert_eq!(snap.agents[0].name, "grok");
        assert_eq!(snap.agents[0].state, AuthState::Authenticated);
    }

    #[test]
    fn snapshot_serializes_the_shape_the_gnome_extension_reads() {
        let pool = PoolStatusInfo { degraded: false, healthy_ratio: 1.0, benched: vec![] };
        let snap = aggregate(&pool, &[], &empty_coord(), &[]);
        let v: serde_json::Value = serde_json::to_value(&snap).unwrap();
        assert!(matches!(v["tone"].as_str(), Some("healthy" | "amber" | "degraded")), "tone must be snake_case: {v}");
        for key in ["healthy_ratio", "total_keys", "providers", "benches", "activity", "agents"] {
            assert!(v.get(key).is_some(), "extension.js reads `{key}`: {v}");
        }
    }

    #[test]
    fn brief_keeps_the_first_line_and_caps_length() {
        assert_eq!(brief("\n  first line\nsecond", 50), "first line");
        assert_eq!(brief("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn detail_counts_goals_by_state_and_flags_only_explicitly_disabled_keys() {
        let mut k = key("nvidia", true, false);
        let mut bad = key("google", true, false);
        bad.disabled_reason = Some("401 unauthorized".into());
        k.disabled_reason = None;
        let d = build_detail(&[k, bad], &empty_coord(), &[]);
        assert_eq!(d.problem_keys.len(), 1);
        assert_eq!(d.problem_keys[0].platform, "google");
        assert_eq!(d.goals_running + d.goals_queued + d.goals_waiting + d.goals_blocked, 0);
    }
}
