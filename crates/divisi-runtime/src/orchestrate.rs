//! Multi-agent orchestration (spec sections 17-22): let several agent CLIs
//! work on one goal, each seeing real output from the one before it.
//!
//! Two real modes, not one spec-shaped compromise:
//! - **`run` — sequential relay.** Every agent runs in turn, in the *same*
//!   git worktree (so agent 2 sees agent 1's actual file changes on disk,
//!   not just a text summary) and is handed agent 1's real captured
//!   output as part of its own prompt. Genuine hand-off, not a fabricated
//!   live conversation.
//! - **`run_parallel` — real concurrent execution (v0.1.17).** Each
//!   `(agent, description)` pair runs on its own OS thread, in its own
//!   git worktree, with its own SQLite connection. No automatic goal
//!   decomposition — the caller supplies each agent's own task
//!   explicitly; divisi runs them concurrently and reports what
//!   happened, it doesn't invent the split or auto-merge the resulting
//!   branches.
//!
//! **Still not built, honestly**: live bidirectional agent-to-agent
//! messaging *during* a run. That would need a persistent process the
//! runtime holds open across requests (still deferred, see ADR 0001) and
//! an inter-agent protocol none of these CLIs actually speak — every
//! agent invocation here (sequential or parallel) is a single blocking
//! `task::run` call under the hood, with the exact same real worktree
//! isolation, artifact capture, and "learn from errors" memory recording
//! as a standalone task. Asynchronous cross-agent communication (notes,
//! shared memory/knowledge-graph context) is real and covered elsewhere
//! — see `task.rs`'s `build_context_preamble` and `notes.rs`.

use crate::context::Context;
use crate::task::{self, RunTaskOptions};
use anyhow::{bail, Context as _, Result};
use rusqlite::Connection;
use divisi_protocol::{TaskRecord, TaskStatus};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct OrchestrateOptions<'a> {
    pub goal: &'a str,
    pub agents: &'a [String],
    pub cwd: &'a Path,
    pub use_worktree: bool,
    /// See `task::RunTaskOptions::real_home` — applies to every step.
    pub real_home: bool,
    pub timeout: Duration,
}

/// Caps how much of one agent's output gets quoted into the next agent's
/// prompt — real CLIs have finite context, and an unbounded relay could
/// otherwise balloon every subsequent prompt with the full transcript of
/// everything before it.
const MAX_HANDOFF_CHARS: usize = 4000;

pub fn run(conn: &Connection, ctx: &Context, opts: OrchestrateOptions) -> Result<Vec<TaskRecord>> {
    if opts.agents.is_empty() {
        bail!("orchestrate needs at least one --agent");
    }

    let shared_cwd = prepare_shared_cwd(ctx, &opts)?;

    crate::state::record_event(
        conn,
        "orchestrate.started",
        &format!("agents={} goal=\"{}\"", opts.agents.join(","), opts.goal),
    )?;

    let mut records = Vec::new();
    let mut previous: Option<(String, String)> = None; // (agent name, captured output)

    for (i, agent) in opts.agents.iter().enumerate() {
        let description = build_prompt(opts.goal, previous.as_ref());

        crate::state::record_event(conn, "orchestrate.step_started", &format!("agent={agent} step={}/{}", i + 1, opts.agents.len()))?;

        let record = task::run(conn, ctx, RunTaskOptions {
            description: &description,
            agent,
            cwd: &shared_cwd,
            use_worktree: false,
            account: None,
            real_home: opts.real_home,
            no_memory_context: false,
            timeout: opts.timeout,
            // Relay steps hand off to the *next* agent in the sequence
            // deliberately (that's the whole point of `orchestrate`) —
            // fallback chains are for a single task quietly retrying
            // itself elsewhere, a different concern.
            allow_fallback: false,
            usage_json: false,
            require_structured_output: false,
            pool_agentic: true,
        })?;

        let failed = record.status == TaskStatus::Failed;
        crate::state::record_event(
            conn,
            if failed { "orchestrate.step_failed" } else { "orchestrate.step_completed" },
            &format!("agent={agent} task=#{}", record.id),
        )?;

        previous = record
            .artifact_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|output| (agent.clone(), output));

        let stop_here = failed;
        records.push(record);
        if stop_here {
            crate::state::record_event(conn, "orchestrate.stopped_early", &format!("after step {}/{}", i + 1, opts.agents.len()))?;
            break;
        }
    }

    crate::state::record_event(conn, "orchestrate.finished", &format!("steps_completed={}", records.len()))?;
    Ok(records)
}

