//! Wire types for the SingleCLI runtime IPC protocol.
//!
//! The CLI and TUI talk to the runtime daemon over a Unix domain socket using
//! newline-delimited JSON: one [`Request`] per line in, one [`Response`] per
//! line out. This keeps the protocol trivially inspectable with `nc`/`socat`
//! during development, at the cost of not being a "real" RPC framework —
//! acceptable for Phase 1's single local daemon + local clients.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Status,
    /// `fix: true` (`single doctor --fix`) also runs a self-heal pass
    /// (E28 spec §9) across every enabled category before returning the
    /// report, instead of only reporting findings.
    Doctor {
        #[serde(default)]
        fix: bool,
    },
    /// Asks a running `single-runtimed` to exit after acknowledging this
    /// request — see `single-cli::daemon::stop_running`. Exists because the
    /// daemon inherits its environment (notably `$PATH`) once at spawn
    /// time and keeps it for the life of the process, so a newly installed
    /// agent CLI is invisible to detection until the daemon is restarted.
    Shutdown,
    AgentList,
    AgentInspect {
        name: String,
    },
    McpList,
    McpAdd {
        server: McpServerSpec,
    },
    McpRemove {
        name: String,
    },
    McpEnable {
        name: String,
    },
    McpDisable {
        name: String,
    },
    McpInspect {
        name: String,
    },
    McpPresetList,
    McpAddPreset {
        name: String,
    },
    /// Toggles gateway mode (see `single_core::mcp::gateway_mode`) — takes
    /// effect on the next `single install-integrations --yes`, not
    /// retroactively.
    McpGatewaySetEnabled {
        enabled: bool,
    },
    McpGatewayStatus,
    LspList,
    LspAdd {
        server: LspServerSpec,
    },
    LspRemove {
        name: String,
    },
    LspEnable {
        name: String,
    },
    LspDisable {
        name: String,
    },
    LspInspect {
        name: String,
    },
    LspPresetList,
    LspAddPreset {
        name: String,
    },
    ToolList,
    ToolAdd {
        tool: ToolSpec,
    },
    ToolInspect {
        name: String,
    },
    ToolEnable {
        name: String,
    },
    ToolDisable {
        name: String,
    },
    SecretList,
    SecretSet {
        name: String,
        value: String,
    },
    /// Returns whether the secret exists and its value in one shot — the
    /// value never passes through the runtime's event log (see
    /// `single-runtime`'s `handlers.rs`), only through this direct response.
    SecretGet {
        name: String,
    },
    SecretDelete {
        name: String,
    },
    /// E29: promotes a live `{{REDACTED:<session>:N}}` redaction alias
    /// (still within its 3h TTL) into a properly named secret, then
    /// deletes the pending-alias row. The CLI caller is expected to have
    /// already confirmed this with the user — the daemon does not ask.
    SecretPromoteAlias {
        alias: String,
        name: String,
    },
    SkillList,
    SkillInstall {
        name: String,
        source_path: String,
    },
    SkillRemove {
        name: String,
    },
    SkillInspect {
        name: String,
    },
    /// Copies a skill into Claude Code's real skill directory
    /// (`~/.claude/skills/<name>/`) — see `single_core::skills::sync_to_claude`.
    SkillSyncClaude {
        name: String,
    },
    /// Lists the curated starter skills bundled with SingleCLI itself —
    /// see `single_core::skills::starter_set`.
    SkillStarterList,
    SkillInstallStarter {
        name: String,
    },
    MemoryStore {
        scope: Option<MemoryScope>,
        source: Option<MemorySource>,
        project: Option<String>,
        agent: Option<String>,
        task: Option<String>,
        title: String,
        content: String,
        confidence: Option<f64>,
        expires_in_seconds: Option<i64>,
    },
    MemorySearch {
        query: String,
        scope: Option<MemoryScope>,
        project: Option<String>,
    },
    /// Embeds `query` and searches the vector store for the nearest
    /// stored memory entries — real semantic search, not `LIKE` matching
    /// (see `single-runtime::embeddings`/`qdrant_backend`). Falls back to
    /// `MemorySearch`'s substring matching when no embeddings key and/or
    /// `SINGLE_QDRANT_URL` are configured, rather than erroring.
    MemorySearchSemantic {
        query: String,
        scope: Option<MemoryScope>,
        project: Option<String>,
        limit: u64,
    },
    MemoryGet {
        id: i64,
    },
    MemoryDelete {
        id: i64,
    },
    MemoryList {
        scope: Option<MemoryScope>,
    },
    /// Leaves a note for another agent (or, with `to_agent: None`, any
    /// agent) working the same project — a minimal inbox, not a live
    /// stream: the recipient picks it up the next time it runs a task in
    /// that project (see `single-runtime::task::run`'s prompt preamble).
    NoteLeave {
        project: Option<String>,
        from_agent: String,
        to_agent: Option<String>,
        topic: String,
        content: String,
    },
    /// `to_agent` also matches notes left with `to_agent: None` (broadcast
    /// to the project). `unread_only` additionally filters to `read_at IS
    /// NULL` without marking anything read — see `NoteMarkRead`.
    NoteInbox {
        project: Option<String>,
        to_agent: String,
        unread_only: bool,
    },
    NoteMarkRead {
        id: i64,
    },
    /// Extracts text from a PDF/image/plain-text file (OCR fallback for
    /// scanned PDFs) and stores it as a searchable memory entry — see
    /// `single-runtime::documents`.
    DocumentIngest {
        path: String,
        project: Option<String>,
        title: Option<String>,
    },
    DocumentList {
        project: Option<String>,
    },
    DocumentGet {
        id: i64,
    },
    ContextShow {
        cwd: String,
    },
    TaskRun {
        description: String,
        agent: String,
        cwd: String,
        use_worktree: bool,
        account: Option<String>,
        /// Skips the usual SingleCLI-managed isolated $HOME
        /// (`single_core::agent_home`) and runs the agent against the
        /// real, ambient $HOME instead — for tasks that need to actually
        /// touch the real system (dotfiles, installed packages, desktop
        /// config), not a sandboxed copy. Off by default: this gives the
        /// agent full access to your real credentials and files, an
        /// explicit choice, not the default posture.
        real_home: bool,
        /// Skips injecting a relevant-memory + unread-notes preamble
        /// ahead of the prompt (on by default — see `task::run`'s
        /// context-injection step). Off by default: memory context helps
        /// more often than it costs, but this stays available for prompts
        /// that need to be sent exactly as given.
        no_memory_context: bool,
        timeout_secs: u64,
        /// When true, the daemon creates the task row, starts it on its
        /// own thread, and responds immediately with the initial
        /// (`Running`) record instead of blocking the connection until
        /// the agent finishes — poll `TaskInspect`/`TaskList` for
        /// progress, `TaskCancel` to stop it early. Off by default so
        /// existing one-shot `single task run` callers keep today's
        /// blocking behavior unchanged.
        background: bool,
        /// Opt-in (default off): when this run fails or times out in a way
        /// that looks like a rate limit — see `single_core::ratelimit` —
        /// and a fallback chain is configured for `agent`/`account` (see
        /// `Request::FallbackSet`), automatically marks that account
        /// `rate_limited` and creates a linked follow-up task against the
        /// chain's next entry, visible as its own task row rather than a
        /// silent retry. Off by default: failing over to a different
        /// agent/account is a real behavior change a user should choose,
        /// not something that happens under a plain `task run`.
        #[serde(default)]
        allow_fallback: bool,
        /// Opt-in (default off): run the agent in a structured-output mode
        /// that reports real token usage where one exists (currently only
        /// `claude --output-format json`). Otherwise a no-op hint — the
        /// task's token counts are parse-or-estimated. See
        /// `AgentAdapter::run_prompt_json`.
        #[serde(default)]
        usage_json: bool,
    },
    TaskList,
    TaskInspect {
        id: i64,
    },
    /// Every distinct workspace (project) a task has ever run against — the
    /// grouping the TUI's Tasks tab drills through, since `TaskList` alone
    /// dumps every task ever run with nothing distinguishing which project
    /// each belongs to. See `single_core::project_context::stable_workspace_id`
    /// for how a workspace's identity survives the project directory moving.
    WorkspaceList,
    /// Stops a `Running` task started with `background: true` (or an
    /// in-flight `OrchestrateParallel{ background: true, .. }` sub-task):
    /// sets its cancel flag, which the agent subprocess's own poll loop
    /// notices and acts on the same way it already does a timeout — see
    /// `single-agent-sdk::run::run_command_live`. A no-op success on a
    /// task that isn't currently running (already finished, or was never
    /// started in the background).
    TaskCancel {
        id: i64,
        /// Mark a row stuck non-terminal with no live process behind it
        /// (e.g. a daemon-crash zombie the startup sweep missed) `Failed`
        /// directly, instead of the "isn't currently running" error.
        #[serde(default)]
        force: bool,
    },
    /// Removes a finished (`Completed`/`Failed`/`Cancelled`) task's git
    /// worktree and any leftover live-output file — SingleCLI never does
    /// this automatically, since a worktree might still be worth
    /// inspecting after the fact. Errors if the task is still `Running`.
    TaskCleanup {
        id: i64,
        /// Clean up a still-`Running`/`Created` row anyway, marking it
        /// `Failed` first so it doesn't linger as a phantom "running".
        #[serde(default)]
        force: bool,
    },
    /// Shows the diff a worktree-isolated task's branch would bring in if
    /// merged — never merges. See `single_core::worktree::diff`.
    WorktreeMergePreview {
        task_id: i64,
    },
    /// Merges a worktree-isolated task's branch into the repo it ran
    /// against. Deliberately a separate request from
    /// `WorktreeMergePreview` — a caller must have looked at the diff
    /// first (`docs/architecture.md`: "branches are never auto-merged;
    /// that stays a human decision").
    WorktreeMergeApply {
        task_id: i64,
    },
    Orchestrate {
        goal: String,
        agents: Vec<String>,
        cwd: String,
        use_worktree: bool,
        real_home: bool,
        timeout_secs: u64,
    },
    /// Real concurrent execution (v0.1.17), as opposed to `Orchestrate`'s
    /// sequential relay: each `ParallelTaskSpec` runs on its own thread, in
    /// its own git worktree, with its own SQLite connection. There's no
    /// automatic goal decomposition here — the caller supplies each
    /// agent's own description explicitly (SingleCLI runs them, it doesn't
    /// invent the split).
    OrchestrateParallel {
        tasks: Vec<ParallelTaskSpec>,
        cwd: String,
        real_home: bool,
        timeout_secs: u64,
        /// Same meaning as `TaskRun::background`: the whole batch runs on
        /// its own thread (which still internally fans each sub-task out
        /// to its own thread, unchanged) and the daemon responds with the
        /// initial records immediately instead of blocking until every
        /// sub-task finishes.
        background: bool,
        #[serde(default)]
        orchestrator: OrchestratorMode,
        #[serde(default)]
        goal: Option<String>,
        #[serde(default)]
        candidate_agents: Vec<String>,
    },
    /// Executes an explicit dependency graph. Nodes become eligible together
    /// once their dependencies have terminal records, then run in isolated
    /// worktrees just like `OrchestrateParallel`.
    OrchestrateGraph {
        nodes: Vec<TaskGraphNode>,
        cwd: String,
        real_home: bool,
        timeout_secs: u64,
        background: bool,
        #[serde(default)]
        orchestrator: OrchestratorMode,
        #[serde(default)]
        goal: Option<String>,
        #[serde(default)]
        candidate_agents: Vec<String>,
    },
    AccountCapture {
        agent: String,
        name: String,
        label: Option<String>,
    },
    AccountUse {
        agent: String,
        name: String,
    },
    AccountList {
        agent: Option<String>,
    },
    AccountRemove {
        agent: String,
        name: String,
    },
    AccountSetStatus {
        agent: String,
        name: String,
        status: AccountStatus,
    },
    /// Opt-in Docker execution backend (see `single_core::docker`) —
    /// `account: None` means the agent-wide setting, `Some` overrides it
    /// for one captured account. Takes effect on the next `single task
    /// run`/orchestrate step for that agent/account, not retroactively.
    DockerEnable {
        agent: String,
        account: Option<String>,
    },
    DockerDisable {
        agent: String,
        account: Option<String>,
    },
    /// `agent: None` lists every configured agent/account pair.
    DockerStatus {
        agent: Option<String>,
    },
    DockerStop {
        agent: String,
        account: Option<String>,
    },
    /// Pending human decisions created by `single_core::preferences::evaluate_and_learn`
    /// — raised by the single-mcp gateway's `invoke_mcp` and, when enabled,
    /// an agent's own mid-run permission hook (see `HooksEnable`). See
    /// `single_core::preferences`.
    ApprovalList,
    /// `remember: true` also records this as a learned preference for the
    /// same resource pattern, so it doesn't ask again next time.
    ApprovalResolve {
        id: i64,
        allow: bool,
        remember: bool,
    },
    PreferenceList,
    /// Opt-in per-agent mid-run permission interception (see
    /// `single_core::hooks`) — an agent's own process pauses mid-task to
    /// ask before using a tool, gated the same way as `single-mcp`'s
    /// `invoke_mcp`. Only `claude` is wired up; other agents error.
    /// Bootstraps the isolated home and writes the hook into its
    /// settings.json immediately, so it takes effect on the next run.
    HooksEnable {
        agent: String,
    },
    HooksDisable {
        agent: String,
    },
    HooksStatus,
    ProviderAdd {
        name: String,
        env_var_name: String,
        base_url: Option<String>,
        models: Vec<ModelSpec>,
    },
    ProviderAddPreset {
        name: String,
    },
    ProviderPresetList,
    ProviderRemove {
        name: String,
    },
    ProviderList,
    /// Same shape as `ProviderList`, filtered to providers that actually
    /// have a key stored (shared `set-key` or any labeled `add-key`) —
    /// `providers.toml` itself carries every built-in preset unconditionally
    /// (see `single_core::providers::sync_missing_presets`; unlike MCP/LSP/
    /// Tools, `ProviderSpec` has no `enabled` field), so plain `ProviderList`
    /// can't answer "which of these did I actually configure."
    ConfiguredProviderList,
    ProviderInspect {
        name: String,
    },
    ProviderSetKey {
        name: String,
        value: String,
    },
    ProviderSync {
        name: String,
        agents: Vec<String>,
        dry_run: bool,
        real_home: bool,
    },
    /// Stores one *labeled* key for a provider (see `ProviderKeySpec`),
    /// distinct from `ProviderSetKey`'s single shared key.
    ProviderAddKey {
        provider: String,
        label: String,
        agent: Option<String>,
        value: String,
    },
    ProviderListKeys {
        provider: String,
    },
    ProviderRemoveKey {
        provider: String,
        label: String,
    },
    /// Same as `ProviderSync` but syncs one specific labeled key (not the
    /// shared `providers.toml` one) into one specific agent.
    ProviderKeySync {
        provider: String,
        label: String,
        agent: String,
        dry_run: bool,
    },
    /// The org/admin-scoped key used only to *query* a provider's usage
    /// API — separate from any inference key in `ProviderSpec`/
    /// `ProviderKeySpec`, since billing endpoints typically need a
    /// different credential scope than making model calls.
    ProviderSetBillingKey {
        provider: String,
        value: String,
    },
    BillingProviderList,
    /// E28 §5.3 — the vendored ~40-provider free-LLM catalog
    /// (`single_core::free_pool::FREE_PROVIDERS`), not `providers.toml`.
    ProviderListFree,
    /// Register one free-pool provider's key and validate it (best-effort
    /// `quirks.validate_url` probe — a failed probe doesn't error the
    /// command). The key value is prompted for client-side (hidden input)
    /// when not passed, so it never crosses the wire unencrypted longer
    /// than necessary.
    ProviderAddFree {
        id: String,
        key: String,
        /// Distinguishes multiple keys for the same platform (e.g. keys
        /// from separate accounts, added to grow the free-pool's real
        /// capacity). Omit to auto-generate a fresh one — the very first
        /// key added for a platform still becomes `"default"` for
        /// backward compatibility, every one after that becomes
        /// `key2`, `key3`, etc. Passing an *existing* key_id explicitly
        /// intentionally overwrites that one key (key rotation).
        #[serde(default)]
        key_id: Option<String>,
    },
    /// Reconciles the vendored catalog into `providers.toml` as
    /// `single-<id>` presets and into `free-pool.toml`'s per-provider
    /// `enabled`/`disabled_reason` state. Idempotent.
    ProviderSyncPool,
    /// Per free-pool provider: keyed?, last validation, disabled reason
    /// (region-walled/`sail`), current cooldown state, and remaining RPD
    /// headroom (when the provider publishes an RPD limit).
    ProviderKeyStatus {
        platform: Option<String>,
    },
    /// Re-runs the same best-effort `quirks.validate_url` probe
    /// `ProviderAddFree` does at registration time, against every
    /// already-keyed key for `platform` (or every platform's keys, if
    /// `platform` is `None`) — the only other way a key's
    /// `valid`/`last_validated_at` fields ever update is from a real
    /// `single-pool` task outcome. For a provider with no `validate_url`
    /// quirk, its keys are skipped (nothing to probe) rather than errored.
    ProviderValidateKeys {
        platform: Option<String>,
    },
    /// `single pool status` — every currently-benched `(platform, model,
    /// key_id)` plus a healthy-ratio snapshot (spec §6.5). The snapshot
    /// is stateless (no persisted entry/exit-grace hysteresis this
    /// iteration — see `PoolStatusInfo`'s doc comment).
    PoolStatus,
    UsageShow {
        provider: Option<String>,
    },
    UsageRefresh,
    /// Query token/cost accounting data (propose-only; no business
    /// data is stored or exposed). Filter by execution_id, trace_id,
    /// agent, or provider.
    AccountingQuery {
        query: AccountingQuery,
    },
    KgCreateEntity {
        name: String,
        entity_type: String,
    },
    KgAddObservation {
        entity: String,
        content: String,
    },
    KgCreateRelation {
        from: String,
        to: String,
        relation_type: String,
    },
    KgDeleteEntity {
        name: String,
    },
    KgGetEntity {
        name: String,
    },
    KgQuery {
        term: String,
    },
    KgReadGraph,
    CacheSet {
        key: String,
        value: String,
        ttl_secs: Option<u64>,
    },
    CacheGet {
        key: String,
    },
    CacheDelete {
        key: String,
    },
    CacheList {
        pattern: String,
    },
    CacheStatus,
    VectorUpsert {
        collection: String,
        id: u64,
        vector: Vec<f32>,
        payload: serde_json::Value,
    },
    VectorSearch {
        collection: String,
        vector: Vec<f32>,
        limit: u64,
    },
    VectorDelete {
        collection: String,
        id: u64,
    },
    VectorStatus,
    AgentInstall {
        name: String,
        dry_run: bool,
    },
    Setup {
        dry_run: bool,
    },
    InstallIntegrations {
        dry_run: bool,
        real_home: bool,
    },
    UninstallIntegrations {
        real_home: bool,
    },
    ProfileList,
    ProfileUse {
        name: String,
    },
    PluginAdd {
        plugin: PluginSpec,
    },
    PluginRemove {
        name: String,
    },
    PluginList,
    PluginInspect {
        name: String,
    },
    PluginSync {
        name: String,
        agents: Vec<String>,
        dry_run: bool,
        real_home: bool,
    },
    /// Saves one ordered fallback chain, replacing any existing chain that
    /// starts with the same first entry (so `fallback set` is idempotent —
    /// re-running it with a new tail updates the chain rather than
    /// accumulating duplicates). See `single_core::fallback`.
    FallbackSet {
        chain: Vec<AgentAccountRef>,
    },
    FallbackList,
    /// Removes the chain whose first entry matches `first`.
    FallbackRemove {
        first: AgentAccountRef,
    },
    /// Adds one task-lifecycle event hook — see `single_core::task_hooks`.
    TaskHookAdd {
        on: Vec<String>,
        command: String,
        agent: Option<String>,
        workspace: Option<String>,
    },
    TaskHookList,
    /// Removes every hook whose `command` matches exactly.
    TaskHookRemove {
        command: String,
    },
    /// Fires every configured hook against a synthetic payload, ignoring
    /// `on`/`agent`/`workspace` filters — lets a user confirm a hook
    /// command actually works before relying on it live.
    TaskHookTest {
        command: String,
    },
    PluginPresetList,
    PluginAddPreset {
        name: String,
    },

    // ---- coordinator (spec E27.02 §6) ---------------------------------
    /// Opens a conversation thread. One per Zed panel thread.
    SessionNew {
        cwd: String,
    },
    SessionList,
    SessionClose {
        session_id: String,
    },
    /// Submits a goal into a session; the coordinator plans and drives it.
    GoalSubmit {
        session_id: String,
        text: String,
        #[serde(default)]
        mode: Option<String>,
        #[serde(default)]
        max_dispatches: Option<u32>,
        #[serde(default)]
        max_minutes: Option<u32>,
        /// Pin every node to this agent instead of routing. `careful`
        /// (`single loop`) uses it for the single iterating node; `auto`
        /// applies it to every planned node after decomposition.
        #[serde(default)]
        agent: Option<String>,
    },
    GoalStatus {
        goal_id: String,
    },
    GoalList {
        #[serde(default)]
        session_id: Option<String>,
    },
    /// Adds context / raises the budget (`budget=N`) / opts into merge
    /// confirmation (`auto-merge=true|false` — see
    /// `single_core::pending_merge`, `single goal merge`) / answers a
    /// blocked question, then re-ticks.
    GoalAmend {
        goal_id: String,
        text: String,
    },
    GoalCancel {
        goal_id: String,
    },
    /// E28 spec §10 (Part F): manual re-tick of a `blocked`/`failed`/
    /// `paused`/`waiting_on_capacity` goal a human judges recoverable —
    /// the same path `coordinator::resume_interrupted` runs automatically
    /// on daemon start.
    GoalResume {
        goal_id: String,
    },
    /// Live-verification finding 2026-09-17 (E30 dispatch): neither
    /// `GoalAmend` nor `GoalResume` clears a supervisor-set `blocked`
    /// *node* (as opposed to a `blocked` *goal*) — a blocked node sits
    /// outside the ready-set forever with no self-service unblock path.
    /// Resets one named node back to `pending` (status, task_id,
    /// attempts, retry stamp) and re-ticks, the same recovery a human
    /// judging it fixable/retriable would want. Does not touch sibling
    /// nodes or the goal's own status.
    GoalRetryNode {
        goal_id: String,
        node_id: String,
    },
    /// Poll (the messenger long-polls) for a session's events after an id.
    SessionEvents {
        session_id: String,
        since_event_id: i64,
    },
    /// Cross-thread snapshot: running/queued goals + pool capacity.
    CoordinatorStatus,
    /// Pending merge confirmations from the coordinator's opt-in
    /// auto-merge (`goal.auto_merge`) — see `single_core::pending_merge`.
    /// "Branches are never auto-merged; that stays a human decision"
    /// (`docs/architecture.md`): a review passing only queues one of
    /// these, it never merges by itself.
    GoalMergeList,
    /// The real diff (`single_core::worktree::diff`) a pending merge
    /// would land, computed fresh rather than cached from request time.
    GoalMergeShow {
        id: i64,
    },
    /// `allow: true` calls `single_core::worktree::merge` after marking
    /// the record confirmed; `allow: false` marks it rejected and never
    /// touches the repo.
    GoalMergeResolve {
        id: i64,
        allow: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { data: ResponseData },
    Error { message: String },
}

