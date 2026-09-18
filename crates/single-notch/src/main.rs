//! `single-notch` binary entry point.
//!
//! `--stub` / `SINGLE_NOTCH_STUB=1` writes a pidfile and blocks until
//! SIGTERM, so `single notch enable|disable|status` (see
//! `single-cli::notch_proc`) has a real companion process to spawn,
//! detect, and stop even while the real UI is still being built.
//! Otherwise runs the real (if still collapsed-pill-only) notch window,
//! polling `single-runtimed` on a timer per
//! `docs/superpowers/plans/2026-09-17-e30-notch-hud.md` Phase 4.

use anyhow::{Context, Result};
use iced::widget::container;
use iced::{Element, Length, Subscription};
use single_core::SingleDirs;
use single_notch::model::NotchSnapshot;
use single_notch::poll::Poller;
use single_notch::ui::pill;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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

struct NotchApp {
    poller: Poller,
    snapshot: Option<NotchSnapshot>,
}

impl Default for NotchApp {
    fn default() -> Self {
        // Best-effort socket discovery -- if `SingleDirs::discover` fails
        // (config dir genuinely missing), fall back to a path that will
        // simply fail every poll rather than panic the whole app on boot.
        let socket = SingleDirs::discover()
            .map(|d| d.socket_path())
            .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/single-notch-no-socket"));
        NotchApp {
            poller: Poller { socket, poll_ms_idle: 1000, poll_ms_active: 400 },
            snapshot: None,
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
}

fn update(state: &mut NotchApp, message: Message) {
    match message {
        Message::Tick => match state.poller.tick() {
            Ok(snap) => state.snapshot = Some(snap),
            // v1 polls best-effort: a daemon that's briefly down (restart,
            // not yet started) just means the last-known snapshot stays on
            // screen instead of the app crashing or flashing empty.
            Err(e) => eprintln!("single-notch: poll failed: {e:#}"),
        },
    }
}

fn view(state: &NotchApp) -> Element<'_, Message> {
    match &state.snapshot {
        Some(snap) => pill::view(snap),
        None => container(iced::widget::text("…")).width(Length::Fixed(128.0)).height(Length::Fixed(28.0)).into(),
    }
}

fn subscription(state: &NotchApp) -> Subscription<Message> {
    let interval = state.poller.interval_ms(false, false);
    iced::time::every(Duration::from_millis(interval)).map(|_| Message::Tick)
}

fn run_ui() -> iced::Result {
    iced::application(NotchApp::default, update, view)
        .subscription(subscription)
        .title("SingleCLI Notch")
        .run()
}
