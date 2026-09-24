# E27 — SingleCLI Coordinator + follow-ups Implementation Plan

**Status (2026-09-24): shipped.** Sessions, goals, planner/supervisor/integrator roles, routing and budgets are all in `crates/divisi-runtime/src/coordinator/`. Checkboxes below were never ticked; the code is the record. Follow-ups live in nbr-workspace `docs/queue/E27-singlecli-followups/`.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn SingleCLI from leaf ops (`task run`, broken `orchestrate-*`) into a coordinator: goal in → deterministic scheduler organises a self-correcting pool of agents, streaming progress; add a native `single acp` bridge, an OpenAI-compatible pool proxy, a `single loop` mode, and close the remaining E27.01 / follow-ups items.

**Architecture:** New `coordinator` module tree inside the existing `single-runtime` crate (needs `task`, `fallback`, `ctx`, sqlite — all already there). Four additive SQLite tables in the existing db, idempotent `CREATE TABLE IF NOT EXISTS`. A pure deterministic `tick()` scheduler is always in charge (capacity, queue, budget, retry, reconcile); an LLM is invoked only for three roles (planner / supervisor / integrator), each one a normal `task::run` with strict-JSON output. New socket requests in `single-protocol`, routed in `handlers.rs`, mirrored by thin `single-cli` subcommands. A scheduler tick-timer thread runs in `single-runtimed`.

**Tech Stack:** Rust 2021, `rusqlite` (bundled), `tokio` (daemon only — coordinator core is sync), `serde`/`serde_json`, `toml` 0.8, `clap` 4 derive, `chrono`. No new dependency is permitted (spec §1 non-goals).

**Spec:**
- `~/Projects/The-Company/nbr-workspace/docs/queue/E27-singlecli-followups/README.md`
- `~/Projects/The-Company/nbr-workspace/docs/queue/E27-singlecli-followups/01-cockpit-reliability.md`
- `~/Projects/The-Company/nbr-workspace/docs/queue/E27-singlecli-followups/02-coordinator-redesign.md` (authoritative for items 1–3, §11 all resolved)
- `~/Projects/The-Company/nbr-workspace/docs/queue/E27-singlecli-followups/singlecli-followups.md`

## Global Constraints

- **Version:** bump workspace `version` `0.9.6` → `0.10.0` in the root `Cargo.toml` `[workspace.package]` only (all crates are `version.workspace = true`). One `## [0.10.0]` block in `CHANGELOG.md`. No schema migration for existing tables — new tables are purely additive.
- **Build discipline (machine constraint, not in git):** `cargo build` / `cargo test` in the FOREGROUND, `-j2`, one or two crates at a time (`-p single-runtime -p single-protocol`). The harness memory watchdog kills concurrent/background cargo even with free RAM. Never background a build.
- **Acceptance gate:** `cargo build --workspace` and `cargo test --workspace` both pass; paste the final `test result:` line for every crate.
- **Commits:** sole-author, atomic per logical change, `type: description` subject (`feat` for new subsystems/subcommands, `fix` for bugs). NO `Co-Authored-By`, NO trailers. The `git-commit-standards` skill applies.
- **Doc-comment style:** terse lowercase `///` / `//`, explain the WHY, mark confirmed-vs-assumed. Match `state.rs` / `task.rs` / `context.rs`.
- **Compatibility (spec §9):** `single task run` and `single orchestrate-*` must keep working unchanged. New tables additive. `single-acp` Python prototype keeps working until native `single acp` ships.
- **No LLM process:** every "brain" call is one `task::run` against a routed agent. No always-on model, no new HTTP surface for the coordinator, no new LLM client.
- **DB access pattern:** `crate::state::open(&ctx.dirs.db_path())?` returns a `Connection` with WAL + 5s busy-timeout and the `events` table. Each handler opens its own connection (see `handlers.rs` `task_db` / `memory_db` helpers). Coordinator modules take `&Connection`, never open their own except on a spawned thread (mirror `task::run_background`).

---

## File Structure

### New — coordinator module tree (`crates/single-runtime/src/coordinator/`)

| File | Responsibility | Approx |
|---|---|---|
| `mod.rs` | public API (`submit_goal`, `goal_status`, `amend_goal`, `cancel_goal`, `session_new/list/close`, `coordinator_status`, `tick`), `ensure_coordinator_schema`, re-exports | ~180 |
| `session.rs` | `sessions` table CRUD; `Session`, `SessionInfo` types | ~200 |
| `goal.rs` | `goals` + `graph_nodes` CRUD; `Goal`, `GoalMode`, `GoalStatus`, `Node`, `NodeKind`, `Effort`, `NodeStatus`; `plan_json` ⇆ `graph_nodes` sync | ~280 |
| `graph.rs` | `TaskGraph` value type: ready-set, critical-path depth, patch-op application (`retarget`/`split`/`mark_optional`/`add_dependency`/`block`/`abort`). Pure, no db. | ~220 |
| `scheduler.rs` | `tick()`: reconcile → ready-set → capacity → admit → budget → retry → integrator trigger. Pure core (`tick_pure`) + a thin db/dispatch shell. | ~420 |
| `brain.rs` | planner / supervisor / integrator: prompt build, `task::run` invocation, first-valid-JSON extractor, response parse into graph / patch / summary | ~360 |
| `routing.rs` | `routing.toml` load + `select_agent(kind, effort, pool_health, routing) -> Option<String>`; `coordinator.toml` load into `CoordinatorConfig` | ~180 |
| `events.rs` | `coordinator_events` append + query; `Event`, `EventKind` | ~130 |

### Modified

| File | Change |
|---|---|
| `crates/single-runtime/src/lib.rs` | `pub mod coordinator;` |
| `crates/single-protocol/src/lib.rs` | new `Request` variants (§6); new `ResponseData` variants; `SessionInfo`, `GoalView`, `NodeView`, `CoordinatorSnapshot`, `CoordinatorEvent` structs |
| `crates/single-runtime/src/handlers.rs` | route the new `Request` variants to `coordinator::`; `coordinator_db` helper |
| `crates/single-runtime/src/bin/single-runtimed.rs` | spawn a scheduler tick-timer thread (interval from `coordinator.toml`, default 5s) that calls `coordinator::tick` |
| `crates/single-runtime/src/server.rs` | call `coordinator::ensure_coordinator_schema` + one `tick` in the startup reconcile block (next to `reconcile_orphaned_tasks`) |
| `crates/single-cli/src/main.rs` | `single session {new,list,close}`, `single goal {submit,status,list,amend,cancel}`, `single coordinator status`, `single acp`, `single loop`, `single serve --openai` subcommands |
| `crates/single-cli/src/render.rs` | pretty-printers for the new response types |
| `crates/single-runtime/src/orchestrate.rs` / `orchestrate_graph.rs` | verify/fix `--task` daemon-side parse (item 7) |
| root `Cargo.toml`, `CHANGELOG.md` | version bump |

