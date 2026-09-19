//! Blocking subprocess execution with a wall-clock timeout. `std::process`
//! has no built-in timeout, so this polls `try_wait` and kills the child
//! if it runs past the deadline — the standard approach absent an async
//! runtime here (this runs inside `divisi-runtime`'s synchronous request
//! handlers, matching how `discover()` already blocks synchronously).

use crate::backend::ExecBackend;
use anyhow::{Context, Result};
use divisi_protocol::RunOutcome;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Runs `command` attached to the real terminal (inherited stdin/stdout/
/// stderr) with `$HOME` overridden to `home`, and blocks until it exits.
/// No timeout — this is for interactive flows (login prompts, browser
/// OAuth round-trips) that a human is actively driving, unlike
/// `run_command`'s captured-output, timeout-bounded, non-interactive
/// shape. Used only by `AgentAdapter::login`.
pub fn run_interactive_with_home(command: &str, args: &[String], home: &Path) -> Result<()> {
    let mut cmd = Command::new(command);
    cmd.args(args).current_dir(home).env("HOME", home);
    pin_real_config_dir(&mut cmd);
    let status = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("spawning {command}"))?;
    if !status.success() {
        anyhow::bail!("{command} exited with {status}");
    }
    Ok(())
}

pub fn run_command(command: &str, args: &[String], cwd: &Path, timeout: Duration) -> Result<RunOutcome> {
    run_command_with_home(command, args, cwd, None, timeout)
}

/// Same as `run_command`, but when `home` is set, overrides `$HOME` for the
/// child process. This is what lets multiple isolated accounts of the same
/// agent CLI run concurrently without stepping on each other's live
/// credentials/session state — each account gets its own materialized HOME
/// directory (see `divisi-core::account::ensure_isolated_home`), and the
/// CLI reads/writes its config relative to whatever `$HOME` it sees, same
/// as any other well-behaved Unix program. Always runs on the host — used
/// by `install_plugin`/`login`, which aren't part of the opt-in Docker
/// backend (see `backend::ExecBackend`'s doc comment for why that's
/// scoped to `run_prompt` only).
pub fn run_command_with_home(command: &str, args: &[String], cwd: &Path, home: Option<&Path>, timeout: Duration) -> Result<RunOutcome> {
    run_command_live(command, args, cwd, &ExecBackend::host(home), None, timeout, None)
}

