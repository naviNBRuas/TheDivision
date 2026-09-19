//! Task-lifecycle event hooks: fire an external command when a task
//! reaches a terminal status, so a caller (a script, a webhook, or
//! another Claude Code session acting as coordinator) can react by
//! subscription instead of polling `divisi task list` in a loop. Stored
//! at `~/.config/divisi/task_hooks.toml`, same load/save shape as
//! `fallback.rs`/`hooks.rs`.
//!
//! Deliberately distinct from `hooks.rs`, which is a completely different
//! feature (Claude Code's PreToolUse mid-run permission interception —
//! pausing an agent *during* a run to ask permission before a tool call).
//! This module never pauses a run; it only reacts *after* a task's status
//! is already final. Different name, different config file, different CLI
//! path (`divisi task-hook`, not nested under `agent`) so the two are
//! never confused.
//!
//! Only fires on `completed`/`failed`/`cancelled` (`TaskStatus`'s
//! terminal values) — not on the transition into `Running`. A "started"
//! event was considered but left out: every real use case behind this
//! feature (notify-on-done, trigger-a-follow-up, log-to-a-dashboard) only
//! cares about the outcome, and a fourth event multiplies the `on` matrix
//! to document/test for no confirmed use case. Easy to add later.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use divisi_protocol::{TaskHookPayload, TaskHookRule};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Hard ceiling on how long a hook command gets before its watcher thread
/// stops waiting on it and kills it. This only bounds that detached
/// thread — see `fire`'s doc comment for why it never blocks the task
/// whose status change triggered the hook.
pub const TASK_HOOK_TIMEOUT_SECS: u64 = 300;

pub const EVENTS: &[&str] = &["completed", "failed", "cancelled"];

/// `on = ["all"]` is shorthand for every event in `EVENTS`, so a rule
/// meant to fire on anything doesn't need to be kept in sync by hand as
/// new events are (rarely) added.
fn matches_event(rule: &TaskHookRule, event: &str) -> bool {
    rule.on.iter().any(|e| e == "all" || e == event)
}

fn matches_filters(rule: &TaskHookRule, agent: &str, workspace: &str) -> bool {
    rule.agent.as_deref().is_none_or(|a| a == agent) && rule.workspace.as_deref().is_none_or(|w| w == workspace)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TaskHooksFile {
    #[serde(default)]
    hook: Vec<TaskHookRule>,
}

pub fn load(path: &Path) -> Result<Vec<TaskHookRule>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: TaskHooksFile = toml::from_str(&text).with_context(|| format!("parsing {} as TOML", path.display()))?;
    Ok(file.hook)
}

fn save(path: &Path, hooks: &[TaskHookRule]) -> Result<()> {
    let rendered = toml::to_string_pretty(&TaskHooksFile { hook: hooks.to_vec() }).context("serializing task hooks")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))
}

pub fn add(path: &Path, rule: TaskHookRule) -> Result<()> {
    anyhow::ensure!(!rule.on.is_empty(), "a task hook needs at least one event in `on` (or [\"all\"])");
    for e in &rule.on {
        anyhow::ensure!(
            e == "all" || EVENTS.contains(&e.as_str()),
            "unknown event '{e}' — expected one of: all, {}",
            EVENTS.join(", ")
        );
    }
    anyhow::ensure!(!rule.command.trim().is_empty(), "a task hook needs a non-empty command");
    let mut hooks = load(path)?;
    hooks.push(rule);
    save(path, &hooks)
}

pub fn list(path: &Path) -> Result<Vec<TaskHookRule>> {
    load(path)
}

/// Removes every rule whose `command` matches exactly — a rule has no
/// other stable identity to key removal on (unlike `fallback.rs`'s
/// chains, which are keyed by their first entry). Returns how many were
/// removed so the caller can report "no such hook" when it's zero.
pub fn remove(path: &Path, command: &str) -> Result<usize> {
    let mut hooks = load(path)?;
    let before = hooks.len();
    hooks.retain(|h| h.command != command);
    let removed = before - hooks.len();
    if removed > 0 {
        save(path, &hooks)?;
    }
    Ok(removed)
}

/// Runs every rule in `path` whose `on`/`agent`/`workspace` match this
/// payload, each as a detached `sh -c` child with `payload` piped to its
/// stdin as one line of JSON. Fire-and-forget: spawns a watcher thread per
/// matching rule that waits up to `TASK_HOOK_TIMEOUT_SECS` and kills the
/// child if it overruns, but never blocks the caller and never turns a
/// hook failure into an error the caller sees — a mis-typed hook command
/// must not be able to make task completion itself look like it failed.
/// Errors (bad config, spawn failure, non-zero exit, timeout) are only
/// logged via `tracing::warn!`, never propagated.
pub fn fire(path: &Path, payload: &TaskHookPayload) {
    let hooks = match load(path) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("task-hook: failed to load {}: {e:#}", path.display());
            return;
        }
    };
    let matching: Vec<TaskHookRule> =
        hooks.into_iter().filter(|h| matches_event(h, &payload.status) && matches_filters(h, &payload.agent, &payload.workspace_id)).collect();
    if matching.is_empty() {
        return;
    }
    let body = match serde_json::to_string(payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("task-hook: failed to serialize payload for task #{}: {e:#}", payload.id);
            return;
        }
    };
    for rule in matching {
        let body = body.clone();
        let task_id = payload.id;
        std::thread::spawn(move || {
            if let Err(e) = run_one(&rule.command, &body) {
                tracing::warn!("task-hook: command for task #{task_id} failed: {e:#}");
            }
        });
    }
}