// Adjacently tagged (not internally tagged): a couple of variants below
// wrap a `Vec<_>`, and serde can't internally-tag a variant whose payload
// serializes to a JSON array rather than an object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ResponseData {
    Status(RuntimeStatus),
    Doctor(DoctorReport),
    Agents(Vec<AgentInfo>),
    Agent(AgentInfo),
    McpServers(Vec<McpServerInfo>),
    McpServer(McpServerSpec),
    McpPresets(Vec<McpPresetInfo>),
    McpGatewayMode(bool),
    LspServers(Vec<LspServerSpec>),
    LspServer(LspServerSpec),
    LspPresets(Vec<LspPresetInfo>),
    Tools(Vec<ToolSpec>),
    Tool(ToolSpec),
    SecretNames(Vec<String>),
    SecretValue(Option<String>),
    Skills(Vec<String>),
    SkillStarters(Vec<SkillStarterInfo>),
    SkillContents(Vec<String>),
    SkillSynced {
        path: String,
    },
    MemoryId(i64),
    MemoryEntry(MemoryEntry),
    MemoryEntries(Vec<MemoryEntry>),
    NoteId(i64),
    Notes(Vec<AgentNote>),
    Document(DocumentInfo),
    Documents(Vec<DocumentInfo>),
    Context(ProjectContext),
    Task(TaskRecord),
    AccountProfile(AccountProfileInfo),
    AccountProfiles(Vec<AccountProfileInfo>),
    AccountSwitched(AccountSwitchResult),
    DockerContainerInfo(DockerContainerInfo),
    DockerContainerList(Vec<DockerContainerInfo>),
    Approvals(Vec<ApprovalInfo>),
    PendingMerges(Vec<PendingMergeInfo>),
    /// `GoalMergeShow`'s result: the record plus the real diff.
    PendingMergeDiff(PendingMergeInfo, String),
    /// `(agent, enabled)` pairs — see `single_core::hooks::status`.
    HooksStatus(Vec<(String, bool)>),
    Preferences(Vec<PreferenceInfo>),
    Provider(ProviderSpec),
    Providers(Vec<ProviderSpec>),
    ProviderPresets(Vec<ProviderPresetInfo>),
    ProviderSyncResults(Vec<ProviderSyncResult>),
    ProviderKeys(Vec<ProviderKeySpec>),
    BillingProviders(Vec<BillingProviderInfo>),
    FreeProviders(Vec<FreeProviderInfo>),
    PoolSyncResult {
        synced: usize,
    },
    PoolKeyStatuses(Vec<PoolKeyStatusInfo>),
    PoolStatus(PoolStatusInfo),
    Usage(UsageSummary),
    /// Queryable accounting data: usage events and their token
    /// breakdown, with totals.
    Accounting(AccountingResult),
    KgEntityId(i64),
    KgEntity(KgEntity),
    KgEntities(Vec<KgEntity>),
    KgGraph(KnowledgeGraphSnapshot),
    CacheValue(Option<String>),
    CacheKeys(Vec<String>),
    CacheStatus {
        configured: bool,
        url: Option<String>,
        reachable: bool,
    },
    VectorHits(Vec<VectorHit>),
    VectorStatus {
        configured: bool,
        url: Option<String>,
        reachable: bool,
    },
    Tasks(Vec<TaskRecord>),
    Workspaces(Vec<WorkspaceInfo>),
    OrchestrateResult(Vec<TaskRecord>),
    OrchestrateGraphResult(Vec<TaskRecord>),
    AgentInstallResult(SetupAction),
    SetupPlan(SetupPlan),
    IntegrationResult(IntegrationResult),
    Profiles(Vec<String>),
    Plugin(PluginSpec),
    Plugins(Vec<PluginSpec>),
    PluginPresets(Vec<PluginPresetInfo>),
    FallbackChains(Vec<Vec<AgentAccountRef>>),
    TaskHooks(Vec<TaskHookRule>),
    /// How many hooks a `TaskHookRemove` actually matched (0 means "no
    /// such hook").
    TaskHookRemoved(usize),
    PluginSyncResults(Vec<PluginInstallResult>),
    WorktreeDiff(String),
    WorktreeMerged(WorktreeMergeResult),

    // ---- coordinator (spec E27.02 §6) --------------------------------
    Session(SessionInfo),
    Sessions(Vec<SessionInfo>),
    GoalId(String),
    GoalView(GoalView),
    Goals(Vec<GoalSummary>),
    CoordinatorEvents(Vec<CoordinatorEvent>),
    CoordinatorSnapshot(CoordinatorSnapshot),

    Empty,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub cwd: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalSummary {
    pub id: String,
    pub session_id: String,
    pub text: String,
    pub mode: String,
    pub status: String,
    pub dispatches: u32,
    pub max_dispatches: u32,
    pub created_at: String,
    /// E28 spec §8: set only when `status == "waiting_on_capacity"` —
    /// which providers/pools are spent, for the `coordinator status` line.
    #[serde(default)]
    pub capacity_reason: Option<String>,
    /// E28 spec §8: RFC3339 ETA for when the earliest-recovering
    /// candidate's cooldown lifts, set alongside `capacity_reason`.
    #[serde(default)]
    pub capacity_eta: Option<String>,
    /// Set when `status == "blocked"` (a hold distinct from
    /// `waiting_on_capacity`, e.g. a dispatch/time cap or a node that
    /// can't proceed) -- was previously only surfaced on the single-goal
    /// `GoalView`, so `goal list` had no way to say why a blocked goal
    /// was stuck without a separate `goal status` call per id.
    #[serde(default)]
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeView {
    pub id: String,
    pub desc: String,
    pub kind: String,
    pub effort: String,
    pub agent: String,
    pub depends_on: Vec<String>,
    pub status: String,
    pub task_id: Option<i64>,
    pub attempts: u32,
    /// Token counts for this node's current task run (E27.03). `None`
    /// until the node has run; `tokens_estimated` true when they are a
    /// parse-or-estimate rather than an agent-reported figure.
    #[serde(default)]
    pub prompt_tokens: Option<i64>,
    #[serde(default)]
    pub completion_tokens: Option<i64>,
    #[serde(default)]
    pub tokens_estimated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorEvent {
    pub id: i64,
    pub goal_id: Option<String>,
    pub ts: String,
    pub kind: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalView {
    pub goal: GoalSummary,
    pub blocked_reason: Option<String>,
    pub result_summary: Option<String>,
    pub nodes: Vec<NodeView>,
    pub recent_events: Vec<CoordinatorEvent>,
    /// Sum of every node's task-run token counts (E27.03).
    #[serde(default)]
    pub total_prompt_tokens: i64,
    #[serde(default)]
    pub total_completion_tokens: i64,
    /// True if any contributing count was a parse-or-estimate.
    #[serde(default)]
    pub any_tokens_estimated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolAgentStatus {
    pub agent: String,
    pub running: usize,
    pub cap: Option<usize>,
    pub rate_limited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorSnapshot {
    pub running_goals: Vec<GoalSummary>,
    pub queued_goals: Vec<GoalSummary>,
    pub blocked_goals: Vec<GoalSummary>,
    /// E28 spec §8: goals in `waiting_on_capacity` — every routable
    /// candidate exhausted/benched, holding until a stamped retry time.
    #[serde(default)]
    pub waiting_goals: Vec<GoalSummary>,
    pub pool: Vec<PoolAgentStatus>,
    pub max_parallel: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub version: String,
    pub active_profile: String,
    pub agents_known: usize,
    pub agents_detected: usize,
    pub socket_path: String,
    pub db_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub adapter: String,
    pub command: String,
    pub detected: bool,
    pub version: Option<String>,
    pub install_method: InstallMethod,
    pub bootstrap_install: Option<BootstrapInstall>,
    pub unverified: bool,
    /// See `HomeRequirement`'s doc comment.
    #[serde(default)]
    pub home_requirement: HomeRequirement,
    #[serde(default)]
    pub max_concurrency: Option<u32>,
    pub capabilities: CapabilityFlags,
    pub config_paths: Vec<String>,
    /// Free-text caveat surfaced in `doctor`/`agent inspect`, e.g. when an
    /// entry doesn't fit the coding-agent model cleanly (see `perplexity`).
    pub notes: Option<String>,
    /// Auto-detected presence of *some* live login for this agent, checked
    /// across both SingleCLI's isolated home and the real ambient home. See
    /// `AuthState` docs for how this differs from `AccountProfileInfo::status`.
    #[serde(default)]
    pub authenticated: AuthState,
}

/// Whether *some* live login is currently present for an agent, auto-
/// detected by checking for the agent's credential file(s) — no notion of
/// *which* account, just "is anything logged in right now". Distinct from
/// `AccountProfileInfo::status` (`AccountStatus`), which is a manually-set
/// usability flag on one *named, captured* profile and is never auto-
/// detected. `authenticated` answers "can I run this agent at all";
/// `status` answers "is this particular saved account currently usable".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    #[default]
    NotAuthenticated,
    Authenticated,
    /// Account-switching/credential-detection isn't implemented for this
    /// agent (e.g. opencode, perplexity) — see `single-core::account`'s
    /// module docs for why.
    Unsupported,
}

/// Describes how an agent CLI is (or would be) installed. Distinct from
/// `BootstrapInstall`, which is the exact command `single setup` runs when
/// the agent is missing — this is just descriptive, for `doctor` output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallMethod {
    Native { detail: String },
    StandaloneBinary { detail: String },
    PackageManager { detail: String },
    Unsupported { reason: String },
}

/// The real, vendor-verified install command `single setup` runs when this
/// agent isn't detected. `source` is the documentation URL it was verified
/// against — kept alongside the command so the registry stays auditable
/// instead of hiding a bare `curl | sh` in code. `None` means no verified
/// install method exists (see `InstallMethod::Unsupported` for why).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapInstall {
    pub command: String,
    pub source: String,
}

/// Whether an agent needs `real_home: true` to authenticate, breaks under
/// it, works either way, or nobody's confirmed it empirically yet.
/// `Unverified` is the honest default — see `single_core::ratelimit`'s
/// module doc for why this project only claims what it's directly
/// checked. See `docs/superpowers/specs/2026-08-26-orchestration-lessons-design.md`
/// for how the known values below were established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeRequirement {
    /// Breaks (fails to authenticate correctly) under `real_home: true`.
    IsolatedOnly,
    /// `real_home: true` is the only way this agent ever authenticates —
    /// either structurally (its isolated home can never hold real
    /// credentials by design) or empirically confirmed.
    RealRequired,
    /// Authenticates correctly either way.
    Either,
    /// Nobody has empirically confirmed either way yet.
    #[default]
    Unverified,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct CapabilityFlags {
    pub streaming: bool,
    pub mcp: bool,
    pub lsp: bool,
    pub tools: bool,
    pub sessions: bool,
    pub structured_output: bool,
    /// False only for an adapter whose `run_prompt` falls through to
    /// `single_agent_sdk::adapter`'s default-trait `bail!` (no
    /// non-interactive mode exists at all, e.g. codebuff) — lets a caller
    /// check this before dispatching instead of discovering it via a
    /// failed `task_run` call. True for every adapter that overrides
    /// `run_prompt`.
    #[serde(default = "default_true")]
    pub non_interactive_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub command: String,
    pub enabled: bool,
    pub synced_to: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupPlan {
    pub actions: Vec<SetupAction>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupAction {
    pub agent: String,
    pub action: SetupActionKind,
    pub detail: String,
    pub executed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupActionKind {
    AlreadyInstalled,
    Install,
    Unsupported,
    ConfigureIntegration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationResult {
    pub dry_run: bool,
    pub writes: Vec<IntegrationWrite>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationWrite {
    pub agent: String,
    pub config_path: String,
    pub backup_path: Option<String>,
    pub applied: bool,
    pub detail: String,
}

/// Envelope helper so a future event stream (Phase 4) can share the same
/// framing without breaking the request/response wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub id: u64,
    pub payload: T,
}

pub type Metadata = BTreeMap<String, String>;

/// A format-agnostic MCP server entry from SingleCLI's unified registry.
/// Lives here (rather than in `single-agent-sdk`, which consumes it) so
/// both `single-core`'s config/registry loading and `single-agent-sdk`'s
/// per-format writers can share one definition without a circular
/// dependency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerSpec {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Env var name -> secret-store key (see `single_core::secrets`), for
    /// values that must never sit in plain text in `mcp.toml` the way
    /// `env` does — an API token for Cloudflare/Postman, for example.
    /// Resolved at spawn time, not stored raw: the `single-mcp` gateway
    /// (crates/single-mcp) reads each key from the OS keychain and sets it
    /// as a real env var on the child process it spawns, so the value
    /// never touches disk anywhere. This resolution currently only
    /// happens in the gateway path — direct native-config sync
    /// (`single install-integrations` without gateway mode) still writes
    /// whatever's in `env` verbatim into each agent's own config file,
    /// same as it always has (matching `provider_sync.rs`'s existing
    /// precedent for provider API keys); a secret-backed server synced
    /// that way needs its value put in `env` directly, same as before.
    #[serde(default)]
    pub secret_env: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// A format-agnostic LSP server entry, mirroring `McpServerSpec`'s shape
/// and reasons for living here (shared by `single-core`'s registry and any
/// future agent-sdk writer without a circular dependency).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LspServerSpec {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub risk_level: RiskLevel,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Memory scope (spec section 9). Lives here so both `single-runtime`'s
/// SQLite-backed store and `single-cli`'s request-building code share one
/// definition without a circular dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Working,
    Project,
    User,
    Agent,
    Task,
    LongTerm,
    Knowledge,
}

/// Provenance classification (spec sections 46-47): the *claimed* source of
/// a memory entry, as given by the caller — SingleCLI does not itself
/// verify or upgrade this classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    UserInstruction,
    AgentOutput,
    ToolOutput,
    ProjectContent,
    ExternalContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: i64,
    pub scope: MemoryScope,
    pub source: MemorySource,
    pub project: Option<String>,
    pub agent: Option<String>,
    pub task: Option<String>,
    pub title: String,
    pub content: String,
    pub confidence: f64,
    pub created_at: String,
    pub expires_at: Option<String>,
}

/// A note one agent leaves for another (or for whoever picks up the
/// project next) — a minimal inbox, not a live event stream. See
/// `Request::NoteLeave`/`NoteInbox`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentNote {
    pub id: i64,
    pub project: Option<String>,
    pub from_agent: String,
    /// `None` = left for any agent working this project, not one in particular.
    pub to_agent: Option<String>,
    pub topic: String,
    pub content: String,
    pub created_at: String,
    pub read_at: Option<String>,
}

/// An ingested document — see `single-runtime::documents`. The extracted
/// text itself lives in the shared memory store (`memory_id` points at
/// it); this only tracks the original file and OCR provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentInfo {
    pub id: i64,
    pub title: String,
    pub project: Option<String>,
    pub source_path: String,
    pub extracted_chars: i64,
    pub memory_id: i64,
    pub ingested_at: String,
}

/// One agent/account's Docker execution setting plus (when known) its
/// live container state — see `single_core::docker` (settings) and
/// `single-runtime::docker` (lifecycle).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerContainerInfo {
    pub agent: String,
    pub account: Option<String>,
    pub container_name: String,
    pub enabled: bool,
    /// `None` when the container doesn't exist yet (e.g. never started,
    /// or `enabled` but no task has run since) — distinct from `Some(false)`,
    /// which means it exists but is stopped.
    pub running: Option<bool>,
}

/// A pending or resolved human decision — see `single_core::preferences`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalInfo {
    pub id: i64,
    pub resource: String,
    pub context: Option<String>,
    /// `"pending"` / `"allowed"` / `"denied"`.
    pub status: String,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

/// A merge awaiting (or given) human confirmation — see
/// `single_core::pending_merge`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingMergeInfo {
    pub id: i64,
    pub goal_id: String,
    pub review_node_id: String,
    pub dep_node_id: String,
    pub branch: String,
    /// `"pending"` / `"confirmed"` / `"rejected"`.
    pub status: String,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

/// A learned decision for a resource pattern — see `single_core::preferences`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreferenceInfo {
    pub id: i64,
    pub pattern: String,
    /// `"deny"` / `"ask"` / `"allow"`.
    pub decision: String,
    pub confidence: f64,
    pub learned_from: Option<String>,
    pub created_at: String,
}

