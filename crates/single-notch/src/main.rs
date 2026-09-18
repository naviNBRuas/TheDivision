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
use iced::keyboard::{self, Key};
use iced::widget::{container, MouseArea};
use iced::{Element, Event, Length, Subscription};
use single_core::SingleDirs;
use single_notch::anim::{AnimConfig, AnimPhase, AnimState};
use single_notch::model::NotchSnapshot;
use single_notch::poll::{diff_notable, Poller};
use single_notch::ui::{card, pill};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use std::time::Duration;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

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

    let dirs = SingleDirs::discover()?;
    if !claim_pidfile(&dirs)? {
        return Ok(()); // a live instance already owns the pidfile
    }
    // SAFETY: `handle_sigterm` (shared with `run_stub`) only touches a
    // static `AtomicBool` -- async-signal-safe. The flag is polled from
    // an iced subscription (see `subscription`'s `sigterm_sub`) rather
    // than acting on it here, so the actual exit + pidfile cleanup always
    // happens on the normal event-loop thread, never inside the handler.
    unsafe {
        libc::signal(libc::SIGTERM, handle_sigterm as *const () as usize);
    }

    let result = run_ui().map_err(|e| anyhow::anyhow!("{e}"));
    remove_pidfile(&dirs);
    result
}

/// Single-instance guard + pidfile write, shared by stub and real-UI
/// mode -- `disable`/`status`/`show`/`hide` all key off this same
/// pidfile regardless of which mode is actually running, so a caller
/// never needs to know or care which one it launched. Returns `Ok(true)`
/// if this process should proceed (it now owns the pidfile), `Ok(false)`
/// if a live instance already owns it (caller should exit cleanly, not
/// race it for the file).
fn claim_pidfile(dirs: &SingleDirs) -> Result<bool> {
    dirs.ensure_created()?;
    let pid_file = dirs.notch_pid_file();
    if let Some(parent) = pid_file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    if let Some(existing_pid) = std::fs::read_to_string(&pid_file).ok().and_then(|text| text.trim().parse::<u32>().ok()) {
        if pid_alive(existing_pid) {
            return Ok(false);
        }
    }
    std::fs::write(&pid_file, std::process::id().to_string()).with_context(|| format!("writing {}", pid_file.display()))?;
    Ok(true)
}

fn remove_pidfile(dirs: &SingleDirs) {
    let _ = std::fs::remove_file(dirs.notch_pid_file());
}

fn run_stub() -> Result<()> {
    let dirs = SingleDirs::discover()?;
    if !claim_pidfile(&dirs)? {
        return Ok(());
    }

    // SAFETY: `handle_sigterm` only touches a static `AtomicBool`, which is
    // async-signal-safe.
    unsafe {
        libc::signal(libc::SIGTERM, handle_sigterm as *const () as usize);
    }

    while !TERMINATED.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    remove_pidfile(&dirs);
    Ok(())
}

struct NotchApp {
    poller: Poller,
    snapshot: Option<NotchSnapshot>,
    anim: AnimState,
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
            anim: AnimState::new(AnimConfig::default()),
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    AnimTick,
    PointerEnter,
    PointerLeave,
    Escape,
    Control(single_notch::control::ControlCommand),
}

fn update(state: &mut NotchApp, message: Message) {
    match message {
        Message::Tick => match state.poller.tick() {
            Ok(next) => {
                if let Some(prev) = &state.snapshot {
                    if !diff_notable(prev, &next).is_empty() {
                        state.anim.on_notable(now_ms());
                    }
                }
                state.snapshot = Some(next);
            }
            // v1 polls best-effort: a daemon that's briefly down (restart,
            // not yet started) just means the last-known snapshot stays on
            // screen instead of the app crashing or flashing empty.
            Err(e) => eprintln!("single-notch: poll failed: {e:#}"),
        },
        Message::AnimTick => {
            state.anim.tick(now_ms());
        }
        Message::PointerEnter => state.anim.on_pointer_enter(now_ms()),
        Message::PointerLeave => state.anim.on_pointer_leave(now_ms()),
        Message::Escape => state.anim.on_escape(now_ms()),
        Message::Control(single_notch::control::ControlCommand::Show) => state.anim.force_expand(now_ms()),
        Message::Control(single_notch::control::ControlCommand::Hide) => state.anim.force_collapse(now_ms()),
        Message::Control(single_notch::control::ControlCommand::Quit) => {
            // `main`'s post-`run_ui()` cleanup never runs after a direct
            // `exit()` -- remove the pidfile here so both the control-
            // socket quit path and a SIGTERM (routed to this same
            // message via `subscription`'s `sigterm_sub`) leave no stale
            // pidfile behind.
            if let Ok(dirs) = SingleDirs::discover() {
                remove_pidfile(&dirs);
            }
            std::process::exit(0);
        }
    }
}

fn view(state: &NotchApp) -> Element<'_, Message> {
    let (w, h, _opacity) = state.anim.width_height_opacity();
    let content: Element<'_, Message> = match &state.snapshot {
        Some(snap) if matches!(state.anim.phase(), AnimPhase::Expanded | AnimPhase::Expanding) => card::view(snap),
        Some(snap) => pill::view(snap),
        None => container(iced::widget::text("…")).into(),
    };
    let sized = container(content).width(Length::Fixed(w)).height(Length::Fixed(h));
    MouseArea::new(sized)
        .on_enter(Message::PointerEnter)
        .on_exit(Message::PointerLeave)
        .into()
}

fn handle_key(event: Event, _status: iced::event::Status, _window: iced::window::Id) -> Option<Message> {
    if let Event::Keyboard(keyboard::Event::KeyPressed { key: Key::Named(keyboard::key::Named::Escape), .. }) = event {
        Some(Message::Escape)
    } else {
        None
    }
}

fn subscription(state: &NotchApp) -> Subscription<Message> {
    let expanded = matches!(state.anim.phase(), AnimPhase::Expanded);
    let interval = state.poller.interval_ms(expanded, false);
    let poll_sub = iced::time::every(Duration::from_millis(interval)).map(|_| Message::Tick);
    let key_sub = iced::event::listen_with(handle_key);
    let anim_sub = if matches!(state.anim.phase(), AnimPhase::Expanding | AnimPhase::Collapsing) {
        iced::time::every(Duration::from_millis(16)).map(|_| Message::AnimTick)
    } else {
        Subscription::none()
    };
    let control_sub = single_notch::control::subscription().map(Message::Control);
    // Poll the SIGTERM flag on the same cadence as the poll timer rather
    // than a dedicated subscription -- `TERMINATED` only ever needs to be
    // noticed within about a second, and this avoids one more always-on
    // timer subscription for something that's normally never set.
    let sigterm_sub = iced::time::every(Duration::from_millis(200)).map(|_| {
        if TERMINATED.load(Ordering::SeqCst) {
            Message::Control(single_notch::control::ControlCommand::Quit)
        } else {
            Message::AnimTick // harmless no-op-ish tick when not terminating (still ticks anim, which is idempotent when Collapsed)
        }
    });
    Subscription::batch([poll_sub, key_sub, anim_sub, control_sub, sigterm_sub])
}

fn run_ui() -> iced::Result {
    iced::application(NotchApp::default, update, view)
        .subscription(subscription)
        .title("SingleCLI Notch")
        .run()
}
