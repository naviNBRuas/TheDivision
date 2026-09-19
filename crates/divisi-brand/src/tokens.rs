pub const GRAPHITE: &str = "#16181d";
pub const BONE: &str = "#f2f2f0";
pub const SIGNAL: &str = "#ff5a1f";
pub const GO: &str = "#3ddc97";
pub const CAUTION: &str = "#ffd23f";
pub const FAULT: &str = "#ff2d55";

pub const ALL: [(&str, &str); 6] =
    [("graphite", GRAPHITE), ("bone", BONE), ("signal", SIGNAL), ("go", GO), ("caution", CAUTION), ("fault", FAULT)];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_matches_the_constants() {
        let css = include_str!("../assets/tokens.css");
        for (name, hex) in ALL {
            assert!(css.contains(&format!("--{name}:{hex}")), "tokens.css is missing --{name}:{hex}");
        }
    }

    #[test]
    fn svgs_use_signal_and_the_48_grid() {
        for svg in [include_str!("../assets/mark-obelus.svg"), include_str!("../assets/mark-slash.svg")] {
            assert!(svg.contains(SIGNAL));
            assert!(svg.contains("viewBox=\"0 0 48 48\""));
        }
    }
}
