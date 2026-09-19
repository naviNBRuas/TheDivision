//! Spawn/stop/liveness for the `divisi-notch` companion process.
//!
//! Mirrors `daemon.rs`'s pidfile-and-binary-path-resolution shape, but for
//! a plain OS process rather than a socket-serving daemon: the Phase 2
//! stub HUD has no IPC yet, so liveness is `kill(pid, 0)` against the
//! pidfile at `DivisiDirs::notch_pid_file()` and shutdown is SIGTERM, not a
//! protocol request.

use anyhow::{Context, Result};
use divisi_core::DivisiDirs;
use std::io::Write;
use std::time::{Duration, Instant};

/// Sends a one-line JSON control command (`{"cmd":"show"|"hide"|"quit"}`)
/// to a running HUD's control socket. Deliberately not shared with
/// `divisi-notch::control`'s identical wire shape -- depending on that
/// crate from here would drag the whole iced/wgpu stack into this plain
/// CLI binary just for a socket write. Errors (no live HUD, stale
/// socket) are the caller's to decide how to handle -- `show`/`hide`
/// surface them, `disable`'s quit path treats them as "fall back to
/// SIGTERM".
pub fn send_command(dirs: &DivisiDirs, cmd: &str) -> Result<()> {
    let mut stream = std::os::unix::net::UnixStream::connect(dirs.notch_socket_path())
        .with_context(|| "connecting to the notch control socket (is it running? `single notch enable`)")?;
    let payload = format!("{{\"cmd\":\"{cmd}\"}}\n");
    stream.write_all(payload.as_bytes())?;
    Ok(())
}

pub fn read_pid(dirs: &DivisiDirs) -> Option<u32> {
    let text = std::fs::read_to_string(dirs.notch_pid_file()).ok()?;
    text.trim().parse().ok()
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 sends nothing — it only probes whether `pid` exists
    // and is signalable by this process.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

pub fn is_alive(dirs: &DivisiDirs) -> bool {
    read_pid(dirs).is_some_and(pid_alive)
}

fn binary_path() -> Result<std::path::PathBuf> {
    let current_exe = std::env::current_exe().context("resolving current executable path")?;
    let dir = current_exe.parent().context("executable has no parent directory")?;
    let candidate = dir.join("divisi-notch");
    if candidate.exists() {
        return Ok(candidate);
    }
    // Fall back to $PATH lookup if not installed alongside `single` (e.g. `cargo run`).
    Ok(std::path::PathBuf::from("divisi-notch"))
}

/// Spawns the `divisi-notch` companion process in stub mode. No-op if a
/// live instance is already running (single-instance guard).
pub fn spawn(dirs: &DivisiDirs) -> Result<()> {
    if is_alive(dirs) {
        return Ok(());
    }

    let path = binary_path()?;
    std::process::Command::new(&path)
        .env("DIVISI_NOTCH_STUB", "1")
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
    anyhow::bail!("divisi-notch did not report a live pid within 3s of spawning")
}

/// Sends SIGTERM to the pidfile's process and waits (up to 3s) for it to
/// exit. Returns `Ok(false)` without waiting if nothing was running.
pub fn stop(dirs: &DivisiDirs) -> Result<bool> {
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
    anyhow::bail!("divisi-notch (pid {pid}) did not exit within 3s of SIGTERM")
}

// ---- GNOME Shell backend ----------------------------------------------------
//
// GNOME's compositor has no wlr-layer-shell, so a client process cannot be an
// overlay there. The notch is instead a Shell extension drawn as compositor
// chrome; it shells out to `divisi-notch --snapshot` for its data.

const GNOME_UUID: &str = "divisi-notch@nbr.company";
const GNOME_EXTENSION_JS: &str = include_str!("../../../extensions/gnome-shell/divisi-notch@nbr.company/extension.js");
const GNOME_METADATA: &str = include_str!("../../../extensions/gnome-shell/divisi-notch@nbr.company/metadata.json");

pub fn is_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("gnome")))
}

#[derive(Debug, PartialEq, Eq)]
pub enum GnomeState {
    Active,
    /// Installed and set to load, but this shell session predates the install
    /// (GNOME on Wayland only discovers new extensions at login).
    NeedsRelogin,
    Disabled,
}

fn gnome_dir() -> Result<std::path::PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(std::path::PathBuf::from(home).join(".local/share/gnome-shell/extensions").join(GNOME_UUID))
}

fn gnome_extensions(args: &[&str]) -> Result<(bool, String)> {
    let out = std::process::Command::new("gnome-extensions").args(args).output().context("running gnome-extensions (is gnome-shell installed?)")?;
    Ok((out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned()))
}

pub fn gnome_install() -> Result<()> {
    let dir = gnome_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(dir.join("extension.js"), GNOME_EXTENSION_JS)?;
    std::fs::write(dir.join("metadata.json"), GNOME_METADATA)?;
    Ok(())
}

pub fn gnome_state() -> GnomeState {
    match gnome_extensions(&["info", GNOME_UUID]) {
        Ok((true, info)) if info.contains("State: ACTIVE") || info.contains("State: ENABLED") => GnomeState::Active,
        Ok((true, info)) if info.contains("Enabled: Yes") => GnomeState::NeedsRelogin,
        _ if gnome_enabled_in_settings() => GnomeState::NeedsRelogin,
        _ => GnomeState::Disabled,
    }
}

fn gnome_enabled_in_settings() -> bool {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.shell", "enabled-extensions"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(GNOME_UUID))
}

pub fn gnome_enable() -> Result<GnomeState> {
    gnome_install()?;
    if gnome_extensions(&["enable", GNOME_UUID])?.0 {
        return Ok(gnome_state());
    }
    // Not known to the running shell yet: record it in `enabled-extensions` so
    // it loads at the next login.
    let (_, current) = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.shell", "enabled-extensions"])
        .output()
        .map(|o| (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned()))?;
    if !current.contains(GNOME_UUID) {
        let trimmed = current.trim().trim_start_matches("@as ").trim_start_matches('[').trim_end_matches(']').trim();
        let updated = if trimmed.is_empty() { format!("['{GNOME_UUID}']") } else { format!("[{trimmed}, '{GNOME_UUID}']") };
        let status = std::process::Command::new("gsettings").args(["set", "org.gnome.shell", "enabled-extensions", &updated]).status()?;
        anyhow::ensure!(status.success(), "gsettings could not update enabled-extensions");
    }
    Ok(GnomeState::NeedsRelogin)
}

pub fn gnome_disable() -> Result<()> {
    let _ = gnome_extensions(&["disable", GNOME_UUID])?;
    Ok(())
}
