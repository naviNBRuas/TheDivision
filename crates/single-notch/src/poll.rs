//! Periodic fetch of the four coordinator/pool ops, aggregated into a
//! `NotchSnapshot`, plus the notable-event diff against the previous
//! tick. See `docs/superpowers/plans/2026-09-17-e30-notch-hud.md` Phase 3
//! Task 6.

use crate::client;
use crate::model::{self, NotchSnapshot};
use anyhow::{Context, Result};
use single_protocol::{AuthState, Request, Response, ResponseData};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotableEvent {
    BenchAdded,
    BenchCleared,
    DegradedFlipped,
    GoalTerminal,
    AuthLost,
    HealthyRatioDrop,
}

const TERMINAL_STATUSES: &[&str] = &["done", "failed", "cancelled"];

/// Diff rules (spec table, exact): new/cleared benches, a `degraded`
/// flip, a goal newly entering a terminal status, an agent's auth going
/// from `Authenticated` to anything else, and a `healthy_ratio` drop of
/// >= 0.25 absolute. Everything else (pure headroom ticks, unchanged
/// state) emits nothing.
pub fn diff_notable(prev: &NotchSnapshot, next: &NotchSnapshot) -> Vec<NotableEvent> {
    let mut events = Vec::new();

    if next.benches.len() > prev.benches.len() {
        events.push(NotableEvent::BenchAdded);
    } else if next.benches.len() < prev.benches.len() {
        events.push(NotableEvent::BenchCleared);
    }

    if prev.degraded != next.degraded {
        events.push(NotableEvent::DegradedFlipped);
    }

    let prev_by_id: HashMap<&str, &str> = prev.activity.iter().map(|a| (a.id.as_str(), a.status.as_str())).collect();
    let goal_went_terminal = next.activity.iter().any(|a| {
        let was_nonterminal = prev_by_id.get(a.id.as_str()).is_some_and(|s| !TERMINAL_STATUSES.contains(s));
        was_nonterminal && TERMINAL_STATUSES.contains(&a.status.as_str())
    });
    // A goal that left `activity` entirely because it went terminal (the
    // aggregator only keeps running/waiting/queued rows) is the more
    // common real case -- a goal present last tick, absent this tick, and
    // not simply replaced by a new one at the same id.
    let goal_disappeared = prev.activity.iter().any(|p| !TERMINAL_STATUSES.contains(&p.status.as_str()) && !next.activity.iter().any(|n| n.id == p.id));
    if goal_went_terminal || goal_disappeared {
        events.push(NotableEvent::GoalTerminal);
    }

    let prev_auth: HashMap<&str, AuthState> = prev.agents.iter().map(|a| (a.name.as_str(), a.state)).collect();
    let auth_lost = next.agents.iter().any(|a| {
        prev_auth.get(a.name.as_str()).is_some_and(|&s| s == AuthState::Authenticated) && a.state != AuthState::Authenticated
    });
    if auth_lost {
        events.push(NotableEvent::AuthLost);
    }

    if prev.healthy_ratio - next.healthy_ratio >= 0.25 {
        events.push(NotableEvent::HealthyRatioDrop);
    }

    events
}

pub struct Poller {
    pub socket: PathBuf,
    pub poll_ms_idle: u64,
    pub poll_ms_active: u64,
}

fn expect<T>(response: Response, extract: impl FnOnce(ResponseData) -> Option<T>) -> Result<T> {
    match response {
        Response::Ok { data } => extract(data).context("unexpected response shape for this request"),
        Response::Error { message } => anyhow::bail!("daemon returned error: {message}"),
    }
}

impl Poller {
    /// Tries the composite `NotchSnapshot` op first (E30 Phase 7) -- one
    /// round trip instead of four. An older daemon that doesn't know
    /// this request answers `Response::Error` (unknown request), which
    /// falls back to `tick_four_op` transparently; any other transport
    /// error (daemon down) also fails over rather than erroring the
    /// whole poll on what's meant to be a pure optimization.
    pub fn tick(&self) -> Result<model::NotchSnapshot> {
        match client::call(&self.socket, &Request::NotchSnapshot) {
            Ok(Response::Ok { data: ResponseData::NotchSnapshot(s) }) => {
                Ok(model::aggregate(&s.pool, &s.keys, &s.coordinator, &s.agents))
            }
            _ => self.tick_four_op(),
        }
    }

    fn tick_four_op(&self) -> Result<model::NotchSnapshot> {
        let pool = expect(client::call(&self.socket, &Request::PoolStatus)?, |d| match d {
            ResponseData::PoolStatus(p) => Some(p),
            _ => None,
        })?;
        let keys = expect(client::call(&self.socket, &Request::ProviderKeyStatus { platform: None })?, |d| match d {
            ResponseData::PoolKeyStatuses(k) => Some(k),
            _ => None,
        })?;
        let coord = expect(client::call(&self.socket, &Request::CoordinatorStatus)?, |d| match d {
            ResponseData::CoordinatorSnapshot(c) => Some(c),
            _ => None,
        })?;
        let agents = expect(client::call(&self.socket, &Request::AgentList)?, |d| match d {
            ResponseData::Agents(a) => Some(a),
            _ => None,
        })?;
        Ok(model::aggregate(&pool, &keys, &coord, &agents))
    }

