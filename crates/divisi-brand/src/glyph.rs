//! Single-cell rendering for headers and status lines. At rest the mark is "÷";
//! while work runs it is a spinning slash (`/ - \ |`). `ascii` is the fallback
//! for `NO_COLOR` / dumb terminals.

/// The resting mark, the divisi logo.
pub const REST: char = '÷';

const SPINNER: [char; 4] = ['/', '-', '\\', '|'];
const SPINNER_STEP_SECS: f32 = 0.1;

/// The working glyph at `t_secs`: a slash that turns through `/ - \ |`, ten steps a second.
pub fn spinner(t_secs: f32) -> char {
    SPINNER[((t_secs.max(0.0) / SPINNER_STEP_SECS) as usize) % SPINNER.len()]
}

pub fn ascii(busy: bool) -> &'static str {
    if busy {
        "/"
    } else {
        "-:-"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rest_is_the_obelus() {
        assert_eq!(REST, '÷');
    }

    #[test]
    fn the_spinner_turns_through_a_slash() {
        assert_eq!(spinner(0.0), '/');
        assert_eq!(spinner(0.11), '-');
        assert_eq!(spinner(0.21), '\\');
        assert_eq!(spinner(0.31), '|');
        assert_eq!(spinner(0.41), '/', "wraps after four steps");
        assert_eq!(spinner(-1.0), '/', "negative time is treated as zero");
    }

    #[test]
    fn ascii_fallback() {
        assert_eq!(ascii(false), "-:-");
        assert_eq!(ascii(true), "/");
    }
}