### New config files (created lazily on first coordinator use, with seeded defaults)

- `~/.config/single/routing.toml` — `routing.rs` writes the §5.4 default table if absent
- `~/.config/single/coordinator.toml` — `max_parallel = 6`, `tick_interval_secs = 5`, `max_dispatches_per_goal = 25`, `max_goal_minutes = 60`, `max_supervisor_patches = 5`

---

## Phase 1 — Coordinator subsystem (item 1, this session's target)

### Task 1: Coordinator module skeleton + schema

**Files:**
- Create: `crates/single-runtime/src/coordinator/mod.rs`, `session.rs`, `goal.rs`, `events.rs` (schema fns only for now), `routing.rs` (config structs only), `graph.rs` (empty type stub), `scheduler.rs` (empty), `brain.rs` (empty)
- Modify: `crates/single-runtime/src/lib.rs` (add `pub mod coordinator;`)
- Test: inline `#[cfg(test)]` in `mod.rs`

**Interfaces:**
- Produces: `coordinator::ensure_coordinator_schema(conn: &rusqlite::Connection) -> anyhow::Result<()>` — idempotent, creates `sessions`, `goals`, `graph_nodes`, `coordinator_events`.

- [ ] **Step 1: Write the failing test** in `coordinator/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn ensure_coordinator_schema_is_idempotent_and_creates_all_tables() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_coordinator_schema(&conn).unwrap();
        ensure_coordinator_schema(&conn).unwrap(); // second call must not error
        for t in ["sessions", "goals", "graph_nodes", "coordinator_events"] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {t} missing");
        }
    }
}
```

- [ ] **Step 2: Run it, expect FAIL** (`ensure_coordinator_schema` undefined):
  `cargo test -p single-runtime coordinator::tests::ensure_coordinator_schema -- --nocapture`

- [ ] **Step 3: Implement.** `mod.rs`:

```rust
//! the coordinator: goal in -> deterministic scheduler drives a
//! self-correcting pool of agents. everything below `task::run` is
//! unchanged; this sits above it. see docs/superpowers/plans/2026-09-06-e27-coordinator.md
//! and nbr-workspace .../E27-singlecli-followups/02-coordinator-redesign.md.

pub mod brain;
pub mod events;
pub mod goal;
pub mod graph;
pub mod routing;
pub mod scheduler;
pub mod session;

use rusqlite::Connection;

/// creates every coordinator table if absent. additive — never touches the
/// existing `tasks` / `memory` / `events` tables. safe to call on every
/// daemon start (same discipline as `task::ensure_schema`).
pub fn ensure_coordinator_schema(conn: &Connection) -> anyhow::Result<()> {
    session::ensure_schema(conn)?;
    goal::ensure_schema(conn)?; // goals + graph_nodes
    events::ensure_schema(conn)?;
    Ok(())
}
```

  `session.rs` `ensure_schema` — columns exactly per spec §3.1 (`id TEXT PRIMARY KEY`, `cwd TEXT NOT NULL`, `title TEXT NOT NULL DEFAULT ''`, `created_at TEXT NOT NULL`, `updated_at TEXT NOT NULL`, `status TEXT NOT NULL DEFAULT 'active'`).

  `goal.rs` `ensure_schema` — `goals` per spec §3.2 (all columns; `plan_json TEXT NOT NULL DEFAULT '{}'`, integer counters `DEFAULT 0`, nullable `result_summary` / `blocked_reason`), then `graph_nodes` per §3.3 (`id TEXT`, `goal_id TEXT NOT NULL`, `desc TEXT NOT NULL`, `kind TEXT NOT NULL`, `effort TEXT NOT NULL`, `agent TEXT NOT NULL DEFAULT ''`, `depends_on TEXT NOT NULL DEFAULT '[]'`, `status TEXT NOT NULL DEFAULT 'pending'`, `task_id INTEGER`, `attempts INTEGER NOT NULL DEFAULT 0`, `worktree INTEGER NOT NULL DEFAULT 0`, `output_ref TEXT`, `PRIMARY KEY (goal_id, id)`).

  `events.rs` `ensure_schema` — `coordinator_events` per §3.5 (`id INTEGER PRIMARY KEY AUTOINCREMENT`, `session_id TEXT NOT NULL`, `goal_id TEXT`, `ts TEXT NOT NULL`, `kind TEXT NOT NULL`, `body TEXT NOT NULL DEFAULT ''`).

  `graph.rs`, `scheduler.rs`, `brain.rs`, `routing.rs` — module doc comment + `use` stubs only; empty otherwise (filled by later tasks). `routing.rs` may already hold the `CoordinatorConfig` / `RoutingTable` structs with `Default`.

- [ ] **Step 4: Run test, expect PASS.** Then `cargo build -p single-runtime -j2` (foreground) — expect clean.

- [ ] **Step 5: Commit:** `git add -A && git commit -m "feat: coordinator module skeleton and sqlite schema"`

---

### Task 2: `graph.rs` — TaskGraph value type (pure)

**Files:**
- Modify: `crates/single-runtime/src/coordinator/graph.rs`
- Modify: `crates/single-runtime/src/coordinator/goal.rs` (add the `Node`/enum types it references, or define them in `graph.rs` and re-export from `goal.rs`)
- Test: inline in `graph.rs`