/// Task lifecycle status (spec section 17's TaskCreated/TaskStarted/
/// TaskCompleted/TaskFailed events, collapsed into a single current-state
/// field — Phase 4 doesn't yet persist the full event sequence as
/// separately queryable rows beyond the generic runtime event log).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Created,
    Running,
    Completed,
    Failed,
    /// Either killed by an explicit `single task cancel` before it finished,
    /// or deliberately skipped by a graph `run_if` condition. The latter
    /// always says so in its summary; `Failed` means an agent actually ran
    /// unsuccessfully (or errored/timed out).
    Cancelled,
}

/// One agent's explicit sub-task within a parallel orchestrate batch —
/// see `Request::OrchestrateParallel`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelTaskSpec {
    pub agent: String,
    pub description: String,
}

/// One named step in a dependency graph. Stable IDs make dependencies
/// inspectable instead of coupling graph wiring to a display description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskGraphNode {
    pub id: String,
    pub agent: String,
    pub description: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub run_if: RunCondition,
}

/// Decides whether a graph node starts after all of its dependencies have
/// reached a terminal state. A skipped conditional node is recorded as
/// `Cancelled`, with a summary that distinguishes it from user cancellation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunCondition {
    #[default]
    Always,
    OnSuccess,
    OnFailure,
}

