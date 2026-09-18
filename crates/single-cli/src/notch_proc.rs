//! Spawn/stop/liveness for the `single-notch` companion process.
//!
//! Mirrors `daemon.rs`'s pidfile-and-binary-path-resolution shape, but for
//! a plain OS process rather than a socket-serving daemon: the Phase 2
//! stub HUD has no IPC yet, so liveness is `kill(pid, 0)` against the
//! pidfile at `SingleDirs::notch_pid_file()` and shutdown is SIGTERM, not a
//! protocol request.

use anyhow::{Context, Result};
use single_core::SingleDirs;
use std::io::Write;
use std::time::{Duration, Instant};

/// Sends a one-line JSON control command (`{"cmd":"show"|"hide"|"quit"}`)
/// to a running HUD's control socket. Deliberately not shared with
/// `single-notch::control`'s identical wire shape -- depending on that
/// crate from here would drag the whole iced/wgpu stack into this plain
/// CLI binary just for a socket write. Errors (no live HUD, stale
/// socket) are the caller's to decide how to handle -- `show`/`hide`
/// surface them, `disable`'s quit path treats them as "fall back to
/// SIGTERM".
pub fn send_command(dirs: &SingleDirs, cmd: &str) -> Result<()> {
    let mut stream = std::os::unix::net::UnixStream::connect(dirs.notch_socket_path())
        .with_context(|| "connecting to the notch control socket (is it running? `single notch enable`)")?;
    let payload = format!("{{\"cmd\":\"{cmd}\"}}\n");
    stream.write_all(payload.as_bytes())?;
    Ok(())
}

pub fn read_pid(dirs: &SingleDirs) -> Option<u32> {
    let text = std::fs::read_to_string(dirs.notch_pid_file()).ok()?;
    text.trim().parse().ok()
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 sends nothing — it only probes whether `pid` exists
    // and is signalable by this process.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

pub fn is_alive(dirs: &SingleDirs) -> bool {
    read_pid(dirs).is_some_and(pid_alive)
}

fn binary_path() -> Result<std::path::PathBuf> {
    let current_exe = std::env::current_exe().context("resolving current executable path")?;
    let dir = current_exe.parent().context("executable has no parent directory")?;
    let candidate = dir.join("single-notch");
    if candidate.exists() {
        return Ok(candidate);
    }
    // Fall back to $PATH lookup if not installed alongside `single` (e.g. `cargo run`).
    Ok(std::path::PathBuf::from("single-notch"))
}

/// Spawns the `single-notch` companion process in stub mode. No-op if a
/// live instance is already running (single-instance guard).
pub fn spawn(dirs: &SingleDirs) -> Result<()> {
    if is_alive(dirs) {
        return Ok(());
    }

    let path = binary_path()?;
    std::process::Command::new(&path)
        .env("SINGLE_NOTCH_STUB", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawning {}", path.display()))?;

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if is_alive(dirs) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    anyhow::bail!("single-notch did not report a live pid within 3s of spawning")
}

/// Sends SIGTERM to the pidfile's process and waits (up to 3s) for it to
/// exit. Returns `Ok(false)` without waiting if nothing was running.
pub fn stop(dirs: &SingleDirs) -> Result<bool> {
    let Some(pid) = read_pid(dirs) else {
        return Ok(false);
    };
    if !pid_alive(pid) {
        return Ok(false);
    }

    // SAFETY: `pid` was just confirmed alive and signalable above.
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    anyhow::bail!("single-notch (pid {pid}) did not exit within 3s of SIGTERM")
}
