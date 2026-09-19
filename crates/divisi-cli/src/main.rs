mod acp;
mod client;
mod daemon;
mod internal_lsp_manifest;
mod migrate_cmd;
mod notch_proc;
mod render;
mod serve_openai;
mod update;

use clap::{Parser, Subcommand};
use divisi_core::DivisiDirs;
use divisi_protocol::{
    LspServerSpec, McpServerSpec, Request, Response, ResponseData, RiskLevel, ToolSpec,
};
use std::collections::BTreeMap;

#[derive(Parser)]
#[command(
    name = "divisi",
    version,
    about = "divisi — unified control plane for AI coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show runtime status.
    Status,
    /// Move a pre-rename install onto divisi names: config dir, systemd unit, notch extension.
    Migrate {
        /// Apply the changes (default: print what would change).
        #[arg(long)]
        apply: bool,
    },
    /// Diagnose installed agent CLIs, config, and runtime health.
    Doctor {
        /// Also runs a self-heal pass (E28 spec §9) across every enabled
        /// category before reporting — stale sockets, corrupt config, db
        /// integrity, coordinator self-correction, agent install/repair.
        #[arg(long)]
        fix: bool,
    },
    /// Install missing agent CLIs and sync divisi's config into all of them.
    Setup {
        /// Actually run install commands and write config. Without this, only shows the plan.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Manage the agent registry.
    Agent {
        #[command(subcommand)]
        action: AgentCommand,
    },
    /// Manage the unified MCP registry.
    Mcp {
        #[command(subcommand)]
        action: McpCommand,
    },
    /// Manage the unified LSP registry.
    Lsp {
        #[command(subcommand)]
        action: LspCommand,
    },
    /// Manage the tool registry (metadata only — no execution engine yet).
    Tool {
        #[command(subcommand)]
        action: ToolCommand,
    },
    /// Manage the `divisid` background daemon directly. Mainly for
    /// `restart`: the daemon inherits its environment (notably `$PATH`)
    /// once at spawn time, so a CLI installed (or a shell rc file edited)
    /// after the daemon was already running stays invisible to agent
    /// detection until it's restarted.
    Daemon {
        #[command(subcommand)]
        action: DaemonCommand,
    },
    /// Manage the opt-in top-center notch HUD companion process (E30).
    Notch {
        #[command(subcommand)]
        action: NotchCommand,
    },
    /// Manage secrets (OS keychain-backed; Linux via secret-tool in Phase 2).
    Secret {
        #[command(subcommand)]
        action: SecretCommand,
    },
    /// Manage skills (local directories under ~/.config/divisi/skills).
    Skill {
        #[command(subcommand)]
        action: SkillCommand,
    },
    /// Manage the shared memory store.
    Memory {
        #[command(subcommand)]
        action: MemoryCommand,
    },
    /// Leave/read notes between agents working the same project — a
    /// minimal inbox, not a live stream (see `divisi-runtime::notes`).
    Note {
        #[command(subcommand)]
        action: NoteCommand,
    },
    /// Ingest a document (PDF/image/text) into the shared memory store — see `divisi-runtime::documents`.
    Doc {
        #[command(subcommand)]
        action: DocCommand,
    },
    /// Pending human decisions (see `divisi_core::preferences`) — currently
    /// raised by the divisi-gateway gateway when a tool call needs approval.
    Approval {
        #[command(subcommand)]
        action: ApprovalCommand,
    },
    /// Learned decisions — what divisi has auto-approved/denied before
    /// without asking, and why (see `divisi_core::preferences`).
    Preference {
        #[command(subcommand)]
        action: PreferenceCommand,
    },
    /// Show resolved project context (git state, project docs) for a directory.
    Context {
        /// Defaults to the current directory.
        cwd: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Run a task: delegate a prompt to one real agent CLI, synchronously.
    Task {
        #[command(subcommand)]
        action: TaskCommand,
    },
    /// Workspaces (projects) tasks have run against — the grouping `single
    /// task list` doesn't show on its own. A workspace's identity survives
    /// its directory moving; see `divisi_core::project_context`.
    Workspace {
        #[command(subcommand)]
        action: WorkspaceCommand,
    },
    /// Ordered agent/account chains `task run --allow-fallback` walks
    /// through when a run hits a detected rate limit — see
    /// `divisi_core::fallback` and `divisi_core::ratelimit`.
    Fallback {
        #[command(subcommand)]
        action: FallbackCommand,
    },
    /// Task-lifecycle event hooks: fire a command when a task reaches a
    /// terminal status, instead of polling `divisi task list` for it —
    /// see `divisi_core::task_hooks`. Not the same thing as `divisi agent
    /// hooks` (that's Claude Code's mid-run permission interception).
    TaskHook {
        #[command(subcommand)]
        action: TaskHookCommand,
    },
    /// Browse the premium-web pattern library (`~/.config/divisi/skills/
    /// web/premium-web/patterns/**/*.md`) — see the `divisi-web` crate
    /// and `docs/web-capability-pack-architecture.md`. Local-only, reads
    /// files directly, no daemon round trip needed.
    Web {
        #[command(subcommand)]
        action: WebCommand,
    },
    /// Run several agents in sequence on one goal: each agent runs in the
    /// same shared git worktree and is handed the previous agent's real
    /// output. A sequential relay, not live parallel/bidirectional
    /// multi-agent chat — see docs/architecture.md for the honest scope.
    Orchestrate {
        goal: String,
        #[arg(long, value_delimiter = ',', required = true)]
        agents: Vec<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        worktree: bool,
        /// See `divisi task run --help`'s --real-home — applies to every step.
        #[arg(long)]
        real_home: bool,
        #[arg(long, default_value = "300")]
        timeout_secs: u64,
    },
    /// Run several agents concurrently, each on its own explicit sub-task
    /// (e.g. `--task claude:"backend API" --task codex:"frontend UI"`),
    /// each in its own git worktree. Real parallel execution, unlike
    /// `orchestrate`'s sequential relay. No automatic goal splitting: you
    /// decide each agent's task, divisi just runs them at the same time
    /// and reports what happened — branches are never auto-merged.
    OrchestrateParallel {
        /// Repeatable: <agent>:<description>, e.g. claude:"implement the API"
        #[arg(long = "task")]
        tasks: Vec<String>,
        /// fixed (default) uses --task; auto/delegate ask an installed candidate CLI to plan a graph.
        #[arg(long, default_value = "fixed")]
        orchestrator: String,
        #[arg(long)]
        goal: Option<String>,
        #[arg(long = "candidate-agent", value_delimiter = ',')]
        candidate_agents: Vec<String>,
        #[arg(long)]
        cwd: Option<String>,
        /// See `divisi task run --help`'s --real-home — applies to every task.
        #[arg(long)]
        real_home: bool,
        #[arg(long, default_value = "300")]
        timeout_secs: u64,
        /// Start the whole batch and return immediately instead of
        /// blocking until every task finishes — poll `task list`/`task
        /// inspect` for each one's progress as it lands.
        #[arg(long)]
        background: bool,
    },
    /// Run an explicit dependency graph. Each --task is a comma-separated
    /// node specification: id=build,agent=codex,desc="build it",depends_on=lint|test,run_if=on_success.
    OrchestrateGraph {
        /// Repeatable node specification; depends_on and run_if are optional.
        #[arg(long = "task")]
        tasks: Vec<String>,
        #[arg(long, default_value = "fixed")]
        orchestrator: String,
        #[arg(long)]
        goal: Option<String>,
        #[arg(long = "candidate-agent", value_delimiter = ',')]
        candidate_agents: Vec<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        real_home: bool,
        #[arg(long, default_value = "300")]
        timeout_secs: u64,
        #[arg(long)]
        background: bool,
    },
    /// Switch between multiple logged-in accounts for an agent (e.g. two
    /// Claude Code accounts). Log in normally with the agent's own CLI
    /// first, then capture that login state as a named profile.
    Account {
        #[command(subcommand)]
        action: AccountCommand,
    },
    /// Manage LLM provider API keys (OpenAI, Anthropic, OpenCode Zen, ...)
    /// and sync them into agents that support custom providers.
    Provider {
        #[command(subcommand)]
        action: ProviderCommand,
    },
    /// Assisted (never automatic) git worktree merge-back for a worktree-isolated task.
    Worktree {
        #[command(subcommand)]
        action: WorktreeCommand,
    },
    /// Real $ spend across connected providers (from billing admin keys)
    /// plus local run stats for every other agent.
    Usage {
        #[command(subcommand)]
        action: UsageCommand,
    },
    /// Queryable token/cost accounting data (propose-only).
    Accounting {
        #[command(subcommand)]
        action: AccountingCommand,
    },
    /// Export/import your entire divisi setup — config, agent
    /// credentials, keychain secrets, task history — as one
    /// password-encrypted archive, to move to another machine. Runs
    /// entirely locally: the passphrase never touches the daemon socket.
    Backup {
        #[command(subcommand)]
        action: BackupCommand,
    },
    /// Manage plugins and sync installs into agents that have a real
    /// plugin-install command (claude, codex, opencode, agy).
    Plugin {
        #[command(subcommand)]
        action: PluginCommand,
    },
    /// Manage profiles.
    Profile {
        #[command(subcommand)]
        action: ProfileCommand,
    },
    /// Check for or apply a newer divisi build from GitHub Releases.
    Update {
        #[arg(long, default_value = "stable")]
        channel: String,
        /// Only report whether a newer build is available; don't download or replace anything.
        #[arg(long)]
        check: bool,
        #[arg(long)]
        yes: bool,
    },
    /// Sync divisi's MCP registry into every agent's native config.
    InstallIntegrations {
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
        /// Write into the real, ambient $HOME instead of the divisi-managed
        /// isolated home — the only way this ever reaches an agent you run
        /// normally, outside divisi. Off by default: same posture as
        /// `divisi task run --real-home`.
        #[arg(long)]
        real_home: bool,
    },
    /// Remove divisi-managed entries from every agent's native config.
    UninstallIntegrations {
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        real_home: bool,
    },
    /// Coordinator conversation threads (spec E27.02 §6).
    Session {
        #[command(subcommand)]
        action: SessionCommand,
    },
    /// Submit and track coordinator goals.
    Goal {
        #[command(subcommand)]
        action: GoalCommand,
    },
    /// Coordinator cross-thread status.
    Coordinator {
        #[command(subcommand)]
        action: CoordinatorCommand,
    },
    /// E28 free-provider pool engine status.
    Pool {
        #[command(subcommand)]
        action: PoolCommand,
    },
    /// Keep one agent iterating on a goal until it reports done. Sugar
    /// over `divisi goal submit --mode careful`: the coordinator
    /// re-dispatches the agent with its previous output appended until it
    /// replies with a line containing only DONE, or `--max-iters` is hit.
    Loop {
        /// The goal (all trailing words are joined).
        text: Vec<String>,
        /// Pin the loop to this agent (otherwise routed).
        #[arg(long)]
        agent: Option<String>,
        /// Iteration cap.
        #[arg(long, default_value = "6")]
        max_iters: u32,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Run the Agent Client Protocol bridge on stdio (for Zed's agent
    /// panel). Speaks newline-delimited JSON-RPC 2.0; every prompt becomes
    /// a coordinator goal, progress streams back. Not a one-shot command —
    /// it runs until stdin closes.
    Acp,
    /// Run a local HTTP server that speaks the OpenAI chat-completions API
    /// over the divisi pool — point Zed's `language_models.openai_compatible`
    /// at `http://<addr>/v1`. Runs until interrupted.
    Serve {
        /// The only supported mode today; required.
        #[arg(long)]
        openai: bool,
        #[arg(long, default_value = "127.0.0.1:8765")]
        addr: String,
        /// Force every request onto this agent instead of routing.
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, default_value = "180")]
        timeout_secs: u64,
        /// Bearer token required on every request. Omit and one is
        /// generated and printed at startup.
        #[arg(long)]
        api_key: Option<String>,
        /// Permit binding a non-loopback address (requires --api-key).
        /// Off by default: the proxy runs agent CLIs.
        #[arg(long)]
        allow_remote: bool,
    },
    /// Undocumented: internal helpers other divisi-owned tooling shells out to.
    #[command(hide = true, subcommand)]
    Internal(InternalCommand),
}

#[derive(Subcommand)]
enum SessionCommand {
    /// Open a new session for a working directory.
    New {
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    Close {
        session_id: String,
    },
}

#[derive(Subcommand)]
enum GoalCommand {
    /// Submit a goal; the coordinator plans and drives it.
    Submit {
        /// The goal text (all trailing words are joined).
        text: Vec<String>,
        /// Existing session id; a new session for --cwd is created if omitted.
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        /// auto (default) / plan / careful / dry.
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        max_dispatches: Option<u32>,
        #[arg(long)]
        max_minutes: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    Status {
        goal_id: String,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Add context, `budget=N` to raise the dispatch cap, `minutes=N` to
    /// raise the wall-clock cap, `capacity-minutes=N` to raise the
    /// capacity-wait wall-clock cap, or `auto-merge=true|false` to opt into
    /// merge confirmation (`divisi goal merge`), and re-tick.
    Amend {
        goal_id: String,
        text: Vec<String>,
    },
    Cancel {
        goal_id: String,
    },
    /// Manual re-tick of a `blocked`/`failed`/`paused`/`waiting_on_capacity`
    /// goal you judge recoverable (E28 spec §10) — the same path the
    /// daemon runs automatically on its own restart.
    Resume {
        goal_id: String,
    },
    /// Live-verification finding 2026-09-17 (E30 dispatch): a node the
    /// supervisor set `blocked` sits outside the ready-set forever —
    /// neither `amend` nor `resume` touches a node's own status, only
    /// goal-level status and capacity-wait stamps. Resets exactly this
    /// node back to `pending` (status, task_id, attempts, retry stamp)
    /// and re-ticks. Works on any node, not just a `blocked` one — retrying
    /// a `failed` node you judge recoverable is the same action.
    RetryNode {
        goal_id: String,
        node_id: String,
    },
    /// Merges awaiting human confirmation from the coordinator's opt-in
    /// auto-merge (`goal.auto_merge`) — a passing review queues one of
    /// these, it never merges by itself. "Branches are never auto-merged;
    /// that stays a human decision" (docs/architecture.md).
    #[command(subcommand)]
    Merge(GoalMergeCommand),
}

#[derive(Subcommand)]
enum GoalMergeCommand {
    /// List merges awaiting confirmation.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show the real diff a pending merge would land.
    Show {
        id: i64,
    },
    /// Merge it (`git merge --no-ff`) after you've reviewed `merge show`.
    Confirm {
        id: i64,
    },
    /// Decline it — never touches the repo.
    Reject {
        id: i64,
    },
}

#[derive(Subcommand)]
enum CoordinatorCommand {
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum PoolCommand {
    /// Benched keys + a healthy-ratio snapshot.
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum InternalCommand {
    /// Prints a shell script installing every registry agent's real
    /// bootstrap_install command — docker/Dockerfile runs this so the
    /// image build always matches whatever's in
    /// divisi_core::registry::builtin_registry() at build time, instead
    /// of a separately maintained install list going stale.
    PrintBootstrapScript,
    /// Claude Code's PreToolUse hook (see `divisi agent hooks enable
    /// claude`): reads the hook's JSON on stdin, evaluates the tool call
    /// against permissions.toml + learned preferences, and — for the
    /// undecided case — blocks polling for a real `divisi approval
    /// resolve` before answering. Prints the exact
    /// hookSpecificOutput/permissionDecision JSON Claude Code expects.
    #[command(name = "claude-pretooluse-hook")]
    ClaudePreToolUseHook,
    /// Regenerates the divisi-lsp Claude Code plugin's marketplace manifest
    /// from the current LSP registry — re-run after `divisi lsp add`/
    /// `enable`/`disable` so the plugin's extensionToLanguage map stays in
    /// sync with what divisi-lsp itself actually routes.
    GenerateLspPluginManifest {
        output_dir: String,
    },
}

#[derive(Subcommand)]
enum AgentCommand {
    List {
        #[arg(long)]
        json: bool,
    },
    Inspect {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Install a single agent (dry run by default).
    Install {
        name: String,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Interactively log in to this agent's divisi-managed home (never
    /// the real, ambient one — see `docs/architecture.md`'s "Isolation"
    /// section). Runs the agent's own real login command attached to
    /// your terminal (browser OAuth or a prompt, whichever that agent
    /// uses); bootstraps the isolated home first if this is the first
    /// time it's used.
    Login { name: String },
    /// Opt-in Docker execution backend: run this agent's tasks inside a
    /// persistent container instead of on the host. Host isolation
    /// ($HOME-swap) stays the default for everyone else.
    Docker {
        #[command(subcommand)]
        action: AgentDockerCommand,
    },
    /// Opt-in mid-run permission interception: the agent's own process
    /// pauses before using a tool and asks (see `divisi approval`). Only
    /// `claude` is wired up right now (its PreToolUse hook).
    Hooks {
        #[command(subcommand)]
        action: AgentHooksCommand,
    },
}

#[derive(Subcommand)]
enum AgentHooksCommand {
    Enable { agent: String },
    Disable { agent: String },
    Status,
}

#[derive(Subcommand)]
enum AgentDockerCommand {
    /// Enable for an agent (all its accounts) or one specific captured account with --account.
    Enable {
        agent: String,
        #[arg(long)]
        account: Option<String>,
    },
    Disable {
        agent: String,
        #[arg(long)]
        account: Option<String>,
    },
    /// Show configured agents/accounts and their live container state. Omit `agent` to list all.
    Status { agent: Option<String> },
    Stop {
        agent: String,
        #[arg(long)]
        account: Option<String>,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    List {
        #[arg(long)]
        json: bool,
    },
    Add {
        name: String,
        command: String,
        /// A secret-backed env var this server needs, e.g.
        /// `--secret CLOUDFLARE_API_TOKEN=abc123`. Repeatable. Stored in
        /// the OS keychain (see `divisi secret`), never written to
        /// mcp.toml in plain text — only resolved at spawn time by the
        /// divisi-gateway gateway (see `divisi mcp gateway enable`).
        #[arg(long = "secret", value_parser = parse_key_val)]
        secrets: Vec<(String, String)>,
        /// Extra arguments passed to `command`, in order (may start with `-`, e.g. `-y`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Remove {
        name: String,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    /// Enables every currently-disabled registered server that doesn't need a secret it
    /// doesn't already have — i.e. `secret_env` is empty, or every key in it already
    /// resolves to a stored secret (`divisi secret set`). Safe to re-run any time you add
    /// more presets or set a new secret; already-enabled/already-skipped servers are untouched.
    EnableAll {
        /// Print what would be enabled without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    Inspect {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// List built-in MCP server presets (brave-search, slack, puppeteer, postgres, cloudflare, postman, distrobox-control).
    Presets,
    /// Register an MCP server from a built-in preset (ships disabled — most need a secret).
    AddPreset {
        name: String,
    },
    /// Dynamic MCP gateway (crates/divisi-gateway): when enabled, `divisi install-integrations`
    /// syncs only divisi-gateway into agents' native config instead of every enabled server —
    /// divisi-gateway then proxies to them lazily. Takes effect on the next install-integrations.
    Gateway {
        #[command(subcommand)]
        action: McpGatewayCommand,
    },
}

fn parse_key_val(s: &str) -> anyhow::Result<(String, String)> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("expected KEY=VALUE, got '{s}'"))?;
    Ok((k.to_string(), v.to_string()))
}

fn parse_model_spec(s: &str) -> anyhow::Result<(String, String)> {
    let (id, name) = s
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("expected ID:NAME, got '{s}'"))?;
    Ok((id.to_string(), name.to_string()))
}

#[derive(Subcommand)]
enum McpGatewayCommand {
    Enable,
    Disable,
    Status,
}

#[derive(Subcommand)]
enum LspCommand {
    List {
        #[arg(long)]
        json: bool,
    },
    Add {
        name: String,
        command: String,
        /// File extensions this server handles, e.g. .rs .toml
        #[arg(long, value_delimiter = ' ')]
        extensions: Vec<String>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Remove {
        name: String,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    /// Enables every currently-disabled registered server, unconditionally — language
    /// servers have no auth concept, and each agent's own LSP client only spawns one when
    /// a matching file is actually opened, so an uninstalled server just never gets used.
    /// Safe to re-run any time you add more presets.
    EnableAll {
        /// Print what would be enabled without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    Inspect {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// List built-in LSP presets (rust-analyzer, pyright, typescript, gopls, dockerfile, clangd, bash, yaml, terraform, json).
    Presets,
    /// Register an LSP server from a built-in preset.
    AddPreset {
        name: String,
    },
}

#[derive(Subcommand)]
enum ToolCommand {
    List {
        #[arg(long)]
        json: bool,
    },
    Add {
        name: String,
        description: String,
        #[arg(long, value_enum, default_value = "medium")]
        risk: RiskArg,
    },
    Inspect {
        name: String,
        #[arg(long)]
        json: bool,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
}

#[derive(Clone, clap::ValueEnum)]
enum RiskArg {
    Low,
    Medium,
    High,
}

#[derive(Subcommand)]
enum SecretCommand {
    List,
    Set { name: String, value: String },
    Get { name: String },
    Delete { name: String },
    /// E29: moves a live redaction alias (e.g. `{{REDACTED:sess_.../3}}`,
    /// still within its 3h TTL) into a properly named secret. Prompts for
    /// confirmation unless `--yes` is given.
    PromoteAlias {
        alias: String,
        name: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum SkillCommand {
    List,
    Install {
        name: String,
        source_path: String,
    },
    Remove {
        name: String,
    },
    Inspect {
        name: String,
    },
    /// Copies a skill into Claude Code's real skill directory
    /// (~/.claude/skills/<name>/) — backs up any existing same-named directory first.
    SyncClaude {
        name: String,
    },
    /// List the curated starter skills bundled with divisi.
    Starters,
    /// Install a bundled starter skill by name (see `divisi skill starters`).
    InstallStarter {
        name: String,
    },
}

#[derive(Subcommand)]
enum MemoryCommand {
    /// Store a memory entry.
    Store {
        title: String,
        content: String,
        #[arg(long, value_enum)]
        scope: Option<MemoryScopeArg>,
        #[arg(long, value_enum)]
        source: Option<MemorySourceArg>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        confidence: Option<f64>,
        /// Auto-delete after this many seconds.
        #[arg(long)]
        expires_in: Option<i64>,
    },
    /// Substring search over title + content (not semantic search — see docs/architecture.md).
    Search {
        query: String,
        #[arg(long, value_enum)]
        scope: Option<MemoryScopeArg>,
        #[arg(long)]
        project: Option<String>,
        /// Embed the query and search by meaning instead of substring —
        /// requires an embeddings key + DIVISI_QDRANT_URL (falls back to
        /// substring search with a warning otherwise).
        #[arg(long)]
        semantic: bool,
        #[arg(long, default_value = "10")]
        limit: u64,
        #[arg(long)]
        json: bool,
    },
    Get {
        id: i64,
        #[arg(long)]
        json: bool,
    },
    Delete {
        id: i64,
    },
    List {
        #[arg(long, value_enum)]
        scope: Option<MemoryScopeArg>,
        #[arg(long)]
        json: bool,
    },
    /// Knowledge-graph memory: entities, observations, and typed relations
    /// between them — a "powerful shared brain" agents can build up over
    /// time, distinct from the scoped store/search memory above.
    Graph {
        #[command(subcommand)]
        action: KgCommand,
    },
    /// Fast ephemeral Redis-backed working memory. Requires DIVISI_REDIS_URL.
    Cache {
        #[command(subcommand)]
        action: CacheCommand,
    },
    /// Vector store for RAG (Qdrant-backed). Requires DIVISI_QDRANT_URL.
    /// Stores/searches pre-computed vectors directly — for text, use
    /// `divisi memory search --semantic` instead, which embeds the query
    /// for you (needs an embeddings key too, see `divisi secret set
    /// embeddings:api_key`).
    Vector {
        #[command(subcommand)]
        action: VectorCommand,
    },
}

#[derive(Subcommand)]
enum NoteCommand {
    /// Leave a note for another agent (or, with no --to, any agent) working this project.
    Leave {
        content: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = "general")]
        topic: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// Read notes addressed to an agent (including broadcast notes) for a project.
    Inbox {
        #[arg(long)]
        to: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        unread_only: bool,
        #[arg(long)]
        json: bool,
    },
    MarkRead {
        id: i64,
    },
}

#[derive(Subcommand)]
enum ApprovalCommand {
    List,
    Resolve {
        id: i64,
        #[arg(long, conflicts_with = "deny")]
        allow: bool,
        #[arg(long, conflicts_with = "allow")]
        deny: bool,
        /// Also learn this as a preference for the same resource pattern, so it doesn't ask again.
        #[arg(long)]
        remember: bool,
    },
}

#[derive(Subcommand)]
enum PreferenceCommand {
    List,
}

#[derive(Subcommand)]
enum DocCommand {
    /// Extract text from a PDF/image/text file (OCR fallback for scanned PDFs) and store it as memory.
    Ingest {
        path: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        title: Option<String>,
    },
    List {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Show {
        id: i64,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum VectorCommand {
    /// vector is a comma-separated list of floats, e.g. 0.1,0.2,0.3
    Upsert {
        collection: String,
        id: u64,
        #[arg(long, value_delimiter = ',')]
        vector: Vec<f32>,
        #[arg(long, default_value = "{}")]
        payload: String,
    },
    Search {
        collection: String,
        #[arg(long, value_delimiter = ',')]
        vector: Vec<f32>,
        #[arg(long, default_value = "5")]
        limit: u64,
    },
    Delete {
        collection: String,
        id: u64,
    },
    Status,
}

#[derive(Subcommand)]
enum CacheCommand {
    Set {
        key: String,
        value: String,
        #[arg(long)]
        ttl_secs: Option<u64>,
    },
    Get {
        key: String,
    },
    Delete {
        key: String,
    },
    List {
        #[arg(default_value = "*")]
        pattern: String,
    },
    Status,
}

#[derive(Subcommand)]
enum KgCommand {
    CreateEntity {
        name: String,
        entity_type: String,
    },
    AddObservation {
        entity: String,
        content: String,
    },
    CreateRelation {
        from: String,
        to: String,
        relation_type: String,
    },
    DeleteEntity {
        name: String,
    },
    Get {
        name: String,
        #[arg(long)]
        json: bool,
    },
    Query {
        term: String,
        #[arg(long)]
        json: bool,
    },
    /// Dump the full graph.
    Show {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, clap::ValueEnum)]
enum MemoryScopeArg {
    Working,
    Project,
    User,
    Agent,
    Task,
    LongTerm,
    Knowledge,
}

impl From<MemoryScopeArg> for divisi_protocol::MemoryScope {
    fn from(v: MemoryScopeArg) -> Self {
        use divisi_protocol::MemoryScope::*;
        match v {
            MemoryScopeArg::Working => Working,
            MemoryScopeArg::Project => Project,
            MemoryScopeArg::User => User,
            MemoryScopeArg::Agent => Agent,
            MemoryScopeArg::Task => Task,
            MemoryScopeArg::LongTerm => LongTerm,
            MemoryScopeArg::Knowledge => Knowledge,
        }
    }
}

#[derive(Clone, clap::ValueEnum)]
enum MemorySourceArg {
    UserInstruction,
    AgentOutput,
    ToolOutput,
    ProjectContent,
    ExternalContent,
}

impl From<MemorySourceArg> for divisi_protocol::MemorySource {
    fn from(v: MemorySourceArg) -> Self {
        use divisi_protocol::MemorySource::*;
        match v {
            MemorySourceArg::UserInstruction => UserInstruction,
            MemorySourceArg::AgentOutput => AgentOutput,
            MemorySourceArg::ToolOutput => ToolOutput,
            MemorySourceArg::ProjectContent => ProjectContent,
            MemorySourceArg::ExternalContent => ExternalContent,
        }
    }
}

#[derive(Subcommand)]
enum ProviderCommand {
    /// Register a provider. The key itself is set separately with `set-key`.
    Add {
        name: String,
        /// The environment variable name an agent needs to see this key as, e.g. ANTHROPIC_API_KEY.
        #[arg(long)]
        env_var: String,
        #[arg(long)]
        base_url: Option<String>,
        /// A model this provider exposes, as `id:display name` (e.g. `auto:Auto (best available)`). Repeatable.
        #[arg(long = "model", value_parser = parse_model_spec)]
        models: Vec<(String, String)>,
    },
    Remove {
        name: String,
    },
    List {
        /// Only show providers that actually have a key stored (shared set-key or any labeled add-key) — every built-in preset shows without this.
        #[arg(long)]
        configured: bool,
        #[arg(long)]
        json: bool,
    },
    Inspect {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Store the actual API key in the OS keychain.
    SetKey {
        name: String,
        value: String,
    },
    /// Write the key into the named agents' real config (all registered agents if none given).
    Sync {
        name: String,
        #[arg(long, value_delimiter = ',')]
        agents: Vec<String>,
        #[arg(long)]
        yes: bool,
        /// Write into the real, ambient $HOME instead of the divisi-managed
        /// isolated home. Off by default: same posture as
        /// `divisi task run --real-home`.
        #[arg(long)]
        real_home: bool,
    },
    /// List built-in provider presets (OpenAI, Anthropic, OpenCode Zen, NVIDIA).
    Presets,
    /// Register a provider from a built-in preset (name, env var, base URL already filled in).
    AddPreset {
        name: String,
    },
    /// Store a *labeled* key for a provider (e.g. one key per agent), distinct from `set-key`'s single shared key.
    AddKey {
        provider: String,
        #[arg(long)]
        label: String,
        /// Which agent this key is for, so the Usage page can attribute billing-API spend to it.
        #[arg(long)]
        agent: Option<String>,
        value: String,
    },
    /// List labeled keys for a provider (labels/agent tags only, never the key value).
    ListKeys {
        provider: String,
    },
    RemoveKey {
        provider: String,
        label: String,
    },
    /// Sync one specific labeled key into one specific agent's real config.
    KeySync {
        provider: String,
        label: String,
        agent: String,
        #[arg(long)]
        yes: bool,
    },
    /// Store the org/admin-scoped key used only to query a provider's usage/billing API — separate from any inference key.
    SetBillingKey {
        provider: String,
        value: String,
    },
    /// List the vendored ~40-provider free-LLM catalog (E28 spec §5.3), not `providers.toml`.
    ListFree {
        #[arg(long)]
        json: bool,
    },
    /// Register one free-pool provider's key (prompted, hidden, if `--key`
    /// is omitted) and best-effort validate it. Safe to call repeatedly
    /// for the same provider with a different key (e.g. from another
    /// account) — each call auto-picks a fresh key_id (default, key2,
    /// key3, ...) instead of overwriting the previous key, growing the
    /// pool's real capacity. Pass `--key-id` explicitly only to rotate
    /// (replace) one specific existing key on purpose.
    AddFree {
        id: String,
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        key_id: Option<String>,
    },
    /// Reconcile the vendored catalog into `providers.toml` (`single-<id>` presets) and `free-pool.toml` (enabled/disabled state). Idempotent.
    SyncPool,
    /// Per free-pool provider: keyed?, last validation, disabled reason, live cooldown state, and headroom (a real remaining-quota count for providers with a declared rate limit, "unbounded/unknown" otherwise).
    KeyStatus {
        #[arg(long)]
        platform: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Re-probes every already-keyed free-pool key (or just `--platform`'s) against its provider's validate_url, the same best-effort check `add-free` does at registration time -- for keys that were added before this existed, or whose status you want to refresh without waiting for a real single-pool task to happen to pick them. A provider with no validate_url quirk is skipped (nothing to probe), not errored. Prints the same table `key-status` does, reflecting the just-run probes.
    Validate {
        #[arg(long)]
        platform: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum WorktreeCommand {
    /// Show the diff a task's worktree branch would bring in if merged — never merges.
    Diff {
        task_id: i64,
        #[arg(long)]
        json: bool,
    },
    /// Merge a task's worktree branch into the repo it ran against.
    Merge {
        task_id: i64,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum UsageCommand {
    /// Show real $ spend (from configured billing admin keys) plus local run stats for every other agent.
    Show {
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Force a live re-fetch from every configured billing provider, bypassing the cache.
    Refresh {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum AccountingCommand {
    /// Query usage events and token breakdown (propose-only; no business data).
    Show {
        /// Filter by execution_id
        #[arg(long)]
        execution_id: Option<String>,
        /// Filter by trace_id
        #[arg(long)]
        trace_id: Option<String>,
        /// Filter by agent name
        #[arg(long)]
        agent: Option<String>,
        /// Filter by provider name
        #[arg(long)]
        provider: Option<String>,
        /// Filter breakdown by event type (input, output, cache)
        #[arg(long)]
        event_type: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum BackupCommand {
    /// Export divisi's entire setup (config, agent credentials, keychain secrets, task history) into one encrypted archive.
    Export { path: String },
    /// Restore from an encrypted archive produced by `export`. Dry-run by default; pass --yes to actually write.
    Import {
        path: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Register a plugin. `target` is the `plugin[@marketplace]` selector
    /// used verbatim by claude/codex/agy; `--opencode-module` is the plain
    /// npm module name OpenCode's real `opencode plugin <module>` command
    /// needs instead (a genuinely different addressing scheme, not set
    /// automatically from `target`).
    Add {
        name: String,
        target: String,
        #[arg(long)]
        opencode_module: Option<String>,
    },
    Remove {
        name: String,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    Inspect {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// List built-in plugin presets, sourced from Anthropic's official
    /// marketplace (github.com/anthropics/claude-plugins-official).
    Presets,
    /// Register a plugin from a built-in preset (verified to sync into `claude`; other agents best-effort).
    AddPreset {
        name: String,
    },
    /// Install the plugin into the named agents (all registered agents if none given).
    Sync {
        name: String,
        #[arg(long, value_delimiter = ',')]
        agents: Vec<String>,
        #[arg(long)]
        yes: bool,
        /// Write into the real, ambient $HOME instead of the divisi-managed
        /// isolated home. Off by default: same posture as
        /// `divisi task run --real-home`.
        #[arg(long)]
        real_home: bool,
    },
}

#[derive(Subcommand)]
enum AccountCommand {
    /// Capture the agent's currently-live login state as a named profile.
    /// Log in normally with the agent's own CLI first.
    Capture {
        agent: String,
        name: String,
        /// Human-readable identity (email/display name) so multiple
        /// accounts per agent are distinguishable later.
        #[arg(long)]
        label: Option<String>,
    },
    /// Swap a captured profile into place as the agent's live login state
    /// (backs up whatever was live first).
    Use {
        agent: String,
        name: String,
    },
    List {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Remove {
        agent: String,
        name: String,
    },
    /// Manually record whether an account is usable, rate-limited, or
    /// needs a top-up. Never auto-detected (no verified quota API across
    /// agents) — you or a failed task tell divisi, and it remembers.
    SetStatus {
        agent: String,
        name: String,
        /// One of: available, rate_limited, needs_topup, unknown
        status: String,
    },
}

#[derive(Subcommand)]
enum WorkspaceCommand {
    /// List every workspace with at least one task, newest activity first.
    List {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum FallbackCommand {
    /// Saves one ordered chain — each entry is `agent` or `agent:account`,
    /// e.g. `divisi fallback set claude:work claude:personal codex`.
    /// Replaces any existing chain starting with the same first entry.
    Set {
        #[arg(required = true, num_args = 2..)]
        chain: Vec<String>,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    /// Removes the chain whose first entry matches `first`.
    Remove { first: String },
}

#[derive(Subcommand)]
enum TaskHookCommand {
    /// Adds one hook: `divisi task-hook add --on completed --on failed --command '...'`.
    /// `--command` receives the task's JSON payload on stdin.
    Add {
        /// Repeatable; one of: all, completed, failed, cancelled.
        #[arg(long = "on", required = true)]
        on: Vec<String>,
        #[arg(long)]
        command: String,
        /// Only fire for this agent.
        #[arg(long)]
        agent: Option<String>,
        /// Only fire for this workspace (project) name.
        #[arg(long)]
        workspace: Option<String>,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    /// Removes every hook whose command matches exactly.
    Remove { command: String },
    /// Runs one already-configured hook (matched by its exact `command`)
    /// against a synthetic payload and blocks until it finishes, so you
    /// can confirm it actually works before relying on it live.
    Test { command: String },
}

#[derive(Subcommand)]
enum WebCommand {
    /// Lists every pattern doc in the library, grouped by category.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Case-insensitive search over pattern name/category/content.
    Search {
        query: String,
        #[arg(long)]
        json: bool,
    },
}

fn print_patterns(patterns: &[divisi_web::PatternInfo], json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(patterns).unwrap());
        return;
    }
    if patterns.is_empty() {
        println!("(no patterns found — see docs/web-capability-pack-architecture.md for where they're expected: ~/.config/divisi/skills/web/premium-web/patterns/)");
        return;
    }
    for p in patterns {
        let cat = if p.category.is_empty() { "-" } else { p.category.as_str() };
        println!("{:<14} {:<22} {}", cat, p.name, p.summary);
    }
}

/// Parses `agent` or `agent:account` into an `AgentAccountRef`.
fn parse_agent_account(s: &str) -> divisi_protocol::AgentAccountRef {
    match s.split_once(':') {
        Some((agent, account)) => divisi_protocol::AgentAccountRef { agent: agent.to_string(), account: Some(account.to_string()) },
        None => divisi_protocol::AgentAccountRef { agent: s.to_string(), account: None },
    }
}

#[derive(Subcommand)]
enum TaskCommand {
    /// Run a prompt against a real agent CLI and block until it finishes.
    Run {
        description: String,
        #[arg(long)]
        agent: String,
        /// Directory to run in; defaults to the current directory.
        #[arg(long)]
        cwd: Option<String>,
        /// Isolate the run in a new git worktree + branch (requires `cwd` to be inside a git repo).
        #[arg(long)]
        worktree: bool,
        /// Run as this captured account (isolated $HOME — see `divisi account capture`)
        /// instead of the real one, so multiple accounts of the same agent can run concurrently.
        #[arg(long)]
        account: Option<String>,
        /// Skip divisi's isolated $HOME and run against your real, ambient one instead —
        /// for tasks that need to actually touch your real system (dotfiles, packages, desktop
        /// config), not a sandboxed copy. The agent gets full access to your real credentials
        /// and files; only use this when that's exactly what you want.
        #[arg(long)]
        real_home: bool,
        /// Skip prepending relevant memory + unread agent notes to the prompt
        /// (on by default — see `divisi-runtime::task::build_context_preamble`).
        #[arg(long)]
        no_memory_context: bool,
        #[arg(long, default_value = "300")]
        timeout_secs: u64,
        /// Start the task and return immediately with its id instead of
        /// blocking until the agent finishes — poll `task inspect`/`task
        /// list` for progress, `task cancel` to stop it early.
        #[arg(long)]
        background: bool,
        /// When this run fails or times out in a way that looks like a
        /// rate limit and a fallback chain is configured for this
        /// agent/account (see `divisi fallback set`), automatically mark
        /// the account rate-limited and start a linked follow-up task
        /// against the chain's next entry. Off by default.
        #[arg(long)]
        allow_fallback: bool,
        /// Run the agent in a structured-output mode that reports real
        /// token usage where one exists (currently only `claude`, via
        /// `--output-format json`). Otherwise a no-op — the task's token
        /// counts are parse-or-estimated.
        #[arg(long)]
        usage_json: bool,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    Inspect {
        id: i64,
        #[arg(long)]
        json: bool,
    },
    /// Stops a task started with `--background` before it finishes on its own.
    Cancel {
        id: i64,
        /// Force-clear a row stuck non-terminal with no live process (a
        /// daemon-crash zombie) — marks it failed.
        #[arg(long)]
        force: bool,
    },
    /// Removes a finished task's git worktree and any leftover live-output file.
    Cleanup {
        id: i64,
        /// Clean up even a still-running/created row, marking it failed first.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ProfileCommand {
    List,
    Use { name: String },
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Report whether divisid is currently running.
    Status,
    /// Ask the running daemon to exit gracefully. A no-op if it isn't running.
    Stop,
    /// Stop the daemon if running, then start a fresh one that inherits
    /// this command's current environment — run this after installing a
    /// new agent CLI or changing PATH/shell rc files so detection picks
    /// it up without needing a reboot or logout.
    Restart,
}

#[derive(Subcommand)]
enum NotchCommand {
    /// Start the notch HUD companion process (spawns it if not already alive).
    Enable,
    /// Stop the notch HUD companion process, if running.
    Disable,
    /// Report whether the notch HUD is running.
    Status,
    /// Force the HUD to show its expanded card (not yet wired — Phase 5).
    Show,
    /// Force the HUD to collapse (not yet wired — Phase 5).
    Hide,
}

fn main() -> anyhow::Result<()> {
    divisi_core::env::adopt_legacy_env();
    let cli = Cli::parse();
    let dirs = DivisiDirs::discover()?;
    dirs.ensure_created()?;
    let socket_path = dirs.socket_path();

    let Some(command) = cli.command else {
        daemon::ensure_running(&dirs)?;
        return divisi_tui::run(dirs);
    };

    // Self-update needs no runtime/socket at all — it just talks to
    // GitHub and replaces files next to the current executable.
    if let Command::Update {
        channel,
        check,
        yes,
    } = command
    {
        return run_update(&channel, check, yes);
    }

    // Pure local logic — no daemon/socket needed.
    if let Command::Internal(InternalCommand::PrintBootstrapScript) = command {
        print_bootstrap_script();
        return Ok(());
    }
    // Runs synchronously inside Claude Code's own hook lifecycle, blocking
    // it — must not depend on a daemon being reachable, so this talks to
    // divisi's state database directly, same as divisi-gateway's gateway.
    if let Command::Internal(InternalCommand::ClaudePreToolUseHook) = command {
        return run_claude_pretooluse_hook();
    }
    if let Command::Internal(InternalCommand::GenerateLspPluginManifest { output_dir }) = command {
        let dirs = divisi_core::DivisiDirs::discover()?;
        let specs = divisi_core::lsp::load(&dirs.lsp_registry_file())?;
        internal_lsp_manifest::write_to(std::path::Path::new(&output_dir), &specs)?;
        println!("wrote divisi-lsp plugin manifest to {output_dir}");
        return Ok(());
    }

    // The ACP bridge is a long-running stdio server, not a one-shot socket
    // request — it manages its own socket calls per ACP method.
    if let Command::Acp = command {
        return acp::run(socket_path);
    }
    // Likewise the OpenAI proxy — a long-running HTTP server.
    if let Command::Serve { openai, addr, agent, timeout_secs, api_key, allow_remote } = command {
        if !openai {
            anyhow::bail!("`divisi serve` currently supports only --openai");
        }
        return serve_openai::run(serve_openai::Config {
            socket_path,
            addr,
            agent,
            timeout_secs,
            api_key,
            allow_remote,
        });
    }

    // Interactive login needs the user's real terminal (browser OAuth
    // round-trips, device codes, password prompts) attached directly —
    // routing it through the daemon over the socket would mean the
    // daemon's stdio, not the user's, which may not even be a TTY. Runs
    // entirely locally: no socket round-trip needed to resolve the
    // isolated home path either (divisi_core::agent_home is pure logic).
    if let Command::Agent {
        action: AgentCommand::Login { name },
    } = &command
    {
        return run_agent_login(&dirs, &socket_path, name);
    }

    match command {
        Command::Status => {
            let response = client::send(&socket_path, Request::Status)?;
            render::print(response, false);
        }
        Command::Daemon { action } => match action {
            DaemonCommand::Status => {
                if daemon::is_running(&dirs) {
                    println!("divisid is running ({})", socket_path.display());
                } else {
                    println!("divisid is not running");
                }
            }
            DaemonCommand::Stop => {
                if daemon::stop_running(&dirs)? {
                    println!("divisid stopped");
                } else {
                    println!("divisid was not running");
                }
            }
            DaemonCommand::Restart => {
                daemon::stop_running(&dirs)?;
                daemon::ensure_running(&dirs)?;
                println!("divisid restarted");
            }
        },
        Command::Notch { action } => match action {
            NotchCommand::Enable if notch_proc::is_gnome() => {
                daemon::ensure_running(&dirs)?;
                match notch_proc::gnome_enable()? {
                    notch_proc::GnomeState::Active => println!("divisi-notch enabled (GNOME Shell extension active)"),
                    _ => println!(
                        "divisi-notch installed and set to load. GNOME on Wayland only discovers new extensions at login: log out and back in once, then the notch appears on the right edge."
                    ),
                }
            }
            NotchCommand::Disable if notch_proc::is_gnome() => {
                notch_proc::gnome_disable()?;
                println!("divisi-notch disabled");
            }
            NotchCommand::Status if notch_proc::is_gnome() => {
                println!("gnome_extension={}", match notch_proc::gnome_state() {
                    notch_proc::GnomeState::Active => "active",
                    notch_proc::GnomeState::NeedsRelogin => "installed (log out and back in to load)",
                    notch_proc::GnomeState::Disabled => "disabled",
                });
            }
            NotchCommand::Show | NotchCommand::Hide if notch_proc::is_gnome() => {
                println!("divisi-notch: on GNOME the notch is hover-driven; show/hide apply only to the layer-shell HUD");
            }
            NotchCommand::Enable => {
                daemon::ensure_running(&dirs)?;
                notch_proc::spawn(&dirs)?;
                match notch_proc::read_pid(&dirs) {
                    Some(pid) => println!("divisi-notch enabled (pid {pid})"),
                    None => println!("divisi-notch enabled"),
                }
            }
            NotchCommand::Disable => {
                if !notch_proc::is_alive(&dirs) {
                    println!("divisi-notch was not running");
                } else {
                    // Prefer a clean quit over the control socket -- lets
                    // the HUD process exit its own event loop and remove
                    // its pidfile itself; SIGTERM (notch_proc::stop) is
                    // the fallback for a HUD that isn't listening (stub
                    // mode, or the control socket genuinely down).
                    if notch_proc::send_command(&dirs, "quit").is_ok() {
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }
                    if notch_proc::is_alive(&dirs) {
                        notch_proc::stop(&dirs)?;
                    }
                    println!("divisi-notch disabled");
                }
            }
            NotchCommand::Status => {
                if notch_proc::is_alive(&dirs) {
                    let pid = notch_proc::read_pid(&dirs).expect("is_alive implies a pidfile");
                    println!("hud_alive=true pid={pid}");
                } else {
                    println!("hud_alive=false");
                }
            }
            NotchCommand::Show => {
                notch_proc::send_command(&dirs, "show")?;
                println!("divisi-notch: show sent");
            }
            NotchCommand::Hide => {
                notch_proc::send_command(&dirs, "hide")?;
                println!("divisi-notch: hide sent");
            }
        },
        Command::Migrate { apply } => migrate_cmd::run(apply)?,
        Command::Doctor { fix } => {
            let response = client::send(&socket_path, Request::Doctor { fix })?;
            render::print(response, false);
        }
        Command::Setup { yes, json } => {
            if !yes {
                eprintln!("Dry run (pass --yes to actually install and configure). This may run vendor install scripts over the network.");
            }
            let response = client::send(&socket_path, Request::Setup { dry_run: !yes })?;
            render::print(response, json);
        }
        Command::Agent { action } => match action {
            AgentCommand::List { json } => {
                let response = client::send(&socket_path, Request::AgentList)?;
                render::print(response, json);
            }
            AgentCommand::Inspect { name, json } => {
                let response = client::send(&socket_path, Request::AgentInspect { name })?;
                render::print(response, json);
            }
            AgentCommand::Install { name, yes, json } => {
                if !yes {
                    eprintln!("Dry run (pass --yes to actually run the install command).");
                }
                let response = client::send(
                    &socket_path,
                    Request::AgentInstall {
                        name,
                        dry_run: !yes,
                    },
                )?;
                render::print(response, json);
            }
            AgentCommand::Login { .. } => {
                unreachable!("Command::Agent{{Login}} is intercepted before this match")
            }
            AgentCommand::Docker { action } => match action {
                AgentDockerCommand::Enable { agent, account } => {
                    let response =
                        client::send(&socket_path, Request::DockerEnable { agent, account })?;
                    render::print(response, false);
                }
                AgentDockerCommand::Disable { agent, account } => {
                    let response =
                        client::send(&socket_path, Request::DockerDisable { agent, account })?;
                    render::print(response, false);
                }
                AgentDockerCommand::Status { agent } => {
                    let response = client::send(&socket_path, Request::DockerStatus { agent })?;
                    render::print(response, false);
                }
                AgentDockerCommand::Stop { agent, account } => {
                    let response =
                        client::send(&socket_path, Request::DockerStop { agent, account })?;
                    render::print(response, false);
                }
            },
            AgentCommand::Hooks { action } => match action {
                AgentHooksCommand::Enable { agent } => {
                    let response = client::send(&socket_path, Request::HooksEnable { agent })?;
                    render::print(response, false);
                }
                AgentHooksCommand::Disable { agent } => {
                    let response = client::send(&socket_path, Request::HooksDisable { agent })?;
                    render::print(response, false);
                }
                AgentHooksCommand::Status => {
                    let response = client::send(&socket_path, Request::HooksStatus)?;
                    render::print(response, false);
                }
            },
        },
        Command::Mcp { action } => match action {
            McpCommand::List { json } => {
                let response = client::send(&socket_path, Request::McpList)?;
                render::print(response, json);
            }
            McpCommand::Add {
                name,
                command,
                secrets,
                args,
            } => {
                let mut secret_env = BTreeMap::new();
                for (env_var, value) in secrets {
                    let secret_key = format!("mcp:{name}:{env_var}");
                    let response = client::send(
                        &socket_path,
                        Request::SecretSet {
                            name: secret_key.clone(),
                            value,
                        },
                    )?;
                    render::print(response, false); // exits on failure, prints nothing on success
                    secret_env.insert(env_var, secret_key);
                }
                let server = McpServerSpec {
                    name,
                    command,
                    args,
                    env: BTreeMap::new(),
                    secret_env,
                    enabled: true,
                };
                let response = client::send(&socket_path, Request::McpAdd { server })?;
                render::print(response, false);
            }
            McpCommand::Remove { name } => {
                let response = client::send(&socket_path, Request::McpRemove { name })?;
                render::print(response, false);
            }
            McpCommand::Enable { name } => {
                let response = client::send(&socket_path, Request::McpEnable { name })?;
                render::print(response, false);
            }
            McpCommand::Disable { name } => {
                let response = client::send(&socket_path, Request::McpDisable { name })?;
                render::print(response, false);
            }
            McpCommand::EnableAll { dry_run } => {
                let Response::Ok {
                    data: ResponseData::McpServers(servers),
                } = client::send(&socket_path, Request::McpList)?
                else {
                    anyhow::bail!("failed to list mcp servers");
                };
                let mut enabled = Vec::new();
                let mut skipped_needs_auth = Vec::new();
                for s in servers.into_iter().filter(|s| !s.enabled) {
                    let Response::Ok {
                        data: ResponseData::McpServer(spec),
                    } = client::send(&socket_path, Request::McpInspect { name: s.name.clone() })?
                    else {
                        continue;
                    };
                    let mut has_all_secrets = true;
                    for secret_key in spec.secret_env.values() {
                        let has_value = matches!(
                            client::send(
                                &socket_path,
                                Request::SecretGet {
                                    name: secret_key.clone()
                                }
                            )?,
                            Response::Ok {
                                data: ResponseData::SecretValue(Some(_))
                            }
                        );
                        if !has_value {
                            has_all_secrets = false;
                            break;
                        }
                    }
                    if !has_all_secrets {
                        skipped_needs_auth.push(s.name);
                        continue;
                    }
                    if !dry_run {
                        client::send(&socket_path, Request::McpEnable { name: s.name.clone() })?;
                    }
                    enabled.push(s.name);
                }
                let verb = if dry_run { "would enable" } else { "enabled" };
                println!("{verb} {} mcp server(s): {}", enabled.len(), enabled.join(", "));
                if !skipped_needs_auth.is_empty() {
                    println!(
                        "left {} disabled (missing a required secret — see `divisi secret set`): {}",
                        skipped_needs_auth.len(),
                        skipped_needs_auth.join(", ")
                    );
                }
            }
            McpCommand::Inspect { name, json } => {
                let response = client::send(&socket_path, Request::McpInspect { name })?;
                render::print(response, json);
            }
            McpCommand::Presets => {
                let response = client::send(&socket_path, Request::McpPresetList)?;
                render::print(response, false);
            }
            McpCommand::AddPreset { name } => {
                let response = client::send(&socket_path, Request::McpAddPreset { name })?;
                render::print(response, false);
            }
            McpCommand::Gateway { action } => match action {
                McpGatewayCommand::Enable => {
                    let response = client::send(
                        &socket_path,
                        Request::McpGatewaySetEnabled { enabled: true },
                    )?;
                    render::print(response, false);
                    eprintln!("run `divisi install-integrations --yes` to apply this to agents' native config.");
                }
                McpGatewayCommand::Disable => {
                    let response = client::send(
                        &socket_path,
                        Request::McpGatewaySetEnabled { enabled: false },
                    )?;
                    render::print(response, false);
                    eprintln!("run `divisi install-integrations --yes` to apply this to agents' native config.");
                }
                McpGatewayCommand::Status => {
                    let response = client::send(&socket_path, Request::McpGatewayStatus)?;
                    render::print(response, false);
                }
            },
        },
        Command::Lsp { action } => match action {
            LspCommand::List { json } => {
                let response = client::send(&socket_path, Request::LspList)?;
                render::print(response, json);
            }
            LspCommand::Add {
                name,
                command,
                extensions,
                args,
            } => {
                let server = LspServerSpec {
                    name,
                    command,
                    args,
                    extensions,
                    enabled: true,
                };
                let response = client::send(&socket_path, Request::LspAdd { server })?;
                render::print(response, false);
            }
            LspCommand::Remove { name } => {
                let response = client::send(&socket_path, Request::LspRemove { name })?;
                render::print(response, false);
            }
            LspCommand::Enable { name } => {
                let response = client::send(&socket_path, Request::LspEnable { name })?;
                render::print(response, false);
            }
            LspCommand::Disable { name } => {
                let response = client::send(&socket_path, Request::LspDisable { name })?;
                render::print(response, false);
            }
            LspCommand::EnableAll { dry_run } => {
                let Response::Ok {
                    data: ResponseData::LspServers(servers),
                } = client::send(&socket_path, Request::LspList)?
                else {
                    anyhow::bail!("failed to list lsp servers");
                };
                let mut enabled = Vec::new();
                for s in servers.into_iter().filter(|s| !s.enabled) {
                    if !dry_run {
                        client::send(&socket_path, Request::LspEnable { name: s.name.clone() })?;
                    }
                    enabled.push(s.name);
                }
                let verb = if dry_run { "would enable" } else { "enabled" };
                println!("{verb} {} lsp server(s): {}", enabled.len(), enabled.join(", "));
            }
            LspCommand::Inspect { name, json } => {
                let response = client::send(&socket_path, Request::LspInspect { name })?;
                render::print(response, json);
            }
            LspCommand::Presets => {
                let response = client::send(&socket_path, Request::LspPresetList)?;
                render::print(response, false);
            }
            LspCommand::AddPreset { name } => {
                let response = client::send(&socket_path, Request::LspAddPreset { name })?;
                render::print(response, false);
            }
        },
        Command::Tool { action } => match action {
            ToolCommand::List { json } => {
                let response = client::send(&socket_path, Request::ToolList)?;
                render::print(response, json);
            }
            ToolCommand::Add {
                name,
                description,
                risk,
            } => {
                let risk_level = match risk {
                    RiskArg::Low => RiskLevel::Low,
                    RiskArg::Medium => RiskLevel::Medium,
                    RiskArg::High => RiskLevel::High,
                };
                let tool = ToolSpec {
                    name,
                    description,
                    risk_level,
                    enabled: true,
                };
                let response = client::send(&socket_path, Request::ToolAdd { tool })?;
                render::print(response, false);
            }
            ToolCommand::Inspect { name, json } => {
                let response = client::send(&socket_path, Request::ToolInspect { name })?;
                render::print(response, json);
            }
            ToolCommand::Enable { name } => {
                let response = client::send(&socket_path, Request::ToolEnable { name })?;
                render::print(response, false);
            }
            ToolCommand::Disable { name } => {
                let response = client::send(&socket_path, Request::ToolDisable { name })?;
                render::print(response, false);
            }
        },
        Command::Secret { action } => match action {
            SecretCommand::List => {
                let response = client::send(&socket_path, Request::SecretList)?;
                render::print(response, false);
            }
            SecretCommand::Set { name, value } => {
                let response = client::send(&socket_path, Request::SecretSet { name, value })?;
                render::print(response, false);
            }
            SecretCommand::Get { name } => {
                let response = client::send(&socket_path, Request::SecretGet { name })?;
                render::print(response, false);
            }
            SecretCommand::Delete { name } => {
                let response = client::send(&socket_path, Request::SecretDelete { name })?;
                render::print(response, false);
            }
            SecretCommand::PromoteAlias { alias, name, yes } => {
                if !yes {
                    eprint!("promote {alias} to secret \"{name}\"? [y/N] ");
                    use std::io::Write as _;
                    std::io::stderr().flush().ok();
                    let mut answer = String::new();
                    std::io::stdin().read_line(&mut answer)?;
                    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
                        eprintln!("cancelled");
                        return Ok(());
                    }
                }
                let response = client::send(&socket_path, Request::SecretPromoteAlias { alias, name })?;
                render::print(response, false);
            }
        },
        Command::Skill { action } => match action {
            SkillCommand::List => {
                let response = client::send(&socket_path, Request::SkillList)?;
                render::print(response, false);
            }
            SkillCommand::Install { name, source_path } => {
                let response =
                    client::send(&socket_path, Request::SkillInstall { name, source_path })?;
                render::print(response, false);
            }
            SkillCommand::Remove { name } => {
                let response = client::send(&socket_path, Request::SkillRemove { name })?;
                render::print(response, false);
            }
            SkillCommand::Inspect { name } => {
                let response = client::send(&socket_path, Request::SkillInspect { name })?;
                render::print(response, false);
            }
            SkillCommand::SyncClaude { name } => {
                let response = client::send(&socket_path, Request::SkillSyncClaude { name })?;
                render::print(response, false);
            }
            SkillCommand::Starters => {
                let response = client::send(&socket_path, Request::SkillStarterList)?;
                render::print(response, false);
            }
            SkillCommand::InstallStarter { name } => {
                let response = client::send(&socket_path, Request::SkillInstallStarter { name })?;
                render::print(response, false);
            }
        },
        Command::Memory { action } => match action {
            MemoryCommand::Store {
                title,
                content,
                scope,
                source,
                project,
                agent,
                task,
                confidence,
                expires_in,
            } => {
                let response = client::send(
                    &socket_path,
                    Request::MemoryStore {
                        scope: scope.map(Into::into),
                        source: source.map(Into::into),
                        project,
                        agent,
                        task,
                        title,
                        content,
                        confidence,
                        expires_in_seconds: expires_in,
                    },
                )?;
                render::print(response, false);
            }
            MemoryCommand::Search {
                query,
                scope,
                project,
                semantic,
                limit,
                json,
            } => {
                let request = if semantic {
                    Request::MemorySearchSemantic {
                        query,
                        scope: scope.map(Into::into),
                        project,
                        limit,
                    }
                } else {
                    Request::MemorySearch {
                        query,
                        scope: scope.map(Into::into),
                        project,
                    }
                };
                let response = client::send(&socket_path, request)?;
                render::print(response, json);
            }
            MemoryCommand::Get { id, json } => {
                let response = client::send(&socket_path, Request::MemoryGet { id })?;
                render::print(response, json);
            }
            MemoryCommand::Delete { id } => {
                let response = client::send(&socket_path, Request::MemoryDelete { id })?;
                render::print(response, false);
            }
            MemoryCommand::List { scope, json } => {
                let response = client::send(
                    &socket_path,
                    Request::MemoryList {
                        scope: scope.map(Into::into),
                    },
                )?;
                render::print(response, json);
            }
            MemoryCommand::Graph { action } => match action {
                KgCommand::CreateEntity { name, entity_type } => {
                    let response =
                        client::send(&socket_path, Request::KgCreateEntity { name, entity_type })?;
                    render::print(response, false);
                }
                KgCommand::AddObservation { entity, content } => {
                    let response =
                        client::send(&socket_path, Request::KgAddObservation { entity, content })?;
                    render::print(response, false);
                }
                KgCommand::CreateRelation {
                    from,
                    to,
                    relation_type,
                } => {
                    let response = client::send(
                        &socket_path,
                        Request::KgCreateRelation {
                            from,
                            to,
                            relation_type,
                        },
                    )?;
                    render::print(response, false);
                }
                KgCommand::DeleteEntity { name } => {
                    let response = client::send(&socket_path, Request::KgDeleteEntity { name })?;
                    render::print(response, false);
                }
                KgCommand::Get { name, json } => {
                    let response = client::send(&socket_path, Request::KgGetEntity { name })?;
                    render::print(response, json);
                }
                KgCommand::Query { term, json } => {
                    let response = client::send(&socket_path, Request::KgQuery { term })?;
                    render::print(response, json);
                }
                KgCommand::Show { json } => {
                    let response = client::send(&socket_path, Request::KgReadGraph)?;
                    render::print(response, json);
                }
            },
            MemoryCommand::Cache { action } => match action {
                CacheCommand::Set {
                    key,
                    value,
                    ttl_secs,
                } => {
                    let response = client::send(
                        &socket_path,
                        Request::CacheSet {
                            key,
                            value,
                            ttl_secs,
                        },
                    )?;
                    render::print(response, false);
                }
                CacheCommand::Get { key } => {
                    let response = client::send(&socket_path, Request::CacheGet { key })?;
                    render::print(response, false);
                }
                CacheCommand::Delete { key } => {
                    let response = client::send(&socket_path, Request::CacheDelete { key })?;
                    render::print(response, false);
                }
                CacheCommand::List { pattern } => {
                    let response = client::send(&socket_path, Request::CacheList { pattern })?;
                    render::print(response, false);
                }
                CacheCommand::Status => {
                    let response = client::send(&socket_path, Request::CacheStatus)?;
                    render::print(response, false);
                }
            },
            MemoryCommand::Vector { action } => match action {
                VectorCommand::Upsert {
                    collection,
                    id,
                    vector,
                    payload,
                } => {
                    let payload: serde_json::Value = serde_json::from_str(&payload)?;
                    let response = client::send(
                        &socket_path,
                        Request::VectorUpsert {
                            collection,
                            id,
                            vector,
                            payload,
                        },
                    )?;
                    render::print(response, false);
                }
                VectorCommand::Search {
                    collection,
                    vector,
                    limit,
                } => {
                    let response = client::send(
                        &socket_path,
                        Request::VectorSearch {
                            collection,
                            vector,
                            limit,
                        },
                    )?;
                    render::print(response, false);
                }
                VectorCommand::Delete { collection, id } => {
                    let response =
                        client::send(&socket_path, Request::VectorDelete { collection, id })?;
                    render::print(response, false);
                }
                VectorCommand::Status => {
                    let response = client::send(&socket_path, Request::VectorStatus)?;
                    render::print(response, false);
                }
            },
        },
        Command::Approval { action } => match action {
            ApprovalCommand::List => {
                let response = client::send(&socket_path, Request::ApprovalList)?;
                render::print(response, false);
            }
            ApprovalCommand::Resolve {
                id,
                allow,
                deny,
                remember,
            } => {
                if !allow && !deny {
                    anyhow::bail!("pass --allow or --deny");
                }
                let response = client::send(
                    &socket_path,
                    Request::ApprovalResolve {
                        id,
                        allow,
                        remember,
                    },
                )?;
                render::print(response, false);
            }
        },
        Command::Preference { action } => match action {
            PreferenceCommand::List => {
                let response = client::send(&socket_path, Request::PreferenceList)?;
                render::print(response, false);
            }
        },
        Command::Note { action } => match action {
            NoteCommand::Leave {
                content,
                from,
                to,
                topic,
                project,
            } => {
                let response = client::send(
                    &socket_path,
                    Request::NoteLeave {
                        project,
                        from_agent: from,
                        to_agent: to,
                        topic,
                        content,
                    },
                )?;
                render::print(response, false);
            }
            NoteCommand::Inbox {
                to,
                project,
                unread_only,
                json,
            } => {
                let response = client::send(
                    &socket_path,
                    Request::NoteInbox {
                        project,
                        to_agent: to,
                        unread_only,
                    },
                )?;
                render::print(response, json);
            }
            NoteCommand::MarkRead { id } => {
                let response = client::send(&socket_path, Request::NoteMarkRead { id })?;
                render::print(response, false);
            }
        },
        Command::Doc { action } => match action {
            DocCommand::Ingest {
                path,
                project,
                title,
            } => {
                let path = std::fs::canonicalize(&path)
                    .map(|p| p.display().to_string())
                    .unwrap_or(path);
                let response = client::send(
                    &socket_path,
                    Request::DocumentIngest {
                        path,
                        project,
                        title,
                    },
                )?;
                render::print(response, false);
            }
            DocCommand::List { project, json } => {
                let response = client::send(&socket_path, Request::DocumentList { project })?;
                render::print(response, json);
            }
            DocCommand::Show { id, json } => {
                let response = client::send(&socket_path, Request::DocumentGet { id })?;
                render::print(response, json);
            }
        },
        Command::Context { cwd, json } => {
            let cwd = cwd.unwrap_or_else(|| ".".to_string());
            let cwd = std::fs::canonicalize(&cwd)
                .map(|p| p.display().to_string())
                .unwrap_or(cwd);
            let response = client::send(&socket_path, Request::ContextShow { cwd })?;
            render::print(response, json);
        }
        Command::Task { action } => match action {
            TaskCommand::Run {
                description,
                agent,
                cwd,
                worktree,
                account,
                real_home,
                no_memory_context,
                timeout_secs,
                background,
                allow_fallback,
                usage_json,
                json,
            } => {
                let cwd = cwd.unwrap_or_else(|| ".".to_string());
                let cwd = std::fs::canonicalize(&cwd)
                    .map(|p| p.display().to_string())
                    .unwrap_or(cwd);
                if real_home {
                    eprintln!("warning: running against your real $HOME — {agent} will see your real credentials/config and can modify real files.");
                }
                let response = client::send(
                    &socket_path,
                    Request::TaskRun {
                        description,
                        agent,
                        cwd,
                        use_worktree: worktree,
                        account,
                        real_home,
                        no_memory_context,
                        timeout_secs,
                        background,
                        allow_fallback,
                        usage_json,
                    },
                )?;
                render::print(response, json);
            }
            TaskCommand::List { json } => {
                let response = client::send(&socket_path, Request::TaskList)?;
                render::print(response, json);
            }
            TaskCommand::Inspect { id, json } => {
                let response = client::send(&socket_path, Request::TaskInspect { id })?;
                render::print(response, json);
            }
            TaskCommand::Cancel { id, force } => {
                let response = client::send(&socket_path, Request::TaskCancel { id, force })?;
                render::print(response, false);
            }
            TaskCommand::Cleanup { id, force } => {
                let response = client::send(&socket_path, Request::TaskCleanup { id, force })?;
                render::print(response, false);
            }
        },
        Command::Workspace { action } => match action {
            WorkspaceCommand::List { json } => {
                let response = client::send(&socket_path, Request::WorkspaceList)?;
                render::print(response, json);
            }
        },
        Command::Fallback { action } => match action {
            FallbackCommand::Set { chain } => {
                let chain: Vec<_> = chain.iter().map(|s| parse_agent_account(s)).collect();
                let response = client::send(&socket_path, Request::FallbackSet { chain })?;
                render::print(response, false);
            }
            FallbackCommand::List { json } => {
                let response = client::send(&socket_path, Request::FallbackList)?;
                render::print(response, json);
            }
            FallbackCommand::Remove { first } => {
                let response = client::send(&socket_path, Request::FallbackRemove { first: parse_agent_account(&first) })?;
                render::print(response, false);
            }
        },
        Command::TaskHook { action } => match action {
            TaskHookCommand::Add { on, command, agent, workspace } => {
                let response = client::send(&socket_path, Request::TaskHookAdd { on, command, agent, workspace })?;
                render::print(response, false);
            }
            TaskHookCommand::List { json } => {
                let response = client::send(&socket_path, Request::TaskHookList)?;
                render::print(response, json);
            }
            TaskHookCommand::Remove { command } => {
                let response = client::send(&socket_path, Request::TaskHookRemove { command })?;
                render::print(response, false);
            }
            TaskHookCommand::Test { command } => {
                let response = client::send(&socket_path, Request::TaskHookTest { command })?;
                render::print(response, false);
            }
        },
        Command::Web { action } => {
            let dirs = DivisiDirs::discover()?;
            let patterns_dir = dirs.skills_dir().join("web").join("premium-web").join("patterns");
            match action {
                WebCommand::List { json } => {
                    let patterns = divisi_web::list_patterns(&patterns_dir)?;
                    print_patterns(&patterns, json);
                }
                WebCommand::Search { query, json } => {
                    let patterns = divisi_web::search_patterns(&patterns_dir, &query)?;
                    print_patterns(&patterns, json);
                }
            }
        }
        Command::Orchestrate {
            goal,
            agents,
            cwd,
            worktree,
            real_home,
            timeout_secs,
        } => {
            let cwd = cwd.unwrap_or_else(|| ".".to_string());
            let cwd = std::fs::canonicalize(&cwd)
                .map(|p| p.display().to_string())
                .unwrap_or(cwd);
            if real_home {
                eprintln!("warning: running against your real $HOME — every agent in this relay will see your real credentials/config and can modify real files.");
            }
            let response = client::send(
                &socket_path,
                Request::Orchestrate {
                    goal,
                    agents,
                    cwd,
                    use_worktree: worktree,
                    real_home,
                    timeout_secs,
                },
            )?;
            render::print(response, false);
        }
        Command::OrchestrateParallel {
            tasks,
            orchestrator,
            goal,
            candidate_agents,
            cwd,
            real_home,
            timeout_secs,
            background,
        } => {
            let cwd = cwd.unwrap_or_else(|| ".".to_string());
            let cwd = std::fs::canonicalize(&cwd)
                .map(|p| p.display().to_string())
                .unwrap_or(cwd);
            if real_home {
                eprintln!("warning: running against your real $HOME — every agent will see your real credentials/config and can modify real files.");
            }
            let orchestrator = parse_orchestrator(&orchestrator)?;
            if orchestrator == divisi_protocol::OrchestratorMode::Fixed && tasks.is_empty() {
                anyhow::bail!("--orchestrator fixed requires at least one --task");
            }
            if orchestrator != divisi_protocol::OrchestratorMode::Fixed && goal.is_none() {
                anyhow::bail!("--orchestrator {orchestrator:?} requires --goal");
            }
            let parsed: Vec<divisi_protocol::ParallelTaskSpec> = tasks
                .iter()
                .map(|t| parse_parallel_task(t))
                .collect::<anyhow::Result<Vec<_>>>()?;
            let dispatched = parsed.len();
            let response = client::send(
                &socket_path,
                Request::OrchestrateParallel {
                    tasks: parsed,
                    cwd,
                    real_home,
                    timeout_secs,
                    background,
                    orchestrator,
                    goal,
                    candidate_agents,
                },
            )?;
            if background {
                // The daemon returns an empty batch immediately for a
                // background run (each sub-task creates its own row on its
                // own thread) — printing `Relay (0 step(s)):` there reads
                // like nothing ran, so say what actually happened.
                println!(
                    "dispatched {dispatched} sub-task(s) in the background — poll `divisi task list` / `divisi task inspect <id>`"
                );
            } else {
                render::print(response, false);
            }
        }
        Command::OrchestrateGraph {
            tasks,
            orchestrator,
            goal,
            candidate_agents,
            cwd,
            real_home,
            timeout_secs,
            background,
        } => {
            let cwd = cwd.unwrap_or_else(|| ".".to_string());
            let cwd = std::fs::canonicalize(&cwd)
                .map(|p| p.display().to_string())
                .unwrap_or(cwd);
            let orchestrator = parse_orchestrator(&orchestrator)?;
            if orchestrator == divisi_protocol::OrchestratorMode::Fixed && tasks.is_empty() {
                anyhow::bail!("--orchestrator fixed requires at least one --task");
            }
            if orchestrator != divisi_protocol::OrchestratorMode::Fixed && goal.is_none() {
                anyhow::bail!("--orchestrator {orchestrator:?} requires --goal");
            }
            let nodes = tasks
                .into_iter()
                .map(|task| parse_graph_task(&task))
                .collect::<anyhow::Result<Vec<_>>>()?;
            let dispatched = nodes.len();
            let response = client::send(
                &socket_path,
                Request::OrchestrateGraph {
                    nodes,
                    cwd,
                    real_home,
                    timeout_secs,
                    background,
                    orchestrator,
                    goal,
                    candidate_agents,
                },
            )?;
            if background {
                println!(
                    "dispatched a {dispatched}-node graph in the background — poll `divisi task list` / `divisi task inspect <id>`"
                );
            } else {
                render::print(response, false);
            }
        }
        Command::Provider { action } => match action {
            ProviderCommand::Add {
                name,
                env_var,
                base_url,
                models,
            } => {
                let response = client::send(
                    &socket_path,
                    Request::ProviderAdd {
                        name,
                        env_var_name: env_var,
                        base_url,
                        models: models
                            .into_iter()
                            .map(|(id, name)| divisi_protocol::ModelSpec { id, name })
                            .collect(),
                    },
                )?;
                render::print(response, false);
            }
            ProviderCommand::Remove { name } => {
                let response = client::send(&socket_path, Request::ProviderRemove { name })?;
                render::print(response, false);
            }
            ProviderCommand::List { configured, json } => {
                let request = if configured {
                    Request::ConfiguredProviderList
                } else {
                    Request::ProviderList
                };
                let response = client::send(&socket_path, request)?;
                render::print(response, json);
            }
            ProviderCommand::Inspect { name, json } => {
                let response = client::send(&socket_path, Request::ProviderInspect { name })?;
                render::print(response, json);
            }
            ProviderCommand::SetKey { name, value } => {
                let response = client::send(&socket_path, Request::ProviderSetKey { name, value })?;
                render::print(response, false);
            }
            ProviderCommand::Sync { name, agents, yes, real_home } => {
                if !yes {
                    eprintln!("Dry run (pass --yes to actually write the key into agent config files; backups are made either way).");
                }
                let response = client::send(
                    &socket_path,
                    Request::ProviderSync {
                        name,
                        agents,
                        dry_run: !yes,
                        real_home,
                    },
                )?;
                render::print(response, false);
            }
            ProviderCommand::Presets => {
                let response = client::send(&socket_path, Request::ProviderPresetList)?;
                render::print(response, false);
            }
            ProviderCommand::AddPreset { name } => {
                let response = client::send(&socket_path, Request::ProviderAddPreset { name })?;
                render::print(response, false);
            }
            ProviderCommand::AddKey {
                provider,
                label,
                agent,
                value,
            } => {
                let response = client::send(
                    &socket_path,
                    Request::ProviderAddKey {
                        provider,
                        label,
                        agent,
                        value,
                    },
                )?;
                render::print(response, false);
            }
            ProviderCommand::ListKeys { provider } => {
                let response = client::send(&socket_path, Request::ProviderListKeys { provider })?;
                render::print(response, false);
            }
            ProviderCommand::RemoveKey { provider, label } => {
                let response =
                    client::send(&socket_path, Request::ProviderRemoveKey { provider, label })?;
                render::print(response, false);
            }
            ProviderCommand::KeySync {
                provider,
                label,
                agent,
                yes,
            } => {
                if !yes {
                    eprintln!("Dry run (pass --yes to actually write the key into the agent's config file; backups are made either way).");
                }
                let response = client::send(
                    &socket_path,
                    Request::ProviderKeySync {
                        provider,
                        label,
                        agent,
                        dry_run: !yes,
                    },
                )?;
                render::print(response, false);
            }
            ProviderCommand::SetBillingKey { provider, value } => {
                let response = client::send(
                    &socket_path,
                    Request::ProviderSetBillingKey { provider, value },
                )?;
                render::print(response, false);
            }
            ProviderCommand::ListFree { json } => {
                let response = client::send(&socket_path, Request::ProviderListFree)?;
                render::print(response, json);
            }
            ProviderCommand::AddFree { id, key, key_id } => {
                let key = match key {
                    Some(key) => key,
                    None => rpassword::prompt_password(format!("API key for '{id}': "))?,
                };
                let response = client::send(&socket_path, Request::ProviderAddFree { id, key, key_id })?;
                render::print(response, false);
            }
            ProviderCommand::SyncPool => {
                let response = client::send(&socket_path, Request::ProviderSyncPool)?;
                render::print(response, false);
            }
            ProviderCommand::KeyStatus { platform, json } => {
                let response = client::send(&socket_path, Request::ProviderKeyStatus { platform })?;
                render::print(response, json);
            }
            ProviderCommand::Validate { platform, json } => {
                let response = client::send(&socket_path, Request::ProviderValidateKeys { platform })?;
                render::print(response, json);
            }
        },
        Command::Worktree { action } => match action {
            WorktreeCommand::Diff { task_id, json } => {
                let response = client::send(&socket_path, Request::WorktreeMergePreview { task_id })?;
                render::print(response, json);
            }
            WorktreeCommand::Merge { task_id, json } => {
                let response = client::send(&socket_path, Request::WorktreeMergeApply { task_id })?;
                let merged = matches!(
                    response,
                    Response::Ok { data: ResponseData::WorktreeMerged(_) }
                );
                render::print(response, json);
                // The worktree dir and its branch both outlive the merge;
                // nothing else points the user at the cleanup step.
                if merged && !json {
                    println!("\nThe worktree and its branch still exist — `divisi task cleanup {task_id}` removes them.");
                }
            }
        },
        Command::Usage { action } => match action {
            UsageCommand::Show { provider, json } => {
                let response = client::send(&socket_path, Request::UsageShow { provider })?;
                render::print(response, json);
            }
            UsageCommand::Refresh { json } => {
                let response = client::send(&socket_path, Request::UsageRefresh)?;
                render::print(response, json);
            }
        },
        Command::Accounting { action } => match action {
            AccountingCommand::Show {
                execution_id,
                trace_id,
                agent,
                provider,
                event_type,
                json,
            } => {
                let response = client::send(
                    &socket_path,
                    Request::AccountingQuery {
                        query: divisi_protocol::AccountingQuery {
                            execution_id,
                            trace_id,
                            agent,
                            provider,
                            event_type,
                        },
                    },
                )?;
                render::print(response, json);
            }
        },
        Command::Backup { action } => run_backup_command(&dirs, action)?,
        Command::Plugin { action } => match action {
            PluginCommand::Add {
                name,
                target,
                opencode_module,
            } => {
                let response = client::send(
                    &socket_path,
                    Request::PluginAdd {
                        plugin: divisi_protocol::PluginSpec {
                            name,
                            target,
                            opencode_module,
                        },
                    },
                )?;
                render::print(response, false);
            }
            PluginCommand::Remove { name } => {
                let response = client::send(&socket_path, Request::PluginRemove { name })?;
                render::print(response, false);
            }
            PluginCommand::List { json } => {
                let response = client::send(&socket_path, Request::PluginList)?;
                render::print(response, json);
            }
            PluginCommand::Inspect { name, json } => {
                let response = client::send(&socket_path, Request::PluginInspect { name })?;
                render::print(response, json);
            }
            PluginCommand::Sync { name, agents, yes, real_home } => {
                if !yes {
                    eprintln!(
                        "Dry run (pass --yes to actually install the plugin into agent CLIs)."
                    );
                }
                let response = client::send(
                    &socket_path,
                    Request::PluginSync {
                        name,
                        agents,
                        dry_run: !yes,
                        real_home,
                    },
                )?;
                render::print(response, false);
            }
            PluginCommand::Presets => {
                let response = client::send(&socket_path, Request::PluginPresetList)?;
                render::print(response, false);
            }
            PluginCommand::AddPreset { name } => {
                let response = client::send(&socket_path, Request::PluginAddPreset { name })?;
                render::print(response, false);
            }
        },
        Command::Account { action } => match action {
            AccountCommand::Capture { agent, name, label } => {
                let response =
                    client::send(&socket_path, Request::AccountCapture { agent, name, label })?;
                render::print(response, false);
            }
            AccountCommand::Use { agent, name } => {
                let response = client::send(&socket_path, Request::AccountUse { agent, name })?;
                render::print(response, false);
            }
            AccountCommand::List { agent, json } => {
                let response = client::send(&socket_path, Request::AccountList { agent })?;
                render::print(response, json);
            }
            AccountCommand::Remove { agent, name } => {
                let response = client::send(&socket_path, Request::AccountRemove { agent, name })?;
                render::print(response, false);
            }
            AccountCommand::SetStatus {
                agent,
                name,
                status,
            } => {
                let status = divisi_protocol::AccountStatus::parse(&status)
                    .ok_or_else(|| anyhow::anyhow!("invalid status '{status}' (expected: available, rate_limited, needs_topup, unknown)"))?;
                let response = client::send(
                    &socket_path,
                    Request::AccountSetStatus {
                        agent,
                        name,
                        status,
                    },
                )?;
                render::print(response, false);
            }
        },
        Command::Profile { action } => match action {
            ProfileCommand::List => {
                let response = client::send(&socket_path, Request::ProfileList)?;
                render::print(response, false);
            }
            ProfileCommand::Use { name } => {
                let response = client::send(&socket_path, Request::ProfileUse { name })?;
                render::print(response, false);
            }
        },
        Command::InstallIntegrations { yes, json, real_home } => {
            if !yes {
                eprintln!("Dry run (pass --yes to actually write config files; backups are made either way).");
            }
            let response =
                client::send(&socket_path, Request::InstallIntegrations { dry_run: !yes, real_home })?;
            render::print(response, json);
        }
        Command::UninstallIntegrations { yes, real_home } => {
            if !yes {
                anyhow::bail!("this removes divisi-managed MCP entries from every agent's config; pass --yes to confirm");
            }
            let response = client::send(&socket_path, Request::UninstallIntegrations { real_home })?;
            render::print(response, false);
        }
        Command::Session { action } => match action {
            SessionCommand::New { cwd, json } => {
                let cwd = resolve_cwd(cwd);
                let response = client::send(&socket_path, Request::SessionNew { cwd })?;
                render::print(response, json);
            }
            SessionCommand::List { json } => {
                let response = client::send(&socket_path, Request::SessionList)?;
                render::print(response, json);
            }
            SessionCommand::Close { session_id } => {
                let response = client::send(&socket_path, Request::SessionClose { session_id })?;
                render::print(response, false);
            }
        },
        Command::Goal { action } => match action {
            GoalCommand::Submit {
                text,
                session,
                cwd,
                mode,
                max_dispatches,
                max_minutes,
                json,
            } => {
                let text = text.join(" ");
                if text.trim().is_empty() {
                    anyhow::bail!("goal text is empty");
                }
                let session_id = match session {
                    Some(s) => s,
                    None => {
                        let cwd = resolve_cwd(cwd);
                        match client::send(&socket_path, Request::SessionNew { cwd })? {
                            Response::Ok { data: ResponseData::Session(s) } => s.id,
                            Response::Ok { data } => {
                                anyhow::bail!("unexpected response creating session: {data:?}")
                            }
                            Response::Error { message } => anyhow::bail!(message),
                        }
                    }
                };
                let response = client::send(
                    &socket_path,
                    Request::GoalSubmit { session_id, text, mode, max_dispatches, max_minutes, agent: None },
                )?;
                render::print(response, json);
            }
            GoalCommand::Status { goal_id, json } => {
                let response = client::send(&socket_path, Request::GoalStatus { goal_id })?;
                render::print(response, json);
            }
            GoalCommand::List { session, json } => {
                let response = client::send(&socket_path, Request::GoalList { session_id: session })?;
                render::print(response, json);
            }
            GoalCommand::Amend { goal_id, text } => {
                let response =
                    client::send(&socket_path, Request::GoalAmend { goal_id, text: text.join(" ") })?;
                render::print(response, false);
            }
            GoalCommand::Cancel { goal_id } => {
                let response = client::send(&socket_path, Request::GoalCancel { goal_id })?;
                render::print(response, false);
            }
            GoalCommand::Resume { goal_id } => {
                let response = client::send(&socket_path, Request::GoalResume { goal_id })?;
                render::print(response, false);
            }
            GoalCommand::RetryNode { goal_id, node_id } => {
                let response = client::send(&socket_path, Request::GoalRetryNode { goal_id, node_id })?;
                render::print(response, false);
            }
            GoalCommand::Merge(merge_cmd) => match merge_cmd {
                GoalMergeCommand::List { json } => {
                    let response = client::send(&socket_path, Request::GoalMergeList)?;
                    render::print(response, json);
                }
                GoalMergeCommand::Show { id } => {
                    let response = client::send(&socket_path, Request::GoalMergeShow { id })?;
                    render::print(response, false);
                }
                GoalMergeCommand::Confirm { id } => {
                    let response = client::send(&socket_path, Request::GoalMergeResolve { id, allow: true })?;
                    render::print(response, false);
                }
                GoalMergeCommand::Reject { id } => {
                    let response = client::send(&socket_path, Request::GoalMergeResolve { id, allow: false })?;
                    render::print(response, false);
                }
            },
        },
        Command::Coordinator { action } => match action {
            CoordinatorCommand::Status { json } => {
                let response = client::send(&socket_path, Request::CoordinatorStatus)?;
                render::print(response, json);
            }
        },
        Command::Pool { action } => match action {
            PoolCommand::Status { json } => {
                let response = client::send(&socket_path, Request::PoolStatus)?;
                render::print(response, json);
            }
        },
        Command::Loop { text, agent, max_iters, session, cwd, json } => {
            let text = text.join(" ");
            if text.trim().is_empty() {
                anyhow::bail!("loop goal is empty");
            }
            let session_id = match session {
                Some(s) => s,
                None => {
                    let cwd = resolve_cwd(cwd);
                    match client::send(&socket_path, Request::SessionNew { cwd })? {
                        Response::Ok { data: ResponseData::Session(s) } => s.id,
                        Response::Ok { data } => anyhow::bail!("unexpected response creating session: {data:?}"),
                        Response::Error { message } => anyhow::bail!(message),
                    }
                }
            };
            let response = client::send(
                &socket_path,
                Request::GoalSubmit {
                    session_id,
                    text,
                    mode: Some("careful".to_string()),
                    max_dispatches: Some(max_iters),
                    max_minutes: None,
                    agent,
                },
            )?;
            render::print(response, json);
            eprintln!("watch it iterate:  divisi goal status <goal_id>");
        }
        Command::Acp => unreachable!("handled before the socket-based dispatch above"),
        Command::Serve { .. } => unreachable!("handled before the socket-based dispatch above"),
        Command::Update { .. } => unreachable!("handled before the socket-based dispatch above"),
        Command::Internal(_) => unreachable!("handled before the socket-based dispatch above"),
    }

    Ok(())
}

/// Canonicalizes an optional `--cwd`, defaulting to the current directory,
/// falling back to the raw string if it can't be resolved.
fn resolve_cwd(cwd: Option<String>) -> String {
    let cwd = cwd.unwrap_or_else(|| ".".to_string());
    std::fs::canonicalize(&cwd).map(|p| p.display().to_string()).unwrap_or(cwd)
}

/// See `InternalCommand::PrintBootstrapScript`'s doc comment.
fn print_bootstrap_script() {
    println!("#!/bin/sh");
    println!("set -u"); // not -e: one agent's install failing shouldn't abort the rest
    for agent in divisi_core::builtin_registry() {
        let Some(install) = agent.bootstrap_install else {
            continue;
        };
        println!("echo '==> installing {}'", agent.name);
        println!(
            "if ! ( {} ); then echo 'WARN: {} install failed' >&2; fi",
            install.command, agent.name
        );
    }
}

/// See `InternalCommand::ClaudePreToolUseHook`'s doc comment. Contract
/// (stdin/stdout JSON shape) verified against a real installed plugin —
/// see `divisi_agent_sdk::formats::claude_settings`'s module doc.
fn run_claude_pretooluse_hook() -> anyhow::Result<()> {
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let input: serde_json::Value = serde_json::from_str(&input).unwrap_or(serde_json::Value::Null);
    let tool_name = input
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_input = input
        .get("tool_input")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let resource = claude_hook_resource(tool_name, &tool_input);

    let dirs = DivisiDirs::discover()?;
    let rules = divisi_core::permissions::load(&dirs.permissions_file())?;
    let db_path = dirs.db_path();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = rusqlite::Connection::open(&db_path)?;
    divisi_core::preferences::ensure_schema(&conn)?;

    let verdict = divisi_core::preferences::evaluate_and_learn(
        &rules.tools,
        &conn,
        &resource,
        Some("claude PreToolUse hook"),
    )?;
    let output = match verdict {
        divisi_core::preferences::Verdict::Allow => serde_json::json!({}),
        divisi_core::preferences::Verdict::Deny => {
            hook_deny_json("blocked by divisi permission policy")
        }
        divisi_core::preferences::Verdict::PendingApproval(id) => wait_for_approval(&conn, id)?,
    };
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

/// Polls the pending approval created for this call until a human
/// resolves it via `divisi approval resolve` (from another terminal or
/// the TUI) or our own margin under `HOOK_TIMEOUT_SECS` runs out —
/// timing out denies (fail closed) rather than guessing.
fn wait_for_approval(conn: &rusqlite::Connection, id: i64) -> anyhow::Result<serde_json::Value> {
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(
            divisi_core::hooks::CLAUDE_HOOK_TIMEOUT_SECS.saturating_sub(20),
        );
    loop {
        let Some(approval) = divisi_core::preferences::get_approval(conn, id)? else {
            return Ok(hook_deny_json("approval record disappeared"));
        };
        match approval.status {
            divisi_core::preferences::ApprovalStatus::Allowed => return Ok(serde_json::json!({})),
            divisi_core::preferences::ApprovalStatus::Denied => {
                return Ok(hook_deny_json("denied via `divisi approval resolve`"))
            }
            // Consumed by an unrelated evaluate_and_learn call for the
            // same resource before this poll observed the Allowed/Denied
            // state — vanishingly unlikely for the hook-wait path (which
            // polls its own specific approval id, not a shared resource
            // string another caller re-evaluates), but fail closed rather
            // than loop on a status this branch can never see resolve.
            divisi_core::preferences::ApprovalStatus::Used => {
                return Ok(hook_deny_json("approval was already consumed by another caller"))
            }
            divisi_core::preferences::ApprovalStatus::Pending => {
                if std::time::Instant::now() >= deadline {
                    return Ok(hook_deny_json(&format!(
                        "timed out waiting for approval #{id} — run `divisi approval resolve {id} --allow`, then retry"
                    )));
                }
                std::thread::sleep(std::time::Duration::from_millis(1000));
            }
        }
    }
}

fn hook_deny_json(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "deny" },
        "systemMessage": reason
    })
}

/// Builds the `permissions.toml`/learned-preference resource pattern for
/// one Claude tool call. Field names (`tool_input.command`/`file_path`)
/// verified against the same real plugin as `claude_settings.rs`.
fn claude_hook_resource(tool_name: &str, tool_input: &serde_json::Value) -> String {
    match tool_name {
        "Bash" => {
            let command = tool_input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("claude:bash:{command}")
        }
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let path = tool_input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("claude:edit:{path}")
        }
        other => format!("claude:{other}"),
    }
}

fn run_update(channel: &str, check_only: bool, yes: bool) -> anyhow::Result<()> {
    println!("current version: {}", update::current_version());
    let release = update::check_latest(channel)?;
    println!("latest {channel}: {}", release.tag);

    let newer = update::is_newer(update::current_version(), &release.tag);
    match newer {
        Some(false) => {
            println!("already up to date.");
            return Ok(());
        }
        Some(true) => println!("update available: {} -> {}", update::current_version(), release.tag),
        None => println!("(can't compare versions for this tag; the {channel} channel always has the latest build available)"),
    }

    if check_only {
        return Ok(());
    }
    if !yes {
        eprintln!("pass --yes to download and install this update.");
        return Ok(());
    }

    println!("downloading and installing...");
    let install_dir = update::apply(&release)?;
    println!("updated in {}", install_dir.display());
    Ok(())
}

/// Runs entirely in-process against `divisi_core::backup` — deliberately
/// never sends a request over the daemon socket, since the passphrase
/// here protects every live credential divisi knows about (see that
/// module's doc comment for the full reasoning). Prompts with
/// `rpassword::prompt_password` (hidden input, not `--flag <value>`) so
/// the passphrase never lands in shell history or `ps` output.
fn run_backup_command(dirs: &DivisiDirs, action: BackupCommand) -> anyhow::Result<()> {
    match action {
        BackupCommand::Export { path } => {
            let passphrase = rpassword::prompt_password("Backup passphrase: ")?;
            let confirm = rpassword::prompt_password("Confirm passphrase: ")?;
            if passphrase != confirm {
                anyhow::bail!("passphrases didn't match");
            }
            if passphrase.is_empty() {
                anyhow::bail!("passphrase cannot be empty");
            }
            eprintln!(
                "note: if divisid is currently running, stop it first with `divisi daemon stop` \
                 so state/divisi.db isn't captured mid-write."
            );
            let warnings = divisi_core::backup::export(
                dirs,
                std::path::Path::new(&path),
                &age::secrecy::SecretString::from(passphrase),
            )?;
            println!("backup written to {path}");
            for warning in warnings {
                eprintln!("warning: {warning}");
            }
        }
        BackupCommand::Import { path, yes } => {
            let passphrase = rpassword::prompt_password("Backup passphrase: ")?;
            let report = divisi_core::backup::import(
                dirs,
                std::path::Path::new(&path),
                &age::secrecy::SecretString::from(passphrase),
                !yes,
            )?;
            if !yes {
                eprintln!("Dry run (pass --yes to actually write files and restore secrets).");
            }
            println!("files:");
            for item in &report.files {
                let mark = if item.success { "✓" } else { "✗" };
                println!("  {mark} {} — {}", item.path, item.detail);
            }
            println!("secrets:");
            for item in &report.secrets {
                let mark = if item.success { "✓" } else { "✗" };
                println!("  {mark} {} — {}", item.path, item.detail);
            }
            let failed = report
                .files
                .iter()
                .chain(&report.secrets)
                .filter(|i| !i.success)
                .count();
            if failed > 0 {
                eprintln!("{failed} item(s) failed to restore — see above.");
            }
        }
    }
    Ok(())
}

fn run_agent_login(
    dirs: &DivisiDirs,
    socket_path: &std::path::Path,
    agent: &str,
) -> anyhow::Result<()> {
    let mut registry = divisi_core::builtin_registry();
    if let Ok((custom, _errors)) = divisi_core::custom_agents::load_all(&dirs.agents_dir()) {
        registry.extend(
            custom
                .iter()
                .map(divisi_core::custom_agents::to_agent_definition),
        );
    }
    let Some(adapter) =
        divisi_agent_sdk::adapters::for_agent_with_custom(agent, &dirs.agents_dir(), &registry)
    else {
        anyhow::bail!("unknown agent: {agent}");
    };
    if !adapter.discover().detected {
        anyhow::bail!("{agent} is not installed; run `divisi agent install {agent} --yes` first");
    }
    let real_home = divisi_core::paths::real_home_dir()?;

    // `RealRequired` agents (codex, cursor) authenticate against the
    // session-global OS keyring — an isolated `$HOME` can't hold that
    // token, so logging in there just loses it. Log in against the real
    // environment, and don't try to snapshot a per-account credential file
    // that doesn't exist. `divisi task run --agent <a>` already routes
    // these to the real home automatically (see task.rs `forced_real_home`).
    let real_required = divisi_core::builtin_registry()
        .into_iter()
        .find(|a| a.name == agent)
        .map(|a| a.home_requirement == divisi_protocol::HomeRequirement::RealRequired)
        .unwrap_or(false);

    if real_required {
        println!("logging in to {agent} (real environment — its auth is keyring-backed and session-global)...");
        adapter.login(&real_home)?;
        println!("done.");
        if divisi_core::account::has_live_login(&real_home, agent) {
            println!("{agent} is logged in. `divisi task run --agent {agent}` will use it.");
        } else {
            eprintln!("note: {agent} did not report a persisted login afterwards — re-run its own `{agent} login` / status command directly to check.");
        }
        return Ok(());
    }

    let home = divisi_core::agent_home::ensure_bootstrapped(&dirs.homes_dir(), &real_home, agent)?;
    println!(
        "logging in to {agent} (isolated home: {})...",
        home.display()
    );
    adapter.login(&home)?;
    println!("done.");

    auto_capture_after_login(dirs, socket_path, &home, agent);

    println!("run `divisi doctor` or `divisi agent inspect {agent}` to confirm.");
    Ok(())
}

/// Parses the compact graph syntax kept at the CLI boundary so the daemon
/// receives typed protocol data, not a second ad-hoc text language. Commas
/// delimit fields and `|` delimits dependencies; descriptions containing
/// either should use the JSON/delegate path instead.
/// Splits `value` on top-level commas only — a naive `str::split(',')`
/// would also split inside a quoted `desc="build it, then test"`,
/// silently truncating the description at the first internal comma
/// instead of erroring or parsing correctly. Doesn't require the whole
/// field to be quoted, just tracks whether a comma is currently inside an
/// (unescaped) double-quoted span.
fn split_top_level_commas(value: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let mut escaped = false;
    for (i, ch) in value.char_indices() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quotes = false;
            }
            continue;
        }
        match ch {
            '"' => in_quotes = true,
            ',' => {
                fields.push(&value[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    fields.push(&value[start..]);
    fields
}

fn parse_graph_task(value: &str) -> anyhow::Result<divisi_protocol::TaskGraphNode> {
    let mut fields = std::collections::BTreeMap::new();
    for field in split_top_level_commas(value) {
        let (key, field_value) = field
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--task '{value}' must use key=value fields"))?;
        let field_value = field_value.trim();
        // Strip one layer of surrounding double quotes, e.g. desc="build it, then test".
        let field_value = field_value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(field_value);
        fields.insert(key.trim(), field_value);
    }
    let required = |key| {
        fields
            .get(key)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("--task '{value}' is missing {key}=..."))
    };
    let run_if = match fields.get("run_if").copied().unwrap_or("always") {
        "always" => divisi_protocol::RunCondition::Always,
        "on_success" => divisi_protocol::RunCondition::OnSuccess,
        "on_failure" => divisi_protocol::RunCondition::OnFailure,
        other => anyhow::bail!(
            "--task '{value}' has invalid run_if '{other}' (use always, on_success, or on_failure)"
        ),
    };
    let depends_on = fields
        .get("depends_on")
        .map(|deps| {
            deps.split('|')
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(divisi_protocol::TaskGraphNode {
        id: required("id")?.to_string(),
        agent: required("agent")?.to_string(),
        description: required("desc")?.to_string(),
        depends_on,
        run_if,
    })
}

fn parse_orchestrator(value: &str) -> anyhow::Result<divisi_protocol::OrchestratorMode> {
    match value {
        "fixed" => Ok(divisi_protocol::OrchestratorMode::Fixed),
        "auto" => Ok(divisi_protocol::OrchestratorMode::Auto),
        "delegate" => Ok(divisi_protocol::OrchestratorMode::Delegate),
        _ => anyhow::bail!("--orchestrator must be fixed, auto, or delegate"),
    }
}

/// One `--task <agent>:<description>` for `divisi orchestrate-parallel`.
/// Splits on the first `:` only, so a description can itself contain colons.
fn parse_parallel_task(value: &str) -> anyhow::Result<divisi_protocol::ParallelTaskSpec> {
    let (agent, description) = value.split_once(':').ok_or_else(|| {
        anyhow::anyhow!(
            "--task '{value}' must be <agent>:<description>, e.g. claude:\"implement the API\""
        )
    })?;
    if agent.trim().is_empty() || description.trim().is_empty() {
        anyhow::bail!("--task '{value}' has an empty agent or description");
    }
    Ok(divisi_protocol::ParallelTaskSpec {
        agent: agent.to_string(),
        description: description.to_string(),
    })
}

/// After a successful `divisi agent login`, registers this login as a
/// named account automatically — otherwise it only appears in `single
/// account list`/the TUI Accounts tab after a separate manual `single
/// account capture` call, which is easy to forget and was the source of
/// "I logged in but don't see an account" confusion. Best-effort: login
/// itself already succeeded, so a capture failure here (e.g. an agent with
/// no account-switching support, like opencode) is only a warning.
fn auto_capture_after_login(
    dirs: &DivisiDirs,
    socket_path: &std::path::Path,
    home: &std::path::Path,
    agent: &str,
) {
    let label = divisi_core::account::derive_label(home, agent);
    let existing =
        divisi_core::account::list(&dirs.accounts_dir(), Some(agent)).unwrap_or_default();

    let base_name = label.clone().unwrap_or_else(|| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("auto-{secs}")
    });
    let slug: String = base_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut name = slug.clone();
    let mut suffix = 2;
    while existing.iter().any(|p| p.name == name) {
        name = format!("{slug}-{suffix}");
        suffix += 1;
    }

    match client::send(
        socket_path,
        Request::AccountCapture {
            agent: agent.to_string(),
            name: name.clone(),
            label: label.clone(),
        },
    ) {
        Ok(Response::Ok { .. }) => println!("captured as: {}", label.unwrap_or(name)),
        Ok(Response::Error { message }) => {
            eprintln!("note: login succeeded, but auto-capturing an account failed: {message}");
            // Don't suggest a manual retry when the failure is "this agent
            // has no capture support at all" (see divisi_core::account::
            // support) — running the same command by hand would hit the
            // identical wall, not a transient issue worth retrying.
            if !message.contains("isn't implemented for") {
                eprintln!("      run `divisi account capture {agent} <name>` manually if you want this login saved as an account.");
            }
        }
        Err(e) => eprintln!("note: login succeeded, but auto-capturing an account failed: {e:#}"),
    }
}

#[cfg(test)]
mod graph_task_parsing_tests {
    use super::*;

    #[test]
    fn parses_a_description_containing_a_comma_without_truncating_it() {
        let node = parse_graph_task(r#"id=build,agent=codex,desc="build it, then test it",depends_on=lint"#).unwrap();
        assert_eq!(node.id, "build");
        assert_eq!(node.agent, "codex");
        assert_eq!(node.description, "build it, then test it");
        assert_eq!(node.depends_on, vec!["lint".to_string()]);
    }

    #[test]
    fn parses_multiple_dependencies_and_defaults_run_if_to_always() {
        let node = parse_graph_task("id=d,agent=codex,desc=x,depends_on=a|b|c").unwrap();
        assert_eq!(node.depends_on, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(node.run_if, divisi_protocol::RunCondition::Always);
    }

    #[test]
    fn rejects_a_task_missing_a_required_field() {
        assert!(parse_graph_task("id=build,agent=codex").is_err());
    }

    #[test]
    fn split_top_level_commas_ignores_commas_inside_quotes() {
        let parts = split_top_level_commas(r#"a="x,y",b=z"#);
        assert_eq!(parts, vec![r#"a="x,y""#, "b=z"]);
    }

    #[test]
    fn loop_subcommand_parses_flags_and_joins_the_goal() {
        let cli = Cli::try_parse_from([
            "divisi", "loop", "make", "the", "tests", "pass",
            "--agent", "grok", "--max-iters", "10",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Loop { text, agent, max_iters, .. }) => {
                assert_eq!(text.join(" "), "make the tests pass");
                assert_eq!(agent.as_deref(), Some("grok"));
                assert_eq!(max_iters, 10);
            }
            _ => panic!("expected Command::Loop"),
        }
    }

    #[test]
    fn loop_subcommand_defaults_max_iters_to_six() {
        let cli = Cli::try_parse_from(["divisi", "loop", "do a thing"]).unwrap();
        match cli.command {
            Some(Command::Loop { max_iters, agent, .. }) => {
                assert_eq!(max_iters, 6);
                assert!(agent.is_none());
            }
            _ => panic!("expected Command::Loop"),
        }
    }

    // --- E27.01 regression: `orchestrate-* --task` must parse every spec.
    //     (The old "Relay (0 step(s)):" was a `--background` render bug, not
    //     a parse failure — see the honest background message in the
    //     dispatch arms — but keep the parse itself pinned.)

    #[test]
    fn orchestrate_parallel_collects_every_repeated_task_flag() {
        let cli = Cli::try_parse_from([
            "divisi", "orchestrate-parallel",
            "--task", "grok:say ONE",
            "--task", "opencode:say TWO",
            "--task", "single-nvidia:say THREE",
        ])
        .unwrap();
        match cli.command {
            Some(Command::OrchestrateParallel { tasks, .. }) => assert_eq!(tasks.len(), 3),
            _ => panic!("expected Command::OrchestrateParallel"),
        }
    }

    #[test]
    fn parse_parallel_task_splits_on_the_first_colon_only() {
        let spec = parse_parallel_task("grok:reply with a URL like https://x.test").unwrap();
        assert_eq!(spec.agent, "grok");
        assert_eq!(spec.description, "reply with a URL like https://x.test");
        assert!(parse_parallel_task("no-colon-here").is_err());
        assert!(parse_parallel_task("grok:").is_err());
        assert!(parse_parallel_task(":desc").is_err());
    }

    #[test]
    fn orchestrate_graph_parses_every_repeated_task_flag_into_nodes() {
        let cli = Cli::try_parse_from([
            "divisi", "orchestrate-graph",
            "--task", "id=a,agent=grok,desc=say AAA",
            "--task", "id=b,agent=opencode,desc=say BBB,depends_on=a",
        ])
        .unwrap();
        let tasks = match cli.command {
            Some(Command::OrchestrateGraph { tasks, .. }) => tasks,
            _ => panic!("expected Command::OrchestrateGraph"),
        };
        let nodes: Vec<_> = tasks.iter().map(|t| parse_graph_task(t).unwrap()).collect();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].id, "a");
        assert_eq!(nodes[1].depends_on, vec!["a"]);
    }

    // --- E28 Part A: `divisi provider list-free`/`add-free`/`sync-pool`/`key-status` parse.

    #[test]
    fn list_free_parses() {
        let cli = Cli::try_parse_from(["divisi", "provider", "list-free"]).unwrap();
        match cli.command {
            Some(Command::Provider { action: ProviderCommand::ListFree { json } }) => assert!(!json),
            _ => panic!("expected Command::Provider(ListFree)"),
        }
    }

    #[test]
    fn add_free_parses_with_key_flag() {
        let cli = Cli::try_parse_from(["divisi", "provider", "add-free", "groq", "--key", "gsk-abc"]).unwrap();
        match cli.command {
            Some(Command::Provider { action: ProviderCommand::AddFree { id, key, key_id } }) => {
                assert_eq!(id, "groq");
                assert_eq!(key.as_deref(), Some("gsk-abc"));
                assert!(key_id.is_none());
            }
            _ => panic!("expected Command::Provider(AddFree)"),
        }
    }

    #[test]
    fn add_free_parses_without_key_flag() {
        let cli = Cli::try_parse_from(["divisi", "provider", "add-free", "groq"]).unwrap();
        match cli.command {
            Some(Command::Provider { action: ProviderCommand::AddFree { id, key, key_id } }) => {
                assert_eq!(id, "groq");
                assert!(key.is_none());
                assert!(key_id.is_none());
            }
            _ => panic!("expected Command::Provider(AddFree)"),
        }
    }

    #[test]
    fn add_free_parses_with_explicit_key_id_for_rotation() {
        let cli = Cli::try_parse_from(["divisi", "provider", "add-free", "groq", "--key", "gsk-new", "--key-id", "key2"]).unwrap();
        match cli.command {
            Some(Command::Provider { action: ProviderCommand::AddFree { id, key, key_id } }) => {
                assert_eq!(id, "groq");
                assert_eq!(key.as_deref(), Some("gsk-new"));
                assert_eq!(key_id.as_deref(), Some("key2"));
            }
            _ => panic!("expected Command::Provider(AddFree)"),
        }
    }

    #[test]
    fn sync_pool_parses() {
        let cli = Cli::try_parse_from(["divisi", "provider", "sync-pool"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Provider { action: ProviderCommand::SyncPool })));
    }

    #[test]
    fn key_status_parses_optional_platform() {
        let cli = Cli::try_parse_from(["divisi", "provider", "key-status"]).unwrap();
        match cli.command {
            Some(Command::Provider { action: ProviderCommand::KeyStatus { platform, .. } }) => assert!(platform.is_none()),
            _ => panic!("expected Command::Provider(KeyStatus)"),
        }

        let cli = Cli::try_parse_from(["divisi", "provider", "key-status", "--platform", "groq"]).unwrap();
        match cli.command {
            Some(Command::Provider { action: ProviderCommand::KeyStatus { platform, .. } }) => {
                assert_eq!(platform.as_deref(), Some("groq"));
            }
            _ => panic!("expected Command::Provider(KeyStatus)"),
        }
    }

    #[test]
    fn validate_parses_optional_platform() {
        let cli = Cli::try_parse_from(["divisi", "provider", "validate"]).unwrap();
        match cli.command {
            Some(Command::Provider { action: ProviderCommand::Validate { platform, .. } }) => assert!(platform.is_none()),
            _ => panic!("expected Command::Provider(Validate)"),
        }

        let cli = Cli::try_parse_from(["divisi", "provider", "validate", "--platform", "groq"]).unwrap();
        match cli.command {
            Some(Command::Provider { action: ProviderCommand::Validate { platform, .. } }) => {
                assert_eq!(platform.as_deref(), Some("groq"));
            }
            _ => panic!("expected Command::Provider(Validate)"),
        }
    }

    #[test]
    fn notch_enable_disable_status_parse() {
        let e = Cli::try_parse_from(["divisi", "notch", "enable"]).unwrap();
        assert!(matches!(e.command, Some(Command::Notch { action: NotchCommand::Enable })));
        let d = Cli::try_parse_from(["divisi", "notch", "disable"]).unwrap();
        assert!(matches!(d.command, Some(Command::Notch { action: NotchCommand::Disable })));
        let s = Cli::try_parse_from(["divisi", "notch", "status"]).unwrap();
        assert!(matches!(s.command, Some(Command::Notch { action: NotchCommand::Status })));
    }

    #[test]
    fn notch_show_hide_parse() {
        let show = Cli::try_parse_from(["divisi", "notch", "show"]).unwrap();
        assert!(matches!(show.command, Some(Command::Notch { action: NotchCommand::Show })));
        let hide = Cli::try_parse_from(["divisi", "notch", "hide"]).unwrap();
        assert!(matches!(hide.command, Some(Command::Notch { action: NotchCommand::Hide })));
    }
}