/// Selects whether a caller supplies a fixed split or asks an installed agent
/// CLI to produce one. `Auto` and `Delegate` intentionally share execution:
/// both avoid direct provider APIs; the distinction is only who chose to ask
/// for planning rather than an accidentally different planning algorithm.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorMode {
    #[default]
    Fixed,
    Auto,
    Delegate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: i64,
    pub description: String,
    pub agent: String,
    pub status: TaskStatus,
    pub worktree_path: Option<String>,
    pub artifact_path: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub summary: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// The directory this task actually ran against, as given to `task
    /// run`/orchestrate — not necessarily the same as the workspace's
    /// current path (see `workspace_id`), since a task might run from a
    /// subdirectory of the project.
    #[serde(default)]
    pub cwd: String,
    /// The stable identity of the workspace (project) this task ran
    /// against — see `single_core::project_context::stable_workspace_id`.
    /// Never the raw `cwd`, so grouping tasks by workspace survives the
    /// project directory being moved. Empty for tasks recorded before this
    /// field existed and never backfilled.
    #[serde(default)]
    pub workspace_id: String,
    /// True if `single_core::ratelimit::looks_like_rate_limit` matched
    /// this task's captured output. Checked unconditionally on every
    /// non-zero-exit completion, regardless of `allow_fallback` — see
    /// `task::execute`.
    #[serde(default)]
    pub rate_limited: bool,
    /// Prompt / completion token counts for this run (E27.03). `None` on a
    /// task that produced no output. `tokens_estimated` is true when they
    /// are a parse-of-output or a chars/4 fallback rather than a real
    /// count reported by the agent (only `claude --output-format json`
    /// reports real ones today).
    #[serde(default)]
    pub prompt_tokens: Option<i64>,
    #[serde(default)]
    pub completion_tokens: Option<i64>,
    #[serde(default)]
    pub tokens_estimated: bool,
}