pub struct ParallelOrchestrateOptions<'a> {
    pub tasks: &'a [(String, String)],
    pub cwd: &'a Path,
    pub real_home: bool,
    pub timeout: Duration,
}

/// Real concurrent execution (v0.1.17), unlike `run`'s sequential relay:
/// every `(agent, description)` pair runs on its own OS thread, each with
/// its own SQLite connection (safe since `state::open` sets WAL +
/// busy_timeout — see that module's doc) and its own git worktree (via
/// `task::run`'s existing `use_worktree` path, keyed by that task's own
/// row id, so concurrent worktrees never collide on a path). There's no
/// automatic goal decomposition — the caller already decided the split;
/// this just runs the pieces at the same time and reports what happened.
/// Branches are never auto-merged back — that's a real git decision a
/// human should make, not something silently resolved here.
pub fn run_parallel(ctx: &Context, opts: ParallelOrchestrateOptions) -> Result<Vec<TaskRecord>> {
    if opts.tasks.is_empty() {
        bail!("orchestrate --parallel needs at least one --task <agent>:<description>");
    }
    let repo_ctx = divisi_core::project_context::resolve(opts.cwd);
    repo_ctx.repo_root.as_ref().context("not a git repository; --parallel requires one (each agent needs its own worktree)")?;

    let db_path = ctx.dirs.db_path();
    {
        let conn = crate::state::open(&db_path)?;
        let agents: Vec<&str> = opts.tasks.iter().map(|(a, _)| a.as_str()).collect();
        crate::state::record_event(&conn, "orchestrate.parallel_started", &format!("agents={} count={}", agents.join(","), opts.tasks.len()))?;
    }

    let handles: Vec<std::thread::JoinHandle<Result<TaskRecord>>> = opts
        .tasks
        .iter()
        .map(|(agent, description)| {
            let agent = agent.clone();
            let description = description.clone();
            let cwd = opts.cwd.to_path_buf();
            let db_path = db_path.clone();
            let ctx = ctx.clone();
            let real_home = opts.real_home;
            let timeout = opts.timeout;
            std::thread::spawn(move || -> Result<TaskRecord> {
                let conn = crate::state::open(&db_path)?;
                task::ensure_schema(&conn)?;
                crate::memory::ensure_schema(&conn)?;
                divisi_core::notes::ensure_schema(&conn)?;
                crate::knowledge_graph::ensure_schema(&conn)?;
                let record = task::run(&conn, &ctx, RunTaskOptions {
                    description: &description,
                    agent: &agent,
                    cwd: &cwd,
                    use_worktree: true,
                    account: None,
                    real_home,
                    no_memory_context: false,
                    timeout,
                    allow_fallback: false,
                    usage_json: false,
                    require_structured_output: false,
                    pool_agentic: true,
                })?;

                // Broadcast what this agent did to the rest of the batch's
                // project — parallel siblings run at the same time, so
                // none of them can see this at the moment it happens
                // (there's no live IPC — see this module's doc), but it's
                // there waiting on their next `divisi task run`/orchestrate
                // step via the same inbox `build_context_preamble` reads,
                // or on-demand via the gateway's `notes_read`. Best-effort:
                // a note-write failure must never fail the task it's
                // reporting on.
                let project = divisi_core::project_context::resolve(&cwd).repo_root;
                let summary = record.summary.clone().unwrap_or_else(|| format!("status: {:?}", record.status));
                let _ = divisi_core::notes::leave(&conn, project, &agent, None, &format!("orchestrate --parallel: {description}"), &summary);

                Ok(record)
            })
        })
        .collect();

    let mut records = Vec::new();
    let mut thread_errors = Vec::new();
    for handle in handles {
        match handle.join() {
            Ok(Ok(record)) => records.push(record),
            Ok(Err(e)) => thread_errors.push(e.to_string()),
            Err(_) => thread_errors.push("a parallel task thread panicked".to_string()),
        }
    }

    let conn = crate::state::open(&db_path)?;
    crate::state::record_event(&conn, "orchestrate.parallel_finished", &format!("completed={} errors={}", records.len(), thread_errors.len()))?;

    if !thread_errors.is_empty() && records.is_empty() {
        bail!("every parallel task failed to start: {}", thread_errors.join("; "));
    }
    Ok(records)
}