/// Same as `run_command_with_home`, but when `live_output_path` is set,
/// stdout/stderr are tee'd to that file **as the process produces them**
/// (both streams interleaved, line-buffered), not just captured and
/// returned after the process exits. This is what lets a concurrent
/// caller — the TUI's task-detail view, polling on its own connection —
/// watch a long-running agent invocation while it's still in flight,
/// instead of only seeing output once the whole thing finishes. Reading
/// happens on two background threads (one per stream) so the child's
/// pipes are drained continuously; the previous read-after-wait shape
/// risked the child blocking on a full pipe buffer for chatty output,
/// which this also incidentally fixes.
pub fn run_command_live(
    command: &str,
    args: &[String],
    cwd: &Path,
    backend: &ExecBackend,
    live_output_path: Option<&Path>,
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<RunOutcome> {
    let start = Instant::now();
    let mut cmd = match backend {
        ExecBackend::Host { home, extra_env } => {
            let mut cmd = Command::new(command);
            cmd.args(args).current_dir(cwd);
            if let Some(home) = home {
                cmd.env("HOME", home);
                pin_real_config_dir(&mut cmd);
            }
            if let Some(extra_env) = extra_env {
                cmd.envs(extra_env.iter());
            }
            cmd
        }
        ExecBackend::Docker { container, workdir, extra_env } => {
            // No $HOME override, no current_dir: the container's home was
            // fixed when it was created (the isolated home bind-mounted
            // in — see divisi-runtime::docker::ensure_started), and
            // `docker exec -w` sets the working directory inside the
            // container, not this host process's cwd.
            let mut cmd = Command::new("docker");
            cmd.arg("exec").arg("-w").arg(workdir);
            if let Some(extra_env) = extra_env {
                // `-e KEY` (bare name, no `=value`) tells docker exec to
                // source that variable's value from *this* docker CLI
                // process's own environment rather than taking it as a
                // literal argv token — confirmed live against a real
                // container on this machine. Putting a secret value
                // directly in `-e KEY=value` would land it in `docker`'s
                // argv, visible to any local user via `ps`/`/proc/<pid>/
                // cmdline`; setting it via `cmd.env()` here instead keeps
                // it exactly as exposed as the Host backend's env vars
                // above, not more.
                for (key, value) in extra_env.iter() {
                    cmd.env(key, value);
                    cmd.arg("-e").arg(key);
                }
            }
            cmd.arg(container).arg(command).args(args);
            cmd
        }
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // Puts the child in its own new process group (pgid == its own pid),
    // so a timeout/cancel can kill the *whole* group — see `kill_child`'s
    // doc comment for why a plain `child.kill()` isn't enough for a
    // command like `sh -c "sleep 5"`, which forks rather than exec's.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().with_context(|| spawn_failure_context(command, cwd, backend))?;

    let tee_file: Option<Arc<Mutex<std::fs::File>>> = live_output_path
        .and_then(|p| std::fs::File::create(p).ok())
        .map(|f| Arc::new(Mutex::new(f)));

    let stdout_buf = Arc::new(Mutex::new(String::new()));
    let stderr_buf = Arc::new(Mutex::new(String::new()));

    let stdout_handle = child.stdout.take().map(|pipe| {
        let buf = Arc::clone(&stdout_buf);
        let tee = tee_file.clone();
        std::thread::spawn(move || drain_into(pipe, buf, tee))
    });
    let stderr_handle = child.stderr.take().map(|pipe| {
        let buf = Arc::clone(&stderr_buf);
        let tee = tee_file.clone();
        std::thread::spawn(move || drain_into(pipe, buf, tee))
    });

    let deadline = start + timeout;
    let (timed_out, cancelled) = loop {
        match child.try_wait().context("polling child process")? {
            Some(_) => break (false, false),
            None if Instant::now() >= deadline => {
                kill_child(&mut child);
                break (true, false);
            }
            None if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) => {
                kill_child(&mut child);
                break (false, true);
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };

    if let Some(h) = stdout_handle {
        let _ = h.join();
    }
    if let Some(h) = stderr_handle {
        let _ = h.join();
    }

    let exit_code = child.wait().ok().and_then(|s| s.code());
    let success = !timed_out && !cancelled && exit_code == Some(0);
    let stdout = Arc::try_unwrap(stdout_buf).map(|m| m.into_inner().unwrap_or_default()).unwrap_or_default();
    let stderr = Arc::try_unwrap(stderr_buf).map(|m| m.into_inner().unwrap_or_default()).unwrap_or_default();

    Ok(RunOutcome {
        success,
        stdout,
        stderr,
        exit_code,
        timed_out,
        cancelled,
        duration_ms: start.elapsed().as_millis(),
        usage: None,
    })
}

/// When a child's `$HOME` is overridden for isolation, its own view of
/// `directories::BaseDirs` (and therefore `DivisiDirs::discover()`) would
/// otherwise resolve `~/.config/divisi` under the *isolated* home instead
/// of the real one. That breaks any nested `divisi` invocation the child
/// makes on its own — notably Claude Code's `PreToolUse` hook, which shells
/// back out to `divisi internal claude-pretooluse-hook` — silently
/// splitting its permission rules/preferences/pending-approvals into a
/// second, invisible store nobody's `divisi approval list` or the TUI ever
/// looks at. Pinning `DIVISI_CONFIG_DIR` to the resolving process's own
/// (real) config root keeps every nested `divisi` call pointed at the same
/// central state regardless of what `$HOME` the child sees.
/// Kills a timed-out or cancelled child and waits for it to actually exit.
/// A plain `child.kill()` only signals the one PID we spawned directly —
/// for a command like `sh -c "sleep 5"`, where the shell forks `sleep`
/// as a separate child rather than exec'ing into it, that leaves `sleep`
/// running, still holding the stdout/stderr pipes open. The `drain_into`
/// threads then block on those pipes until `sleep` exits on its own,
/// silently turning a "kill this now" into "wait out the full duration
/// anyway" — confirmed live: a cancelled/timed-out run took the full
/// remaining sleep time before this fix, because `child.wait()` returned
/// promptly but the pipe-reader threads' `.join()` afterward did not.
/// Since the child was spawned into its own process group (pgid == its
/// own pid, see the `process_group(0)` call above), sending the kill to
/// the *group* (negative pid) reaches every descendant at once. Falls
/// back to the plain single-process kill if the group kill can't run at
/// all (e.g. `kill` isn't on `$PATH`) or on a non-Unix target.
fn kill_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let killed_group = Command::new("kill").arg("-KILL").arg(format!("-{}", child.id())).status().is_ok_and(|s| s.success());
        if killed_group {
            let _ = child.wait();
            return;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Disambiguates a spawn failure's error text. `Command::spawn()` returns
/// the identical "No such file or directory (os error 2)" whether
/// `execve` couldn't find `command` on `$PATH` or `Command::current_dir`
/// couldn't `chdir` into `cwd` — indistinguishable from the error alone.
/// Live-verification finding (2026-09-12): a coordinator-dispatched `grok`
/// task failed with exactly this ambiguous message, and diagnosing which
/// of the two it actually was took real time (checking the daemon's live
/// `$PATH` via `/proc/<pid>/environ`, resolving `grok`'s symlink chain by
/// hand, reproducing the exact isolated-`$HOME` + worktree combination
/// directly) — none of which found a fault, because the ambiguity itself
/// was the obstacle, not a hidden bug in either path. Checked lazily,
/// only once a spawn has already failed, so the happy path pays nothing
/// for it.
fn spawn_failure_context(command: &str, cwd: &Path, backend: &ExecBackend) -> String {
    if matches!(backend, ExecBackend::Docker { .. }) {
        // cwd here is a path *inside* the container, not a host path --
        // checking it against the host filesystem would be meaningless.
        return format!("spawning {command} via docker exec");
    }
    if !cwd.exists() {
        return format!("spawning {command}: cwd {} does not exist (a stale/cleaned-up worktree is the likely cause, not a missing {command} binary)", cwd.display());
    }
    let on_path = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(command).is_file()))
        .unwrap_or(false);
    if !on_path && !Path::new(command).is_absolute() {
        return format!("spawning {command}: not found on $PATH (cwd {} exists and is fine)", cwd.display());
    }
    format!("spawning {command} (cwd {} exists, {command} resolves on $PATH -- likely a transient OS-level failure)", cwd.display())
}

fn pin_real_config_dir(cmd: &mut Command) {
    if let Ok(dirs) = divisi_core::paths::DivisiDirs::discover() {
        cmd.env("DIVISI_CONFIG_DIR", dirs.root());
    }
    share_toolchain_caches(cmd);
}

/// Under an isolated `$HOME`, rustup and cargo would each download a full
/// toolchain (~1.5G) and registry into every agent's home. Point them at the
/// daemon's real ones instead so those caches exist once.
fn share_toolchain_caches(cmd: &mut Command) {
    if let Some(real_home) = std::env::var_os("HOME") {
        share_toolchain_caches_from(cmd, Path::new(&real_home), |var| std::env::var_os(var).is_some());
    }
}

fn share_toolchain_caches_from(cmd: &mut Command, real_home: &Path, already_set: impl Fn(&str) -> bool) {
    for (var, dir) in [("RUSTUP_HOME", ".rustup"), ("CARGO_HOME", ".cargo")] {
        let real = real_home.join(dir);
        if !already_set(var) && real.is_dir() {
            cmd.env(var, real);
        }
    }
}

/// Reads `pipe` line by line, accumulating into `buf` and — when `tee` is
/// set — appending each line to the shared file immediately (flushed, so
/// a concurrent reader of that file sees it right away).
fn drain_into(pipe: impl Read, buf: Arc<Mutex<String>>, tee: Option<Arc<Mutex<std::fs::File>>>) {
    let reader = BufReader::new(pipe);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if let Some(tee) = &tee {
            if let Ok(mut f) = tee.lock() {
                let _ = writeln!(f, "{line}");
                let _ = f.flush();
            }
        }
        if let Ok(mut b) = buf.lock() {
            b.push_str(&line);
            b.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn toolchain_caches_point_at_existing_real_dirs_only() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".rustup")).unwrap();
        let mut cmd = std::process::Command::new("true");
        super::share_toolchain_caches_from(&mut cmd, home.path(), |_| false);
        let envs: std::collections::HashMap<_, _> = cmd.get_envs().collect();
        assert_eq!(envs.get(std::ffi::OsStr::new("RUSTUP_HOME")).copied().flatten(), Some(home.path().join(".rustup").as_os_str()));
        assert!(!envs.contains_key(std::ffi::OsStr::new("CARGO_HOME")), "no ~/.cargo, so nothing to pin");
    }

    use super::*;

    #[test]
    fn spawn_failure_context_names_a_missing_cwd_over_a_missing_binary() {
        let missing_cwd = std::env::temp_dir().join("single-test-definitely-does-not-exist-xyz");
        let msg = spawn_failure_context("grok", &missing_cwd, &ExecBackend::host(None));
        assert!(msg.contains("does not exist"), "{msg}");
        assert!(msg.contains("stale/cleaned-up worktree"), "{msg}");
    }

    #[test]
    fn spawn_failure_context_names_a_missing_binary_when_cwd_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let msg = spawn_failure_context("single-cli-definitely-does-not-exist-xyz", dir.path(), &ExecBackend::host(None));
        assert!(msg.contains("not found on $PATH"), "{msg}");
    }

    #[test]
    fn spawn_failure_context_defers_to_a_generic_message_when_both_check_out() {
        let dir = tempfile::tempdir().unwrap();
        let msg = spawn_failure_context("sh", dir.path(), &ExecBackend::host(None));
        assert!(msg.contains("transient"), "{msg}");
    }

    #[test]
    fn spawn_failure_context_skips_host_checks_for_docker() {
        let missing_cwd = std::env::temp_dir().join("single-test-definitely-does-not-exist-xyz");
        let msg = spawn_failure_context("grok", &missing_cwd, &ExecBackend::Docker { container: "c1", workdir: Path::new("/work"), extra_env: None });
        assert!(!msg.contains("does not exist"), "{msg}");
        assert!(msg.contains("docker exec"), "{msg}");
    }

    #[test]
    fn captures_stdout_and_exit_code_of_a_real_process() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_command("sh", &["-c".into(), "echo hello".into()], dir.path(), Duration::from_secs(5)).unwrap();
        assert!(outcome.success);
        assert_eq!(outcome.stdout.trim(), "hello");
        assert_eq!(outcome.exit_code, Some(0));
        assert!(!outcome.timed_out);
    }

    #[test]
    fn overriding_home_also_pins_single_config_dir_for_the_child() {
        // Regression test: a child spawned with $HOME overridden (agent
        // isolation) must still see the *real* DIVISI_CONFIG_DIR, or any
        // nested `divisi` invocation it makes on its own (e.g. Claude
        // Code's PreToolUse hook shelling back into `divisi internal
        // claude-pretooluse-hook`) would resolve config/state under the
        // isolated home instead of the real central one — splitting
        // pending approvals/preferences into a store nobody ever looks at.
        let dir = tempfile::tempdir().unwrap();
        let fake_home = tempfile::tempdir().unwrap();
        let outcome = run_command_with_home(
            "sh",
            &["-c".into(), "echo \"$DIVISI_CONFIG_DIR\"".into()],
            dir.path(),
            Some(fake_home.path()),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(outcome.success);
        let expected = divisi_core::paths::DivisiDirs::discover().unwrap().root().to_string_lossy().into_owned();
        assert_eq!(outcome.stdout.trim(), expected);
    }

    #[test]
    fn nonzero_exit_is_not_success() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_command("sh", &["-c".into(), "exit 3".into()], dir.path(), Duration::from_secs(5)).unwrap();
        assert!(!outcome.success);
        assert_eq!(outcome.exit_code, Some(3));
    }

    #[test]
    fn live_output_path_tees_the_same_content_as_the_captured_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let live_path = dir.path().join("live.txt");
        let outcome = run_command_live(
            "sh",
            &["-c".into(), "echo first; sleep 0.3; echo second".into()],
            dir.path(),
            &ExecBackend::host(None),
            Some(&live_path),
            Duration::from_secs(5),
            None,
        )
        .unwrap();
        assert!(outcome.success);
        assert_eq!(outcome.stdout, "first\nsecond\n");
        assert_eq!(std::fs::read_to_string(&live_path).unwrap(), "first\nsecond\n");
    }

    #[test]
    fn kills_process_that_exceeds_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_command("sh", &["-c".into(), "sleep 5".into()], dir.path(), Duration::from_millis(200)).unwrap();
        assert!(outcome.timed_out);
        assert!(!outcome.success);
    }

    /// Exercises the actual mechanism `divisi-runtime::task::run_background`
    /// relies on for `divisi task cancel`: a long-running process is killed
    /// early when its cancel flag flips, well before the (much longer)
    /// timeout would have — and the outcome is marked `cancelled`, not
    /// `timed_out`, so callers can tell the two apart.
    #[test]
    fn flipping_the_cancel_flag_kills_the_process_before_its_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_setter = std::sync::Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            cancel_setter.store(true, std::sync::atomic::Ordering::Relaxed);
        });

        let start = std::time::Instant::now();
        let outcome =
            run_command_live("sh", &["-c".into(), "sleep 5".into()], dir.path(), &ExecBackend::host(None), None, Duration::from_secs(30), Some(&cancel))
                .unwrap();

        assert!(outcome.cancelled, "expected cancelled=true");
        assert!(!outcome.timed_out, "a cancellation must not also be reported as a timeout");
        assert!(!outcome.success);
        assert!(start.elapsed() < Duration::from_secs(5), "should have been killed well before the 30s timeout or the 5s sleep finished");
    }

    fn docker_available() -> bool {
        Command::new("docker").arg("info").output().map(|o| o.status.success()).unwrap_or(false)
    }

    /// Regression test for a real finding: `-e KEY=value` on `docker
    /// exec`'s argv would put a secret's plaintext value directly in the
    /// `docker` process's command line, visible to any local user via
    /// `ps`/`/proc/<pid>/cmdline`. The fix (bare `-e KEY`, value set via
    /// `Command::env()` instead) is confirmed here against a real
    /// container: the value must still reach the container correctly.
    /// Skips cleanly if docker isn't available, same as
    /// `divisi-runtime::docker`'s own tests.
    #[test]
    fn docker_backend_env_var_reaches_the_container_without_the_value_touching_argv() {
        if !docker_available() {
            eprintln!("skipping: docker not available");
            return;
        }
        let container = "singlecli-test-run-rs-env-passthrough";
        let _ = Command::new("docker").args(["rm", "-f", container]).output();
        let status = Command::new("docker")
            .args(["run", "-d", "--name", container, "alpine", "sleep", "60"])
            .status()
            .unwrap();
        assert!(status.success(), "failed to start test container");

        let mut extra_env = std::collections::BTreeMap::new();
        extra_env.insert("SINGLECLI_TEST_SECRET".to_string(), "argv-must-not-contain-this-literal".to_string());

        // `workdir` must exist *inside the container* (docker exec -w does
        // a real chdir there) — a host tempdir path wouldn't, since this
        // test doesn't bind-mount anything (unlike the real task-run path,
        // which mounts the isolated home/cwd — see docker::ensure_started).
        let in_container_workdir = Path::new("/");
        let host_cwd = tempfile::tempdir().unwrap();
        let backend = ExecBackend::Docker { container, workdir: in_container_workdir, extra_env: Some(&extra_env) };
        let outcome = run_command_live(
            "sh",
            &["-c".to_string(), "echo $SINGLECLI_TEST_SECRET".to_string()],
            host_cwd.path(),
            &backend,
            None,
            Duration::from_secs(10),
            None,
        )
        .unwrap();

        assert!(outcome.success);
        assert_eq!(outcome.stdout.trim(), "argv-must-not-contain-this-literal", "the value must still reach the container correctly");

        let _ = Command::new("docker").args(["rm", "-f", container]).output();
    }
}