/// One workspace (project) that at least one task has run against — the
/// grouping shown by the TUI's Tasks tab before drilling into that
/// workspace's own task list. `path` is *last-known*, not fixed: it's
/// re-recorded every time a new task runs against this workspace, so it
/// self-heals to wherever the project currently lives after a move — see
/// `single_core::project_context::stable_workspace_id`'s doc comment for
/// why `id` itself doesn't change when that happens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: String,
    pub name: String,
    pub path: String,
    pub task_count: i64,
    pub last_activity_at: String,
}

/// The result of running an agent CLI non-interactively against a single
/// prompt (spec section 39's `send`/lifecycle, scoped down to Phase 4's
/// synchronous one-shot invocation — see `single-agent-sdk::adapter` docs).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunOutcome {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// Killed by an explicit `single task cancel`/`TaskCancel` request
    /// rather than running past its timeout — distinct from `timed_out`
    /// so callers can tell "we gave up on it" from "you told it to stop".
    pub cancelled: bool,
    pub duration_ms: u128,
    /// Real token counts, only ever set when the agent was run in a
    /// structured-output mode that reports them (currently `claude
    /// --output-format json` via `AgentAdapter::run_prompt_json`). `None`
    /// means the caller must fall back to a parse-or-estimate — see
    /// `single-runtime::task::record_token_usage`.
    #[serde(default)]
    pub usage: Option<TokenUsage>,
}