fn prepare_shared_cwd(ctx: &Context, opts: &OrchestrateOptions) -> Result<PathBuf> {
    if !opts.use_worktree {
        return Ok(opts.cwd.to_path_buf());
    }
    let repo_ctx = divisi_core::project_context::resolve(opts.cwd);
    let repo_root = repo_ctx.repo_root.context("not a git repository; --worktree requires one")?;
    let relay_id = chrono::Utc::now().timestamp_millis();
    let worktree_path = ctx.dirs.state_dir().join("worktrees").join(format!("relay-{relay_id}"));
    let branch = format!("single/relay-{relay_id}");
    divisi_core::worktree::add(Path::new(&repo_root), &worktree_path, &branch)?;
    Ok(worktree_path)
}

fn build_prompt(goal: &str, previous: Option<&(String, String)>) -> String {
    match previous {
        None => goal.to_string(),
        Some((agent, output)) => {
            let truncated = truncate(output, MAX_HANDOFF_CHARS);
            format!(
                "Overall goal: {goal}\n\n--- Output from the previous agent ({agent}) in this relay ---\n{truncated}\n--- end previous output ---\n\nContinue this work toward the overall goal above."
            )
        }
    }
}

pub(crate) fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut truncated: String = s.chars().take(max_chars).collect();
    truncated.push_str("\n[...truncated...]");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_uses_goal_verbatim_for_first_step() {
        let prompt = build_prompt("add a .gitignore", None);
        assert_eq!(prompt, "add a .gitignore");
    }

    #[test]
    fn build_prompt_includes_previous_output_and_goal() {
        let prompt = build_prompt("add tests", Some(&("claude".to_string(), "created foo.rs".to_string())));
        assert!(prompt.contains("add tests"));
        assert!(prompt.contains("claude"));
        assert!(prompt.contains("created foo.rs"));
    }

    #[test]
    fn truncate_leaves_short_strings_alone() {
        assert_eq!(truncate("hello", 100), "hello");
    }

    #[test]
    fn truncate_caps_long_strings() {
        let long = "x".repeat(10_000);
        let result = truncate(&long, 50);
        assert!(result.starts_with(&"x".repeat(50)));
        assert!(result.contains("truncated"));
    }

    #[test]
    fn run_rejects_empty_agent_list() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DIVISI_CONFIG_DIR", dir.path());
        let conn = Connection::open_in_memory().unwrap();
        task::ensure_schema(&conn).unwrap();
        crate::state::ensure_events_schema(&conn).unwrap();
        let dirs = divisi_core::DivisiDirs::from_root(dir.path().to_path_buf());
        dirs.ensure_created().unwrap();
        let ctx = Context { dirs, resolved: divisi_core::ResolvedConfig::default(), registry: divisi_core::builtin_registry() };

        let opts = OrchestrateOptions { goal: "x", agents: &[], cwd: dir.path(), use_worktree: false, real_home: false, timeout: Duration::from_secs(1) };
        assert!(run(&conn, &ctx, opts).is_err());
    }

    fn test_ctx(dir: &std::path::Path) -> Context {
        std::env::set_var("DIVISI_CONFIG_DIR", dir);
        let dirs = divisi_core::DivisiDirs::from_root(dir.to_path_buf());
        dirs.ensure_created().unwrap();
        Context { dirs, resolved: divisi_core::ResolvedConfig::default(), registry: divisi_core::builtin_registry() }
    }

    #[test]
    fn run_parallel_rejects_empty_task_list() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let opts = ParallelOrchestrateOptions { tasks: &[], cwd: dir.path(), real_home: false, timeout: Duration::from_secs(1) };
        let err = run_parallel(&ctx, opts).unwrap_err();
        assert!(err.to_string().contains("--task"));
    }

    #[test]
    fn run_parallel_requires_a_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let tasks = vec![("claude".to_string(), "do something".to_string())];
        let opts = ParallelOrchestrateOptions { tasks: &tasks, cwd: dir.path(), real_home: false, timeout: Duration::from_secs(1) };
        let err = run_parallel(&ctx, opts).unwrap_err();
        assert!(err.to_string().contains("not a git repository"));
    }

    /// Two agents with distinct tasks actually run concurrently, each in
    /// its own worktree, without the two `task::run` calls (each opening
    /// its own SQLite connection to the same db file, per `state::open`'s
    /// WAL fix) locking each other out. This makes two real, billed
    /// `claude -p` calls, so unlike this project's other "skip if the
    /// agent isn't installed" tests, it's opt-in behind an env var rather
    /// than running by default on every `cargo test` — set
    /// `DIVISI_TEST_REAL_AGENT=1` to actually run it.
    #[test]
    fn run_parallel_runs_two_real_agents_concurrently_in_separate_worktrees() {
        if std::env::var("DIVISI_TEST_REAL_AGENT").as_deref() != Ok("1") {
            eprintln!("skipping: set DIVISI_TEST_REAL_AGENT=1 to run this real-agent test");
            return;
        }
        if !divisi_agent_sdk::adapters::for_agent("claude").unwrap().discover().detected {
            eprintln!("skipping: claude not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::process::Command::new("git").arg("init").arg("-q").arg(repo).status().unwrap();
        std::process::Command::new("git").args(["-C", repo.to_str().unwrap(), "config", "user.email", "test@example.com"]).status().unwrap();
        std::process::Command::new("git").args(["-C", repo.to_str().unwrap(), "config", "user.name", "test"]).status().unwrap();
        std::fs::write(repo.join("README.md"), "test repo").unwrap();
        std::process::Command::new("git").args(["-C", repo.to_str().unwrap(), "add", "-A"]).status().unwrap();
        std::process::Command::new("git").args(["-C", repo.to_str().unwrap(), "commit", "-q", "-m", "init"]).status().unwrap();

        let config_dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(config_dir.path());

        let tasks = vec![
            ("claude".to_string(), "say the single word: alpha".to_string()),
            ("claude".to_string(), "say the single word: beta".to_string()),
        ];
        let opts = ParallelOrchestrateOptions { tasks: &tasks, cwd: repo, real_home: false, timeout: Duration::from_secs(120) };
        let records = run_parallel(&ctx, opts).unwrap();
        assert_eq!(records.len(), 2);
        let worktrees: std::collections::HashSet<_> = records.iter().filter_map(|r| r.worktree_path.clone()).collect();
        assert_eq!(worktrees.len(), 2, "each parallel task must get its own worktree");

        // Each completed step should have broadcast a note about what it did.
        let conn = crate::state::open(&ctx.dirs.db_path()).unwrap();
        let project = divisi_core::project_context::resolve(repo).repo_root;
        let inbox = divisi_core::notes::inbox(&conn, project.as_deref(), "some-third-agent", false).unwrap();
        assert_eq!(inbox.len(), 2, "both parallel steps should have left a broadcast note");
        assert!(inbox.iter().any(|n| n.from_agent == "claude"));
    }
}