fn run_one(command: &str, stdin_body: &str) -> Result<()> {
    let mut child =
        Command::new("sh").arg("-c").arg(command).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().with_context(|| {
            format!("spawning hook command: {command}")
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        // A hook command that doesn't read stdin (e.g. a bare `notify-send
        // ...`) would otherwise make this write block forever once the
        // pipe buffer fills — write from a second thread so a hung reader
        // can't wedge the watcher thread that's also enforcing the
        // timeout below.
        let body = stdin_body.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(body.as_bytes());
        });
    }
    let deadline = Duration::from_secs(TASK_HOOK_TIMEOUT_SECS);
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::ensure!(status.success(), "hook command exited with {status}");
            return Ok(());
        }
        if start.elapsed() >= deadline {
            let _ = child.kill();
            anyhow::bail!("hook command timed out after {TASK_HOOK_TIMEOUT_SECS}s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Runs the configured hook whose `command` matches exactly, synchronously,
/// against a synthetic payload — for `divisi task-hook test`, where the
/// user wants an immediate pass/fail, not a detached fire-and-forget (that's
/// what `fire` is for). Errors if no hook has that exact command, or if the
/// command itself fails/times out, so the CLI can report either clearly.
pub fn test(path: &Path, command: &str) -> Result<()> {
    let hooks = load(path)?;
    anyhow::ensure!(hooks.iter().any(|h| h.command == command), "no configured task hook has command: {command}");
    let payload = TaskHookPayload {
        id: 0,
        status: "completed".to_string(),
        agent: "test".to_string(),
        cwd: ".".to_string(),
        workspace_id: "test".to_string(),
        summary: Some("divisi task-hook test".to_string()),
    };
    let body = serde_json::to_string(&payload).context("serializing test payload")?;
    run_one(command, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload(status: &str) -> TaskHookPayload {
        TaskHookPayload {
            id: 1,
            status: status.to_string(),
            agent: "opencode".to_string(),
            cwd: "/tmp/example".to_string(),
            workspace_id: "example".to_string(),
            summary: Some("done".to_string()),
        }
    }

    #[test]
    fn round_trips_with_optional_filters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task_hooks.toml");
        add(&path, TaskHookRule { on: vec!["completed".to_string()], command: "true".to_string(), agent: None, workspace: None }).unwrap();
        add(
            &path,
            TaskHookRule {
                on: vec!["failed".to_string(), "cancelled".to_string()],
                command: "false".to_string(),
                agent: Some("opencode".to_string()),
                workspace: Some("example".to_string()),
            },
        )
        .unwrap();
        let loaded = list(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[1].agent.as_deref(), Some("opencode"));
        assert_eq!(loaded[1].workspace.as_deref(), Some("example"));
    }

    #[test]
    fn rejects_unknown_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task_hooks.toml");
        let err = add(&path, TaskHookRule { on: vec!["bogus".to_string()], command: "true".to_string(), agent: None, workspace: None });
        assert!(err.is_err());
    }

    #[test]
    fn remove_reports_how_many_matched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task_hooks.toml");
        add(&path, TaskHookRule { on: vec!["all".to_string()], command: "true".to_string(), agent: None, workspace: None }).unwrap();
        assert_eq!(remove(&path, "true").unwrap(), 1);
        assert_eq!(remove(&path, "true").unwrap(), 0);
        assert!(list(&path).unwrap().is_empty());
    }

    #[test]
    fn non_matching_event_does_not_fire() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task_hooks.toml");
        let marker = dir.path().join("marker");
        add(&path, TaskHookRule { on: vec!["failed".to_string()], command: format!("touch {}", marker.display()), agent: None, workspace: None })
            .unwrap();
        fire(&path, &sample_payload("completed"));
        std::thread::sleep(Duration::from_millis(300));
        assert!(!marker.exists(), "hook fired for an event not in its `on` list");
    }

    #[test]
    fn matching_event_fires_with_well_formed_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task_hooks.toml");
        let out = dir.path().join("out.json");
        add(&path, TaskHookRule { on: vec!["completed".to_string()], command: format!("cat > {}", out.display()), agent: None, workspace: None })
            .unwrap();
        fire(&path, &sample_payload("completed"));
        std::thread::sleep(Duration::from_millis(500));
        let text = std::fs::read_to_string(&out).expect("hook did not write its captured stdin");
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("hook stdin was not well-formed JSON");
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["status"], "completed");
    }

    #[test]
    fn filters_restrict_firing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task_hooks.toml");
        let marker = dir.path().join("marker");
        add(
            &path,
            TaskHookRule {
                on: vec!["all".to_string()],
                command: format!("touch {}", marker.display()),
                agent: Some("codex".to_string()),
                workspace: None,
            },
        )
        .unwrap();
        fire(&path, &sample_payload("completed")); // agent is "opencode", not "codex"
        std::thread::sleep(Duration::from_millis(300));
        assert!(!marker.exists(), "hook fired despite agent filter mismatch");
    }

    #[test]
    fn failing_command_does_not_panic_or_propagate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task_hooks.toml");
        add(&path, TaskHookRule { on: vec!["all".to_string()], command: "exit 1".to_string(), agent: None, workspace: None }).unwrap();
        // `fire` returns () — this is really asserting it doesn't panic
        // and the caller's flow (here, just this test continuing) isn't
        // disrupted by the failing hook underneath it.
        fire(&path, &sample_payload("failed"));
        std::thread::sleep(Duration::from_millis(300));
    }
}
