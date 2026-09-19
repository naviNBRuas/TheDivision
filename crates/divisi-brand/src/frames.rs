//! Precomputed frames for surfaces that cannot run Rust (the GNOME Shell extension).
//! A surface shows `rest` when idle; when work starts it plays `enter` once then loops
//! `spin`; when work ends it finishes the current turn, plays `exit` once, and returns
//! to `rest`. The committed `mark-frames.json` must equal `to_json(30, &sequences(30))`.

use crate::motion::{state_at, working_at, MarkState, ENTER_SECS, EXIT_SECS, SPIN_SECS};

pub struct Sequences {
    pub rest: MarkState,
    pub enter: Vec<MarkState>,
    pub spin: Vec<MarkState>,
    pub exit: Vec<MarkState>,
}

fn count(secs: f32, fps: u32) -> usize {
    (secs * fps as f32).round() as usize
}

pub fn sequences(fps: u32) -> Sequences {
    let (ne, ns, nx) = (count(ENTER_SECS, fps), count(SPIN_SECS, fps), count(EXIT_SECS, fps));
    Sequences {
        rest: state_at(1.0),
        // "÷" to "/": starts on the obelus, stops just short of the slash so `spin` picks up exactly there.
        enter: (0..ne).map(|i| state_at(1.0 - i as f32 / ne as f32)).collect(),
        spin: (0..ns).map(|i| working_at(i as f32 / fps as f32)).collect(),
        // "/" to "÷": the last frame is the obelus.
        exit: (0..nx).map(|i| state_at((i + 1) as f32 / nx as f32)).collect(),
    }
}

fn frame(s: &MarkState) -> String {
    format!("{{\"a\":{:.2},\"l\":{:.2},\"o\":{:.2},\"r\":{:.2}}}", s.angle_deg, s.bar_len, s.dot_offset, s.dot_radius)
}

fn list(v: &[MarkState]) -> String {
    format!("[{}]", v.iter().map(frame).collect::<Vec<_>>().join(","))
}

pub fn to_json(fps: u32, s: &Sequences) -> String {
    format!("{{\"fps\":{fps},\"rest\":{},\"enter\":{},\"spin\":{},\"exit\":{}}}", frame(&s.rest), list(&s.enter), list(&s.spin), list(&s.exit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::{DOT_RADIUS, SLASH_ANGLE};

    #[test]
    fn thirty_fps_frame_counts() {
        let s = sequences(30);
        assert_eq!((s.enter.len(), s.spin.len(), s.exit.len()), (18, 36, 30));
    }

    #[test]
    fn the_sequences_join_without_a_jump() {
        let s = sequences(30);
        assert_eq!(s.enter[0], s.rest, "enter starts on the obelus");
        assert_eq!(s.spin[0].angle_deg, SLASH_ANGLE, "spin starts exactly at the slash");
        assert_eq!(s.spin[0].dot_radius, 0.0);
        let last_enter = s.enter.last().unwrap();
        assert!((last_enter.angle_deg - SLASH_ANGLE).abs() < 40.0 && last_enter.dot_radius == 0.0, "enter ends near the slash: {last_enter:?}");
        let last = s.exit.last().unwrap();
        assert!((last.dot_radius - DOT_RADIUS).abs() < 1e-3, "exit ends on the obelus dots");
        assert_eq!(*last, s.rest);
    }

    #[test]
    fn json_has_every_frame_once() {
        let json = to_json(30, &sequences(30));
        assert!(json.starts_with("{\"fps\":30,\"rest\":{"));
        assert_eq!(json.matches("\"a\":").count(), 1 + 18 + 36 + 30);
        for key in ["\"enter\":[", "\"spin\":[", "\"exit\":["] {
            assert!(json.contains(key), "missing {key}");
        }
    }

    #[test]
    fn committed_notch_frames_are_current() {
        let committed = include_str!("../../../extensions/gnome-shell/divisi-notch@nbr.company/mark-frames.json");
        assert_eq!(committed.trim_end(), to_json(30, &sequences(30)), "regenerate with: cargo run -p divisi-brand --example emit_frames");
    }
}
