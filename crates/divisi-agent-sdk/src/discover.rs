//! Real detection of an agent CLI on `$PATH` — shells out to the same
//! commands used during manual investigation (`which`, `<cmd> --version`).
//! No capability is inferred here beyond "is it installed and what version
//! does it report."
//!
//! Both spawns are bounded by a short timeout: some agent CLIs never exit
//! on `--version` (they wait on stdin, or `--version` isn't wired and they
//! drop into a REPL). Without a bound, one hung probe blocks the parallel
//! `AgentList` / `Status` / `doctor` fan-out forever and, run twice,
//! leaks enough stuck child processes to OOM the daemon.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Return `$PATH` with the well-known agent bin dirs that exist on disk
/// appended, without touching the order or precedence of anything already
/// there.
///
/// The daemon inherits whatever `PATH` its launcher pinned — for the
/// systemd unit that is a hand-maintained list, so a newly installed
/// agent (or a bumped Node version under nvm) silently reads as "not
/// installed" until someone edits the unit. Appending the standard
/// install locations here means detection keeps working without that
/// edit; existing entries stay first, so nothing overrides a binary the
/// operator deliberately put earlier on `PATH`.
pub fn augmented_path(current: Option<&str>, home: &Path) -> String {
    let mut entries: Vec<String> = current
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let mut candidates: Vec<std::path::PathBuf> = [
        ".local/bin",
        ".local/share/single/bin",
        ".opencode/bin",
        ".bun/bin",
        ".deno/bin",
        ".cargo/bin",
        ".codex/bin",
        ".kilo/bin",
        "go/bin",
        ".local/go/bin",
        ".npm-global/bin",
        ".local/share/pnpm",
    ]
    .iter()
    .map(|rel| home.join(rel))
    .collect();

    // Every installed Node under nvm has its own `bin`; the active one
    // changes on a version bump, so add all of them rather than guess.
    if let Ok(versions) = std::fs::read_dir(home.join(".nvm/versions/node")) {
        for entry in versions.flatten() {
            if entry.path().is_dir() {
                candidates.push(entry.path().join("bin"));
            }
        }
    }

    for cand in candidates {
        let Some(cand) = cand.to_str().map(str::to_string) else { continue };
        // Only append a dir that (a) is not already on PATH at any
        // position and (b) actually exists — a missing dir on PATH just
        // slows every lookup.
        if !entries.iter().any(|e| e == &cand) && Path::new(&cand).is_dir() {
            entries.push(cand);
        }
    }

    entries.join(":")
}

/// Upper bound for `<cmd> --version`. Real CLIs answer in well under a
/// second; anything past this is a hang, not a slow start.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// `which` is a cheap builtin-like lookup; give it very little rope.
const WHICH_TIMEOUT: Duration = Duration::from_secs(3);
/// Per-stream cap on captured probe output. A `--version` string is a
/// handful of bytes; a misbehaving CLI that dumps its help text or a REPL
/// banner instead must not be buffered without bound (30 of those at once
/// is real memory on the daemon). Confirmed cause of the RSS spike, along
/// with `MAX_CONCURRENT_PROBES` below.
const MAX_PROBE_OUTPUT: u64 = 64 * 1024;
/// Global ceiling on concurrent probes. Every probe spawns up to two
/// children (`which`, then `<cmd> --version`); most agent CLIs are
/// node/bun bundles that fault in a ~100 MB runtime just to print a
/// version. The daemon fans `discover()` out across ~30 agents at once for
/// `Status` / `AgentList` / `doctor` — uncapped that is ~30 runtimes
/// resident together, which is what drove `divisid` to ~2.6 GB.
/// 4 keeps the fan-out moving without the pile-up.
const MAX_CONCURRENT_PROBES: usize = 4;

/// (in-flight count, wakeup). Shared by every `discover()` caller in the
/// process, so the cap holds whether probes come from `doctor`'s
/// sequential loop or the handlers' per-agent thread fan-out.
fn probe_gate() -> &'static (Mutex<usize>, Condvar) {
    static GATE: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();
    GATE.get_or_init(|| (Mutex::new(0), Condvar::new()))
}

/// RAII permit: blocks until a probe slot is free, frees it on drop
/// (including panic-unwind), so a killed or panicking probe never leaks a
/// slot.
struct ProbePermit;