**Interfaces:**
- Produces:
  - `NodeKind` (`Code Test Research Review Docs Infra Plan Supervise Integrate`), `Effort` (`Quick Standard Deep`), `NodeStatus` (`Pending Ready Running Done Failed Skipped Blocked`), `GoalMode` (`Auto Plan Careful Dry`), `GoalStatus` (`Planning Running Queued Blocked Done Failed Cancelled`) — all `#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]`, `#[serde(rename_all = "snake_case")]`, each with `as_str()` + `FromStr`/`parse` for db round-trips.
  - `struct Node { id: String, desc: String, kind: NodeKind, effort: Effort, agent: String, depends_on: Vec<String>, status: NodeStatus, task_id: Option<i64>, attempts: u32, worktree: bool, output_ref: Option<String> }`
  - `struct TaskGraph { nodes: Vec<Node> }` with:
    - `ready_set(&self) -> Vec<&Node>` — `status ∈ {Pending, Ready}` and every `depends_on` id is a node with `status ∈ {Done, Skipped}`.
    - `critical_path_depth(&self, id: &str) -> usize` — longest chain of dependents rooted at `id` (memoised via local `HashMap`); used to prefer critical-path nodes at admit time.
    - `apply_patch(&mut self, ops: &[PatchOp]) -> Result<()>` where `enum PatchOp { Retarget { node: String, kind: Option<NodeKind>, agent: Option<String> }, Split { node: String, into: Vec<Node> }, MarkOptional { node: String }, AddDependency { node: String, on: String }, Block { node: String, question: String }, Abort { reason: String } }`. `MarkOptional` sets status `Skipped` if not yet run else leaves it. `Split` replaces `node` with `into`, rewiring dependents of `node` to depend on all `into` ids. `Block` sets the node `Blocked`. `Abort` returns `Err` tagged so the scheduler fails the goal.
    - `is_all_terminal(&self) -> bool` — every node `∈ {Done, Failed, Skipped, Blocked}`.

- [ ] **Step 1: Write failing tests** (one `#[test]` each):
  - `ready_set_returns_only_dependency_satisfied_pending_nodes` — graph `s1(none)->pending, s2(dep s1)->pending`; ready-set is `[s1]`; mark `s1` Done; ready-set is `[s2]`.
  - `critical_path_prefers_longest_chain` — `s1->s2->s3` and lone `s4`; `critical_path_depth("s1") == 2`, `("s4") == 0`.
  - `apply_patch_split_rewires_dependents` — `s1, s2 depends_on [s1]`; split `s1` into `s1a, s1b`; `s2.depends_on == ["s1a","s1b"]` and `s1` gone.
  - `apply_patch_mark_optional_skips_unrun_node` — pending `s1` → `MarkOptional` → `Skipped`.
  - `apply_patch_abort_is_an_error`.
- [ ] **Step 2:** run `cargo test -p single-runtime coordinator::graph -- --nocapture`, expect FAIL (types undefined).
- [ ] **Step 3:** implement the types and methods. Keep everything `serde`-derivable so `goal.rs` can store the whole `TaskGraph` in `goals.plan_json` with `serde_json::to_string`.
- [ ] **Step 4:** run tests, expect PASS. `cargo build -p single-runtime -j2`.
- [ ] **Step 5: Commit:** `feat: coordinator TaskGraph type with ready-set, critical path, patch ops`

---

### Task 3: `events.rs` — coordinator_events append/query

**Files:** Modify `crates/single-runtime/src/coordinator/events.rs`; test inline.

**Interfaces:**
- Produces:
  - `enum EventKind` (`Plan NodeStarted NodeOutput NodeDone NodeFailed Supervisor Queued Blocked Integrated Budget Message`), snake_case serde + `as_str`.
  - `struct Event { id: i64, session_id: String, goal_id: Option<String>, ts: String, kind: EventKind, body: String }`
  - `fn append(conn, session_id: &str, goal_id: Option<&str>, kind: EventKind, body: &str) -> Result<i64>` (returns new id; `ts = chrono::Utc::now().to_rfc3339()`)
  - `fn since(conn, session_id: &str, since_event_id: i64) -> Result<Vec<Event>>` (ordered by `id ASC`, `id > since_event_id`)
  - `fn for_goal(conn, goal_id: &str, limit: usize) -> Result<Vec<Event>>` (most recent `limit`, returned oldest-first)

- [ ] **Step 1:** failing test `append_then_since_returns_new_events_only` — append 3 events for `sess_x`, `since(conn,"sess_x",0)` → 3, `since(conn,"sess_x",2nd_id)` → 1.
- [ ] **Step 2:** `cargo test -p single-runtime coordinator::events`, expect FAIL.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** test PASS; `cargo build -p single-runtime -j2`.
- [ ] **Step 5: Commit:** `feat: coordinator event log (append + since/for_goal queries)`

---

### Task 4: `session.rs` — sessions CRUD

**Files:** Modify `crates/single-runtime/src/coordinator/session.rs`; test inline.

