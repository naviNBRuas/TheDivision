//! Persistent Docker container lifecycle for the opt-in Docker execution
//! backend (`divisi_core::docker` holds the per-agent/account enable
//! settings; this module actually talks to the `docker` CLI, mirroring
//! `bootstrap.rs`'s `Command::new("sh")...status()` precedent — plain
//! process shell-outs, no Docker SDK dependency).
//!
//! Containers are persistent, not `--rm` per run: `ensure_started` creates
//! one only if it doesn't exist yet, or `docker start`s it if it exists
//! but is stopped, so repeated task runs reuse the same container (like a
//! devcontainer) instead of paying container-creation cost every time.
//!
//! **Known limitation**: the project-directory bind mount is fixed at
//! container *creation* time. Running a task against a different project
//! through the same persistent container needs `single agent docker stop`
//! first (so the next run recreates it with the new mount) — there's no
//! way to add a bind mount to an already-created container without
//! recreating it.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// The shared image `docker/Dockerfile` builds — every supported agent
/// CLI installed via the same `bootstrap_install` commands
/// `divisi_core::registry::builtin_registry()` already knows about.
pub const DEFAULT_IMAGE: &str = "singlecli-agents";

pub fn docker_available() -> bool {
    Command::new("which").arg("docker").output().map(|o| o.status.success()).unwrap_or(false)
}

/// `None` if the container doesn't exist at all.
pub fn is_running(container: &str) -> Result<Option<bool>> {
    let output = Command::new("docker")
        .args(["inspect", "-f", "{{.State.Running}}", container])
        .output()
        .context("running docker inspect")?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).trim() == "true"))
}

/// Ensures `container` is running: no-op if already running, `docker
/// start`s it if it exists but is stopped, otherwise creates it fresh
/// with `home_mount` bind-mounted to `/root` (the same isolated-home
/// directory the host execution backend would use — see
/// `divisi_core::agent_home`/`account`) and `cwd_mount` bind-mounted at
/// the identical path inside the container, so `docker exec -w
/// <cwd_mount>` (see `divisi-agent-sdk::run::run_command_live`) finds the
/// project directory exactly where the caller expects it.
pub fn ensure_started(container: &str, image: &str, home_mount: &Path, cwd_mount: &Path) -> Result<()> {
    match is_running(container)? {
        Some(true) => return Ok(()),
        Some(false) => {
            let status = Command::new("docker").args(["start", container]).status().context("running docker start")?;
            if !status.success() {
                bail!("docker start {container} failed");
            }
            return Ok(());
        }
        None => {}
    }

    let cwd_str = cwd_mount.display().to_string();
    let status = Command::new("docker")
        .args([
            "run",
            "-d",
            "--name",
            container,
            "-v",
            &format!("{}:/root", home_mount.display()),
            "-v",
            &format!("{cwd_str}:{cwd_str}"),
            image,
            "sleep",
            "infinity",
        ])
        .status()
        .context("running docker run")?;
    if !status.success() {
        bail!("docker run {container} (image {image}) failed — is the image built? see docker/Dockerfile");
    }
    Ok(())
}

pub fn stop(container: &str) -> Result<()> {
    let status = Command::new("docker").args(["stop", container]).status().context("running docker stop")?;
    if !status.success() {
        bail!("docker stop {container} failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real end-to-end lifecycle test against the actual `docker` CLI —
    /// skips cleanly (not fails) when Docker isn't available, same
    /// discipline as `redis_backend`/`qdrant_backend`'s live-service
    /// tests. Uses a tiny public image (not `DEFAULT_IMAGE`, which needs
    /// `docker/Dockerfile` built first) purely to exercise the lifecycle
    /// mechanics: create -> running -> stop -> restart -> running.
    #[test]
    fn ensure_started_creates_then_reuses_then_restarts_a_real_container() {
        if !docker_available() {
            eprintln!("skipping: docker not installed");
            return;
        }
        let container = "singlecli-test-lifecycle";
        let _ = Command::new("docker").args(["rm", "-f", container]).output(); // clean slate
        let home_dir = tempfile::tempdir().unwrap();
        let cwd_dir = tempfile::tempdir().unwrap();

        let result = ensure_started(container, "alpine:latest", home_dir.path(), cwd_dir.path());
        if let Err(e) = &result {
            eprintln!("skipping: docker daemon not reachable ({e:#})");
            return;
        }
        assert_eq!(is_running(container).unwrap(), Some(true));

        // Calling again while already running must be a cheap no-op, not
        // an error from trying to `docker run` a name that already exists.
        ensure_started(container, "alpine:latest", home_dir.path(), cwd_dir.path()).unwrap();
        assert_eq!(is_running(container).unwrap(), Some(true));

        stop(container).unwrap();
        assert_eq!(is_running(container).unwrap(), Some(false));

        // Stopped-but-exists must restart via `docker start`, not fail
        // trying to `docker run` a name collision.
        ensure_started(container, "alpine:latest", home_dir.path(), cwd_dir.path()).unwrap();
        assert_eq!(is_running(container).unwrap(), Some(true));

        let _ = Command::new("docker").args(["rm", "-f", container]).output();
    }

    #[test]
    fn is_running_returns_none_for_a_nonexistent_container() {
        if !docker_available() {
            eprintln!("skipping: docker not installed");
            return;
        }
        assert_eq!(is_running("singlecli-definitely-does-not-exist").unwrap(), None);
    }
}
