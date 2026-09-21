//! Task orchestration (spec sections 17-21), Phase 4 scope: a single task
//! delegated to a single named agent, run synchronously (the request that
//! starts it blocks until the agent finishes or times out), optionally
//! isolated in its own git worktree.
//!
//! This is honestly narrower than the full spec: no task graph/DAG, no
//! automatic agent selection, no parallel multi-agent coordination, no
//! background/cancellable execution. Those all need the runtime to hold
//! live process state across multiple requests (spec section 3's daemon
//! lifecycle, deferred since Phase 1's ADR 0001) or an actual reasoning
//! step to pick agents (spec section 20) that divisi itself doesn't
//! have. What's here is real: a real agent subprocess, real git worktree
//! isolation, a real captured artifact, and a real persisted record —
//! just for one agent at a time, one task at a time.

use crate::context::Context;
use crate::integrations;
use crate::memory;
use anyhow::{Context as _, Result};
use rusqlite::{params, Connection, OptionalExtension};
use divisi_agent_sdk::adapters::for_agent_with_custom;
use divisi_protocol::{MemoryScope, MemorySource, TaskRecord, TaskStatus};
use std::path::Path;
use std::time::Duration;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// Per-agent-name counting semaphore, keyed the same way
/// `handlers.rs::DISCOVERY_LOCKS` already keys its per-agent cache — one
/// `(Mutex<u32>, Condvar)` pair per agent name, created on first use.
/// Blocks the calling thread (not async — this whole module is
/// thread-based, see `run_background`'s `std::thread::spawn`) until a
/// slot is free, rather than failing the caller outright.
static AGENT_SLOTS: OnceLock<Mutex<std::collections::HashMap<String, Arc<(Mutex<u32>, Condvar)>>>> = OnceLock::new();

struct AgentSlotGuard {
    slot: Option<Arc<(Mutex<u32>, Condvar)>>,
}

impl Drop for AgentSlotGuard {
    fn drop(&mut self) {
        if let Some(slot) = &self.slot {
            let (lock, cvar) = &**slot;
            let mut count = lock.lock().unwrap();
            *count -= 1;
            cvar.notify_one();
        }
    }
}

/// Blocks until a concurrency slot for `agent` is available, per its
/// registry `max_concurrency` (`None` = unlimited, returns immediately
/// with a no-op guard). See `AgentDefinition.max_concurrency`'s doc
/// comment for why this exists (opencode's own SQLite session lock).
fn acquire_agent_slot(agent: &str, max_concurrency: Option<u32>) -> AgentSlotGuard {
    let Some(limit) = max_concurrency else {
        return AgentSlotGuard { slot: None };
    };
    let registry = AGENT_SLOTS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let slot = {
        let mut map = registry.lock().unwrap();
        map.entry(agent.to_string())
            .or_insert_with(|| Arc::new((Mutex::new(0u32), Condvar::new())))
            .clone()
    };
    let (lock, cvar) = &*slot;
    let mut count = lock.lock().unwrap();
    while *count >= limit {
        count = cvar.wait(count).unwrap();
    }
    *count += 1;
    drop(count);
    AgentSlotGuard { slot: Some(slot) }
}

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS tasks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            description TEXT NOT NULL,
            agent TEXT NOT NULL,
            status TEXT NOT NULL,
            worktree_path TEXT,
            artifact_path TEXT,
            exit_code INTEGER,
            timed_out INTEGER NOT NULL DEFAULT 0,
            summary TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
        (),
    )?;
    // Added for workspace-scoped tasks: a task predating this migration
    // gets '' for both (never NULL — `TaskRecord::cwd`/`workspace_id` are
    // plain `String`s, not `Option`), which the TUI shows as an explicit
    // "(unknown workspace)" bucket rather than silently misattributing it.
    add_column_if_missing(conn, "tasks", "cwd", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(conn, "tasks", "workspace_id", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(conn, "tasks", "rate_limited", "INTEGER NOT NULL DEFAULT 0")?;
    // Per-turn token accounting (E27.03). Real when the agent ran in a
    // usage-reporting mode (`claude --output-format json`), otherwise a
    // parse-of-output or a chars/4 estimate — `tokens_estimated = 1` says
    // which. NULL on a task that never produced output (setup failure).
    add_column_if_missing(conn, "tasks", "prompt_tokens", "INTEGER")?;
    add_column_if_missing(conn, "tasks", "completion_tokens", "INTEGER")?;
    add_column_if_missing(conn, "tasks", "tokens_estimated", "INTEGER NOT NULL DEFAULT 0")?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS workspaces (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            path TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
        (),
    )?;
    Ok(())
}

/// SQLite has no `ADD COLUMN IF NOT EXISTS`; check `PRAGMA table_info`
/// first so re-running `ensure_schema` against an already-migrated
/// database (every daemon startup) doesn't error trying to re-add a
/// column that's already there.
pub(crate) fn add_column_if_missing(conn: &Connection, table: &str, column: &str, ddl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>("name"))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == column);
    if !exists {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {ddl}"), ())?;
    }
    Ok(())
}

/// Records (or refreshes) a workspace's last-known display path — called
/// every time a task runs against it, so a project that's been moved on
/// disk self-heals back to showing its current location without anyone
/// having to fix it up by hand.
fn upsert_workspace(conn: &Connection, id: &str, path: &str, name: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO workspaces (id, name, path, updated_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, path = excluded.path, updated_at = excluded.updated_at",
        params![id, name, path, now],
    )?;
    Ok(())
}