**Interfaces:**
- Produces:
  - `struct Session { id: String, cwd: String, title: String, created_at: String, updated_at: String, status: String }`
  - `fn new_session(conn, cwd: &Path) -> Result<Session>` — id `sess_<ulid>` (use `single_core`'s existing id helper if present — CHECK `single-core` for a ulid/nanoid util; else format `sess_<epoch_ms>_<4 rand hex>` with `chrono` + a tiny `std::hash`-based rand, no new dep).
  - `fn get(conn, id: &str) -> Result<Option<Session>>`
  - `fn list(conn) -> Result<Vec<Session>>` (newest first)
  - `fn close(conn, id: &str) -> Result<()>` (`status='closed'`, bump `updated_at`)
  - `fn set_title_if_empty(conn, id: &str, title: &str) -> Result<()>` — called by `goal::submit` with the first goal's truncated text.

- [ ] **Step 1:** failing test `new_list_close_roundtrip`.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement. Decide the id helper — **CHECK FIRST**: `grep -rn "ulid\|nanoid\|fn short_id\|gen_id" crates/single-core/src`. Note in the commit which was used.
- [ ] **Step 4:** PASS; build.
- [ ] **Step 5: Commit:** `feat: coordinator session store`

---

### Task 5: `goal.rs` — goals + graph_nodes CRUD and plan_json sync

**Files:** Modify `crates/single-runtime/src/coordinator/goal.rs`; test inline.

**Interfaces:**
- Consumes: `graph::{TaskGraph, Node, NodeKind, Effort, NodeStatus, GoalMode, GoalStatus}`, `session`.
- Produces:
  - `struct Goal { id, session_id, text, mode: GoalMode, status: GoalStatus, max_dispatches: u32, max_minutes: u32, dispatches: u32, supervisor_patches: u32, plan_json: String, result_summary: Option<String>, blocked_reason: Option<String>, created_at: String, updated_at: String }`
  - `fn create(conn, session_id, text, mode, max_dispatches, max_minutes) -> Result<Goal>` — id `goal_<ulid>`, status `Planning`, writes row, appends a `message` event, calls `session::set_title_if_empty`.
  - `fn get(conn, id) -> Result<Option<Goal>>`, `fn list(conn, session_id: Option<&str>) -> Result<Vec<Goal>>`
  - `fn set_status(conn, id, GoalStatus) -> Result<()>`, `fn set_blocked(conn, id, reason: &str) -> Result<()>`, `fn set_summary(conn, id, &str) -> Result<()>`
  - `fn bump_dispatches(conn, id) -> Result<u32>`, `fn bump_supervisor_patches(conn, id) -> Result<u32>`
  - `fn save_graph(conn, goal_id, &TaskGraph) -> Result<()>` — writes `plan_json` AND upserts every node into `graph_nodes` (delete-all-then-insert for that goal_id is fine; keep it one transaction).
  - `fn load_graph(conn, goal_id) -> Result<TaskGraph>` — reads `graph_nodes` rows (authoritative over `plan_json` for status; `plan_json` is the human-readable mirror).
  - `fn update_node(conn, goal_id, node_id, NodeStatus, task_id: Option<i64>, output_ref: Option<&str>, attempts: Option<u32>) -> Result<()>`

- [ ] **Step 1:** failing tests: `create_goal_persists_and_titles_session`; `save_then_load_graph_roundtrips_node_status` (save graph, `update_node` to Done, `load_graph` reflects it); `bump_counters_increment`.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [ ] **Step 5: Commit:** `feat: coordinator goal + graph_nodes store with plan_json sync`

---

### Task 6: `routing.rs` — routing.toml / coordinator.toml + agent selection

**Files:** Modify `crates/single-runtime/src/coordinator/routing.rs`; test inline.

**Interfaces:**
- Produces:
  - `struct CoordinatorConfig { max_parallel: usize, tick_interval_secs: u64, max_dispatches_per_goal: u32, max_goal_minutes: u32, max_supervisor_patches: u32 }` + `Default` (6 / 5 / 25 / 60 / 5) + `fn load(dirs: &SingleDirs) -> CoordinatorConfig` (read `~/.config/single/coordinator.toml`, write defaults if absent, tolerate missing keys).
  - `struct RoutingTable { kinds: BTreeMap<String, BTreeMap<String, Vec<String>>>, effort_max_steps: BTreeMap<String, u32>, fallback_default: Vec<String> }` + `fn load(dirs) -> RoutingTable` (seed §5.4 defaults to disk if absent).
  - `struct PoolHealth { detected_authed: BTreeSet<String>, rate_limited: BTreeSet<String> }` + `fn probe(ctx: &Context, conn: &Connection) -> PoolHealth` — `detected_authed` from `doctor`/registry discovery (reuse `doctor::` snapshot or `adapter.discover()`), `rate_limited` from `tasks.rate_limited=1` rows in the last N minutes + any `single_core::account` status marker. Keep cheap; cache is fine.
  - `fn select_agent(table: &RoutingTable, kind: NodeKind, effort: Effort, health: &PoolHealth) -> Option<String>` — walk `kinds[kind][effort]`, return first that is in `detected_authed` and not in `rate_limited`; then `kinds[kind]["standard"]`; then `fallback_default`; else `None`.
  - `fn max_steps(table, effort) -> u32`

- [ ] **Step 1:** failing tests: `select_agent_skips_rate_limited_and_unauthed`; `select_agent_falls_back_to_default_when_kind_list_exhausted`; `config_load_writes_defaults_when_absent` (tempdir).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement. `PoolHealth::probe` may be shimmed behind a trait / taken as data in tests so `select_agent` stays pure.
- [ ] **Step 4:** PASS; build.
- [ ] **Step 5: Commit:** `feat: coordinator routing table and pool-aware agent selection`

---

### Task 7: `scheduler.rs` — pure `tick_pure` core

**Files:** Modify `crates/single-runtime/src/coordinator/scheduler.rs`; test inline (this is the spec's headline pure-unit target, §4 + §10).

**Interfaces:**
- Consumes: `graph::{TaskGraph, Node, NodeStatus}`, `routing::{RoutingTable, PoolHealth, CoordinatorConfig}`.
- Produces:
  - `struct Capacity { global_running: usize, per_agent_running: BTreeMap<String, usize>, per_agent_cap: BTreeMap<String, usize> }`
  - `struct GoalBudget { dispatches: u32, max_dispatches: u32, started_at: DateTime<Utc>, max_minutes: u32, now: DateTime<Utc> }` + `fn exhausted(&self) -> Option<String>` (returns the human reason or `None`).
  - `enum TickAction { Dispatch { node_id: String, agent: String, effort: Effort, worktree: bool, max_steps: u32 }, RunIntegrator, Block { reason: String }, Fail { reason: String }, Noop }`
  - `fn tick_pure(graph: &TaskGraph, cfg: &CoordinatorConfig, cap: &Capacity, budget: &GoalBudget, table: &RoutingTable, health: &PoolHealth) -> Vec<TickAction>` — implements spec §4 steps 2–7 deterministically: compute ready-set, order by `(critical_path_depth desc, effort asc)`, admit up to `min(cfg.max_parallel - cap.global_running, per-agent headroom)`, emit `Dispatch`; if `budget.exhausted()` → single `Block`; if `graph.is_all_terminal()` → `RunIntegrator`; if a node's `PatchOp::Abort` was recorded / all terminal with failures unrecoverable → `Fail`. NO db, NO subprocess, NO LLM.
  - `fn retry_decision(node: &Node) -> RetryDecision` where `enum RetryDecision { RetrySameNextAgent, Supervisor, GiveUp }` per §4.6 (`attempts < 2` → next agent; `>= 2` or semantic failure → supervisor).

- [ ] **Step 1: Write failing tests** (the core deliverable — cover every §4 branch):
  - `admits_independent_nodes_up_to_global_cap` — 4 independent ready nodes, `max_parallel=2`, `global_running=0` → exactly 2 `Dispatch`.
  - `respects_per_agent_cap` — 3 ready nodes all routing to `opencode` with `per_agent_cap[opencode]=1` → 1 `Dispatch`.
  - `prefers_critical_path_then_cheaper_effort` — assert dispatch order.
  - `queues_when_pool_saturated` — `global_running == max_parallel` → `Noop` (no dispatch), nodes stay ready.
  - `budget_dispatch_cap_blocks_the_goal` — `dispatches == max_dispatches` → single `TickAction::Block` with a reason mentioning dispatches.
  - `budget_wallclock_cap_blocks_the_goal` — `now - started_at > max_minutes`.
  - `all_terminal_triggers_integrator`.
  - `retry_decision_advances_agent_then_escalates_to_supervisor`.
  - `reconcile helper` test lives in Task 8 (needs db).
- [ ] **Step 2:** `cargo test -p single-runtime coordinator::scheduler -- --nocapture`, expect FAIL.
- [ ] **Step 3:** implement `tick_pure` + helpers.
- [ ] **Step 4:** all PASS; `cargo build -p single-runtime -j2`.
- [ ] **Step 5: Commit:** `feat: deterministic coordinator scheduler (pure tick core)`

---

### Task 8: `scheduler.rs` — db/dispatch shell + reconcile

**Files:** Modify `scheduler.rs`, `mod.rs`; test inline (db-backed).

**Interfaces:**
- Consumes: `Context`, `Connection`, everything above, `crate::task`, `crate::registry::TaskRegistry`.
- Produces:
  - `fn reconcile(conn: &Connection) -> Result<usize>` — any `graph_nodes.status='running'` whose `task_id` row is terminal or whose PID is gone → set `failed`; any goal with status `running`/`planning` and no live node → leave for `tick` to re-evaluate. Returns count reconciled. (This also satisfies E27.01 "no permanent zombie rows" for coordinator nodes — the `tasks` table already has `reconcile_orphaned_tasks`.)
  - `fn tick(ctx: &Context, conn: &Connection, registry: &TaskRegistry) -> Result<()>` — for each non-terminal goal: load graph + goal, build `Capacity` (count running nodes across ALL goals per agent; caps from registry `max_concurrency` ∧ routing), build `GoalBudget`, call `tick_pure`, then execute the actions:
    - `Dispatch` → `goal::update_node(... Running ...)`, `events::append(NodeStarted)`, `goal::bump_dispatches`, then hand to `task::run_background` with `OwnedRunTaskOptions { description: node.desc + dependency outputs, agent, cwd, use_worktree: node.worktree, real_home: false, no_memory_context: false, timeout: from effort, allow_fallback: true, account: None }`; store returned `task_id` on the node.
    - `RunIntegrator` → call `brain::integrate` (Task 10); on ok set goal `Done` + summary + `events::append(Integrated)`.
    - `Block` → `goal::set_blocked` + `events::append(Blocked)`.
    - `Fail` → goal `Failed` + event.
  - `fn on_task_finished(ctx, conn, registry, task_id: i64) -> Result<()>` — find the node with that `task_id`, read the `tasks` row status/artifact, set node `Done`+`output_ref` or `Failed`, apply `retry_decision`, then `tick`.
- Consumed by: `handlers.rs` (on `GoalSubmit` and after any `TaskRun`-family completion), `single-runtimed` timer thread.

- [ ] **Step 1:** failing tests (in-memory / tempdir db, NO real agents — stub the dispatch by injecting a `Dispatcher` trait or gating `task::run_background` behind a test flag that just inserts a fake terminal `tasks` row):
  - `reconcile_marks_running_node_with_dead_task_as_failed`
  - `tick_dispatches_ready_node_and_records_event` (with the fake dispatcher)
  - `on_task_finished_success_marks_node_done_and_advances`
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement. Introduce `trait Dispatcher { fn dispatch(&self, opts: OwnedRunTaskOptions) -> Result<i64>; }` with a real impl (`task::run_background`) and a test impl, so `tick` stays testable. Note the seam in the commit body.
- [ ] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [ ] **Step 5: Commit:** `feat: coordinator scheduler db shell, dispatch seam, node reconcile`

---

### Task 9: `brain.rs` — JSON extractor + planner

**Files:** Modify `crates/single-runtime/src/coordinator/brain.rs`; test inline for the extractor; planner integration test `#[ignore]`.

**Interfaces:**
- Consumes: `Context`, `Connection`, `crate::task::{run, RunTaskOptions}`, `routing`, `graph`.
- Produces:
  - `fn extract_first_json(s: &str) -> Option<serde_json::Value>` — port of the Python `single-acp` first-valid-JSON scanner: scan for `{` or `[`, track brace/bracket depth respecting strings + escapes, return the first balanced span that `serde_json` parses. (Reference: `~/.local/bin/single-acp` / `nbr-workspace/tools/single-acp`.)
  - `fn plan(ctx, conn, goal: &Goal, health: &PoolHealth, table: &RoutingTable) -> Result<TaskGraph>` — build the §5.1 prompt (goal text + `single context` for cwd + epic README/GAPS if the text names `docs/queue/E**`), route via `select_agent(NodeKind::Plan, Effort::Standard, ...)`, `task::run` with a strict "output ONLY a JSON array of `{id,desc,kind,effort,depends_on}`" instruction, `extract_first_json`, validate 2–8 nodes, assign `agent` per node via `select_agent(node.kind, node.effort, health)`, set `worktree` default (true for `code`/`infra`).
- Produces (used by Task 8): a `PlanError` that the scheduler turns into goal `Failed` if planning can't produce a valid graph after the routed fallback chain is exhausted.

- [ ] **Step 1:** failing tests for `extract_first_json`:
  - `extracts_json_object_from_surrounding_prose`
  - `handles_braces_inside_strings`
  - `returns_none_when_no_balanced_json`
  - `prefers_first_of_two_json_blocks`
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement the extractor + `plan`. Planner's live call is only exercised by an `#[ignore]` integration test (`plan_produces_a_valid_small_graph_against_a_cheap_agent`).
- [ ] **Step 4:** extractor tests PASS; `cargo build -p single-runtime -j2`.
- [ ] **Step 5: Commit:** `feat: coordinator brain — first-valid-JSON extractor and planner role`

---

### Task 10: `brain.rs` — supervisor + integrator

**Files:** Modify `brain.rs`; test the parsers inline.

**Interfaces:**
- Produces:
  - `fn supervise(ctx, conn, goal, graph: &TaskGraph, failing_node_id: &str, health, table) -> Result<Vec<graph::PatchOp>>` — §5.2 prompt (current graph + failing node output + sibling outputs), routed via `select_agent(NodeKind::Supervise, ...)`, strict-JSON patch-op list, `extract_first_json`, parse into `Vec<PatchOp>`. Caller (scheduler) enforces the 5-patch cap and converts an over-cap trigger into `Block` (spec §5.2, §11.4).
  - `fn integrate(ctx, conn, goal, graph: &TaskGraph, health, table) -> Result<IntegrationOutcome>` where `struct IntegrationOutcome { summary: String, files_changed: Vec<String>, checks_pass: bool, residual_gaps: Vec<String>, unrecoverable: bool }` — §5.3 prompt (every node's `output_ref` + the goal), run in goal cwd (worktree merge is Phase 2 / item deferred — for now assume nodes ran in cwd or note the gap), strict-JSON out.
  - `fn parse_patch_ops(v: &serde_json::Value) -> Result<Vec<PatchOp>>`, `fn parse_integration(v) -> Result<IntegrationOutcome>` — pure, tested with fixtures.

- [ ] **Step 1:** failing tests: `parse_patch_ops_reads_retarget_and_split`; `parse_patch_ops_rejects_unknown_op`; `parse_integration_reads_summary_and_flags`.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [ ] **Step 5: Commit:** `feat: coordinator brain — supervisor patch ops and integrator`

---

### Task 11: `single-protocol` — new requests/responses

**Files:** Modify `crates/single-protocol/src/lib.rs`; round-trip test inline.

**Interfaces:** (spec §6) new `Request` variants — `SessionNew { cwd: String }`, `SessionList`, `SessionClose { session_id: String }`, `GoalSubmit { session_id: String, text: String, mode: Option<String>, max_dispatches: Option<u32>, max_minutes: Option<u32> }`, `GoalStatus { goal_id: String }`, `GoalAmend { goal_id: String, text: String }`, `GoalCancel { goal_id: String }`, `SessionEvents { session_id: String, since_event_id: i64 }`, `CoordinatorStatus`.
New `ResponseData` — `SessionInfo(SessionInfoData)`, `SessionInfos(Vec<..>)`, `GoalId { goal_id: String }`, `GoalView(GoalViewData)` (`{ goal, nodes: Vec<NodeView>, recent_events: Vec<CoordinatorEvent> }`), `Goals(Vec<GoalSummary>)`, `CoordinatorEvents(Vec<CoordinatorEvent>)`, `CoordinatorSnapshot(CoordinatorSnapshotData)` (`{ running_goals, queued, pool: Vec<PoolAgent>, budgets }`). All structs `#[derive(Debug, Clone, Serialize, Deserialize)]`.

- [ ] **Step 1:** failing test `each_new_request_round_trips_through_json` (serialize→deserialize each variant, assert equality via `serde_json::to_value`).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** add the variants/structs. `cargo build -p single-protocol -j2`.
- [ ] **Step 4:** PASS.
- [ ] **Step 5: Commit:** `feat: coordinator socket protocol (session/goal/coordinator requests)`

---

### Task 12: `handlers.rs` — route the new requests

**Files:** Modify `crates/single-runtime/src/handlers.rs`; extend the existing handler test harness (`handlers.rs` `#[cfg(test)]`, see line ~1961).

**Interfaces:**
- Consumes: `coordinator::`, protocol types from Task 11.
- Produces: a `coordinator_db(ctx) -> Result<Connection>` helper (mirror `task_db`, but also calls `coordinator::ensure_coordinator_schema`). New `match` arms in `dispatch`:
  - `SessionNew { cwd }` → `session::new_session` → `ResponseData::SessionInfo`
  - `SessionList` → `session::list`
  - `SessionClose { session_id }` → `session::close` → `Empty`
  - `GoalSubmit { .. }` → `coordinator::submit_goal` (creates goal `Planning`, runs `brain::plan` inline via `spawn_blocking` path — it's already on the blocking pool per `server.rs`, so a synchronous plan call is acceptable; long dispatches go async via the scheduler), returns `GoalId`. Then one `scheduler::tick`.
  - `GoalStatus { goal_id }` → assemble `GoalView` from `goal::get` + `goal::load_graph` + `events::for_goal`.
  - `GoalAmend { goal_id, text }` → append text as a goal note / raise budget if the text parses as `budget=N` / answer a blocked question; re-tick.
  - `GoalCancel` → set goal `Cancelled`, flip any running node's task cancel flag via `registry`.
  - `SessionEvents { session_id, since_event_id }` → `events::since`
  - `CoordinatorStatus` → cross-goal snapshot.
- Also: after every `Request::TaskRun*` / orchestrate completion in `dispatch`, if the finished task's id maps to a coordinator node, call `scheduler::on_task_finished`. (Simplest: `on_task_finished` is a no-op when the task id isn't a coordinator node.)

- [ ] **Step 1:** failing test `goal_submit_then_status_returns_a_planning_goal` using a stub planner (feature-gate `brain::plan` behind a test hook that returns a fixed 2-node graph so no LLM runs).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement the arms + helper.
- [ ] **Step 4:** PASS; `cargo build -p single-runtime -p single-protocol -j2`.
- [ ] **Step 5: Commit:** `feat: route coordinator requests in the daemon handlers`

---

### Task 13: `single-runtimed` tick timer + `single-cli` mirrors

**Files:** Modify `crates/single-runtime/src/bin/single-runtimed.rs`, `crates/single-runtime/src/server.rs`, `crates/single-cli/src/main.rs`, `crates/single-cli/src/render.rs`, `crates/single-cli/src/client.rs` (if a helper is needed).

**Interfaces:**
- `single-runtimed`: after `server::serve` setup, spawn a `std::thread` (not a tokio task — coordinator core is sync and may block on `task::run` for the integrator) that loops `sleep(cfg.tick_interval_secs); Context::load(); open db; coordinator::ensure_coordinator_schema; scheduler::reconcile (first iter only); scheduler::tick(...)`. Guard with a `OnceLock<()>`-style "one tick at a time" flag (mirror `DoctorGuard`).
- `server.rs`: in the startup reconcile block, add `coordinator::ensure_coordinator_schema(&conn)?; coordinator::scheduler::reconcile(&conn)?;`.
- `single-cli` `main.rs`: `clap` subcommands (thin — each builds a `Request`, calls `client::send`, formats via `render.rs`):
  - `single session new [--cwd PATH]` / `single session list` / `single session close <id>`
  - `single goal submit <text...> [--session <id>] [--mode auto|plan|careful|dry] [--max-dispatches N] [--max-minutes N]` (auto-creates a session for cwd if `--session` omitted)
  - `single goal status <goal_id>` / `single goal list [--session <id>]` / `single goal amend <goal_id> <text...>` / `single goal cancel <goal_id>`
  - `single coordinator status`

- [ ] **Step 1:** failing test — `single-cli` arg-parse test (the crate already has clap tests? CHECK; if not, a `try_parse_from` unit test) `goal_submit_parses_flags`.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement. Keep `render.rs` output terse and greppable (one line per node in `goal status`: `s2  code/standard  running  opencode  #771`).
- [ ] **Step 4:** PASS; `cargo build -p single-cli -p single-runtime -j2`.
- [ ] **Step 5: Commit:** `feat: single session/goal/coordinator CLI subcommands and tick timer`

---

### Task 14: workspace build + test + version bump

- [ ] **Step 1:** `cargo build --workspace -j2` (foreground, may take a while) — must be clean.
- [ ] **Step 2:** `cargo test --workspace -j2` — capture every `test result:` line.
- [ ] **Step 3:** fix any breakage (most likely: `handlers.rs` exhaustive-match on `Request`, `render.rs` exhaustive-match on `ResponseData`).
- [ ] **Step 4:** bump `Cargo.toml` `[workspace.package] version` → `0.10.0`; add `CHANGELOG.md` `## [0.10.0]` block summarising the coordinator subsystem + new subcommands.
- [ ] **Step 5: Commit:** `chore: bump version to 0.10.0`

---

## Phase 2 — native `single acp` (item 2 + Zed backlog #3) — next session

**Files:** Create `crates/single-cli/src/acp.rs` (ACP stdio server, ~500). Modify `main.rs` (`single acp` subcommand). Reference: `nbr-workspace/tools/single-acp` (Python), schema `nbr-workspace/.../acp-schema.json`.

**Tasks:**
1. ACP JSON-RPC 2.0 framing (newline-delimited, `initialize` → static capabilities; error objects per spec). Unit-test the framing with scripted stdin.
2. `session/new` → `Request::SessionNew`; advertise modes (`auto`/`plan`/`careful`/`dry` + forced-agent shortcuts) + slash commands (`/status /queue /agents /usage /mcp /lsp /providers /dashboard /epic`).
3. `session/prompt` routing: `/`-prefixed → run against socket; status-y heuristic → `GoalStatus`/`CoordinatorStatus` (no agent burned); else → `GoalSubmit` + long-poll `SessionEvents`, translate `plan`→ACP `plan`, `node_output`→`agent_message_chunk`, `node_done`+diff→`tool_call`+`diff`, `blocked`→`session/request_permission`, `integrated`→final chunk + `stopReason: end_turn`.
4. `session/cancel` → `GoalCancel`; `session/load` → rebind + replay recent events.
5. Scripted-ACP-client test (as used for the Python prototype): `initialize` / `session/new` / status-question prompt / goal prompt / `session/load`.
6. Retire `~/.local/bin/single-acp`; switch Zed `agent_servers.SingleCLI.command` → `["single","acp"]`.

**Commit:** `feat: native single acp ACP bridge subcommand`

---

## Phase 3 — resumable sessions + per-turn usage (Zed backlog #4, #5; E27 gap #9) — next session

1. **Per-task token capture:** in `task::execute`, parse the agent adapter's reported usage if the CLI emits it (CHECK each adapter in `single-agent-sdk` for a usage line); add `tasks.prompt_tokens` / `completion_tokens` columns (`add_column_if_missing`). Fall back to a rough `chars/4` estimate flagged `estimated=1` when the CLI is silent.
2. **`GoalStatus` / `SessionEvents`** expose per-node + per-turn token counts (sum of the node's task rows).
3. **`session/load`** in `single acp` rebinds to the existing `sessions` row and replays `coordinator_events` as context — verify with a scripted client (round-trip a goal, `session/load`, assert replay).

**Commit:** `feat: per-task token accounting and session load/replay`

---

## Phase 4 — `single loop --until-done` (E27.01 P2) — next session

Thin sugar over the coordinator: `single loop <goal> [--agent X] [--until-done] [--max-iters N=6]` → `GoalSubmit { mode: "careful" }` where `careful` mode makes the scheduler re-dispatch the terminal node with the prior output appended until the agent emits a line `DONE` or `max_iters` is hit (mirrors the Python prototype). If `--agent` is given, pin routing to it for this goal. Implement `careful` re-dispatch in `scheduler::on_task_finished`. **Note in the commit** whether this is pure sugar over `GoalSubmit` (preferred) or a separate mini-engine.

**Commit:** `feat: single loop subcommand (careful-mode goal sugar)`

---

## Phase 5 — `single serve --openai` (E27.01 backlog #2, Zed backlog #2) — next session

**Files:** Create `crates/single-cli/src/serve_openai.rs` (~300). Modify `main.rs`.

A minimal OpenAI-compatible HTTP server (`reqwest` is blocking-only here — use `std::net::TcpListener` + a hand-rolled HTTP/1.1 request parser, or CHECK whether `single-web` already vendors a tiny HTTP server to reuse). Endpoints: `POST /v1/chat/completions` (non-stream + SSE stream), `GET /v1/models`. Each request → pick a pool agent via routing (`kind=code, effort=quick` default) → `task::run` one-shot with the prompt → map output back to the OpenAI response shape. A 429-shaped failure hops the fallback chain (reuse `single_core::fallback` / `allow_fallback: true`). No new dependency.

**Verify:** `curl localhost:PORT/v1/chat/completions -d '{"model":"pool","messages":[{"role":"user","content":"say hi"}]}'` → a pool agent answers; force a 429 and confirm the hop.

**Commit:** `feat: single serve --openai pool proxy`

---

## Phase 6 — opencode EOL-catalog filter (followups §2/§3) — any session

- [ ] **Re-verify current state first** (spec specifics are stale, omniroute is gone): `single provider inspect nvidia`; `opencode models | grep -c nvidia`; check whether opencode's `@ai-sdk/openai-compatible` now honours a declared `models` map or has a `disableModelDiscovery` / `models.discovery = false` flag (`opencode` docs / provider schema, not just `--help`).
- [ ] If an opencode flag exists → set it in `single_core`'s opencode-sync path (`crates/single-core/src/.../opencode` — CHECK) so synced provider blocks carry it.
- [ ] Else → filter SingleCLI-side: in the `provider sync` → opencode-config writer, emit only the provider's declared `models` and set whatever opencode key suppresses live merge; if none, at minimum stop writing providers whose live catalog can't be constrained and document the limitation in `single provider sync`'s output.

**Commit:** `fix: constrain opencode model discovery to declared provider catalog`

---

## Phase 7 — verify `orchestrate-parallel` / `orchestrate-graph` `--task` (item 7) — any session

- [ ] Repro against the daemon: `single orchestrate-parallel --task 'grok:say ONE' --task 'opencode:say TWO'` and `single orchestrate-graph --task 'id=a,agent=grok,desc=say AAA'`.
- [ ] If 2dcf6c6 fixed it → add a regression test in `orchestrate.rs` / `orchestrate_graph.rs` (`parses_multiple_task_specs`) and update `01-cockpit-reliability.md` "Still open" to strike the item.
- [ ] If still broken → the parse bug is daemon-side in `run_parallel` / `run_graph` (CLI parse is confirmed fine per the spec). Fix the `--task` spec splitter there. Since the coordinator scheduler now subsumes multi-agent fan-out, a valid alternative (spec §2) is to keep `orchestrate-*` working but document it as a low-level escape hatch — either way it must parse ≥1 node.

**Commit:** `fix: orchestrate-* --task specs parse on the daemon side` (or `test:` if already fixed)

---

## Deploy + live verification (after Phase 1, then re-run after each later phase)

1. Reinstall release binaries to `~/.local/bin`: build `--release -j2`, copy `single single-runtimed single-mcp singlecli-mcp single-lsp single-agent` from `target/release/`.
2. `systemctl --user stop single-runtimed; pkill -x single-runtimed; rm -f ~/.config/single/state/runtime.sock; systemctl --user start single-runtimed`.
3. `single --version` → `0.10.0`; `single daemon status` → running.
4. **Coordinator happy path:** `single goal submit "add a CHANGELOG stub and a hello function with a test" --max-dispatches 8` against cheap agents → watch `single goal status <id>` go plan → dispatch (parallel where the graph allows) → integrate → `done`; `coordinator_events` streamed.
5. **Cross-thread:** `single coordinator status` shows running/queued goals + pool caps.
6. **Budget stop:** submit a goal with `--max-dispatches 1` that needs more → goal `blocked` with a reason → `single goal amend <id> "budget=10"` → re-ticks and proceeds.
7. **Reconcile:** kill the daemon mid-goal, restart → running nodes → `failed`, no permanent zombie rows (`single goal status` + `select * from graph_nodes where status='running'` empty).
8. **`single acp`** (Phase 2): scripted ACP client — `initialize` / `session/new` / status-question prompt (no agent burned) / real goal prompt (events → ACP updates) / `session/load` replay.
9. **Zed switch** (Phase 2): `agent_servers.SingleCLI.command = ["single","acp"]`; retire `~/.local/bin/single-acp`.
10. **`single serve --openai`** (Phase 5): curl `/v1/chat/completions`; confirm pool answer + 429 fallback hop.
11. **`single loop`** (Phase 4): `single loop "iterate on X" --until-done` finishes on `DONE` within the iter cap.

---

## Self-Review

**Spec coverage (02-coordinator-redesign.md):**
- §2 architecture — Phase 1 (module tree in `single-runtime`, unchanged executor). ✓
- §3 job model — Task 1 (schema), 4 (sessions), 5 (goals/nodes), 3 (events). ✓
- §4 scheduler — Task 7 (`tick_pure`, all branches), Task 8 (reconcile + db shell). ✓
- §5 brain + routing — Task 6 (routing), 9 (planner + extractor), 10 (supervisor + integrator). ✓
- §6 protocol — Task 11. ✓
- §7 messenger — Phase 2. ✓ (worktree merge for `code`/`infra` nodes flagged as a Phase 2/3 gap in Task 10 — the pure scheduler carries the `worktree` bool but assisted merge-back is deferred; called out here explicitly, not silently.)
- §8 module layout / config / orchestrate fix — Task 1/13 (modules, config), Phase 7 (orchestrate). ✓
- §9 migration — Global Constraints (additive tables, version bump, prototypes keep working). ✓
- §10 testing — every code Task is TDD; scheduler/graph/routing/brain-parsers pure-unit; planner/supervisor/integrator live calls `#[ignore]`; protocol round-trip Task 11; messenger scripted-client Phase 2. ✓
- §11 decisions — dynamic routing (Task 6/9), dispatch+wallclock budget (Task 7), worktree-per-code-node (carried, merge deferred — flagged), 5-patch cap (Task 8/10), module-not-crate (Task 1). ✓

**01-cockpit-reliability.md remaining:** zombie reconciliation — coordinator nodes Task 8; `tasks` table already done (`reconcile_orphaned_tasks`). `orchestrate-* --task` — Phase 7. `single loop` — Phase 4. `single acp` — Phase 2. `single serve --openai` — Phase 5. Backlog #1 keyring codex/cursor — already shipped (0.9.5). ✓

**followups.md live items:** §1 lazy-load verify — already verified live per the epic brief (no code). §2/§3 opencode catalog — Phase 6. §4 polish — mostly landed in 0.9.6 (c727a39); any stragglers fold into Phase 1 commits opportunistically. §5 `provider add` upsert — shipped (42a4c14). ✓

**Placeholder scan:** every code Task has concrete signatures + test code. Deferred items (worktree merge-back, live LLM calls) are explicitly marked deferred with the reason, not hidden.

**Type consistency:** `NodeKind`/`Effort`/`NodeStatus`/`GoalStatus`/`GoalMode` defined in Task 2 (`graph.rs`), consumed unchanged in 5/6/7/8/9/10/11/12. `TaskGraph` / `Node` / `PatchOp` — Task 2, consumed everywhere after. `Dispatcher` seam — Task 8. `extract_first_json` — Task 9, reused Task 10. Protocol structs — Task 11, consumed Task 12/13.