/// Metadata about a captured account-switch profile (spec section 41's
/// "reusable agent definitions" adjacent concept, but scoped to login
/// state rather than full persona config). Never carries token contents —
/// see `single-core::account`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountProfileInfo {
    pub agent: String,
    pub name: String,
    /// Human-readable identity (email or display name) for this captured
    /// login, so multiple accounts per agent are distinguishable at a
    /// glance. Set at capture time; SingleCLI has no way to read it back
    /// out of the agent's own credential files, so it's user-supplied.
    pub label: Option<String>,
    pub captured_at: String,
    pub unverified_complete: bool,
    /// Manually-set usability of this account. There is no verified,
    /// stable API across claude/codex/agy for querying live quota/rate-
    /// limit state, so this is never auto-detected — the user (or a task
    /// failure surfaced elsewhere) sets it, and SingleCLI just remembers
    /// and displays it. See `AuthState` (on `AgentInfo`) for the auto-
    /// detected "is anything logged in" question this does NOT answer.
    #[serde(default)]
    pub status: AccountStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    #[default]
    Unknown,
    Available,
    RateLimited,
    NeedsTopup,
}

impl AccountStatus {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "unknown" => Self::Unknown,
            "available" => Self::Available,
            "rate_limited" | "rate-limited" => Self::RateLimited,
            "needs_topup" | "needs-topup" => Self::NeedsTopup,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Available => "available",
            Self::RateLimited => "rate_limited",
            Self::NeedsTopup => "needs_topup",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSwitchResult {
    pub agent: String,
    pub name: String,
    pub backed_up: Vec<String>,
}

/// A knowledge-graph entity with its accumulated observations (the same
/// entity/observation/relation shape as the widely-used MCP memory-server
/// convention already configured on this project's own reference machine
/// — a real, proven pattern, not invented for this project).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KgEntity {
    pub name: String,
    pub entity_type: String,
    pub observations: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KgRelation {
    pub from_entity: String,
    pub to_entity: String,
    pub relation_type: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeGraphSnapshot {
    pub entities: Vec<KgEntity>,
    pub relations: Vec<KgRelation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorHit {
    pub id: u64,
    pub score: f32,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderPresetInfo {
    pub name: String,
    pub env_var_name: String,
    pub base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPresetInfo {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspPresetInfo {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginPresetInfo {
    pub name: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillStarterInfo {
    pub name: String,
    pub description: String,
}

/// A plugin registered across agents (spec section 29/41). `target` is
/// used verbatim for the three agents that share the real, verified
/// `plugin[@marketplace]` convention (`claude plugin install`, `codex
/// plugin add`, `agy plugin install`); `opencode_module`, if set, is used
/// for OpenCode, whose real plugin command (`opencode plugin <module>`)
/// takes a plain npm module name instead — a genuinely different
/// addressing scheme, not a naming inconsistency this project invented.
/// One entry in a fallback chain (`single_core::fallback`): an agent, and
/// optionally a specific captured account of it (`None` means that
/// agent's default isolated home, not any particular named account).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentAccountRef {
    pub agent: String,
    pub account: Option<String>,
}

/// One task-lifecycle event hook rule — see `single_core::task_hooks`.
/// Lives here (not in `single-core`) for the same reason `AgentAccountRef`
/// does: it crosses the CLI/daemon protocol boundary, so both sides need
/// the same type rather than each defining their own shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskHookRule {
    pub on: Vec<String>,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// One task's outcome, as handed to a firing hook's stdin (JSON, single
/// line) — see `single_core::task_hooks::fire`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskHookPayload {
    pub id: i64,
    pub status: String,
    pub agent: String,
    pub cwd: String,
    pub workspace_id: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginSpec {
    pub name: String,
    pub target: String,
    pub opencode_module: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInstallResult {
    pub plugin: String,
    pub agent: String,
    pub applied: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: String,
    pub name: String,
}

/// A registered LLM provider (spec section 30): OpenAI, Anthropic,
/// OpenCode Zen, a local model server, etc. The actual API key is never
/// stored here — only a reference (`secret_name`) into the OS keychain
/// (`single-core::secrets`), and `env_var_name` says which environment
/// variable name that key needs to become for an agent to pick it up.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderSpec {
    pub name: String,
    pub env_var_name: String,
    pub secret_name: String,
    pub base_url: Option<String>,
    /// Models this provider exposes, declared when the provider is added
    /// via `--model <id>:<name>` (repeatable). Empty for every built-in
    /// preset and for a provider added without `--model`. Only consumed
    /// today by `formats::opencode::apply_provider` — a provider with no
    /// declared models has nothing to add to opencode's picker.
    #[serde(default)]
    pub models: Vec<ModelSpec>,
}

/// One labeled API key for a provider, distinct from `ProviderSpec`'s
/// single shared key (`providers.toml`) — lets the same provider have
/// several real keys, one per agent, so `single usage show` can attribute
/// billing-API spend to a specific agent instead of one undifferentiated
/// provider total. `secret_name` is always `"provider-key:{provider}:{label}"`.
/// `label` defaults to `"default"` for the common single-key case, keeping
/// `single provider set-key`'s existing behavior unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderKeySpec {
    pub provider: String,
    pub label: String,
    pub agent: Option<String>,
    pub secret_name: String,
}

/// One row of `single provider list-free` — a read-only view over
/// `single_core::free_pool::FreeProvider`, flattened to plain
/// serializable fields for the wire (the source struct holds `Duration`/
/// enums that don't need to cross the CLI<->daemon boundary as-is).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreeProviderInfo {
    pub id: String,
    pub display: String,
    pub signup_url: String,
    pub rpm: Option<u32>,
    pub rpd: Option<u32>,
    pub tpm: Option<u32>,
    pub tpd: Option<u64>,
    pub free_note: String,
    /// Present only for the §17-resolved default-disabled providers
    /// (`sail`, `modelscope`, `qianfan`, `volcengine`, `xfyun`).
    pub disabled_reason: Option<String>,
}

/// One row of `single provider key-status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolKeyStatusInfo {
    pub platform: String,
    pub keyed: bool,
    pub valid: bool,
    pub last_validated_at: Option<String>,
    pub disabled_reason: Option<String>,
    /// `"clear"`, `"benched <N>s"`, or `"n/a"` if the cooldown table
    /// couldn't be read.
    pub cooldown: String,
    /// `"<remaining>/<limit> rpd"` when the provider publishes an RPD
    /// limit, else `"unbounded/unknown"`.
    pub headroom: String,
}

/// `single pool status`. `degraded`/`healthy_ratio` are a **stateless
/// snapshot** — spec §6.5's entry/exit-grace hysteresis needs a
/// `DegradeState` persisted across ticks, which this iteration doesn't
/// wire into the daemon yet (documented follow-up); this reports the
/// instantaneous ratio, not a debounced mode with a "degraded since
/// <ts>" timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStatusInfo {
    pub degraded: bool,
    pub healthy_ratio: f64,
    pub benched: Vec<PoolBenchedKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolBenchedKey {
    pub platform: String,
    pub model: String,
    pub key_id: String,
    pub remaining_secs: u64,
    pub provenance: String,
}

/// The result of trying to sync one provider's key into one agent's real
/// config. Mirrors `IntegrationWrite`'s shape/spirit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSyncResult {
    pub provider: String,
    pub agent: String,
    pub config_path: String,
    pub backup_path: Option<String>,
    pub applied: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeMergeResult {
    pub task_id: i64,
    pub branch: String,
    pub output: String,
}

/// One billing-provider registry entry (`single_core::billing`) — mirrors
/// `registry::AgentDefinition`'s honesty convention: `verified` is only
/// `true` once that provider's real usage endpoint has actually been
/// called successfully, not assumed from reading its docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingProviderInfo {
    pub provider: String,
    pub verified: bool,
    pub admin_key_env_hint: String,
    pub admin_key_configured: bool,
    pub notes: Option<String>,
}

/// One line item from a provider's real usage/billing API — the `$`
/// SingleCLI didn't compute itself, just relayed. `key_label` is `Some`
/// only where that provider's API exposes a per-key breakdown *and* the
/// key matches a locally-registered `ProviderKeySpec::label`; otherwise
/// it's `None` and the amount is an undifferentiated provider total.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub provider: String,
    pub key_label: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub period_start: String,
    pub period_end: String,
}

/// Local-only activity for an agent with no billing-API `$` data (every
/// OAuth-authenticated agent — claude, codex, cursor, copilot, kiro,
/// cody, ...) — sourced from `TaskRecord`, not a provider API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLocalStats {
    pub agent: String,
    pub run_count: u64,
    pub avg_duration_ms: u64,
    pub last_run_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageSummary {
    pub provider_usage: Vec<UsageRecord>,
    pub agent_local_stats: Vec<AgentLocalStats>,
    pub total_usd: f64,
    pub last_refreshed: Option<String>,
}

/// Filter criteria for querying accounting data. All fields are
/// optional — empty means "no filter".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountingQuery {
    pub execution_id: Option<String>,
    pub trace_id: Option<String>,
    pub agent: Option<String>,
    pub provider: Option<String>,
    /// Filter breakdown entries by event type ("input", "output", "cache").
    pub event_type: Option<String>,
}

/// Result of an accounting query: matching usage events, their
/// token breakdown entries, and aggregated totals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountingResult {
    pub events: Vec<UsageEvent>,
    pub breakdowns: Vec<UsageEventBreakdown>,
    pub totals: Totals,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Totals {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cache_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd: f64,
}

/// One row from the `usage_events` table: a single token-usage event
/// linked to an execution and trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub id: i64,
    pub execution_id: String,
    pub trace_id: String,
    pub agent: String,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cache_tokens: i64,
    pub cost_usd: f64,
    pub occurred_at: String,
}

