//! Spawns `divisid` as a detached background process if the socket
//! isn't already live, so the TUI gets a real daemon to talk to over IPC
//! (per spec section 3: the runtime is headless, the TUI is one client).
//! One-shot CLI commands don't need this — see `client.rs`'s in-process
//! fallback — this is only exercised by the no-subcommand TUI path.
//!
//! Also owns `stop_running`/`is_running` (`single daemon stop|status`):
//! the daemon inherits its environment, notably `$PATH`, once at spawn
//! time and keeps it for the rest of the process's life, so installing a
//! new agent CLI or editing shell rc files never becomes visible to an
//! already-running daemon — detection (`divisi-agent-sdk::discover`)
//! shells out to `which` as a *child of the daemon*, not the user's
//! current shell. `single daemon restart` is the fix.

use anyhow::{Context, Result};
use divisi_core::DivisiDirs;
use divisi_protocol::{Request, Response};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

pub fn is_running(dirs: &DivisiDirs) -> bool {
    UnixStream::connect(dirs.socket_path()).is_ok()
}

/// Asks a running daemon to exit and waits (up to 3s) for it to actually
/// do so, so a caller that immediately calls `ensure_running` afterward
/// doesn't race the old process's exit and try to bind the same socket
/// path concurrently. Returns `Ok(false)` without waiting if nothing was
/// running to begin with.
pub fn stop_running(dirs: &DivisiDirs) -> Result<bool> {
    let socket_path = dirs.socket_path();
    if UnixStream::connect(&socket_path).is_err() {
        return Ok(false);
    }

    if let Response::Error { message } = crate::client::send(&socket_path, Request::Shutdown)? {
        anyhow::bail!("shutdown request failed: {message}");
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if UnixStream::connect(&socket_path).is_err() {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("divisid did not exit within 3s of the shutdown request")
}

pub fn ensure_running(dirs: &DivisiDirs) -> Result<()> {
    let socket_path = dirs.socket_path();
    if UnixStream::connect(&socket_path).is_ok() {
        return Ok(());
    }

    let daemon_path = daemon_binary_path()?;
    std::process::Command::new(&daemon_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawning {}", daemon_path.display()))?;

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if UnixStream::connect(&socket_path).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!(
        "divisid did not come up at {} within 3s; run it manually with `divisid`",
        socket_path.display()
    )
}

fn daemon_binary_path() -> Result<std::path::PathBuf> {
    let current_exe = std::env::current_exe().context("resolving current executable path")?;
    let dir = current_exe.parent().context("executable has no parent directory")?;
    let candidate = dir.join("divisid");
    if candidate.exists() {
        return Ok(candidate);
    }
    // Fall back to $PATH lookup if not installed alongside `single` (e.g. `cargo run`).
    Ok(std::path::PathBuf::from("divisid"))
}
