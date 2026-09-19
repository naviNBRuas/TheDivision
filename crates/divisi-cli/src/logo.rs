//! `divisi logo [--animate]`: the half-block mark, static or one animated loop.

use divisi_brand::{motion, raster};
use std::io::IsTerminal;

const ROWS: usize = 15;

pub fn print_logo(animate: bool) {
    let color = std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();
    let paint = |s: &str| if color { format!("\x1b[38;2;255;90;31m{s}\x1b[0m") } else { s.to_string() };
    if !animate || !std::io::stdout().is_terminal() || std::env::var_os("NO_MOTION").is_some() {
        for line in raster::render(&motion::state_at(1.0), ROWS) {
            println!("{}", paint(&line));
        }
        return;
    }
    let start = std::time::Instant::now();
    print!("\x1b[?25l");
    let mut first = true;
    while start.elapsed().as_secs_f32() < motion::LOOP_SECS {
        let frame = motion::loop_at(start.elapsed().as_secs_f32());
        if !first {
            print!("\x1b[{ROWS}A");
        }
        first = false;
        for line in raster::render(&frame.state, ROWS) {
            println!("{}", paint(&line));
        }
        std::thread::sleep(std::time::Duration::from_millis(33));
    }
    print!("\x1b[?25h");
}