/// One row from the `usage_event_breakdown` table: per-input/output/cache
/// token breakdown for a usage event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEventBreakdown {
    pub id: i64,
    pub execution_id: String,
    pub trace_id: String,
    pub event_type: String,
    pub token_count: i64,
}

/// Repository/git state + project doc discovery for a working directory
/// (spec section 10).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectContext {
    pub cwd: String,
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub changed_files: Vec<String>,
    pub project_docs: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for Finding 4: a still-running older daemon's
    /// `agent_list`/`agent_inspect` response predates `home_requirement`
    /// and `max_concurrency` — since there's no daemon/CLI version
    /// negotiation in this project (the daemon is only replaced on an
    /// explicit `single restart`), a freshly-built CLI must still be able
    /// to deserialize that older JSON rather than failing with a confusing
    /// parse error. Fails without `#[serde(default)]` on both fields.
    #[test]
    fn agent_info_deserializes_when_home_requirement_and_max_concurrency_are_absent() {
        let json = serde_json::json!({
            "name": "claude",
            "adapter": "claude",
            "command": "claude",
            "detected": true,
            "version": null,
            "install_method": { "kind": "native", "detail": "npm install -g @anthropic-ai/claude-code" },
            "bootstrap_install": null,
            "unverified": false,
            // "home_requirement" and "max_concurrency" deliberately omitted
            // — simulating a pre-this-branch daemon's response.
            "capabilities": {
                "streaming": true,
                "mcp": true,
                "lsp": false,
                "tools": true,
                "sessions": true,
                "structured_output": false
            },
            "config_paths": [],
            "notes": null,
            "authenticated": "authenticated"
        });

        let info: AgentInfo = serde_json::from_value(json).expect(
            "AgentInfo must deserialize even without home_requirement/max_concurrency (older daemon compatibility)",
        );
        assert_eq!(info.home_requirement, HomeRequirement::Unverified);
        assert_eq!(info.max_concurrency, None);
    }

    #[test]
    fn home_requirement_default_is_unverified() {
        assert_eq!(HomeRequirement::default(), HomeRequirement::Unverified);
    }

    /// Locks the exact wire path `single-cli::client` and
    /// `single-runtime::server` use (serde JSON to_string then from_str
    /// over the `Request` enum). A pre-0.9.1 daemon whose variant shape
    /// predated the `orchestrator`/`goal`/`candidate_agents` fields would
    /// deserialize `tasks` as an empty `Vec`, and the CLI then printed a
    /// cheerful `Relay (0 step(s)):` with nothing run. This asserts every
    /// field survives so that regression can't return silently.
    #[test]
    fn orchestrate_parallel_request_round_trips_all_fields() {
        let request = Request::OrchestrateParallel {
            tasks: vec![
                ParallelTaskSpec {
                    agent: "grok".into(),
                    description: "say A".into(),
                },
                ParallelTaskSpec {
                    agent: "grok".into(),
                    description: "say B".into(),
                },
            ],
            cwd: "/tmp".into(),
            real_home: false,
            timeout_secs: 300,
            background: false,
            orchestrator: OrchestratorMode::Fixed,
            goal: None,
            candidate_agents: vec![],
        };
        let json = serde_json::to_string(&request).expect("serialize OrchestrateParallel");
        let parsed: Request =
            serde_json::from_str(&json).expect("deserialize OrchestrateParallel");
        match parsed {
            Request::OrchestrateParallel {
                tasks,
                orchestrator,
                background,
                goal,
                candidate_agents,
                ..
            } => {
                assert_eq!(
                    tasks.len(),
                    2,
                    "tasks dropped on round-trip; json={json}"
                );
                assert_eq!(
                    orchestrator,
                    OrchestratorMode::Fixed,
                    "orchestrator dropped/changed on round-trip; json={json}"
                );
                assert!(!background, "background flipped on round-trip; json={json}");
                assert_eq!(goal, None);
                assert!(candidate_agents.is_empty());
            }
            other => panic!("expected OrchestrateParallel, got {other:?}; json={json}"),
        }
    }

    /// Same guard as `orchestrate_parallel_request_round_trips_all_fields`
    /// for `single orchestrate-graph`, whose equivalent regression printed
    /// `Graph (0 node(s)):` after a version-skewed daemon dropped `nodes`.
    #[test]
    fn orchestrate_graph_request_round_trips_all_fields() {
        let request = Request::OrchestrateGraph {
            nodes: vec![
                TaskGraphNode {
                    id: "a".into(),
                    agent: "grok".into(),
                    description: "say A".into(),
                    depends_on: vec![],
                    run_if: RunCondition::Always,
                },
                TaskGraphNode {
                    id: "b".into(),
                    agent: "grok".into(),
                    description: "say B".into(),
                    depends_on: vec!["a".into()],
                    run_if: RunCondition::OnSuccess,
                },
            ],
            cwd: "/tmp".into(),
            real_home: false,
            timeout_secs: 300,
            background: false,
            orchestrator: OrchestratorMode::Fixed,
            goal: None,
            candidate_agents: vec![],
        };
        let json = serde_json::to_string(&request).expect("serialize OrchestrateGraph");
        let parsed: Request = serde_json::from_str(&json).expect("deserialize OrchestrateGraph");
        match parsed {
            Request::OrchestrateGraph {
                nodes,
                orchestrator,
                background,
                goal,
                candidate_agents,
                ..
            } => {
                assert_eq!(
                    nodes.len(),
                    2,
                    "nodes dropped on round-trip; json={json}"
                );
                assert_eq!(
                    orchestrator,
                    OrchestratorMode::Fixed,
                    "orchestrator dropped/changed on round-trip; json={json}"
                );
                assert!(!background, "background flipped on round-trip; json={json}");
                assert_eq!(goal, None);
                assert!(candidate_agents.is_empty());
            }
            other => panic!("expected OrchestrateGraph, got {other:?}; json={json}"),
        }
    }

    #[test]
    fn coordinator_requests_round_trip_through_json() {
        let reqs = vec![
            Request::SessionNew { cwd: "/tmp/p".into() },
            Request::SessionList,
            Request::SessionClose { session_id: "sess_1".into() },
            Request::GoalSubmit {
                session_id: "sess_1".into(),
                text: "do the thing".into(),
                mode: Some("auto".into()),
                max_dispatches: Some(10),
                max_minutes: None,
                agent: None,
            },
            Request::GoalSubmit {
                session_id: "sess_1".into(),
                text: "loop on it".into(),
                mode: Some("careful".into()),
                max_dispatches: Some(6),
                max_minutes: None,
                agent: Some("grok".into()),
            },
            Request::GoalStatus { goal_id: "goal_1".into() },
            Request::GoalList { session_id: None },
            Request::GoalAmend { goal_id: "goal_1".into(), text: "budget=30".into() },
            Request::GoalCancel { goal_id: "goal_1".into() },
            Request::GoalResume { goal_id: "goal_1".into() },
            Request::GoalRetryNode { goal_id: "goal_1".into(), node_id: "s2".into() },
            Request::SessionEvents { session_id: "sess_1".into(), since_event_id: 4 },
            Request::CoordinatorStatus,
            Request::GoalMergeList,
            Request::GoalMergeShow { id: 1 },
            Request::GoalMergeResolve { id: 1, allow: true },
        ];
        for r in reqs {
            let json = serde_json::to_string(&r).unwrap();
            let back: Request = serde_json::from_str(&json).unwrap();
            assert_eq!(
                serde_json::to_value(&r).unwrap(),
                serde_json::to_value(&back).unwrap(),
                "round-trip changed {json}"
            );
        }
    }

    #[test]
    fn coordinator_responses_round_trip_through_json() {
        let data = vec![
            ResponseData::GoalId("goal_1".into()),
            ResponseData::Sessions(vec![SessionInfo {
                id: "sess_1".into(),
                cwd: "/tmp".into(),
                title: "t".into(),
                created_at: "now".into(),
                updated_at: "now".into(),
                status: "active".into(),
            }]),
            ResponseData::Empty,
        ];
        for d in data {
            let json = serde_json::to_string(&d).unwrap();
            let _back: ResponseData = serde_json::from_str(&json).unwrap();
        }
    }
}