/// Every workspace with at least one task, newest activity first — the
/// grouping the TUI's Tasks tab shows before drilling into one workspace's
/// tasks (see `divisi_protocol::WorkspaceInfo`).
pub fn list_workspaces(conn: &Connection) -> Result<Vec<divisi_protocol::WorkspaceInfo>> {
    let mut stmt = conn.prepare(
        "SELECT w.id, w.name, w.path, COUNT(t.id) AS task_count, MAX(t.updated_at) AS last_activity_at
         FROM workspaces w JOIN tasks t ON t.workspace_id = w.id
         GROUP BY w.id ORDER BY last_activity_at DESC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(divisi_protocol::WorkspaceInfo {
                id: row.get("id")?,
                name: row.get("name")?,
                path: row.get("path")?,
                task_count: row.get("task_count")?,
                last_activity_at: row.get("last_activity_at")?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Fetches `id`'s just-finished record and fires any matching
/// `divisi_core::task_hooks` rules — called right before every point
/// `execute` returns, so every real outcome (setup failure, agent
/// failure, timeout, cancellation, success) notifies subscribers exactly
/// once. Errors fetching the record are swallowed (logged by `fire`'s own
/// callers would be redundant here — a missing record after a status
/// write is already an anomaly `get(...).context(...)` upstream surfaces)
/// since a hook-notification problem must never mask the task's own
/// result from its caller.
fn notify_task_hooks(conn: &Connection, ctx: &Context, id: i64) {
    if let Ok(Some(record)) = get(conn, id) {
        divisi_core::task_hooks::fire(
            &ctx.dirs.task_hooks_registry_file(),
            &divisi_protocol::TaskHookPayload {
                id: record.id,
                status: status_as_str(record.status).to_string(),
                agent: record.agent,
                cwd: record.cwd,
                workspace_id: record.workspace_id,
                summary: record.summary,
            },
        );
    }
}

pub(crate) fn status_as_str(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Created => "created",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

fn parse_status(s: &str) -> Result<TaskStatus> {
    Ok(match s {
        "created" => TaskStatus::Created,
        "running" => TaskStatus::Running,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        other => anyhow::bail!("unknown task status: {other}"),
    })
}

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<TaskRecord> {
    let status_str: String = row.get("status")?;
    Ok(TaskRecord {
        id: row.get("id")?,
        description: row.get("description")?,
        agent: row.get("agent")?,
        status: parse_status(&status_str).map_err(|e| {
            rusqlite::Error::InvalidColumnType(0, e.to_string(), rusqlite::types::Type::Text)
        })?,
        worktree_path: row.get("worktree_path")?,
        artifact_path: row.get("artifact_path")?,
        exit_code: row.get("exit_code")?,
        timed_out: row.get::<_, i64>("timed_out")? != 0,
        summary: row.get("summary")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        cwd: row.get("cwd")?,
        workspace_id: row.get("workspace_id")?,
        rate_limited: row.get::<_, i64>("rate_limited")? != 0,
        prompt_tokens: row.get("prompt_tokens").ok().flatten(),
        completion_tokens: row.get("completion_tokens").ok().flatten(),
        tokens_estimated: row.get::<_, i64>("tokens_estimated").unwrap_or(0) != 0,
    })
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<TaskRecord>> {
    conn.query_row(
        "SELECT * FROM tasks WHERE id = ?1",
        params![id],
        row_to_task,
    )
    .optional()
    .context("querying task by id")
}

pub fn list(conn: &Connection) -> Result<Vec<TaskRecord>> {
    let mut stmt = conn.prepare("SELECT * FROM tasks ORDER BY created_at DESC")?;
    let rows = stmt
        .query_map([], row_to_task)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The newest `limit` tasks -- a bounded read for callers (the notch) that
/// must not pull the entire history on every poll.
pub fn list_recent(conn: &Connection, limit: usize) -> Result<Vec<TaskRecord>> {
    let mut stmt = conn.prepare("SELECT * FROM tasks ORDER BY created_at DESC LIMIT ?1")?;
    let rows = stmt
        .query_map(params![limit as i64], row_to_task)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Per-agent activity for the Usage page's "connected agents" table (see
/// `divisi_protocol::AgentLocalStats`) — every OAuth-authenticated agent
/// has no billing-API `$` data, so this is the only real signal available
/// for them: how often they've actually run, for how long, and when last.
/// Computed in Rust from `list()` rather than SQL date arithmetic, since
/// `created_at`/`updated_at` are RFC3339 strings and task volume here is
/// small enough that loading them all first is simpler than getting
/// SQLite's date functions to agree with `chrono`'s formatting.
pub fn local_stats_by_agent(conn: &Connection) -> Result<Vec<divisi_protocol::AgentLocalStats>> {
    use std::collections::BTreeMap;

    // Per-run token counts above this are treated as parse noise, not usage.
    const MAX_PLAUSIBLE_TOKENS: i64 = 5_000_000;

    // Only the light columns: `description`/`summary` can be many KB each and
    // this runs on every poll of the notch.
    let mut stmt = conn.prepare(
        "SELECT agent, created_at, updated_at, prompt_tokens, completion_tokens, tokens_estimated, rate_limited FROM tasks",
    )?;
    let now = chrono::Utc::now();
    let mut by_agent: BTreeMap<String, divisi_protocol::AgentLocalStats> = BTreeMap::new();
    let mut totals: BTreeMap<String, (u64, u64)> = BTreeMap::new(); // (duration_ms_sum, counted)
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, i64>(5)? != 0,
            r.get::<_, i64>(6)? != 0,
        ))
    })?;
    for row in rows {
        let (agent, created_at, updated_at, prompt, completion, estimated, rate_limited) = row?;
        let stat = by_agent.entry(agent.clone()).or_insert_with(|| divisi_protocol::AgentLocalStats {
            agent: agent.clone(),
            run_count: 0,
            avg_duration_ms: 0,
            last_run_at: None,
            runs_24h: 0,
            runs_7d: 0,
            prompt_tokens_7d: 0,
            completion_tokens_7d: 0,
            estimated_runs_7d: 0,
            rate_limited_7d: 0,
            discarded_token_rows: 0,
        });
        stat.run_count += 1;
        let created = chrono::DateTime::parse_from_rfc3339(&created_at);
        if let (Ok(c), Ok(u)) = (created, chrono::DateTime::parse_from_rfc3339(&updated_at)) {
            let delta = (u - c).num_milliseconds();
            if delta >= 0 {
                let t = totals.entry(agent.clone()).or_default();
                t.0 += delta as u64;
                t.1 += 1;
            }
        }
        if stat.last_run_at.as_deref().is_none_or(|last| updated_at.as_str() > last) {
            stat.last_run_at = Some(updated_at.clone());
        }
        let age = created.map(|c| now.signed_duration_since(c.with_timezone(&chrono::Utc)));
        if let Ok(age) = age {
            if age.num_hours() < 24 {
                stat.runs_24h += 1;
            }
            if age.num_days() < 7 {
                stat.runs_7d += 1;
                if rate_limited {
                    stat.rate_limited_7d += 1;
                }
                let (p, c) = (prompt.unwrap_or(0), completion.unwrap_or(0));
                if p > MAX_PLAUSIBLE_TOKENS || c > MAX_PLAUSIBLE_TOKENS {
                    stat.discarded_token_rows += 1;
                } else {
                    stat.prompt_tokens_7d += p.max(0) as u64;
                    stat.completion_tokens_7d += c.max(0) as u64;
                    if estimated {
                        stat.estimated_runs_7d += 1;
                    }
                }
            }
        }
    }
    let mut stats: Vec<_> = by_agent.into_values().collect();
    for stat in &mut stats {
        if let Some((sum, counted)) = totals.get(&stat.agent) {
            stat.avg_duration_ms = sum.checked_div(*counted).unwrap_or(0);
        }
    }
    Ok(stats)
}

fn create(conn: &Connection, description: &str, agent: &str, cwd: &str, workspace_id: &str) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO tasks (description, agent, status, timed_out, created_at, updated_at, cwd, workspace_id)
         VALUES (?1, ?2, ?3, 0, ?4, ?4, ?5, ?6)",
        params![description, agent, status_as_str(TaskStatus::Created), now, cwd, workspace_id],
    )?;
    Ok(conn.last_insert_rowid())
}

/// `create()` plus the workspace bookkeeping every real call site needs:
/// resolves `cwd`'s stable workspace identity (survives the project
/// directory moving — see `stable_workspace_id`'s doc comment) and
/// records/refreshes that workspace's last-known display path.
pub(crate) fn create_for_cwd(conn: &Connection, description: &str, agent: &str, cwd: &Path) -> Result<i64> {
    let workspace_id = divisi_core::project_context::stable_workspace_id(cwd);
    let display_path = divisi_core::project_context::resolve(cwd).repo_root.unwrap_or_else(|| cwd.display().to_string());
    let name = divisi_core::project_context::workspace_display_name(&display_path);
    let id = create(conn, description, agent, &cwd.display().to_string(), &workspace_id)?;
    upsert_workspace(conn, &workspace_id, &display_path, &name)?;
    Ok(id)
}

/// Persists a graph node that deliberately did not run. `Cancelled` is used
/// rather than inventing a graph-only status; the summary makes clear this
/// was a dependency condition, not an explicit user cancellation.
pub fn record_conditionally_skipped(
    conn: &Connection,
    description: &str,
    agent: &str,
    cwd: &Path,
    summary: &str,
) -> Result<TaskRecord> {
    let id = create_for_cwd(conn, description, agent, cwd)?;
    finish(
        conn,
        id,
        TaskStatus::Cancelled,
        None,
        None,
        None,
        false,
        Some(summary),
        false,
    )?;
    crate::state::record_event(conn, "task.cancelled", &format!("#{id} {summary}"))?;
    get(conn, id)?.context("task disappeared after being recorded as skipped")
}

#[allow(clippy::too_many_arguments)]
fn finish(
    conn: &Connection,
    id: i64,
    status: TaskStatus,
    worktree_path: Option<&str>,
    artifact_path: Option<&str>,
    exit_code: Option<i32>,
    timed_out: bool,
    summary: Option<&str>,
    rate_limited: bool,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE tasks SET status = ?1, worktree_path = ?2, artifact_path = ?3, exit_code = ?4, timed_out = ?5, summary = ?6, updated_at = ?7, rate_limited = ?8 WHERE id = ?9",
        params![status_as_str(status), worktree_path, artifact_path, exit_code, timed_out as i64, summary, now, rate_limited as i64, id],
    )?;
    Ok(())
}

/// Records a finished task's token counts (E27.03). `estimated` is stored
/// as `tokens_estimated` so downstream consumers can flag the number as
/// approximate.
fn record_token_usage(
    conn: &Connection,
    id: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
    estimated: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE tasks SET prompt_tokens = ?1, completion_tokens = ?2, tokens_estimated = ?3 WHERE id = ?4",
        params![prompt_tokens, completion_tokens, estimated as i64, id],
    )?;
    Ok(())
}

/// Best-effort token counts for an agent that did not run in a
/// usage-reporting mode. First tries to lift real numbers out of the
/// captured output (some CLIs print a usage line even in text mode), then
/// falls back to a `chars / 4` estimate. The returned bool is
/// `estimated` — true when the chars/4 fallback was used.
fn parse_or_estimate_tokens(_agent: &str, prompt: &str, stdout: &str, stderr: &str) -> (i64, i64, bool) {
    let hay = format!("{stdout}\n{stderr}");

    // shape 1: a JSON `"input_tokens": N ... "output_tokens": N` anywhere
    // (claude/codex event lines, some wrappers).
    let json_num = |key: &str| -> Option<i64> {
        let pat = format!("\"{key}\"");
        let idx = hay.find(&pat)? + pat.len();
        let rest = hay[idx..].trim_start().strip_prefix(':')?.trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    };
    if let (Some(i), Some(o)) = (json_num("input_tokens"), json_num("output_tokens")) {
        return (i, o, false);
    }

    // shape 2: a human line like "tokens: 1234 in, 567 out" / "input: 12
    // output: 34" — grab the first two integers on any line mentioning
    // "token".
    for line in hay.lines().filter(|l| l.to_lowercase().contains("token")) {
        let nums: Vec<i64> = line
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        if nums.len() >= 2 {
            return (nums[0], nums[1], false);
        }
    }

    // fallback: rough estimate. ~4 chars per token is the usual
    // back-of-envelope for English + code.
    let est = |s: &str| (s.chars().count() as i64 + 3) / 4;
    (est(prompt), est(stdout), true)
}

fn set_status(conn: &Connection, id: i64, status: TaskStatus) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
        params![status_as_str(status), now, id],
    )?;
    Ok(())
}

/// Run once at daemon startup, before the socket accepts connections. A
/// background task runs on a thread *inside* `divisid` (see
/// `run_background`) — there is no separate child process — so if the
/// daemon was killed mid-run the work is gone and the row is orphaned:
/// stuck `Running`/`Created` forever, with `task cancel` refusing it
/// ("isn't running in the background") and `task cleanup` refusing it
/// ("still running"). A freshly started daemon owns no in-flight tasks by
/// definition, so every non-terminal row it finds at boot is one of these
/// zombies. Mark them `Failed` with an "interrupted" summary and return
/// the count. Best-effort at the call site — a failure here must not stop
/// the daemon coming up.
pub fn reconcile_orphaned_tasks(conn: &Connection) -> Result<usize> {
    let now = chrono::Utc::now().to_rfc3339();
    let summary = "interrupted: divisid restarted while this task was in flight";
    let affected = conn.execute(
        "UPDATE tasks SET status = ?1, summary = ?2, updated_at = ?3 \
         WHERE status IN ('created', 'running')",
        params![status_as_str(TaskStatus::Failed), summary, now],
    )?;
    if affected > 0 {
        let _ = crate::state::record_event(
            conn,
            "task.reconciled",
            &format!("marked {affected} orphaned task(s) failed on daemon startup"),
        );
    }
    Ok(affected)
}

/// Forces a stuck row terminal when there is no live process to signal —
/// backs `divisi task cancel --force`. Used on a zombie the in-memory
/// registry has no cancel handle for (a row wedged by a bug, or one a
/// pre-reconciliation daemon left behind). No-op on an already-terminal
/// row, so `--force` is safe to pass blindly.
pub fn force_fail(conn: &Connection, id: i64, reason: &str) -> Result<TaskRecord> {
    let task = get(conn, id)?.context("no such task")?;
    if matches!(task.status, TaskStatus::Running | TaskStatus::Created) {
        finish(conn, id, TaskStatus::Failed, task.worktree_path.as_deref(), task.artifact_path.as_deref(), None, false, Some(reason), false)?;
        crate::state::record_event(conn, "task.force_cancelled", &format!("#{id} {reason}"))?;
    }
    get(conn, id)?.context("task disappeared after force-fail")
}

