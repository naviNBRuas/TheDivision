//! Single-cell rendering for headers and status lines: a spinning bar that
//! resolves into "÷". `ascii` is the fallback for `NO_COLOR` / dumb terminals.

pub fn glyph(progress: f32) -> char {
    let r = progress.clamp(0.0, 1.0);
    if r >= 0.85 {
        return '÷';
    }
    ['/', '|', '\\', '-'][((r / 0.85) * 4.0) as usize % 4]
}

pub fn ascii(progress: f32) -> &'static str {
    if progress < 0.5 {
        "/"
    } else {
        "-:-"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spins_then_resolves() {
        assert_eq!(glyph(0.0), '/');
        assert_eq!(glyph(0.25), '|');
        assert_eq!(glyph(0.5), '\\');
        assert_eq!(glyph(0.75), '-');
        assert_eq!(glyph(1.0), '÷');
    }

    #[test]
    fn ascii_fallback() {
        assert_eq!(ascii(0.0), "/");
        assert_eq!(ascii(1.0), "-:-");
    }
}
