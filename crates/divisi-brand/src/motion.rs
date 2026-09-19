//! Spin & pop: "/" morphs to "÷" (progress 0.0 -> 1.0) and back.

pub const SLASH_ANGLE: f32 = -62.0;
pub const BAR_SLASH_LEN: f32 = 44.0;
pub const BAR_OBELUS_LEN: f32 = 32.0;
pub const DOT_OFFSET: f32 = 14.0;
pub const DOT_RADIUS: f32 = 4.6;
pub const LOOP_SECS: f32 = 4.2;

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

/// The 4.2s loop: hold "/" 0.5s, morph in 1.1s, hold "÷" 0.9s, morph out 1.1s, rest 0.6s.
pub fn loop_at(t_secs: f32) -> LoopFrame {
    let t = t_secs.rem_euclid(LOOP_SECS);
    let (progress, phase) = if t < 0.5 {
        (0.0, MarkPhase::Idle)
    } else if t < 1.6 {
        ((t - 0.5) / 1.1, MarkPhase::Starting)
    } else if t < 2.5 {
        (1.0, MarkPhase::Working)
    } else if t < 3.6 {
        (1.0 - (t - 2.5) / 1.1, MarkPhase::Finishing)
    } else {
        (0.0, MarkPhase::Idle)
    };
    LoopFrame { progress, phase, state: state_at(progress) }
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
    fn loop_follows_the_timeline() {
        let idle = loop_at(0.2);
        assert_eq!((idle.progress, idle.phase), (0.0, MarkPhase::Idle));
        let starting = loop_at(1.05);
        assert!((starting.progress - 0.5).abs() < 1e-3);
        assert_eq!(starting.phase, MarkPhase::Starting);
        let working = loop_at(2.0);
        assert_eq!((working.progress, working.phase), (1.0, MarkPhase::Working));
        let finishing = loop_at(3.05);
        assert!((finishing.progress - 0.5).abs() < 1e-3);
        assert_eq!(finishing.phase, MarkPhase::Finishing);
        assert_eq!(loop_at(4.0).phase, MarkPhase::Idle);
    }

    #[test]
    fn loop_wraps() {
        assert!((loop_at(4.2 + 1.05).progress - loop_at(1.05).progress).abs() < 1e-3);
    }
}