pub struct RunTaskOptions<'a> {
    pub description: &'a str,
    pub agent: &'a str,
    pub cwd: &'a Path,
    pub use_worktree: bool,
    /// If set, runs against this captured account's isolated `$HOME`
    /// (`divisi_core::account::ensure_isolated_home`) instead of the real
    /// one — lets multiple accounts of the same agent run concurrently.
    /// Ignored when `real_home` is set.
    pub account: Option<&'a str>,
    /// Skips isolated-home materialization entirely and runs the agent
    /// against the real, ambient `$HOME` — for tasks that need to
    /// actually modify the real system (dotfiles, installed packages,
    /// desktop config), not a divisi-managed sandbox copy. An
    /// explicit opt-in: the agent gets full access to real credentials
    /// and files, which is exactly what the isolated-home default exists
    /// to avoid in the ordinary case.
    pub real_home: bool,
    /// Skips the relevant-memory + unread-notes prompt preamble (on by
    /// default — see `build_context_preamble`).
    pub no_memory_context: bool,
    pub timeout: Duration,
    /// Opt-in (default off): fail over to the next entry in a configured
    /// fallback chain (`divisi_core::fallback`) when this run's failure
    /// looks like a rate limit — see `execute`'s `maybe_fail_over`.
    pub allow_fallback: bool,
    /// Opt-in (default off): run the agent in a structured-output mode
    /// that reports real token usage where one exists (currently only
    /// `claude --output-format json`); otherwise a no-op hint and the
    /// run's usage is parse-or-estimated. See `AgentAdapter::run_prompt_json`.
    pub usage_json: bool,
    /// Opt-in (default off): for the `single-pool` agent only, drops any
    /// free-pool provider flagged `chat_prose_only` (aihorde-class —
    /// see `divisi_core::free_pool::structured_output_ok`) from
    /// candidacy. Set by `code`/`plan`/`integrate` role dispatch, which
    /// needs either real tool-calling or strict single-shot JSON
    /// compliance those providers' wire contracts don't guarantee.
    pub require_structured_output: bool,
}

/// Cap on the injected memory/notes/knowledge preamble so it can't dwarf
/// the actual prompt — same discipline as `orchestrate::MAX_HANDOFF_CHARS`.
const MAX_CONTEXT_CHARS: usize = 4000;
/// How many past memory entries get pulled into the preamble, at most.
const MAX_CONTEXT_MEMORIES: usize = 5;
/// How many knowledge-graph entities get pulled into the preamble, at most.
const MAX_CONTEXT_KG_ENTITIES: usize = 5;

/// Builds the prompt actually sent to the agent: relevant memory, relevant
/// knowledge-graph entities, and any unread notes addressed to it in this
/// project, prepended ahead of `description`. Best-effort and additive — a
/// lookup failure here must never block the task itself, so errors are
/// swallowed and just result in a smaller (or absent) preamble. Delivered
/// notes are marked read as part of this call so the same note isn't
/// re-delivered on the next run. Works for every agent, including ones
/// with no MCP support, since it needs no cooperation from the agent CLI
/// itself — the divisi-gateway gateway's live `notes_leave`/`notes_read` tools
/// (v0.1.17) are the complementary on-demand path for agents that want to
/// pull/push notes mid-session instead of only at the start of a run.
fn build_context_preamble(
    conn: &Connection,
    description: &str,
    agent: &str,
    project: Option<&str>,
) -> String {
    let mut sections = String::new();

    let mut memories = Vec::new();
    for keyword in significant_keywords(description) {
        if let Ok(hits) = memory::search(conn, &keyword, None, project) {
            for hit in hits {
                // `Task`-scoped rows are per-task failure diagnostics
                // written by `remember_failure` — useful to look up
                // deliberately (`divisi memory list --scope task`), but
                // noise if replayed into every later agent's prompt (a
                // busy project accumulates hundreds). Everything else a
                // keyword search turns up is fair game.
                if hit.scope == MemoryScope::Task {
                    continue;
                }
                if !memories
                    .iter()
                    .any(|m: &divisi_protocol::MemoryEntry| m.id == hit.id)
                {
                    memories.push(hit);
                }
            }
        }
    }
    memories.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    memories.truncate(MAX_CONTEXT_MEMORIES);
    if !memories.is_empty() {
        sections.push_str("--- Relevant memory (from past sessions) ---\n");
        for m in &memories {
            sections.push_str(&format!(
                "- [{}] {}: {}\n",
                m.created_at,
                m.title,
                crate::orchestrate::truncate(&m.content, 400)
            ));
        }
        sections.push_str("--- end memory ---\n\n");
    }

    // Shared blackboard state: the knowledge graph is written manually
    // (`divisi memory graph create-entity`/`add-observation`) by humans or
    // agents that choose to, not auto-populated from task output — see
    // knowledge_graph.rs's module doc. This is the read half: whatever's
    // there that's relevant gets surfaced to every agent automatically,
    // the same way memory/notes already are.
    let mut kg_entities = Vec::new();
    for keyword in significant_keywords(description) {
        if let Ok(hits) = crate::knowledge_graph::query(conn, &keyword) {
            for hit in hits {
                if !kg_entities
                    .iter()
                    .any(|e: &divisi_protocol::KgEntity| e.name == hit.name)
                {
                    kg_entities.push(hit);
                }
            }
        }
    }
    kg_entities.truncate(MAX_CONTEXT_KG_ENTITIES);
    if !kg_entities.is_empty() {
        sections.push_str("--- Shared knowledge (from the team's knowledge graph) ---\n");
        for e in &kg_entities {
            let observations = e.observations.join("; ");
            sections.push_str(&format!(
                "- {} ({}): {}\n",
                e.name,
                e.entity_type,
                crate::orchestrate::truncate(&observations, 300)
            ));
        }
        sections.push_str("--- end shared knowledge ---\n\n");
    }

    if let Ok(notes) = divisi_core::notes::inbox(conn, project, agent, true) {
        if !notes.is_empty() {
            sections.push_str("--- Notes left by other agents ---\n");
            for n in &notes {
                sections.push_str(&format!(
                    "- from {} [{}]: {}\n",
                    n.from_agent,
                    n.topic,
                    crate::orchestrate::truncate(&n.content, 400)
                ));
                let _ = divisi_core::notes::mark_read(conn, n.id);
            }
            sections.push_str("--- end notes ---\n\n");
        }
    }

    if sections.is_empty() {
        return description.to_string();
    }
    format!(
        "{}Task: {description}",
        crate::orchestrate::truncate(&sections, MAX_CONTEXT_CHARS)
    )
}

/// Pulls a few distinctive words out of a task description to drive the
/// (substring, not semantic — see `memory` module docs) memory search: too
/// short/common a word matches everything and defeats the point of
/// "relevant". Capped at 3 so `build_context_preamble` doesn't fan out
/// into an unbounded number of searches.
fn significant_keywords(description: &str) -> Vec<String> {
    description
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(|w| w.to_lowercase())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .take(3)
        .collect()
}

/// Runs a task to completion synchronously: creates the record, optionally
/// isolates it in a new git worktree off `cwd`'s repo, invokes the agent's
/// real non-interactive mode, captures the output as an artifact file, and
/// records the final status. Every stage emits an event via
/// `crate::state::record_event` so the run is auditable (spec section 32)
/// even though there's no live event *stream* yet (spec section 25) — only
/// a persisted log.
pub fn run(conn: &Connection, ctx: &Context, opts: RunTaskOptions) -> Result<TaskRecord> {
    let Some(adapter) = for_agent_with_custom(opts.agent, &ctx.dirs.agents_dir(), &ctx.registry)
    else {
        anyhow::bail!("unknown agent: {}", opts.agent);
    };
    // single-pool never shells a binary (see task::execute's special
    // case + PoolAdapter's doc comment) so "is it on $PATH" is meaningless
    // for it -- always considered available.
    if opts.agent != "single-pool" && !adapter.discover().detected {
        anyhow::bail!(
            "agent '{}' is not installed; run `divisi setup --yes` first",
            opts.agent
        );
    }
    // Fail fast, before shelling out at all, for an agent the registry
    // already knows can't run headless (e.g. codebuff: no verified
    // non-interactive mode — see registry.rs). Previously this reached
    // `execute` anyway and burned a real subprocess spawn every time,
    // producing an opaque failure instead of the actual reason.
    if let Some(def) = ctx.registry.iter().find(|a| a.name == opts.agent) {
        if !def.capabilities.non_interactive_run {
            anyhow::bail!(
                "agent '{}' has no non-interactive run mode; it cannot be dispatched headlessly",
                opts.agent
            );
        }
    }

    let id = create_for_cwd(conn, opts.description, opts.agent, opts.cwd)?;
    crate::state::record_event(
        conn,
        "task.created",
        &format!("#{id} agent={} \"{}\"", opts.agent, opts.description),
    )?;
    execute(conn, ctx, id, &opts, None)
}

/// Owned counterpart to `RunTaskOptions`, needed because a `background`
/// run's actual work happens on a detached `std::thread` outlasting the
/// request handler's stack frame — the borrowed strings/paths in
/// `RunTaskOptions<'a>` can't cross that boundary, so `run_background`
/// takes this instead and borrows from it fresh inside the spawned thread.
pub struct OwnedRunTaskOptions {
    pub description: String,
    pub agent: String,
    pub cwd: std::path::PathBuf,
    pub use_worktree: bool,
    pub account: Option<String>,
    pub real_home: bool,
    pub no_memory_context: bool,
    pub timeout: Duration,
    pub allow_fallback: bool,
    pub usage_json: bool,
    pub require_structured_output: bool,
}

impl OwnedRunTaskOptions {
    fn as_borrowed(&self) -> RunTaskOptions<'_> {
        RunTaskOptions {
            description: &self.description,
            agent: &self.agent,
            cwd: &self.cwd,
            use_worktree: self.use_worktree,
            account: self.account.as_deref(),
            real_home: self.real_home,
            no_memory_context: self.no_memory_context,
            timeout: self.timeout,
            allow_fallback: self.allow_fallback,
            usage_json: self.usage_json,
            require_structured_output: self.require_structured_output,
        }
    }
}