impl ProbePermit {
    fn acquire() -> Self {
        let (lock, cvar) = probe_gate();
        let mut n = lock.lock().unwrap();
        while *n >= MAX_CONCURRENT_PROBES {
            n = cvar.wait(n).unwrap();
        }
        *n += 1;
        ProbePermit
    }
}

impl Drop for ProbePermit {
    fn drop(&mut self) {
        let (lock, cvar) = probe_gate();
        *lock.lock().unwrap() -= 1;
        cvar.notify_one();
    }
}

/// Read at most `MAX_PROBE_OUTPUT` bytes from a finished child's pipe.
/// Reading after exit can't deadlock (see `output_within`), and the cap
/// only matters for a CLI that ignored `--version` and printed a wall of
/// text.
fn read_capped(mut pipe: impl Read) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = pipe.by_ref().take(MAX_PROBE_OUTPUT).read_to_end(&mut buf);
    buf
}

#[derive(Debug, Clone)]
pub struct Discovery {
    pub detected: bool,
    pub resolved_path: Option<String>,
    pub version: Option<String>,
}

/// Run `cmd` to completion, or kill it and return `None` after `timeout`.
/// stdin is closed so a well-behaved child sees EOF immediately; a
/// misbehaving one is force-killed. Output is small (`--version`), so
/// reading after exit rather than concurrently cannot deadlock here.
fn output_within(cmd: &mut Command, timeout: Duration) -> Option<Output> {
    let _permit = ProbePermit::acquire();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = child.stdout.take().map(read_capped).unwrap_or_default();
                let stderr = child.stderr.take().map(read_capped).unwrap_or_default();
                return Some(Output { status, stdout, stderr });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            // `try_wait` itself failed (unusual). Still reap the child we
            // spawned — dropping the handle would leave a zombie until the
            // daemon exits.
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

pub fn discover(command: &str) -> Discovery {
    let Some(resolved_path) = resolve_path(command) else {
        return Discovery { detected: false, resolved_path: None, version: None };
    };

    let version = output_within(Command::new(command).arg("--version"), VERSION_TIMEOUT)
        .filter(|out| out.status.success())
        .and_then(|out| version_line(&String::from_utf8_lossy(&out.stdout)));

    Discovery { detected: true, resolved_path: Some(resolved_path), version }
}

/// The version out of a `--version` answer: its first non-empty line that has a digit in it. Some CLIs add
/// an update nag on a second line (copilot) or print bootstrap chatter before any version (codebuff).
pub fn version_line(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && l.chars().any(|c| c.is_ascii_digit()))
        .map(str::to_string)
}

/// Same existence check as `discover()`, but never runs `<command>
/// --version`. Live-verification finding: for a `[run]`-mode custom agent
/// (a one-shot `divisi-agent run --provider X --prompt "$2"` wrapper
/// script — see `divisi-core::custom_agents`), the script blindly forwards
/// whatever it's given as `--prompt`, so a real `--version` probe from
/// `discover()` either sends an empty prompt or misparses `--version`
/// itself as the prompt — either way it fires a real, billed LLM call and
/// the model's free-text chat reply (e.g. "I'm ready to help... could you
/// provide more details?") gets reported back as the agent's "version".
/// Use this for any custom agent with a `run` spec instead.
pub fn discover_detect_only(command: &str) -> Discovery {
    match resolve_path(command) {
        Some(resolved_path) => Discovery { detected: true, resolved_path: Some(resolved_path), version: None },
        None => Discovery { detected: false, resolved_path: None, version: None },
    }
}

