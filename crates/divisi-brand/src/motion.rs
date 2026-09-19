//! The divisi mark. At rest it is the obelus "÷" (the logo). While work is running
//! it becomes a bare spinning slash "/": the dots collapse, the bar spins into the
//! slash and keeps turning, then settles back into "÷" with a spring when the work
//! ends. `state_at` is the morph between the two (progress 0.0 = "/", 1.0 = "÷").

pub const SLASH_ANGLE: f32 = -62.0;
pub const BAR_SLASH_LEN: f32 = 44.0;
pub const BAR_OBELUS_LEN: f32 = 32.0;
pub const DOT_OFFSET: f32 = 14.0;
pub const DOT_RADIUS: f32 = 4.6;
/// Hold the obelus at rest.
pub const IDLE_SECS: f32 = 0.8;
/// Obelus to slash (the dots collapse as the bar spins).
pub const ENTER_SECS: f32 = 0.6;
/// One full turn of the working slash.
pub const SPIN_SECS: f32 = 1.2;
/// Working time in the demo loop: two full turns.
pub const WORK_SECS: f32 = 2.4;
/// Slash back to obelus (the dots spring in).
pub const EXIT_SECS: f32 = 1.0;
/// Demo loop: rest, start, work, finish (4.8s).
pub const LOOP_SECS: f32 = IDLE_SECS + ENTER_SECS + WORK_SECS + EXIT_SECS;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkState {
    pub angle_deg: f32,
    pub bar_len: f32,
    pub dot_offset: f32,
    pub dot_radius: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkPhase {
    Idle,
    Starting,
    Working,
    Finishing,
}

impl MarkPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            MarkPhase::Idle => "idle",
            MarkPhase::Starting => "starting",
            MarkPhase::Working => "working",
            MarkPhase::Finishing => "finishing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoopFrame {
    pub progress: f32,
    pub phase: MarkPhase,
    pub state: MarkState,
}

fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