/// Non-blocking counterpart to `run`: creates the task record (fast — one
/// INSERT) and returns it immediately with status `Running`, having handed
/// the actual agent invocation to a detached thread with its own SQLite
/// connection (SQLite connections aren't shareable across threads — same
/// discipline `orchestrate::run_parallel` already uses). Registers a
/// cancel flag in `registry` before spawning so a later `TaskCancel{id}`
/// can find and flip it; `divisi-agent-sdk::run::run_command_live`'s poll
/// loop notices the flip the same way it already notices a timeout.
pub fn run_background(
    ctx: &Context,
    opts: OwnedRunTaskOptions,
    registry: crate::registry::TaskRegistry,
) -> Result<TaskRecord> {
    let conn = crate::state::open(&ctx.dirs.db_path())?;
    ensure_schema(&conn)?;

    let Some(adapter) = for_agent_with_custom(&opts.agent, &ctx.dirs.agents_dir(), &ctx.registry)
    else {
        anyhow::bail!("unknown agent: {}", opts.agent);
    };
    // single-pool never shells a binary (see task::execute's special
    // case + PoolAdapter's doc comment) so "is it on $PATH" is meaningless
    // for it -- always considered available.
    if opts.agent != "single-pool" && !adapter.discover().detected {
        anyhow::bail!(
            "agent '{}' is not installed; run `divisi setup --yes` first",
            opts.agent
        );
    }
    // See the identical check in `run` above.
    if let Some(def) = ctx.registry.iter().find(|a| a.name == opts.agent) {
        if !def.capabilities.non_interactive_run {
            anyhow::bail!(
                "agent '{}' has no non-interactive run mode; it cannot be dispatched headlessly",
                opts.agent
            );
        }
    }

    let id = create_for_cwd(&conn, &opts.description, &opts.agent, &opts.cwd)?;
    crate::state::record_event(
        &conn,
        "task.created",
        &format!(
            "#{id} agent={} \"{}\" (background)",
            opts.agent, opts.description
        ),
    )?;
    let cancel_flag = registry.register(id);
    set_status(&conn, id, TaskStatus::Running)?;

    let ctx = ctx.clone();
    std::thread::spawn(move || {
        if let Ok(thread_conn) = crate::state::open(&ctx.dirs.db_path()) {
            let _ = execute(
                &thread_conn,
                &ctx,
                id,
                &opts.as_borrowed(),
                Some(cancel_flag.as_ref()),
            );
        }
        registry.unregister(id);
    });

    get(&conn, id)?.context("task disappeared after being created")
}

/// Removes a finished task's git worktree and any leftover live-output
/// file — never done automatically, since a worktree might still be worth
/// inspecting right after a run. Errors on a task that's still `Running`
/// rather than silently killing a worktree out from under a live agent —
/// unless `force` is set (`divisi task cleanup --force`), the escape hatch
/// for a row wedged non-terminal with no live process behind it, which
/// also gets marked `Failed` so it doesn't linger as a phantom "running".
pub fn cleanup(conn: &Connection, ctx: &Context, id: i64, force: bool) -> Result<()> {
    let task = get(conn, id)?.context("no such task")?;
    if task.status == TaskStatus::Running || task.status == TaskStatus::Created {
        if !force {
            anyhow::bail!(
                "task #{id} is still {}; cancel it first, or pass --force to clean it up anyway",
                status_as_str(task.status)
            );
        }
        force_fail(conn, id, "force-cleaned up while non-terminal")?;
    }
    if let Some(worktree_path) = &task.worktree_path {
        let path = Path::new(worktree_path);
        if let Some(repo_ctx) = divisi_core::project_context::resolve(path).repo_root {
            let _ = divisi_core::worktree::remove(Path::new(&repo_ctx), path, true);
        }
    }
    let _ = std::fs::remove_file(ctx.dirs.task_live_output_path(id));
    Ok(())
}

