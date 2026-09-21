//! Regression: `divisi agent list` and `divisi status` must return within a bounded time
//! whether the daemon is up or down (E27/01 P1 hang). Every run uses an isolated
//! `DIVISI_CONFIG_DIR`, so the real daemon is never touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Audited worst case is ~10 s (daemon-side agent detection); a hang would blow far past this.
const BOUND: Duration = Duration::from_secs(45);

fn divisi(cfg: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_divisi"));
    cmd.args(args)
        .env("DIVISI_CONFIG_DIR", cfg)
        .env_remove("SINGLE_CONFIG_DIR")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Runs to completion or kills the child at `BOUND`; a timeout is a test failure.
fn run_bounded(cfg: &Path, args: &[&str]) -> std::process::Output {
    let mut child = divisi(cfg, args).spawn().expect("spawn divisi");
    let start = Instant::now();
    loop {
        if child.try_wait().expect("poll divisi").is_some() {
            return child.wait_with_output().expect("collect output");
        }
        if start.elapsed() > BOUND {
            let _ = child.kill();
            panic!("`divisi {}` still running after {:?}: it hangs", args.join(" "), BOUND);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn assert_returns_ok(cfg: &Path, args: &[&str]) {
    let out = run_bounded(cfg, args);
    assert!(
        out.status.success(),
        "`divisi {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.stdout.is_empty(), "`divisi {}` printed nothing", args.join(" "));
}

/// Stops the isolated daemon even when an assertion panics.
struct DaemonGuard(PathBuf);
impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = divisi(&self.0, &["daemon", "stop"]).status();
    }
}

#[test]
fn agent_list_and_status_return_in_bounded_time_with_daemon_down() {
    let cfg = tempfile::tempdir().unwrap();
    let _guard = DaemonGuard(cfg.path().to_path_buf()); // they may auto-spawn a daemon
    assert_returns_ok(cfg.path(), &["agent", "list"]);
    assert_returns_ok(cfg.path(), &["status"]);
}

#[test]
fn agent_list_and_status_return_in_bounded_time_with_daemon_up() {
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_divisi"));
    if !exe.with_file_name("divisid").exists() {
        eprintln!("skipping: divisid is not built next to divisi (cargo build -p divisi-runtime)");
        return;
    }
    let cfg = tempfile::tempdir().unwrap();
    let _guard = DaemonGuard(cfg.path().to_path_buf());
    assert_returns_ok(cfg.path(), &["daemon", "restart"]);
    let up = run_bounded(cfg.path(), &["daemon", "status"]);
    assert!(String::from_utf8_lossy(&up.stdout).contains("is running"), "daemon must be up");

    assert_returns_ok(cfg.path(), &["agent", "list"]);
    assert_returns_ok(cfg.path(), &["status"]);
}