fn ease_in_out_cubic(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

fn ease_out_back(t: f32) -> f32 {
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

/// Mark geometry at `progress` (0.0 = "/", 1.0 = "÷"); the reverse morph is
/// the same function walked backwards.
pub fn state_at(progress: f32) -> MarkState {
    let r = clamp01(progress);
    let e = ease_in_out_cubic(r);
    let dots = ease_out_back(clamp01((r - 0.5) / 0.5)).max(0.0);
    MarkState {
        angle_deg: SLASH_ANGLE + (-360.0 - SLASH_ANGLE) * e,
        bar_len: BAR_SLASH_LEN + (BAR_OBELUS_LEN - BAR_SLASH_LEN) * e,
        dot_offset: DOT_OFFSET,
        dot_radius: DOT_RADIUS * dots,
    }
}

/// The bare working slash: no dots, turning one full revolution every `SPIN_SECS`,
/// starting exactly at the slash angle so it joins the morphs without a jump.
pub fn working_at(t_secs: f32) -> MarkState {
    let turns = (t_secs / SPIN_SECS).rem_euclid(1.0);
    MarkState { angle_deg: SLASH_ANGLE - 360.0 * turns, bar_len: BAR_SLASH_LEN, dot_offset: DOT_OFFSET, dot_radius: 0.0 }
}

/// The 4.8s demo loop: rest as "÷" 0.8s, spin into "/" 0.6s, work as a spinning "/" 2.4s,
/// settle back into "÷" 1.0s. `progress` is the morph progress (1.0 = "÷", 0.0 = "/").
pub fn loop_at(t_secs: f32) -> LoopFrame {
    let t = t_secs.rem_euclid(LOOP_SECS);
    let work_start = IDLE_SECS + ENTER_SECS;
    let exit_start = work_start + WORK_SECS;
    if t < IDLE_SECS {
        LoopFrame { progress: 1.0, phase: MarkPhase::Idle, state: state_at(1.0) }
    } else if t < work_start {
        let p = 1.0 - (t - IDLE_SECS) / ENTER_SECS;
        LoopFrame { progress: p, phase: MarkPhase::Starting, state: state_at(p) }
    } else if t < exit_start {
        LoopFrame { progress: 0.0, phase: MarkPhase::Working, state: working_at(t - work_start) }
    } else {
        let p = (t - exit_start) / EXIT_SECS;
        LoopFrame { progress: p, phase: MarkPhase::Finishing, state: state_at(p) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_zero_is_the_slash() {
        let s = state_at(0.0);
        assert_eq!(s.angle_deg, -62.0);
        assert_eq!(s.bar_len, 44.0);
        assert!(s.dot_radius.abs() < 1e-4, "dots hidden at rest: {}", s.dot_radius);
    }

    #[test]
    fn progress_one_is_the_obelus() {
        let s = state_at(1.0);
        assert!((s.angle_deg - -360.0).abs() < 1e-3);
        assert!((s.bar_len - 32.0).abs() < 1e-3);
        assert!((s.dot_radius - DOT_RADIUS).abs() < 1e-3);
        assert_eq!(s.dot_offset, DOT_OFFSET);
    }

    #[test]
    fn dots_stay_hidden_until_halfway_then_overshoot() {
        assert!(state_at(0.5).dot_radius.abs() < 1e-4);
        assert!(state_at(0.3).dot_radius.abs() < 1e-4);
        let peak = (0..=100).map(|i| state_at(i as f32 / 100.0).dot_radius).fold(0.0_f32, f32::max);
        assert!(peak > DOT_RADIUS, "spring should overshoot, peak was {peak}");
    }

    #[test]
    fn the_bar_only_turns_one_way() {
        let mut prev = state_at(0.0).angle_deg;
        for i in 1..=100 {
            let a = state_at(i as f32 / 100.0).angle_deg;
            assert!(a <= prev + 1e-4, "angle went back up at {i}: {prev} -> {a}");
            prev = a;
        }
    }

    #[test]
    fn progress_is_clamped() {
        assert_eq!(state_at(-3.0), state_at(0.0));
        assert_eq!(state_at(9.0), state_at(1.0));
    }

    #[test]
    fn working_is_a_bare_spinning_slash() {
        for t in [0.0_f32, 0.3, 0.6, 0.9, 1.19] {
            let s = working_at(t);
            assert_eq!(s.dot_radius, 0.0, "no dots while working");
            assert_eq!(s.bar_len, BAR_SLASH_LEN);
        }
        assert_eq!(working_at(0.0).angle_deg, SLASH_ANGLE, "starts exactly at the slash");
        assert_ne!(working_at(0.3).angle_deg, working_at(0.6).angle_deg, "and it turns");
    }

    #[test]
    fn a_spin_period_returns_to_the_slash_angle() {
        let a = working_at(SPIN_SECS - 1e-4).angle_deg;
        assert!(((a - SLASH_ANGLE).rem_euclid(360.0)).min(360.0 - (a - SLASH_ANGLE).rem_euclid(360.0)) < 1.0, "angle {a}");
    }

    #[test]
    fn loop_rests_on_the_obelus_and_works_as_a_slash() {
        let idle = loop_at(0.3);
        assert_eq!(idle.phase, MarkPhase::Idle);
        assert_eq!(idle.state, state_at(1.0), "at rest the mark is the obelus");

        let starting = loop_at(1.1);
        assert_eq!(starting.phase, MarkPhase::Starting);
        assert!((starting.progress - 0.5).abs() < 1e-3, "halfway from obelus to slash");

        let working = loop_at(2.5);
        assert_eq!(working.phase, MarkPhase::Working);
        assert_eq!(working.state.dot_radius, 0.0);
        assert_eq!(working.state.bar_len, BAR_SLASH_LEN);

        let finishing = loop_at(4.3);
        assert_eq!(finishing.phase, MarkPhase::Finishing);
        assert!((finishing.progress - 0.5).abs() < 1e-3, "halfway from slash back to obelus");
    }

    #[test]
    fn the_loop_is_continuous_at_every_seam() {
        let close = |a: MarkState, b: MarkState| {
            ((a.angle_deg - b.angle_deg).rem_euclid(360.0).min(360.0 - (a.angle_deg - b.angle_deg).rem_euclid(360.0)) < 2.0)
                && (a.bar_len - b.bar_len).abs() < 0.5
                && (a.dot_radius - b.dot_radius).abs() < 0.5
        };
        // idle -> starting, starting -> working, working -> finishing, finishing -> idle (wrap)
        for seam in [IDLE_SECS, IDLE_SECS + ENTER_SECS, IDLE_SECS + ENTER_SECS + WORK_SECS, LOOP_SECS] {
            let before = loop_at(seam - 1e-3).state;
            let after = loop_at(seam + 1e-3).state;
            assert!(close(before, after), "jump at {seam}s: {before:?} -> {after:?}");
        }
    }

    #[test]
    fn loop_wraps() {
        assert_eq!(loop_at(LOOP_SECS + 1.1).phase, loop_at(1.1).phase);
        assert!((loop_at(LOOP_SECS + 1.1).progress - loop_at(1.1).progress).abs() < 1e-3);
    }
}
