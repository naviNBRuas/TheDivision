//! Degraded-mode health state machine — spec §6.5. Pure transition
//! function over a caller-supplied healthy ratio; owns no I/O so it's
//! trivially testable and cheap to call on every tick.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Degraded,
}

#[derive(Debug, Clone, Copy)]
pub struct DegradeState {
    pub mode: Mode,
    /// When the ratio first crossed into the *opposite* zone of the
    /// current mode -- used to measure whether `entry_grace`/`exit_grace`
    /// has elapsed. `None` means "not currently crossing."
    pub since_ms: Option<i64>,
}

impl Default for DegradeState {
    fn default() -> Self {
        DegradeState { mode: Mode::Normal, since_ms: None }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DegradeConfig {
    pub healthy_ratio: f64,
    pub min_providers: usize,
    pub entry_grace_ms: i64,
    pub exit_grace_ms: i64,
}

impl Default for DegradeConfig {
    fn default() -> Self {
        DegradeConfig { healthy_ratio: 0.5, min_providers: 3, entry_grace_ms: 60_000, exit_grace_ms: 120_000 }
    }
}

impl DegradeConfig {
    /// `SINGLE_POOL_DEGRADED_{HEALTHY_RATIO,MIN_PROVIDERS,ENTRY_GRACE_MS,EXIT_GRACE_MS}`
    /// overrides (spec §6.5); malformed/missing values fall back to the
    /// struct default for that field.
    pub fn from_env() -> Self {
        let base = Self::default();
        DegradeConfig {
            healthy_ratio: env_f64("SINGLE_POOL_DEGRADED_HEALTHY_RATIO").unwrap_or(base.healthy_ratio),
            min_providers: env_usize("SINGLE_POOL_DEGRADED_MIN_PROVIDERS").unwrap_or(base.min_providers),
            entry_grace_ms: env_i64("SINGLE_POOL_DEGRADED_ENTRY_GRACE_MS").unwrap_or(base.entry_grace_ms),
            exit_grace_ms: env_i64("SINGLE_POOL_DEGRADED_EXIT_GRACE_MS").unwrap_or(base.exit_grace_ms),
        }
    }
}

fn env_f64(name: &str) -> Option<f64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}
fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}
fn env_i64(name: &str) -> Option<i64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// Unknown key status counts as healthy (spec §6.5) — the caller
/// pre-counts `usable_keys` that way before calling this.
pub fn healthy_ratio(usable_keys: usize, enabled_providers: usize) -> f64 {
    if enabled_providers == 0 {
        return 1.0;
    }
    usable_keys as f64 / enabled_providers as f64
}

/// Pure transition: `Normal -> Degraded` when `ratio < healthy_ratio`
/// sustained for `entry_grace_ms`; `Degraded -> Normal` when
/// `ratio >= healthy_ratio` sustained for `exit_grace_ms` (hysteresis —
/// the two thresholds and two grace periods differ so the state doesn't
/// flap at the boundary). No transition below `min_providers` enabled.
pub fn tick(state: &mut DegradeState, ratio: f64, enabled_count: usize, now_ms: i64, cfg: &DegradeConfig) -> Mode {
    if enabled_count < cfg.min_providers {
        state.since_ms = None;
        return state.mode;
    }

    match state.mode {
        Mode::Normal => {
            if ratio < cfg.healthy_ratio {
                let crossing_since = *state.since_ms.get_or_insert(now_ms);
                if now_ms - crossing_since >= cfg.entry_grace_ms {
                    state.mode = Mode::Degraded;
                    state.since_ms = Some(now_ms);
                }
            } else {
                state.since_ms = None;
            }
        }
        Mode::Degraded => {
            if ratio >= cfg.healthy_ratio {
                let crossing_since = *state.since_ms.get_or_insert(now_ms);
                if now_ms - crossing_since >= cfg.exit_grace_ms {
                    state.mode = Mode::Normal;
                    state.since_ms = Some(now_ms);
                }
            } else {
                state.since_ms = None;
            }
        }
    }
    state.mode
}

pub fn exploration_enabled(mode: Mode) -> bool {
    mode == Mode::Normal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DegradeConfig {
        DegradeConfig::default()
    }

    #[test]
    fn enters_degraded_after_entry_grace_below_threshold() {
        let mut state = DegradeState::default();
        let cfg = cfg();
        let now = 1_700_000_000_000;

        assert_eq!(tick(&mut state, 0.3, 5, now, &cfg), Mode::Normal); // crossing starts
        assert_eq!(tick(&mut state, 0.3, 5, now + cfg.entry_grace_ms - 1, &cfg), Mode::Normal); // not yet
        assert_eq!(tick(&mut state, 0.3, 5, now + cfg.entry_grace_ms, &cfg), Mode::Degraded);
    }

    #[test]
    fn does_not_flap_before_entry_grace_elapsed() {
        let mut state = DegradeState::default();
        let cfg = cfg();
        let now = 1_700_000_000_000;

        tick(&mut state, 0.3, 5, now, &cfg);
        // Ratio recovers before entry_grace elapses -> crossing resets, no transition.
        assert_eq!(tick(&mut state, 0.9, 5, now + 30_000, &cfg), Mode::Normal);
        assert_eq!(tick(&mut state, 0.3, 5, now + 40_000, &cfg), Mode::Normal); // fresh crossing, not yet elapsed
    }

    #[test]
    fn exits_degraded_after_exit_grace_above_threshold() {
        let mut state = DegradeState { mode: Mode::Degraded, since_ms: None };
        let cfg = cfg();
        let now = 1_700_000_000_000;

        assert_eq!(tick(&mut state, 0.9, 5, now, &cfg), Mode::Degraded);
        assert_eq!(tick(&mut state, 0.9, 5, now + cfg.exit_grace_ms - 1, &cfg), Mode::Degraded);
        assert_eq!(tick(&mut state, 0.9, 5, now + cfg.exit_grace_ms, &cfg), Mode::Normal);
    }

    #[test]
    fn no_transition_below_min_providers() {
        let mut state = DegradeState::default();
        let cfg = cfg();
        let now = 1_700_000_000_000;
        // Only 2 providers enabled (< min_providers=3) -> never transitions
        // even with a terrible ratio held for a long time.
        for i in 0..10 {
            assert_eq!(tick(&mut state, 0.0, 2, now + i * cfg.entry_grace_ms, &cfg), Mode::Normal);
        }
    }

    #[test]
    fn exploration_disabled_in_degraded_mode() {
        assert!(exploration_enabled(Mode::Normal));
        assert!(!exploration_enabled(Mode::Degraded));
    }
}
