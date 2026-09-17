//! `single-notch` binary entry point.
//!
//! `--stub` / `SINGLE_NOTCH_STUB=1` writes a pidfile and blocks until
//! SIGTERM, so `single notch enable|disable|status` (see
//! `single-cli::notch_proc`) has a real companion process to spawn,
//! detect, and stop even while the real UI is still being built.
//! Otherwise runs the Phase 1 spike window: minimal, no custom container
//! styling or close-key handling yet -- both need iced 0.14's real widget
//! API confirmed live (docs.rs, not memory) before adding; the OS window
//! chrome's own close control is enough for a discardable spike. See
//! `docs/superpowers/plans/2026-09-17-e30-notch-hud.md` Phase 1 Task 1
//! and Phase 2's stub-lifecycle goal.

use anyhow::{Context, Result};
use iced::widget::{column, container, text};
use iced::{Element, Length};
use single_core::SingleDirs;
use std::sync::atomic::{AtomicBool, Ordering};

static TERMINATED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigterm(_signum: libc::c_int) {
    TERMINATED.store(true, Ordering::SeqCst);
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 sends nothing — it only probes whether `pid` exists
    // and is signalable by this process.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn main() -> Result<()> {
    let stub_flag = std::env::args().any(|arg| arg == "--stub");
    let stub_env = std::env::var("SINGLE_NOTCH_STUB").as_deref() == Ok("1");
    if stub_flag || stub_env {
        return run_stub();
    }
    run_ui().map_err(|e| anyhow::anyhow!("{e}"))
}

fn run_stub() -> Result<()> {
    let dirs = SingleDirs::discover()?;
    dirs.ensure_created()?;

    let pid_file = dirs.notch_pid_file();
    if let Some(parent) = pid_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    // Single-instance guard: a second launch while a live instance already
    // owns the pidfile exits cleanly rather than racing it for the file.
    if let Some(existing_pid) = std::fs::read_to_string(&pid_file)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
    {
        if pid_alive(existing_pid) {
            return Ok(());
        }
    }

    std::fs::write(&pid_file, std::process::id().to_string())
        .with_context(|| format!("writing {}", pid_file.display()))?;

    // SAFETY: `handle_sigterm` only touches a static `AtomicBool`, which is
    // async-signal-safe.
    unsafe {
        libc::signal(libc::SIGTERM, handle_sigterm as *const () as usize);
    }

    while !TERMINATED.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let _ = std::fs::remove_file(&pid_file);
    Ok(())
}

#[derive(Default)]
struct NotchApp;

#[derive(Debug, Clone)]
enum Message {}

fn update(_state: &mut NotchApp, message: Message) {
    match message {}
}

fn view(_state: &NotchApp) -> Element<'_, Message> {
    container(column![text("SingleCLI Notch").size(14)].padding(10))
        .width(Length::Fixed(128.0))
        .height(Length::Fixed(28.0))
        .into()
}

fn run_ui() -> iced::Result {
    iced::application(NotchApp::default, update, view)
        .title("SingleCLI Notch")
        .run()
}