    pub fn interval_ms(&self, expanded: bool, notable_recent: bool) -> u64 {
        if expanded || notable_recent {
            self.poll_ms_active
        } else {
            self.poll_ms_idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentAuthDot, BenchRow, GoalActivity, HealthTone, NotchSnapshot, ProviderTally};

    fn empty_snapshot() -> NotchSnapshot {
        NotchSnapshot {
            tone: HealthTone::Healthy,
            degraded: false,
            healthy_ratio: 1.0,
            provider_count: 0,
            total_keys: 0,
            providers: vec![],
            benches: vec![],
            activity: vec![],
            agents: vec![],
            any_goal_running: false,
        }
    }

    #[test]
    fn unchanged_snapshots_emit_nothing() {
        let s = empty_snapshot();
        assert!(diff_notable(&s, &s).is_empty());
    }

    #[test]
    fn new_bench_emits_bench_added() {
        let prev = empty_snapshot();
        let mut next = empty_snapshot();
        next.benches = vec![BenchRow { platform: "openai".into(), model: "gpt".into(), key_id: "a".into(), remaining_secs: 10, provenance: "x".into() }];
        assert_eq!(diff_notable(&prev, &next), vec![NotableEvent::BenchAdded]);
    }

    #[test]
    fn cleared_bench_emits_bench_cleared() {
        let mut prev = empty_snapshot();
        prev.benches = vec![BenchRow { platform: "openai".into(), model: "gpt".into(), key_id: "a".into(), remaining_secs: 10, provenance: "x".into() }];
        let next = empty_snapshot();
        assert_eq!(diff_notable(&prev, &next), vec![NotableEvent::BenchCleared]);
    }

    #[test]
    fn degraded_flip_emits_event() {
        let prev = empty_snapshot();
        let mut next = empty_snapshot();
        next.degraded = true;
        assert_eq!(diff_notable(&prev, &next), vec![NotableEvent::DegradedFlipped]);
    }

    #[test]
    fn goal_entering_terminal_emits_event() {
        let mut prev = empty_snapshot();
        prev.activity = vec![GoalActivity { id: "g1".into(), text: "t".into(), status: "running".into() }];
        let next = empty_snapshot(); // g1 dropped out -- went terminal
        assert_eq!(diff_notable(&prev, &next), vec![NotableEvent::GoalTerminal]);
    }

    #[test]
    fn agent_auth_lost_emits_event() {
        let mut prev = empty_snapshot();
        prev.agents = vec![AgentAuthDot { name: "grok".into(), state: AuthState::Authenticated }];
        let mut next = empty_snapshot();
        next.agents = vec![AgentAuthDot { name: "grok".into(), state: AuthState::NotAuthenticated }];
        assert_eq!(diff_notable(&prev, &next), vec![NotableEvent::AuthLost]);
    }

    #[test]
    fn healthy_ratio_drop_of_quarter_or_more_emits_event() {
        let mut prev = empty_snapshot();
        prev.healthy_ratio = 0.9;
        let mut next = empty_snapshot();
        next.healthy_ratio = 0.6;
        assert_eq!(diff_notable(&prev, &next), vec![NotableEvent::HealthyRatioDrop]);
    }

    #[test]
    fn small_healthy_ratio_dip_emits_nothing() {
        let mut prev = empty_snapshot();
        prev.healthy_ratio = 0.9;
        let mut next = empty_snapshot();
        next.healthy_ratio = 0.8;
        assert!(diff_notable(&prev, &next).is_empty());
    }

    #[test]
    fn interval_ms_prefers_active_when_expanded_or_notable_recent() {
        let p = Poller { socket: PathBuf::from("/tmp/does-not-exist.sock"), poll_ms_idle: 1000, poll_ms_active: 400 };
        assert_eq!(p.interval_ms(true, false), 400);
        assert_eq!(p.interval_ms(false, true), 400);
        assert_eq!(p.interval_ms(false, false), 1000);
    }

    // avoid unused-import warning for ProviderTally in doc-adjacent test scaffolding
    #[allow(dead_code)]
    fn _unused(_: ProviderTally) {}

    /// Phase 3 Task 7: live smoke test against a real `single-runtimed`.
    /// `#[ignore]`d (needs a live daemon + real socket path) — run
    /// explicitly with `cargo test -p single-notch -- --ignored poll::tests::live_tick_against_real_daemon`.
    #[test]
    #[ignore]
    fn live_tick_against_real_daemon() {
        let socket = single_core::SingleDirs::discover().unwrap().socket_path();
        let poller = Poller { socket, poll_ms_idle: 1000, poll_ms_active: 400 };
        let snap = poller.tick().unwrap();
        println!("{snap:?}");
    }
}