fn resolve_path(command: &str) -> Option<String> {
    let which = output_within(Command::new("which").arg(command), WHICH_TIMEOUT);
    match which {
        Some(out) if out.status.success() => {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if path.is_empty() { None } else { Some(path) }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn version_line_keeps_only_the_version() {
        assert_eq!(version_line("GitHub Copilot CLI 1.0.82.\nRun 'copilot update' to check for updates.\n").as_deref(), Some("GitHub Copilot CLI 1.0.82."));
        assert_eq!(version_line("Download complete! Starting Codebuff...\ncodebuff 1.0.4\n").as_deref(), Some("codebuff 1.0.4"));
        assert_eq!(version_line("Download complete! Starting Codebuff...\n"), None);
        assert_eq!(version_line("\n  2.1.283 (Claude Code)  \n").as_deref(), Some("2.1.283 (Claude Code)"));
    }

    use super::*;

    #[test]
    fn missing_command_is_not_detected() {
        let d = discover("divisi-definitely-does-not-exist-xyz");
        assert!(!d.detected);
        assert!(d.version.is_none());
    }

    #[test]
    fn detect_only_never_probes_version_even_for_a_present_command() {
        // `echo` always exits 0 and would happily answer `--version` with
        // literal output, so this proves detect_only truly skips the
        // version probe rather than the command just failing to answer.
        let d = discover_detect_only("echo");
        assert!(d.detected);
        assert!(d.version.is_none());
    }

    #[test]
    fn detect_only_reports_undetected_for_a_missing_command() {
        let d = discover_detect_only("divisi-definitely-does-not-exist-xyz");
        assert!(!d.detected);
        assert!(d.version.is_none());
    }

    #[test]
    fn a_hanging_version_probe_is_killed_and_returns_no_version() {
        // `sleep 60` stands in for a CLI that never exits on `--version`.
        // `which sleep` succeeds, so `detected` is true, but the version
        // probe must time out fast rather than hang the test.
        let start = Instant::now();
        let out = output_within(Command::new("sleep").arg("60"), Duration::from_millis(300));
        assert!(out.is_none());
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_killed_probe_reaps_its_child_and_returns_promptly() {
        // A timed-out probe must `wait()` the child it killed, not just
        // drop the handle — a leaked zombie per probe is the accumulation
        // that OOM'd the daemon. We can't see the pid from here, but the
        // reap is synchronous inside `output_within`, so a fast return
        // means the `kill` + `wait` both completed.
        let start = Instant::now();
        let out = output_within(Command::new("sleep").arg("30"), Duration::from_millis(200));
        assert!(out.is_none());
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn augmented_path_appends_existing_agent_dirs_without_reordering() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".local/bin")).unwrap();
        std::fs::create_dir_all(home.path().join(".bun/bin")).unwrap();
        // .opencode/bin deliberately not created.

        let out = augmented_path(Some("/usr/bin:/bin"), home.path());
        let parts: Vec<&str> = out.split(':').collect();

        // Original entries stay first, in order.
        assert_eq!(&parts[..2], &["/usr/bin", "/bin"]);
        // Existing default dirs are appended...
        let local_bin = home.path().join(".local/bin").to_str().unwrap().to_string();
        let bun_bin = home.path().join(".bun/bin").to_str().unwrap().to_string();
        assert!(parts.contains(&local_bin.as_str()));
        assert!(parts.contains(&bun_bin.as_str()));
        // ...but a default dir that does not exist on disk is not.
        let opencode_bin = home.path().join(".opencode/bin").to_str().unwrap().to_string();
        assert!(!parts.contains(&opencode_bin.as_str()));

        // Every original entry precedes every appended one.
        let last_original = parts.iter().position(|p| *p == "/bin").unwrap();
        let first_appended = parts.iter().position(|p| *p == local_bin).unwrap();
        assert!(last_original < first_appended);
    }

    #[test]
    fn augmented_path_does_not_duplicate_a_dir_already_on_path() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".local/bin")).unwrap();
        let local_bin = home.path().join(".local/bin").to_str().unwrap().to_string();

        let out = augmented_path(Some(&format!("{local_bin}:/usr/bin")), home.path());
        assert_eq!(out.matches(&local_bin).count(), 1);
    }

    #[test]
    fn probe_output_is_captured_but_bounded() {
        // Normal `--version`-sized output passes through intact...
        let out = output_within(Command::new("sh").args(["-c", "printf hello"]), Duration::from_secs(5))
            .expect("sh exits promptly");
        assert_eq!(out.stdout, b"hello");
        // ...and whatever is captured is never more than the cap. (A CLI
        // that streams *more* than a pipe buffer without exiting can't be
        // read pre-exit anyway — it blocks on the full pipe and is killed
        // at the timeout; the cap just bounds the exited-but-chatty case.)
        assert!((out.stdout.len() as u64) <= MAX_PROBE_OUTPUT);
    }
}
