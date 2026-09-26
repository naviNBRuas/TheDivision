//! Writes the looping mark as a self-contained animated SVG (SMIL, no script), sampled from `loop_at`
//! so the README shows the same motion as the TUI and the notch:
//!   cargo run -q -p divisi-brand --example animated_mark -- [size] [colour] > .github/assets/mark-animated.svg
use divisi_brand::motion::{loop_at, DOT_OFFSET, LOOP_SECS};

const SAMPLES: usize = 144; // 30 fps over the 4.8 s loop

fn main() {
    let mut args = std::env::args().skip(1);
    let size: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(128);
    let colour = args.next().unwrap_or_else(|| "#ff5a1f".to_string());

    let (mut angles, mut widths, mut xs, mut radii) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut prev: Option<f32> = None;
    for i in 0..=SAMPLES {
        let s = loop_at(LOOP_SECS * i as f32 / SAMPLES as f32 * 0.99999).state;
        // Unwrap the angle so consecutive samples never differ by a jump of 360 degrees, which a
        // linear interpolation would play as an extra spin.
        let mut a = s.angle_deg;
        if let Some(p) = prev {
            while a - p > 180.0 {
                a -= 360.0;
            }
            while a - p < -180.0 {
                a += 360.0;
            }
        }
        prev = Some(a);
        angles.push(format!("{a:.2}"));
        widths.push(format!("{:.2}", s.bar_len));
        xs.push(format!("{:.2}", -s.bar_len / 2.0));
        radii.push(format!("{:.2}", s.dot_radius.max(0.0)));
    }
    let key_times = (0..=SAMPLES).map(|i| format!("{:.4}", i as f32 / SAMPLES as f32)).collect::<Vec<_>>().join(";");
    let anim = |attr: &str, values: &[String]| {
        format!(
            r#"<animate attributeName="{attr}" dur="{LOOP_SECS}s" repeatCount="indefinite" keyTimes="{key_times}" values="{}"/>"#,
            values.join(";")
        )
    };
    let dot = |cy: f32| format!(r#"<circle cx="0" cy="{cy}" r="{}">{}</circle>"#, radii[0], anim("r", &radii));
    println!(
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48" width="{size}" height="{size}" role="img" aria-label="divisi: the obelus mark, spinning into a slash while it works">"#,
            r#"<g fill="{colour}" transform="translate(24 24)">"#,
            r#"<g><animateTransform attributeName="transform" type="rotate" dur="{dur}s" repeatCount="indefinite" keyTimes="{kt}" values="{angles}"/>"#,
            r#"<rect x="{x0}" y="-3" width="{w0}" height="6" rx="1.5">{w}{x}</rect></g>"#,
            "{d1}{d2}</g></svg>"
        ),
        size = size,
        colour = colour,
        dur = LOOP_SECS,
        kt = key_times,
        angles = angles.join(";"),
        x0 = xs[0],
        w0 = widths[0],
        w = anim("width", &widths),
        x = anim("x", &xs),
        d1 = dot(-DOT_OFFSET),
        d2 = dot(DOT_OFFSET),
    );
}
