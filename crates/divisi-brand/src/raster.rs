//! Half-block (`▀ ▄ █`) rendering of the mark for terminals. A terminal cell is
//! about 0.6 wide per 1.0 tall and a half-block row is 0.5 tall, so one
//! sub-row is `48 / (rows * 2)` world units tall and one column is 1.2x that wide.

use crate::motion::MarkState;

const WORLD: f32 = 48.0;
const CELL_ASPECT: f32 = 1.2;
const BAR_HALF: f32 = 3.1;

fn seg_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((px - (ax + t * dx)).powi(2) + (py - (ay + t * dy)).powi(2)).sqrt()
}

/// Renders `state` into `rows` character rows (columns follow from the aspect ratio).
pub fn render(state: &MarkState, rows: usize) -> Vec<String> {
    let sub = rows * 2;
    let uy = WORLD / sub as f32;
    let ux = uy * CELL_ASPECT;
    let cols = (WORLD / ux).round() as usize;
    let reach = uy * 0.5;
    let rad = state.angle_deg.to_radians();
    let (hx, hy) = (rad.cos() * state.bar_len / 2.0, rad.sin() * state.bar_len / 2.0);
    let dot_r = state.dot_radius.max(0.0);

    let on = |x: usize, y: usize| -> bool {
        let (px, py) = ((x as f32 + 0.5) * ux, (y as f32 + 0.5) * uy);
        if seg_dist(px, py, 24.0 - hx, 24.0 - hy, 24.0 + hx, 24.0 + hy) <= BAR_HALF.max(reach) {
            return true;
        }
        dot_r > 0.05 && {
            let r = dot_r.max(reach);
            let (up, down) = (24.0 - state.dot_offset, 24.0 + state.dot_offset);
            ((px - 24.0).powi(2) + (py - up).powi(2)).sqrt() <= r || ((px - 24.0).powi(2) + (py - down).powi(2)).sqrt() <= r
        }
    };

    (0..rows)
        .map(|r| {
            (0..cols)
                .map(|c| match (on(c, 2 * r), on(c, 2 * r + 1)) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::state_at;

    #[test]
    fn full_size_is_25_columns_by_15_rows() {
        let lines = render(&state_at(1.0), 15);
        assert_eq!(lines.len(), 15);
        assert!(lines.iter().all(|l| l.chars().count() == 25));
    }

    #[test]
    fn slash_leans_up_and_to_the_right() {
        let lines = render(&state_at(0.0), 15);
        for (row, line) in lines.iter().take(7).enumerate() {
            let left: String = line.chars().take(10).collect();
            assert!(left.trim().is_empty(), "row {row} has ink on the left: {left:?}");
        }
        assert!(lines[1].chars().skip(15).any(|c| c != ' '), "top right should be inked");
        assert!(lines[13].chars().take(10).any(|c| c != ' '), "bottom left should be inked");
    }

    #[test]
    fn obelus_has_a_bar_and_two_dots() {
        let lines = render(&state_at(1.0), 15);
        let bar = lines[7].chars().filter(|c| *c != ' ').count();
        assert!(bar >= 14, "bar row too short: {bar}");
        assert_ne!(lines[3].chars().nth(12), Some(' '), "upper dot missing");
        assert_ne!(lines[11].chars().nth(12), Some(' '), "lower dot missing");
    }
}