/// The actual work of running one task against an already-created `id`:
/// optionally isolates it in a new git worktree, invokes the agent's real
/// non-interactive mode (threading `cancel` through so a background run
/// can be stopped early), captures the output as an artifact file, and
/// records the final status. Shared by both the synchronous `run` (always
/// `cancel: None`, since nothing outlives the blocking call that could
/// flip it) and `run_background`.
fn execute(
    conn: &Connection,
    ctx: &Context,
    id: i64,
    opts: &RunTaskOptions,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<TaskRecord> {
    let Some(adapter) = for_agent_with_custom(opts.agent, &ctx.dirs.agents_dir(), &ctx.registry)
    else {
        anyhow::bail!("unknown agent: {}", opts.agent);
    };

    let run_cwd: std::path::PathBuf = if opts.use_worktree {
        let repo_ctx = divisi_core::project_context::resolve(opts.cwd);
        if let Some(repo_root) = repo_ctx.repo_root {
            let worktree_path = ctx
                .dirs
                .state_dir()
                .join("worktrees")
                .join(format!("task-{id}"));
            let branch = format!("single/task-{id}");
            // retry-safety: a prior failed attempt at this same task id may
            // have left a partial worktree dir / branch behind (`add`
            // derives both purely from `id`) -- clear it before trying
            // again, or the retry collides with itself.
            divisi_core::worktree::reset_stale(Path::new(&repo_root), &worktree_path, &branch);
            if let Err(e) = divisi_core::worktree::add(Path::new(&repo_root), &worktree_path, &branch) {
                finish(
                    conn,
                    id,
                    TaskStatus::Failed,
                    None,
                    None,
                    None,
                    false,
                    Some(&format!("worktree setup failed: {e:#}")),
                    false,
                )?;
                crate::state::record_event(
                    conn,
                    "task.failed",
                    &format!("#{id} worktree setup failed: {e:#}"),
                )?;
                remember_failure(
                    conn,
                    id,
                    opts.agent,
                    Some(repo_root.clone()),
                    opts.description,
                    &format!("worktree setup failed: {e:#}"),
                );
                notify_task_hooks(conn, ctx, id);
                return get(conn, id)?.context("task disappeared after being created");
            }
            worktree_path
        } else {
            // `cwd` isn't inside a git repo, so worktree isolation is
            // impossible there — that's not a reason to fail the task
            // outright (it was structurally doomed to fail every retry
            // too, since the cwd never becomes a repo on its own). Fall
            // back to running directly in `cwd` instead.
            crate::state::record_event(
                conn,
                "task.worktree_fallback",
                &format!("#{id} cwd is not a git repository; running without worktree isolation"),
            )?;
            opts.cwd.to_path_buf()
        }
    } else {
        opts.cwd.to_path_buf()
    };

    // Resolved once and reused by every `remember_failure` call below so a
    // task's failures land in the same project scope its later memory
    // searches will be filtered by (`divisi memory search --project ...`,
    // and the memory/notes preamble task runs inject going forward).
    let project = divisi_core::project_context::resolve(&run_cwd).repo_root;

    set_status(conn, id, TaskStatus::Running)?;
    crate::state::record_event(
        conn,
        "task.started",
        &format!("#{id} cwd={}", run_cwd.display()),
    )?;

    // Agents whose auth is `RealRequired` (registry) can only ever
    // authenticate against the real ambient environment — e.g. codex and
    // cursor keep their OAuth token in the session-global OS keyring, which
    // an isolated `$HOME` neither contains nor can shadow. Running them in
    // an isolated home just produces a guaranteed 401. Treat them like
    // `--real-home` unless a specific `--account` was asked for (which the
    // caller must have a reason to expect works).
    let forced_real_home = !opts.real_home
        && opts.account.is_none()
        && ctx
            .registry
            .iter()
            .find(|a| a.name == opts.agent)
            .map(|a| a.home_requirement == divisi_protocol::HomeRequirement::RealRequired)
            .unwrap_or(false);
    if forced_real_home {
        crate::state::record_event(
            conn,
            "task.real_home",
            &format!("#{id} {} authenticates only against the real environment", opts.agent),
        )?;
    }

    // Every run goes against a divisi-managed home, never the real
    // ambient $HOME (divisi_core::agent_home docs) — either the default
    // per-agent isolated home, or, when --account is given, that named
    // account's own isolated home (divisi_core::account docs) — unless
    // `real_home` explicitly opts out (see RunTaskOptions::real_home
    // docs), or the agent is `RealRequired` (above), in which case `home`
    // stays None and the agent inherits the daemon's own real environment.
    let home: Option<std::path::PathBuf> = if opts.real_home || forced_real_home {
        None
    } else {
        let resolved = match opts.account {
            Some(name) => integrations::home_dir()
                .and_then(|home| {
                    divisi_core::account::ensure_isolated_home(
                        &ctx.dirs.accounts_dir(),
                        &home,
                        opts.agent,
                        name,
                    )
                })
                .map_err(|e| {
                    format!("failed to materialize isolated home for account '{name}': {e:#}")
                }),
            None => integrations::home_dir()
                .and_then(|home| {
                    divisi_core::agent_home::ensure_bootstrapped(
                        &ctx.dirs.homes_dir(),
                        &home,
                        opts.agent,
                    )
                })
                .map_err(|e| {
                    format!(
                        "failed to materialize isolated home for agent '{}': {e:#}",
                        opts.agent
                    )
                }),
        };
        match resolved {
            Ok(dir) => Some(dir),
            Err(detail) => {
                finish(
                    conn,
                    id,
                    TaskStatus::Failed,
                    None,
                    None,
                    None,
                    false,
                    Some(&detail),
                    false,
                )?;
                crate::state::record_event(conn, "task.failed", &format!("#{id} {detail}"))?;
                remember_failure(
                    conn,
                    id,
                    opts.agent,
                    project.clone(),
                    opts.description,
                    &detail,
                );
                notify_task_hooks(conn, ctx, id);
                return get(conn, id)?.context("task disappeared after being created");
            }
        }
    };

    let prompt = if opts.no_memory_context {
        opts.description.to_string()
    } else {
        build_context_preamble(conn, opts.description, opts.agent, project.as_deref())
    };

    // Opt-in Docker execution (divisi_core::docker) — only reachable when
    // the isolated-home path above actually ran (real_home skips both).
    let docker_container = if let Some(home) = &home {
        match divisi_core::docker::is_enabled(
            &ctx.dirs.docker_registry_file(),
            opts.agent,
            opts.account,
        ) {
            Ok(true) => {
                let container = divisi_core::docker::container_name(opts.agent, opts.account);
                match crate::docker::ensure_started(
                    &container,
                    crate::docker::DEFAULT_IMAGE,
                    home,
                    &run_cwd,
                ) {
                    Ok(()) => Some(container),
                    Err(e) => {
                        let detail = format!(
                            "failed to start docker container for '{}': {e:#}",
                            opts.agent
                        );
                        finish(
                            conn,
                            id,
                            TaskStatus::Failed,
                            None,
                            None,
                            None,
                            false,
                            Some(&detail),
                            false,
                        )?;
                        crate::state::record_event(
                            conn,
                            "task.failed",
                            &format!("#{id} {detail}"),
                        )?;
                        remember_failure(
                            conn,
                            id,
                            opts.agent,
                            project.clone(),
                            opts.description,
                            &detail,
                        );
                        notify_task_hooks(conn, ctx, id);
                        return get(conn, id)?.context("task disappeared after being created");
                    }
                }
            }
            Ok(false) => None,
            Err(_) => None, // no docker.toml / unreadable — treat as not configured, don't fail the task
        }
    } else {
        None
    };
    // Provider API keys (divisi_core::provider_keys) an agent that
    // authenticates via a plain env var — not OAuth — needs to actually
    // see. Resolved fresh per run rather than cached: cheap (local
    // keychain lookups only), and picks up a key the user just added
    // without needing a daemon restart.
    let extra_env = divisi_core::provider_keys::resolve_env_for_agent(&ctx.dirs, opts.agent);
    let backend = match &docker_container {
        Some(container) => divisi_agent_sdk::backend::ExecBackend::Docker {
            container,
            workdir: &run_cwd,
            extra_env: Some(&extra_env),
        },
        None => divisi_agent_sdk::backend::ExecBackend::host_with_env(home.as_deref(), &extra_env),
    };

    std::fs::create_dir_all(ctx.dirs.artifacts_dir())?;
    let live_output_path = ctx.dirs.task_live_output_path(id);
    let max_concurrency = ctx.find_agent(opts.agent).and_then(|a| a.max_concurrency);
    // Scoped tightly around the subprocess run only: `maybe_fail_over`
    // below can recursively call `execute()` again on this same thread
    // for the *same* agent (e.g. an opencode/acct-a -> opencode/acct-b
    // fallback chain) — holding this guard any longer than the run
    // itself would deadlock that recursive call forever waiting on a
    // slot only this (blocked) thread could ever release.
    let outcome = if opts.agent == "single-pool" {
        // Never shells a binary: `pool_agent::run_as_task` dispatches
        // straight to a provider's HTTP API via the ledger/bandit/cooldown
        // engine, which needs `&Connection` — a parameter `AgentAdapter::
        // run_prompt` doesn't have. This bypasses `adapter.run_prompt`
        // entirely rather than shoehorning a DB handle through the trait.
        // Task 14 note: `Exhausted` maps to the same "rate limited" text
        // signal `ratelimit::looks_like_rate_limit` already recognizes, so
        // every existing rate-limit-aware code path below (fallback,
        // `remember_failure`, etc.) treats it correctly with no new
        // plumbing; goal-level `waiting_on_capacity` semantics land in a
        // later phase.
        crate::pool_agent::run_as_task(
            conn,
            &prompt,
            opts.timeout,
            opts.account,
            crate::pool_agent::global_handoff_store(),
            opts.require_structured_output,
        )
    } else {
        let _slot_guard = acquire_agent_slot(opts.agent, max_concurrency);
        let lop = Some(live_output_path.as_path());
        if opts.usage_json {
            adapter.run_prompt_json(&run_cwd, &prompt, &backend, lop, opts.timeout, cancel)
        } else {
            adapter.run_prompt(&run_cwd, &prompt, &backend, lop, opts.timeout, cancel)
        }
    };
    // Left in place (not deleted here) so a task's live output stays
    // inspectable right after it finishes, not just while it's running —
    // an explicit `TaskCleanup{id}` removes it once the caller is done
    // looking, see `cleanup` above.

    let worktree_path_str = opts.use_worktree.then(|| run_cwd.display().to_string());

    // Set inside the failure arms below (never on success/cancellation),
    // then checked once after the match — see `maybe_fail_over`.
    let mut failure_text: Option<String> = None;
    let rate_limited_for_fallback;

    match outcome {
        Ok(outcome) => {
            let artifact_path = ctx.dirs.task_artifact_path(id);
            std::fs::write(
                &artifact_path,
                format!("{}\n--- stderr ---\n{}", outcome.stdout, outcome.stderr),
            )?;

            // Live-verification finding (E30 dispatch, 2026-09-17): `kiro`
            // exits 0 with an empty stdout and a "Monthly request limit
            // reached" stderr banner when it's out of quota -- a real
            // failure the exit code alone can't reveal. Narrow: only
            // overrides a reported success when stdout is empty AND
            // stderr matches a known unavailability signal, so it can
            // never catch a genuinely productive run that happens to
            // mention "429" in real stdout output (see
            // `rate_limited_stays_false_for_a_successful_task_whose_
            // output_merely_contains_429`, which puts its "429" in
            // stdout, not stderr, and is unaffected by this).
            let hollow_success = outcome.success
                && !outcome.cancelled
                && outcome.stdout.trim().is_empty()
                && divisi_core::ratelimit::looks_like_unavailable(&outcome.stderr);
            let treat_as_failed = (!outcome.success && !outcome.cancelled) || hollow_success;

            let status = if outcome.cancelled {
                TaskStatus::Cancelled
            } else if treat_as_failed {
                TaskStatus::Failed
            } else {
                TaskStatus::Completed
            };
            if status == TaskStatus::Completed {
                crate::agent_cooldown::succeeded(conn, opts.agent);
            }
            let summary = if hollow_success {
                format!("agent exited 0 but produced no output and its stderr looks like a quota/rate limit: {}", outcome.stderr.trim())
            } else {
                summarize(&outcome.stdout, &outcome.stderr, outcome.timed_out, outcome.exit_code)
            };
            // A real failure (including a hollow success reclassified
            // above) is checked for rate-limit signals — an *actually*
            // successful task's stdout can innocently contain a
            // rate-limit-shaped substring (a line number, a diff hunk
            // header, a byte count) and must not be flagged.
            let rate_limited = if treat_as_failed {
                let combined_output = format!("{}\n{}", outcome.stdout, outcome.stderr);
                let unavailable = divisi_core::ratelimit::looks_like_unavailable(&combined_output);
                // If the agent said when it recovers, keep routing away until then.
                if unavailable {
                    crate::agent_cooldown::note(conn, opts.agent, &combined_output);
                } else {
                    crate::agent_cooldown::note_failure_streak(conn, opts.agent);
                }
                unavailable
            } else {
                false
            };
            rate_limited_for_fallback = rate_limited;
            finish(
                conn,
                id,
                status,
                worktree_path_str.as_deref(),
                Some(&artifact_path.display().to_string()),
                outcome.exit_code,
                outcome.timed_out,
                Some(&summary),
                rate_limited,
            )?;
            let event = if outcome.cancelled {
                "task.cancelled"
            } else if treat_as_failed {
                "task.failed"
            } else {
                "task.completed"
            };
            crate::state::record_event(conn, event, &format!("#{id} {summary}"))?;
            // Per-turn token accounting (E27.03): real counts if the agent
            // reported them (usage-json mode), else parse-or-estimate from
            // the captured output. Best-effort — a bookkeeping failure
            // must not fail the task.
            let (pt, ct, estimated) = match outcome.usage {
                Some(u) => (u.prompt_tokens as i64, u.completion_tokens as i64, false),
                None => parse_or_estimate_tokens(opts.agent, &prompt, &outcome.stdout, &outcome.stderr),
            };
            let _ = record_token_usage(conn, id, pt, ct, estimated);
            // A cancellation was requested, not a real failure — no
            // lesson to learn from it, so it skips `remember_failure`.
            if treat_as_failed {
                remember_failure(
                    conn,
                    id,
                    opts.agent,
                    project.clone(),
                    opts.description,
                    &summary,
                );
                failure_text = Some(format!("{}\n{}\n{summary}", outcome.stdout, outcome.stderr));
            }
        }
        Err(e) => {
            let error_text = format!("{e:#}");
            let rate_limited = divisi_core::ratelimit::looks_like_unavailable(&error_text);
            if rate_limited {
                crate::agent_cooldown::note(conn, opts.agent, &error_text);
            }
            rate_limited_for_fallback = rate_limited;
            finish(
                conn,
                id,
                TaskStatus::Failed,
                worktree_path_str.as_deref(),
                None,
                None,
                false,
                Some(&error_text),
                rate_limited,
            )?;
            crate::state::record_event(conn, "task.failed", &format!("#{id} {error_text}"))?;
            remember_failure(
                conn,
                id,
                opts.agent,
                project.clone(),
                opts.description,
                &error_text,
            );
            failure_text = Some(error_text);
        }
    }

    if opts.allow_fallback {
        if let Some(text) = failure_text {
            maybe_fail_over(conn, ctx, id, opts, &text, rate_limited_for_fallback, cancel);
        }
    }

    notify_task_hooks(conn, ctx, id);
    get(conn, id)?.context("task disappeared after finishing")
}

/// One hop of `task run --allow-fallback`: if `id`'s failure looks like a
/// rate limit — `rate_limited` is decided by the caller (`execute`, via
/// `divisi_core::ratelimit::looks_like_rate_limit` on the output), or the
/// account was already marked `rate_limited` here — and a fallback chain
/// has an entry after `opts.agent`/`opts.account`, marks the account
/// rate-limited (if not already) and runs a linked follow-up task against
/// the chain's next entry. The follow-up also carries
/// `allow_fallback: true`, so a chain of several hops walks itself one
/// link at a time via this same function — bounded automatically, since
/// `divisi_core::fallback::next_after` only ever advances forward through
/// a finite saved chain (see its doc comment for why that can't loop).
/// Errors are logged as an event rather than propagated — a failed
/// failover attempt must never mask the original task's own recorded
/// failure.
fn maybe_fail_over(conn: &Connection, ctx: &Context, id: i64, opts: &RunTaskOptions, _failure_text: &str, rate_limited: bool, cancel: Option<&std::sync::atomic::AtomicBool>) {
    let already_rate_limited = opts.account.is_some_and(|account_name| {
        divisi_core::account::list(&ctx.dirs.accounts_dir(), Some(opts.agent))
            .unwrap_or_default()
            .into_iter()
            .any(|a| a.name == account_name && a.status == divisi_protocol::AccountStatus::RateLimited)
    });
    if !already_rate_limited && !rate_limited {
        return;
    }

    let current = divisi_protocol::AgentAccountRef { agent: opts.agent.to_string(), account: opts.account.map(String::from) };
    let fallback_path = ctx.dirs.fallback_registry_file();
    let next = match divisi_core::fallback::next_after(&fallback_path, &current) {
        Ok(next) => next,
        Err(e) => {
            let _ = crate::state::record_event(conn, "task.fallback_error", &format!("#{id} reading fallback registry: {e:#}"));
            return;
        }
    };
    let Some(next) = next else { return };
    let detected = for_agent_with_custom(&next.agent, &ctx.dirs.agents_dir(), &ctx.registry).is_some_and(|a| a.discover().detected);
    if !detected {
        let _ = crate::state::record_event(conn, "task.fallback_error", &format!("#{id} fallback target '{}' isn't installed", next.agent));
        return;
    }

    if let Some(account) = &opts.account {
        if let Err(e) = divisi_core::account::set_status(&ctx.dirs.accounts_dir(), opts.agent, account, divisi_protocol::AccountStatus::RateLimited) {
            let _ = crate::state::record_event(conn, "task.fallback_error", &format!("#{id} marking {}/{account} rate_limited: {e:#}", opts.agent));
        }
    }

    let description = format!("[fallback from #{id}, {} looked rate-limited] {}", opts.agent, opts.description);
    let next_opts = RunTaskOptions {
        description: &description,
        agent: &next.agent,
        cwd: opts.cwd,
        use_worktree: opts.use_worktree,
        account: next.account.as_deref(),
        real_home: opts.real_home,
        no_memory_context: opts.no_memory_context,
        timeout: opts.timeout,
        allow_fallback: true,
        usage_json: false,
        require_structured_output: false,
    };
    match create_for_cwd(conn, next_opts.description, next_opts.agent, next_opts.cwd) {
        Ok(next_id) => {
            let _ = crate::state::record_event(conn, "task.fallback_started", &format!("#{id} -> #{next_id} agent={}", next.agent));
            let _ = execute(conn, ctx, next_id, &next_opts, cancel);
        }
        Err(e) => {
            let _ = crate::state::record_event(conn, "task.fallback_error", &format!("#{id} starting follow-up task: {e:#}"));
        }
    }
}

/// "Learn from errors": every task failure is also written to the memory
/// store (source=tool_output, since it's this project's own tooling
/// reporting what went wrong rather than something a user or agent
/// asserted) so `divisi memory list --scope task` surfaces past failures
/// for whoever — human or agent — looks next. Written at `Task` scope, not
/// `Project`: `build_context_preamble` skips `Task`-scoped rows, so these
/// diagnostics stay out of every later agent's prompt (a busy project
/// otherwise piled up hundreds of "task #N failed" lines in-context).
/// Best-effort: a memory-write failure must never mask the real task
/// failure it's trying to record, so errors here are swallowed.
fn remember_failure(
    conn: &Connection,
    id: i64,
    agent: &str,
    project: Option<String>,
    description: &str,
    detail: &str,
) {
    let _ = memory::ensure_schema(conn);
    let _ = memory::store(
        conn,
        memory::NewMemory {
            scope: Some(MemoryScope::Task),
            source: Some(MemorySource::ToolOutput),
            project,
            agent: Some(agent.to_string()),
            task: Some(id.to_string()),
            title: format!("task #{id} failed ({agent})"),
            content: format!("prompt: {description}\nfailure: {detail}"),
            confidence: Some(1.0),
            expires_in_seconds: None,
        },
    );
}

fn summarize(stdout: &str, stderr: &str, timed_out: bool, exit_code: Option<i32>) -> String {
    if timed_out {
        return "timed out".to_string();
    }
    // An error-marked line is a better summary than whichever line happens
    // to come first: CLIs routinely print startup/banner/progress noise on
    // both streams before (or instead of) their real failure, so the first
    // non-empty line is often useless ("Reading additional input from
    // stdin..." rather than "ERROR: ..."). Take the LAST such line in each
    // stream — the final error is usually the actual outcome, earlier ones
    // are often incidental (e.g. a background refresh failing).
    let last_error_line = |s: &str| {
        s.lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty() && (l.contains("Error") || l.contains("ERROR")))
            .map(str::to_string)
    };
    // Skip lines that are pure decoration (box-drawing borders, "---",
    // "===") — several TUI-styled agents (goose included) print these
    // around their real output, and a summary of just "────────" is as
    // useless as "no output".
    let is_decorative = |l: &str| !l.chars().any(|c| c.is_alphanumeric());
    let first_nonempty_line = |s: &str| {
        s.lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !is_decorative(l))
            .unwrap_or("")
            .to_string()
    };
    let (first_line, mut from_stderr) = match (last_error_line(stderr), last_error_line(stdout)) {
        (Some(l), _) => (l, true),
        (None, Some(l)) => (l, false),
        (None, None) => {
            let s = first_nonempty_line(stdout);
            if !s.is_empty() {
                (s, false)
            } else {
                (first_nonempty_line(stderr), true)
            }
        }
    };
    if from_stderr && first_line.is_empty() {
        from_stderr = false;
    }
    let truncated: String = first_line.chars().take(200).collect();
    if truncated.is_empty() {
        format!("exit code {:?}, no output on stdout or stderr", exit_code)
    } else if from_stderr {
        format!("[stderr] {truncated}")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_or_estimate_tokens_reads_json_usage() {
        let stdout = r#"...noise... {"usage": {"input_tokens": 1200, "output_tokens": 345}} tail"#;
        let (p, c, est) = parse_or_estimate_tokens("claude", "prompt", stdout, "");
        assert_eq!((p, c, est), (1200, 345, false));
    }

    #[test]
    fn parse_or_estimate_tokens_reads_a_human_token_line() {
        let stdout = "done.\nTokens: 900 prompt, 120 completion\n";
        let (p, c, est) = parse_or_estimate_tokens("codex", "prompt", stdout, "");
        assert_eq!((p, c, est), (900, 120, false));
    }

    #[test]
    fn parse_or_estimate_tokens_falls_back_to_chars_over_four() {
        let prompt = "a".repeat(40); // -> 10
        let stdout = "b".repeat(20); // -> 5
        let (p, c, est) = parse_or_estimate_tokens("opencode", &prompt, &stdout, "");
        assert_eq!((p, c, est), (10, 5, true));
    }

    #[test]
    fn record_token_usage_persists_and_round_trips_through_get() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        let id = create_for_cwd(&conn, "t", "claude", std::path::Path::new("/tmp")).unwrap();
        record_token_usage(&conn, id, 111, 22, true).unwrap();
        let rec = get(&conn, id).unwrap().unwrap();
        assert_eq!(rec.prompt_tokens, Some(111));
        assert_eq!(rec.completion_tokens, Some(22));
        assert!(rec.tokens_estimated);
    }

    #[test]
    fn local_stats_window_tokens_and_drop_implausible_counts() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        let cwd = std::path::Path::new("/tmp");
        let a = create_for_cwd(&conn, "t", "claude", cwd).unwrap();
        let b = create_for_cwd(&conn, "t", "claude", cwd).unwrap();
        let c = create_for_cwd(&conn, "t", "claude", cwd).unwrap();
        record_token_usage(&conn, a, 100, 20, false).unwrap();
        record_token_usage(&conn, b, 50, 10, true).unwrap();
        record_token_usage(&conn, c, 1, 1_234_573_874, true).unwrap();
        // An old run: counted in totals but outside the 7-day window.
        let old = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let d = create_for_cwd(&conn, "t", "claude", cwd).unwrap();
        conn.execute("UPDATE tasks SET created_at = ?1 WHERE id = ?2", params![old, d]).unwrap();
        record_token_usage(&conn, d, 999, 999, false).unwrap();

        let stats = local_stats_by_agent(&conn).unwrap();
        let s = stats.iter().find(|s| s.agent == "claude").unwrap();
        assert_eq!(s.run_count, 4);
        assert_eq!(s.runs_7d, 3);
        assert_eq!(s.runs_24h, 3);
        assert_eq!(s.prompt_tokens_7d, 150);
        assert_eq!(s.completion_tokens_7d, 30);
        assert_eq!(s.estimated_runs_7d, 1);
        assert_eq!(s.discarded_token_rows, 1);
    }

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        ensure_schema(&conn).unwrap();
        crate::state::ensure_events_schema(&conn).unwrap();
        memory::ensure_schema(&conn).unwrap();
        divisi_core::notes::ensure_schema(&conn).unwrap();
        crate::knowledge_graph::ensure_schema(&conn).unwrap();
        crate::pool::ensure_pool_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn context_preamble_includes_relevant_memory_and_marks_notes_read() {
        let conn = test_conn();
        memory::store(
            &conn,
            memory::NewMemory {
                scope: Some(MemoryScope::Project),
                project: Some("proj".into()),
                title: "auth bug".into(),
                content: "root cause was a token refresh race".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let note_id = divisi_core::notes::leave(
            &conn,
            Some("proj".into()),
            "claude",
            Some("codex"),
            "heads up",
            "watch the flaky test",
        )
        .unwrap();

        let prompt =
            build_context_preamble(&conn, "fix the token refresh bug", "codex", Some("proj"));
        assert!(prompt.contains("root cause was a token refresh race"));
        assert!(prompt.contains("watch the flaky test"));
        assert!(prompt.ends_with("Task: fix the token refresh bug"));

        let note = divisi_core::notes::get(&conn, note_id).unwrap().unwrap();
        assert!(
            note.read_at.is_some(),
            "a note delivered in the preamble should be marked read"
        );
    }

    /// `remember_failure` writes at `Task` scope precisely so its rows
    /// don't get replayed into every later agent's prompt — `divisi memory
    /// list --scope task` is the way to look at them. A keyword-matching
    /// `Project` row in the same search still comes through.
    #[test]
    fn context_preamble_excludes_task_scoped_failure_memories() {
        let conn = test_conn();
        remember_failure(
            &conn,
            42,
            "codex",
            Some("proj".into()),
            "fix the token refresh bug",
            "exited with code 1",
        );
        memory::store(
            &conn,
            memory::NewMemory {
                scope: Some(MemoryScope::Project),
                project: Some("proj".into()),
                title: "token refresh lesson".into(),
                content: "the refresh endpoint needs a retry".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let prompt =
            build_context_preamble(&conn, "fix the token refresh bug", "codex", Some("proj"));
        assert!(prompt.contains("the refresh endpoint needs a retry"));
        assert!(
            !prompt.contains("task #42 failed"),
            "task-scoped failure diagnostics must not enter the prompt"
        );

        // ...but they are still queryable on their own scope.
        let diagnostics = memory::list(&conn, Some(MemoryScope::Task)).unwrap();
        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn context_preamble_includes_relevant_knowledge_graph_entities() {
        let conn = test_conn();
        crate::knowledge_graph::create_entity(&conn, "token-refresh-race", "bug").unwrap();
        crate::knowledge_graph::add_observation(
            &conn,
            "token-refresh-race",
            "fixed by adding a mutex around refresh",
        )
        .unwrap();
        crate::knowledge_graph::create_entity(&conn, "unrelated-thing", "note").unwrap();

        let prompt =
            build_context_preamble(&conn, "investigate the token refresh race", "codex", None);
        assert!(prompt.contains("token-refresh-race"));
        assert!(prompt.contains("fixed by adding a mutex around refresh"));
        assert!(!prompt.contains("unrelated-thing"));
    }

    #[test]
    fn context_preamble_is_plain_description_when_nothing_relevant_exists() {
        let conn = test_conn();
        let prompt = build_context_preamble(&conn, "some totally unrelated task", "codex", None);
        assert_eq!(prompt, "some totally unrelated task");
    }

    #[test]
    fn significant_keywords_filters_short_words_and_dedupes() {
        let words = significant_keywords("fix the auth auth bug in login flow");
        assert!(words.contains(&"auth".to_string()));
        assert!(words.contains(&"login".to_string()));
        assert!(!words.iter().any(|w| w == "the" || w == "fix" || w == "bug"));
    }

    #[test]
    fn create_then_get_round_trips_as_created() {
        let conn = test_conn();
        let id = create(&conn, "test task", "claude", "/tmp/test-cwd", "workspace-1").unwrap();
        let task = get(&conn, id).unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Created);
        assert_eq!(task.agent, "claude");
    }

    #[test]
    fn rate_limited_flag_is_set_when_output_matches_a_known_signal() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        let id = create(&conn, "test task", "fake-agent", dir.path().to_str().unwrap(), "ws1").unwrap();
        finish(&conn, id, TaskStatus::Failed, None, None, Some(1), false, Some("HTTP 429 Too Many Requests"), true).unwrap();
        let task = get(&conn, id).unwrap().unwrap();
        assert!(task.rate_limited);
    }

    #[test]
    fn rate_limited_flag_defaults_false_for_an_ordinary_failure() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        let id = create(&conn, "test task", "fake-agent", dir.path().to_str().unwrap(), "ws1").unwrap();
        finish(&conn, id, TaskStatus::Failed, None, None, Some(1), false, Some("panic: index out of bounds"), false).unwrap();
        let task = get(&conn, id).unwrap().unwrap();
        assert!(!task.rate_limited);
    }

    /// Regression test for the "rate_limited computed on successful runs
    /// too" bug: a task whose (successful) stdout merely *contains* a
    /// rate-limit-shaped substring (here, "429") must NOT be flagged.
    /// Exercises the real `execute()` Ok/success path — not `finish()`
    /// directly — via a custom agent (`sh -c <prompt>`) so it doesn't
    /// depend on any real CLI agent being installed. Fails against the
    /// pre-fix code (which computed `rate_limited` unconditionally) and
    /// passes against the fix (which gates on `!outcome.success &&
    /// !outcome.cancelled`).
    #[test]
    fn rate_limited_stays_false_for_a_successful_task_whose_output_merely_contains_429() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DIVISI_CONFIG_DIR", dir.path());
        let conn = test_conn();
        let dirs = divisi_core::DivisiDirs::from_root(dir.path().to_path_buf());
        dirs.ensure_created().unwrap();

        // A custom agent whose "run" is just `sh -c <prompt>" — lets the
        // test control stdout/exit-code precisely without needing claude,
        // codex, or any other real agent CLI installed.
        std::fs::write(
            dirs.agents_dir().join("fake-ok-agent.toml"),
            r#"
name = "fake-ok-agent"
command = "sh"

[run]
mode = "flag"
value = "-c"
"#,
        )
        .unwrap();

        let ctx = Context {
            dirs,
            resolved: divisi_core::ResolvedConfig::default(),
            registry: divisi_core::builtin_registry(),
        };

        let this_repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let opts = RunTaskOptions {
            // The whole prompt becomes the `sh -c` argument: prints an
            // HTTP-429-shaped line and exits 0 (a genuine success).
            description: "echo 'HTTP 429 Too Many Requests'; exit 0",
            agent: "fake-ok-agent",
            cwd: &this_repo,
            use_worktree: false,
            account: None,
            real_home: true, // skip isolated-home materialization, not under test here
            no_memory_context: true, // prompt must equal `description` verbatim
            timeout: Duration::from_secs(5),
            allow_fallback: false,
            usage_json: false,
            require_structured_output: false,
        };
        let task = run(&conn, &ctx, opts).unwrap();
        assert_eq!(task.status, TaskStatus::Completed, "expected the sh command to succeed");
        assert!(
            !task.rate_limited,
            "a successful task must never be flagged rate_limited, even if its output contains '429'"
        );
    }

    /// Live-verification regression (E30 dispatch, 2026-09-17): `kiro`
    /// exits 0 with empty stdout and a "Monthly request limit reached"
    /// stderr banner when out of quota -- the Coordinator marked all 3
    /// dispatched nodes `done` and produced zero real work. An exit-0,
    /// empty-stdout task whose stderr matches a known unavailability
    /// signal must be reclassified as a real, rate-limited failure.
    #[test]
    fn a_zero_exit_with_empty_stdout_and_rate_limit_shaped_stderr_is_reclassified_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DIVISI_CONFIG_DIR", dir.path());
        let conn = test_conn();
        let dirs = divisi_core::DivisiDirs::from_root(dir.path().to_path_buf());
        dirs.ensure_created().unwrap();

        std::fs::write(
            dirs.agents_dir().join("fake-hollow-agent.toml"),
            r#"
name = "fake-hollow-agent"
command = "sh"

[run]
mode = "flag"
value = "-c"
"#,
        )
        .unwrap();

        let ctx = Context {
            dirs,
            resolved: divisi_core::ResolvedConfig::default(),
            registry: divisi_core::builtin_registry(),
        };

        let this_repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let opts = RunTaskOptions {
            // Empty stdout, kiro's real quota banner on stderr, exit 0 --
            // exactly kiro's live-observed behavior.
            description: "echo 'Monthly request limit reached' 1>&2; exit 0",
            agent: "fake-hollow-agent",
            cwd: &this_repo,
            use_worktree: false,
            account: None,
            real_home: true,
            no_memory_context: true,
            timeout: Duration::from_secs(5),
            allow_fallback: false,
            usage_json: false,
            require_structured_output: false,
        };
        let task = run(&conn, &ctx, opts).unwrap();
        assert_eq!(task.status, TaskStatus::Failed, "a hollow exit-0 with no real output must not read as success");
        assert!(task.rate_limited, "the quota-shaped stderr must be recognized as a rate-limit signal");
    }

    #[test]
    fn run_fails_cleanly_for_unknown_agent() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DIVISI_CONFIG_DIR", dir.path());
        let conn = test_conn();
        let dirs = divisi_core::DivisiDirs::from_root(dir.path().to_path_buf());
        dirs.ensure_created().unwrap();
        let ctx = Context {
            dirs,
            resolved: divisi_core::ResolvedConfig::default(),
            registry: divisi_core::builtin_registry(),
        };
        let result = task_run_result(&conn, &ctx, dir.path(), "ghost-agent");
        assert!(result.is_err());
        // No task row should be left dangling from an agent-name check that failed before creation.
        assert!(list(&conn).unwrap().is_empty());
    }

    #[test]
    fn run_falls_back_to_no_worktree_outside_a_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DIVISI_CONFIG_DIR", dir.path());
        let conn = test_conn();
        let dirs = divisi_core::DivisiDirs::from_root(dir.path().to_path_buf());
        dirs.ensure_created().unwrap();
        let ctx = Context {
            dirs,
            resolved: divisi_core::ResolvedConfig::default(),
            registry: divisi_core::builtin_registry(),
        };

        // "claude" is used here only as a registry entry; if it's not
        // actually installed this test would fail at the detection check
        // before ever reaching the worktree logic, so skip cleanly.
        if !divisi_agent_sdk::adapters::for_agent("claude")
            .unwrap()
            .discover()
            .detected
        {
            return;
        }
        let opts = RunTaskOptions {
            description: "x",
            agent: "claude",
            cwd: dir.path(),
            use_worktree: true,
            account: None,
            real_home: false,
            no_memory_context: false,
            timeout: Duration::from_secs(1),
            allow_fallback: false,
            usage_json: false,
            require_structured_output: false,
        };
        // A non-git cwd with --worktree requested should no longer be a
        // guaranteed, un-retriable failure: it should fall back to running
        // directly in `cwd`. The 1s timeout means this specific run will
        // likely still fail (real agent invocation can't finish that
        // fast), but it must NOT fail with the old "not a git repository"
        // reason, and the fallback event must be recorded.
        let task = run(&conn, &ctx, opts).unwrap();
        assert!(
            task.summary
                .as_deref()
                .map(|s| !s.contains("not a git repository"))
                .unwrap_or(true)
        );

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind = 'task.worktree_fallback'",
                (),
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    fn task_run_result(
        conn: &Connection,
        ctx: &Context,
        cwd: &Path,
        agent: &str,
    ) -> Result<TaskRecord> {
        run(
            conn,
            ctx,
            RunTaskOptions {
                description: "x",
                agent,
                cwd,
                use_worktree: false,
                account: None,
                real_home: false,
                no_memory_context: false,
                timeout: Duration::from_secs(1),
                allow_fallback: false,
                usage_json: false,
                require_structured_output: false,
            },
        )
    }

    #[test]
    fn remember_failure_records_the_resolved_project_when_one_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DIVISI_CONFIG_DIR", dir.path());
        let conn = test_conn();
        let dirs = divisi_core::DivisiDirs::from_root(dir.path().to_path_buf());
        dirs.ensure_created().unwrap();
        let ctx = Context {
            dirs,
            resolved: divisi_core::ResolvedConfig::default(),
            registry: divisi_core::builtin_registry(),
        };

        if !divisi_agent_sdk::adapters::for_agent("claude")
            .unwrap()
            .discover()
            .detected
        {
            return;
        }
        // Run with cwd inside this crate's own (real) git repo, and a
        // nonexistent account so home materialization fails before any
        // subprocess spawns — exercises remember_failure's project
        // threading without needing a real agent invocation.
        let this_repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let opts = RunTaskOptions {
            description: "x",
            agent: "claude",
            cwd: &this_repo,
            use_worktree: false,
            account: Some("definitely-not-a-real-account"),
            real_home: false,
            no_memory_context: false,
            timeout: Duration::from_secs(1),
            allow_fallback: false,
            usage_json: false,
            require_structured_output: false,
        };
        let task = run(&conn, &ctx, opts).unwrap();
        assert_eq!(task.status, TaskStatus::Failed);

        let memories = memory::search(&conn, "definitely-not-a-real-account", None, None).unwrap();
        assert_eq!(memories.len(), 1);
        assert!(
            memories[0].project.is_some(),
            "expected the resolved repo root to be recorded as the memory's project"
        );
    }

    #[test]
    fn fail_over_starts_a_linked_follow_up_task_when_the_chain_has_one() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DIVISI_CONFIG_DIR", dir.path());
        let conn = test_conn();
        let dirs = divisi_core::DivisiDirs::from_root(dir.path().to_path_buf());
        dirs.ensure_created().unwrap();
        let ctx = Context {
            dirs,
            resolved: divisi_core::ResolvedConfig::default(),
            registry: divisi_core::builtin_registry(),
        };

        if !divisi_agent_sdk::adapters::for_agent("claude").unwrap().discover().detected {
            return;
        }
        // Two entries for the same agent (no named account, just the
        // default isolated home) — exercises the chain-walk and follow-up
        // creation without needing a real captured account profile, which
        // `set_status` (called only when `opts.account` is `Some`) would
        // otherwise require to already exist.
        divisi_core::fallback::set(
            &ctx.dirs.fallback_registry_file(),
            vec![
                divisi_protocol::AgentAccountRef { agent: "claude".into(), account: None },
                divisi_protocol::AgentAccountRef { agent: "codex".into(), account: None },
            ],
        )
        .unwrap();

        let this_repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let id = create_for_cwd(&conn, "do the thing", "claude", &this_repo).unwrap();
        let opts = RunTaskOptions {
            description: "do the thing",
            agent: "claude",
            cwd: &this_repo,
            use_worktree: false,
            account: None,
            real_home: false,
            no_memory_context: true,
            timeout: Duration::from_secs(1),
            allow_fallback: true,
            usage_json: false,
            require_structured_output: false,
        };
        maybe_fail_over(&conn, &ctx, id, &opts, "Error: rate limit exceeded, try again later", true, None);

        let tasks = list(&conn).unwrap();
        if !divisi_agent_sdk::adapters::for_agent("codex").unwrap().discover().detected {
            // The chain's next hop isn't installed either — maybe_fail_over
            // correctly declines rather than starting a doomed task.
            assert_eq!(tasks.len(), 1);
            return;
        }
        assert_eq!(tasks.len(), 2, "the original task plus one linked follow-up");
        let follow_up = tasks.iter().find(|t| t.id != id).unwrap();
        assert_eq!(follow_up.agent, "codex");
        assert!(follow_up.description.contains(&format!("fallback from #{id}")));
    }

    #[test]
    fn fail_over_does_nothing_when_the_failure_does_not_look_like_a_rate_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DIVISI_CONFIG_DIR", dir.path());
        let conn = test_conn();
        let dirs = divisi_core::DivisiDirs::from_root(dir.path().to_path_buf());
        dirs.ensure_created().unwrap();
        let ctx = Context {
            dirs,
            resolved: divisi_core::ResolvedConfig::default(),
            registry: divisi_core::builtin_registry(),
        };
        divisi_core::fallback::set(
            &ctx.dirs.fallback_registry_file(),
            vec![
                divisi_protocol::AgentAccountRef { agent: "claude".into(), account: None },
                divisi_protocol::AgentAccountRef { agent: "codex".into(), account: None },
            ],
        )
        .unwrap();

        let this_repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let id = create_for_cwd(&conn, "do the thing", "claude", &this_repo).unwrap();
        let opts = RunTaskOptions {
            description: "do the thing",
            agent: "claude",
            cwd: &this_repo,
            use_worktree: false,
            account: None,
            real_home: false,
            no_memory_context: true,
            timeout: Duration::from_secs(1),
            allow_fallback: true,
            usage_json: false,
            require_structured_output: false,
        };
        maybe_fail_over(&conn, &ctx, id, &opts, "error: file not found", false, None);

        assert_eq!(list(&conn).unwrap().len(), 1, "an ordinary failure must not trigger a follow-up task");
    }

    #[test]
    fn acquire_agent_slot_serializes_when_limit_is_one() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc as StdArc;

        let concurrent_count = StdArc::new(AtomicU32::new(0));
        let max_seen = StdArc::new(AtomicU32::new(0));

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let concurrent_count = concurrent_count.clone();
                let max_seen = max_seen.clone();
                std::thread::spawn(move || {
                    let _guard = acquire_agent_slot("test-agent-serialize", Some(1));
                    let now = concurrent_count.fetch_add(1, Ordering::SeqCst) + 1;
                    max_seen.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    concurrent_count.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(max_seen.load(Ordering::SeqCst), 1, "at most 1 concurrent run should ever have been observed");
    }

    #[test]
    fn acquire_agent_slot_is_a_noop_when_unlimited() {
        let _guard = acquire_agent_slot("test-agent-unlimited", None);
        // No assertion needed beyond "doesn't block/panic" — unlimited
        // means immediate return every time, proven by this test completing.
    }

    /// Regression test for the fallback-recursion self-deadlock: `execute()`
    /// used to hold `_slot_guard` across the whole function body, including
    /// `maybe_fail_over`'s recursive same-thread `execute()` call for the
    /// next agent in a fallback chain. When that chain repeats the same
    /// agent (e.g. opencode/acct-a -> opencode/acct-b, the natural way to
    /// use account-fallback for the one agent with `max_concurrency` set),
    /// the recursive call's `acquire_agent_slot` would block forever in
    /// `cvar.wait()` waiting on a release only the same, now-permanently-
    /// blocked thread could ever perform.
    ///
    /// This mirrors the fixed shape directly: acquire+drop the slot inside
    /// a block (standing in for the scoped `{ let _slot_guard = ...; ... }`
    /// around `adapter.run_prompt` in `execute()`), then acquire the SAME
    /// agent's slot again on the SAME thread (standing in for
    /// `maybe_fail_over`'s recursive `execute()` call reusing the agent).
    /// Run on a background thread with a bounded `recv_timeout` so that if
    /// this regresses, the test fails loudly instead of hanging the suite
    /// forever the way the real bug would hang a task runtime thread.
    #[test]
    fn slot_released_before_reentrant_same_thread_call_does_not_deadlock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            {
                let _guard = acquire_agent_slot("test-agent-reentrant", Some(1));
            } // guard dropped here, before any further same-thread work
            // If the first guard were still held here (the pre-fix bug),
            // this second acquire on the SAME thread would block forever,
            // since nothing else could ever release the only outstanding slot.
            let _guard2 = acquire_agent_slot("test-agent-reentrant", Some(1));
            tx.send(()).unwrap();
        });

        rx.recv_timeout(Duration::from_secs(5)).expect(
            "reentrant same-thread acquire_agent_slot call deadlocked - slot guard held too long",
        );
    }

    /// The daemon-restart sweep: `Running`/`Created` rows a dead daemon
    /// abandoned become `Failed` with an "interrupted" summary; already
    /// terminal rows are left exactly as they were.
    #[test]
    fn reconcile_orphaned_tasks_fails_only_non_terminal_rows() {
        let conn = test_conn();
        let running = create_for_cwd(&conn, "a", "claude", Path::new("/x")).unwrap();
        set_status(&conn, running, TaskStatus::Running).unwrap();
        let created = create_for_cwd(&conn, "b", "claude", Path::new("/x")).unwrap();
        let done = create_for_cwd(&conn, "c", "claude", Path::new("/x")).unwrap();
        finish(&conn, done, TaskStatus::Completed, None, None, Some(0), false, Some("ok"), false)
            .unwrap();

        assert_eq!(reconcile_orphaned_tasks(&conn).unwrap(), 2);
        assert_eq!(get(&conn, running).unwrap().unwrap().status, TaskStatus::Failed);
        assert_eq!(get(&conn, created).unwrap().unwrap().status, TaskStatus::Failed);
        let done_row = get(&conn, done).unwrap().unwrap();
        assert_eq!(done_row.status, TaskStatus::Completed);
        assert_eq!(done_row.summary.as_deref(), Some("ok"));
        assert!(get(&conn, running)
            .unwrap()
            .unwrap()
            .summary
            .unwrap()
            .contains("interrupted"));
    }

    /// `task cancel --force` / `task cleanup --force` route through
    /// `force_fail`: it clears a wedged non-terminal row and is a no-op on
    /// one that already finished, so the flag is safe to pass blindly.
    #[test]
    fn force_fail_clears_stuck_rows_and_ignores_terminal_ones() {
        let conn = test_conn();
        let stuck = create_for_cwd(&conn, "a", "claude", Path::new("/x")).unwrap();
        set_status(&conn, stuck, TaskStatus::Running).unwrap();
        let rec = force_fail(&conn, stuck, "force-cancelled").unwrap();
        assert_eq!(rec.status, TaskStatus::Failed);
        assert_eq!(rec.summary.as_deref(), Some("force-cancelled"));

        let done = create_for_cwd(&conn, "b", "claude", Path::new("/x")).unwrap();
        finish(&conn, done, TaskStatus::Completed, None, None, Some(0), false, Some("ok"), false)
            .unwrap();
        let rec = force_fail(&conn, done, "should be ignored").unwrap();
        assert_eq!(rec.status, TaskStatus::Completed);
        assert_eq!(rec.summary.as_deref(), Some("ok"));
    }

    /// E28 Task 14: `agent == "single-pool"` must never reach
    /// `adapter.run_prompt` (which would try to shell a binary literally
    /// named "single-pool" and fail) — it goes through
    /// `pool_agent::run_as_task` instead. With zero pool provider keys
    /// seeded, `bandit::pick` finds no candidates and `execute` returns
    /// `Exhausted` immediately, with no network call — a clean signal
    /// that the dispatch path taken was `pool_agent`, not a CLI shell
    /// (which would instead fail with "unknown agent" or a shell error).
    #[test]
    fn task_run_with_agent_single_pool_calls_pool_agent_not_a_cli() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DIVISI_CONFIG_DIR", dir.path());
        let conn = test_conn();
        let dirs = divisi_core::DivisiDirs::from_root(dir.path().to_path_buf());
        dirs.ensure_created().unwrap();

        let ctx = Context { dirs, resolved: divisi_core::ResolvedConfig::default(), registry: divisi_core::builtin_registry() };
        let this_repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let opts = RunTaskOptions {
            description: "hello from the pool",
            agent: "single-pool",
            cwd: &this_repo,
            use_worktree: false,
            account: None,
            real_home: true,
            no_memory_context: true,
            timeout: Duration::from_secs(5),
            allow_fallback: false,
            usage_json: false,
            require_structured_output: false,
        };

        let task = run(&conn, &ctx, opts).unwrap();
        assert_eq!(task.status, TaskStatus::Failed, "no keyed providers -> Exhausted -> a failed RunOutcome");
        assert!(
            task.rate_limited,
            "Exhausted must map onto the existing rate-limited terminal shape (Task 14), got summary: {:?}",
            task.summary
        );
    }
}
