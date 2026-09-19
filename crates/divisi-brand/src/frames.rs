//! Precomputed loop frames for surfaces that cannot run Rust (the GNOME Shell
//! extension). The committed `mark-frames.json` must equal `to_json(30, ...)`.

use crate::motion::{loop_at, LoopFrame, LOOP_SECS};

pub fn loop_frames(fps: u32) -> Vec<LoopFrame> {
    let n = (LOOP_SECS * fps as f32).round() as u32;
    (0..n).map(|i| loop_at(i as f32 / fps as f32)).collect()
}

pub fn to_json(fps: u32, frames: &[LoopFrame]) -> String {
    let body: Vec<String> = frames
        .iter()
        .map(|f| {
            format!(
                "{{\"a\":{:.2},\"l\":{:.2},\"o\":{:.2},\"r\":{:.2},\"p\":\"{}\"}}",
                f.state.angle_deg,
                f.state.bar_len,
                f.state.dot_offset,
                f.state.dot_radius,
                f.phase.as_str()
            )
        })
        .collect();
    format!("{{\"fps\":{fps},\"loop_secs\":{LOOP_SECS},\"frames\":[{}]}}", body.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thirty_fps_gives_126_frames() {
        assert_eq!(loop_frames(30).len(), 126);
    }

    #[test]
    fn first_frame_is_the_idle_slash() {
        let f = &loop_frames(30)[0];
        assert_eq!(f.phase.as_str(), "idle");
        assert_eq!(f.state.angle_deg, -62.0);
    }

    #[test]
    fn json_carries_every_frame() {
        let frames = loop_frames(30);
        let json = to_json(30, &frames);
        assert!(json.starts_with("{\"fps\":30,\"loop_secs\":4.2,\"frames\":["));
        assert_eq!(json.matches("\"a\":").count(), 126);
        assert!(json.contains("\"p\":\"working\""));
    }

    #[test]
    fn committed_notch_frames_are_current() {
        let committed = include_str!("../../../extensions/gnome-shell/divisi-notch@nbr.company/mark-frames.json");
        assert_eq!(committed.trim_end(), to_json(30, &loop_frames(30)), "regenerate with: cargo run -p divisi-brand --example emit_frames");
    }
}
