//! Expand/collapse animation state machine, pure and clock-injectable
//! (`now_ms` is always passed in, never read from the wall clock) so it's
//! fully unit-testable without a real event loop. See
//! `docs/superpowers/plans/2026-09-17-e30-notch-hud.md` Phase 5 Task 9.

const PILL: (f32, f32) = (128.0, 28.0);
const CARD: (f32, f32) = (340.0, 148.0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimPhase {
    Collapsed,
    Expanding,
    Expanded,
    Collapsing,
}

#[derive(Debug, Clone, Copy)]
pub struct AnimConfig {
    pub expand_ms: u64,
    pub collapse_ms: u64,
    pub auto_hide_ms: u64,
    pub hover_leave_ms: u64,
    pub reduced_motion: bool,
}

impl Default for AnimConfig {
    fn default() -> Self {
        AnimConfig { expand_ms: 220, collapse_ms: 280, auto_hide_ms: 2500, hover_leave_ms: 400, reduced_motion: false }
    }
}

pub struct AnimState {
    config: AnimConfig,
    phase: AnimPhase,
    t0_ms: u64,
    progress: f32,
    pointer_inside: bool,
    /// Set whenever something (hover-leave debounce or a notable event's
    /// auto-hide window) should keep the card up until this timestamp,
    /// even with the pointer outside. Cleared once honored.
    hold_until_ms: Option<u64>,
}

impl AnimState {
    pub fn new(config: AnimConfig) -> Self {
        AnimState { config, phase: AnimPhase::Collapsed, t0_ms: 0, progress: 0.0, pointer_inside: false, hold_until_ms: None }
    }

    fn begin_expand(&mut self, now_ms: u64) {
        if self.phase != AnimPhase::Expanded {
            self.phase = AnimPhase::Expanding;
            self.t0_ms = now_ms;
        }
    }

    fn begin_collapse(&mut self, now_ms: u64) {
        if self.phase != AnimPhase::Collapsed {
            self.phase = AnimPhase::Collapsing;
            self.t0_ms = now_ms;
        }
    }

    pub fn on_pointer_enter(&mut self, now_ms: u64) {
        self.pointer_inside = true;
        self.hold_until_ms = None;
        self.begin_expand(now_ms);
    }

    pub fn on_pointer_leave(&mut self, now_ms: u64) {
        self.pointer_inside = false;
        // grace period before actually starting to collapse, so a quick
        // flick across the pill doesn't immediately snap it shut.
        self.hold_until_ms = Some(now_ms + self.config.hover_leave_ms);
    }

    pub fn on_notable(&mut self, now_ms: u64) {
        self.hold_until_ms = Some(now_ms + self.config.auto_hide_ms);
        self.begin_expand(now_ms);
    }

    /// Collapses immediately -- no `Collapsing` tween, straight to
    /// `Collapsed`, matching the spec's "Escape collapses immediately".
    pub fn on_escape(&mut self, now_ms: u64) {
        self.hold_until_ms = None;
        self.phase = AnimPhase::Collapsed;
        self.t0_ms = now_ms;
        self.progress = 0.0;
    }

    pub fn force_expand(&mut self, now_ms: u64) {
        self.hold_until_ms = None;
        self.begin_expand(now_ms);
    }

    pub fn force_collapse(&mut self, now_ms: u64) {
        self.hold_until_ms = None;
        self.begin_collapse(now_ms);
    }

    pub fn tick(&mut self, now_ms: u64) -> AnimPhase {
        match self.phase {
            AnimPhase::Expanding => {
                let elapsed = now_ms.saturating_sub(self.t0_ms) as f32;
                self.progress = (elapsed / self.config.expand_ms as f32).clamp(0.0, 1.0);
                if self.progress >= 1.0 {
                    self.phase = AnimPhase::Expanded;
                }
            }
            AnimPhase::Collapsing => {
                let elapsed = now_ms.saturating_sub(self.t0_ms) as f32;
                self.progress = 1.0 - (elapsed / self.config.collapse_ms as f32).clamp(0.0, 1.0);
                if self.progress <= 0.0 {
                    self.phase = AnimPhase::Collapsed;
                }
            }
            AnimPhase::Expanded => {
                // Once past any held-until deadline and the pointer is
                // outside, start collapsing on the very next tick.
                if !self.pointer_inside {
                    if let Some(hold) = self.hold_until_ms {
                        if now_ms >= hold {
                            self.hold_until_ms = None;
                            self.begin_collapse(now_ms);
                        }
                    }
                }
            }
            AnimPhase::Collapsed => {}
        }
        self.phase
    }

    /// `(width, height, opacity)`, lerped pill -> card by `progress`.
    /// Reduced motion skips the geometry tween entirely -- size snaps to
    /// whichever endpoint the current phase implies, only opacity animates.
    pub fn width_height_opacity(&self) -> (f32, f32, f32) {
        if self.config.reduced_motion {
            let expanded = matches!(self.phase, AnimPhase::Expanding | AnimPhase::Expanded);
            let (w, h) = if expanded { CARD } else { PILL };
            let opacity = match self.phase {
                AnimPhase::Expanding | AnimPhase::Collapsing => self.progress,
                AnimPhase::Expanded | AnimPhase::Collapsed => 1.0,
            };
            return (w, h, opacity);
        }
        let t = ease_out_cubic(self.progress);
        let w = PILL.0 + (CARD.0 - PILL.0) * t;
        let h = PILL.1 + (CARD.1 - PILL.1) * t;
        (w, h, 1.0)
    }
}

pub fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

pub fn ease_in_cubic(t: f32) -> f32 {
    t.powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_expands_and_leave_collapses_after_hover_leave_ms() {
        let mut a = AnimState::new(AnimConfig { expand_ms: 100, collapse_ms: 100, hover_leave_ms: 50, ..Default::default() });
        a.on_pointer_enter(0);
        assert_eq!(a.tick(100), AnimPhase::Expanded);

        a.on_pointer_leave(1000);
        // still within the hover-leave grace period -- must not have
        // started collapsing yet.
        assert_eq!(a.tick(1010), AnimPhase::Expanded);
        // past the grace period -- collapsing should now be under way.
        assert_eq!(a.tick(1060), AnimPhase::Collapsing);
        assert_eq!(a.tick(1200), AnimPhase::Collapsed);
    }

    #[test]
    fn notable_holds_auto_hide_ms_then_collapses_if_pointer_outside() {
        let mut a = AnimState::new(AnimConfig { expand_ms: 50, collapse_ms: 50, auto_hide_ms: 500, ..Default::default() });
        a.on_notable(0);
        assert_eq!(a.tick(50), AnimPhase::Expanded);
        // well before auto_hide_ms elapses -- still held open.
        assert_eq!(a.tick(400), AnimPhase::Expanded);
        // past auto_hide_ms, pointer never entered -- collapses.
        assert_eq!(a.tick(600), AnimPhase::Collapsing);
    }

    #[test]
    fn escape_collapses_immediately() {
        let mut a = AnimState::new(AnimConfig { expand_ms: 100, collapse_ms: 100, ..Default::default() });
        a.on_pointer_enter(0);
        a.tick(100);
        assert_eq!(a.tick(100), AnimPhase::Expanded);
        a.on_escape(150);
        assert_eq!(a.tick(150), AnimPhase::Collapsed);
    }

    #[test]
    fn reduced_motion_skips_geometry_progress() {
        let mut a = AnimState::new(AnimConfig { expand_ms: 100, collapse_ms: 100, reduced_motion: true, ..Default::default() });
        a.on_pointer_enter(0);
        a.tick(50); // mid-tween in normal mode, but reduced motion snaps size
        let (w, h, _) = a.width_height_opacity();
        assert_eq!((w, h), CARD, "reduced motion must snap straight to card size, no tween");
    }

    #[test]
    fn easing_endpoints() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 1e-6);
        assert_eq!(ease_in_cubic(0.0), 0.0);
        assert!((ease_in_cubic(1.0) - 1.0).abs() < 1e-6);
    }
}
