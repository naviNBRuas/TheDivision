//! Pure aggregation of the four coordinator/pool wire types into one
//! `NotchSnapshot` the UI renders. No iced, no I/O -- fully unit-testable.
//! See `docs/superpowers/plans/2026-09-17-e30-notch-hud.md` Phase 3 Task 5.

use single_protocol::{AgentInfo, AuthState, CoordinatorSnapshot, GoalSummary, PoolBenchedKey, PoolKeyStatusInfo, PoolStatusInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthTone {
    Healthy,
    Amber,
    Degraded,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderTally {
    pub platform: String,
    pub key_count: usize,
    pub cooldown: String,
    pub headroom: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BenchRow {
    pub platform: String,
    pub model: String,
    pub key_id: String,
    pub remaining_secs: u64,
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GoalActivity {
    pub id: String,
    pub text: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentAuthDot {
    pub name: String,
    pub state: AuthState,
}

#[derive(Debug, Clone, PartialEq)]
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
            single_protocol::AgentInfo {
                name: "grok".into(),
                adapter: "grok".into(),
                command: "grok".into(),
                detected: true,
                version: None,
                install_method: single_protocol::InstallMethod::StandaloneBinary { detail: "test fixture".into() },
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
}
