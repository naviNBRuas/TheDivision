use crate::client::call;
use single_core::SingleDirs;
use single_protocol::{
    AccountProfileInfo, AgentInfo, GoalSummary, LspServerSpec, McpServerInfo, PluginSpec, PoolKeyStatusInfo,
    PoolStatusInfo, ProviderPresetInfo, ProviderSpec, Request, Response, ResponseData, RuntimeStatus,
    SetupAction, TaskRecord, TaskStatus, ToolSpec, UsageSummary, WorkspaceInfo,
};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Which level of the Tasks tab the user is looking at — see `App::task_view`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskView {
    Workspaces,
    Tasks { workspace_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Agents,
    Goals,
    Tasks,
    Mcp,
    Lsp,
    Plugins,
    Tools,
    Providers,
    Accounts,
    Usage,
    Pool,
    Backup,
    Memory,
    Help,
}

impl Tab {
    pub const ALL: [Tab; 14] = [
        Tab::Agents, Tab::Goals, Tab::Tasks, Tab::Mcp, Tab::Lsp, Tab::Plugins, Tab::Tools, Tab::Providers, Tab::Accounts,
        Tab::Usage, Tab::Pool, Tab::Backup, Tab::Memory, Tab::Help,
    ];

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Agents => "Agents",
            Tab::Goals => "Goals",
            Tab::Tasks => "Tasks",
            Tab::Mcp => "MCP",
            Tab::Lsp => "LSP",
            Tab::Plugins => "Plugins",
            Tab::Tools => "Tools",
            Tab::Providers => "Providers",
            Tab::Accounts => "Accounts",
            Tab::Usage => "Usage",
            Tab::Pool => "Pool",
            Tab::Backup => "Backup",
            Tab::Memory => "Memory",
            Tab::Help => "Help",
        }
    }

    fn next(self) -> Tab {
        let idx = Tab::ALL.iter().position(|t| *t == self).unwrap();
        Tab::ALL[(idx + 1) % Tab::ALL.len()]
    }

    fn prev(self) -> Tab {
        let idx = Tab::ALL.iter().position(|t| *t == self).unwrap();
        Tab::ALL[(idx + Tab::ALL.len() - 1) % Tab::ALL.len()]
    }
}

/// State of the in-TUI agent install flow (spec: "for agents install lets
/// make them inside the tui"). A confirm step (showing the real bootstrap
/// command before running it — never install silently) then a background
/// thread runs the real install so the UI keeps redrawing/spinning instead
/// of freezing for however long the download takes.
pub enum InstallFlow {
    Idle,
    Confirming { agent: String, command: String, source: String },
    Running { agent: String, started_at: Instant, rx: mpsc::Receiver<anyhow::Result<SetupAction>> },
    Done { agent: String, action: SetupAction },
    Failed { agent: String, error: String },
}

/// State of the in-TUI "add a provider" flow: pick a preset (OpenAI,
/// Anthropic, OpenCode Zen, NVIDIA — spec: "id like to add providers,
/// configuring them using the TUI"), type the API key (masked), submit.
/// Registration and key storage happen on a background thread — the key
/// itself never lands in `App` once submitted, only the masked input
/// buffer while typing.
pub enum ProviderAddFlow {
    Idle,
    PickingPreset { presets: Vec<ProviderPresetInfo>, selected: usize },
    EnteringKey { preset: ProviderPresetInfo, input: String },
    Submitting { preset_name: String, rx: mpsc::Receiver<anyhow::Result<()>> },
    Done { preset_name: String },
    Failed { preset_name: String, error: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupMode {
    Export,
    Import,
}

/// What actually happened, shown on the flow's `Done` screen. Distinct
/// from `single_core::backup::BackupReport` (import-only) since export
/// has nothing to report but warnings.
pub enum BackupOutcome {
    Exported { path: String, warnings: Vec<String> },
    Imported { report: single_core::backup::BackupReport },
}

/// State of the in-TUI backup export/import flow — path, then a masked
/// passphrase (typed twice for export, to catch typos before they get
/// baked into an unreadable archive; once for import), then a background
/// thread runs the real `single_core::backup::export`/`import` call.
/// Deliberately never goes through `Request`/`ResponseData` or the daemon
/// socket — see `single_core::backup`'s module doc for why the passphrase
/// specifically must stay in-process.
pub enum BackupFlow {
    Idle,
    EnteringPath { mode: BackupMode, input: String },
    EnteringPassphrase { mode: BackupMode, path: String, input: String },
    /// Export only — a second entry to catch a typo before it's baked
    /// into an archive nobody can decrypt.
    ConfirmingPassphrase { path: String, first: String, input: String },
    Submitting { mode: BackupMode, rx: mpsc::Receiver<anyhow::Result<BackupOutcome>> },
    Done { mode: BackupMode, outcome: BackupOutcome },
    Failed { mode: BackupMode, error: String },
}

/// State of the in-TUI "create a task" flow: description, workspace path,
/// then one or more agents (toggle-select — picking more than one runs
/// `Orchestrate` instead of a plain `TaskRun`). Mirrors `ProviderAddFlow`'s
/// step-then-background-thread shape.
pub enum TaskAddFlow {
    Idle,
    EnteringDescription { input: String },
    EnteringCwd { description: String, input: String },
    PickingAgents { description: String, cwd: String, agent_names: Vec<String>, chosen: Vec<bool>, cursor: usize, real_home: bool },
    Submitting { rx: mpsc::Receiver<anyhow::Result<usize>> },
    Done { count: usize },
    Failed { error: String },
}

/// Which registry a `QuickAddFlow` is adding into, and how its single-line
/// input is parsed. Deliberately one compact form (`field|field|...`)
/// instead of a per-type multi-step wizard — this is a power-user quick
/// add for the TUI; anything needing finer control (env vars on an MCP
/// server, etc.) still has the full `single ... add` CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickAddKind {
    Mcp,
    Lsp,
    Plugin,
    Tool,
}

impl QuickAddKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Mcp => "MCP server",
            Self::Lsp => "LSP server",
            Self::Plugin => "plugin",
            Self::Tool => "tool",
        }
    }

    pub fn format_hint(&self) -> &'static str {
        match self {
            Self::Mcp => "name|command|arg1,arg2 (args optional)",
            Self::Lsp => "name|command|arg1,arg2|.ext1,.ext2 (args/extensions optional)",
            Self::Plugin => "name|target|opencode_module (opencode_module optional)",
            Self::Tool => "name|description|risk_level (low/medium/high)",
        }
    }
}

/// State of the in-TUI "quick add" flow shared by the MCP/LSP/Plugins/Tools
/// tabs: one line of `field|field|...` input, parsed per `QuickAddKind`,
/// submitted on a background thread (same shape as `ProviderAddFlow`).
pub enum QuickAddFlow {
    Idle,
    EnteringLine { kind: QuickAddKind, input: String },
    Submitting { kind: QuickAddKind, rx: mpsc::Receiver<anyhow::Result<()>> },
    Done { kind: QuickAddKind },
    Failed { kind: QuickAddKind, error: String },
}

/// State of the task-detail viewer (Tasks tab, `Enter` on a row): shows a
/// single task's full output, live-tailed while it's still running (see
/// `single_core::SingleDirs::task_live_output_path` /
/// `single-agent-sdk::run::run_command_live`) and switching to the final
/// artifact once it finishes. Since an `orchestrate` run creates one task
/// row per agent per step, this is also how each agent in a multi-agent
/// run gets inspected individually — select its row, press `Enter`.
pub enum TaskDetailFlow {
    Idle,
    Viewing { task: TaskRecord, output: String, last_polled: Instant },
}

pub struct App {
    pub socket_path: PathBuf,
    pub dirs: SingleDirs,
    pub tab: Tab,
    pub selected: usize,
    pub status: Option<RuntimeStatus>,
    pub agents: Vec<AgentInfo>,
    /// The coordinator's goal list — the primary unit of work since the
    /// Coordinator subsystem (E27.02) superseded raw ad-hoc `task run` as
    /// how work actually gets done; `None` until the first fetch
    /// completes, same convention as `pool_status`/`provider_key_statuses`
    /// below. Tab-gated fetch, same pattern as Usage/Pool.
    pub goals: Option<Vec<GoalSummary>>,
    pub goals_loading: bool,
    goals_rx: Option<mpsc::Receiver<Option<Vec<GoalSummary>>>>,
    pub tasks: Vec<TaskRecord>,
    pub workspaces: Vec<WorkspaceInfo>,
    /// Which level of the Tasks tab is showing — the workspace list, or
    /// one workspace's tasks after drilling in. Reset to `Workspaces`
    /// whenever the tab changes (same as `selected`), but deliberately
    /// left alone across a plain `refresh()` so an in-progress action
    /// (adding a task, watching one run) doesn't kick the view back out
    /// to the workspace list underneath the user.
    pub task_view: TaskView,
    pub mcp_servers: Vec<McpServerInfo>,
    /// Whether `single-mcp`'s dynamic gateway is on — see
    /// `Request::McpGatewayStatus`'s doc comment. Fetched every `refresh()`
    /// so the Mcp tab's title reflects a toggle made from another `single`
    /// invocation, not just this one.
    pub mcp_gateway_enabled: bool,
    pub lsp_servers: Vec<LspServerSpec>,
    pub plugins: Vec<PluginSpec>,
    pub tools: Vec<ToolSpec>,
    pub providers: Vec<ProviderSpec>,
    /// Every OS-keychain secret name currently stored — cross-referenced
    /// against each `ProviderSpec::secret_name` to show "has a key" vs.
    /// "known preset, never keyed" in the Providers tab, since
    /// `ProviderSpec` itself carries no such flag (see `refresh()`'s doc
    /// comment on why). Never key *values*, only names.
    pub secret_names: Vec<String>,
    pub accounts: Vec<AccountProfileInfo>,
    pub usage: Option<UsageSummary>,
    pub usage_loading: bool,
    usage_rx: Option<mpsc::Receiver<Option<UsageSummary>>>,
    /// Current pool-wide health snapshot — `None` until the first fetch
    /// completes or when the daemon returned an error.
    pub pool_status: Option<PoolStatusInfo>,
    pub pool_status_loading: bool,
    pool_status_rx: Option<mpsc::Receiver<Option<PoolStatusInfo>>>,
    /// Per-platform free-pool key status rows — `None` until the first
    /// fetch completes. `Some(vec![])` means the daemon answered but has
    /// no keys configured, distinct from "never fetched".
    pub provider_key_statuses: Option<Vec<PoolKeyStatusInfo>>,
    pub provider_key_statuses_loading: bool,
    provider_key_statuses_rx: Option<mpsc::Receiver<Option<Vec<PoolKeyStatusInfo>>>>,
    pub kg_entity_count: Option<usize>,
    pub cache_configured: bool,
    pub cache_reachable: bool,
    pub vector_configured: bool,
    pub vector_reachable: bool,
    pub error: Option<String>,
    pub install: InstallFlow,
    pub provider_add: ProviderAddFlow,
    pub task_add: TaskAddFlow,
    pub quick_add: QuickAddFlow,
    pub task_detail: TaskDetailFlow,
    pub backup: BackupFlow,
    pub last_refresh: Instant,
    /// True from construction until the first `refresh()` completes — a
    /// cold daemon can take a few seconds to answer (agent discovery
    /// shells out `which`/`--version` per registered agent), and blocking
    /// `App::new()` on that meant nothing drew at all until it finished.
    /// `draw_content` shows a loading spinner instead while this is true;
    /// left `false` on every refresh after the first, so a `[r]` reload
    /// or a background action's post-completion refresh updates data in
    /// place rather than blanking the screen again.
    pub loading: bool,
    /// Fixed reference point for the loading spinner's animation phase —
    /// picking a frame from elapsed time needs no mutable state touched
    /// from the (immutable) render path.
    started_at: Instant,
    refresh_rx: Option<mpsc::Receiver<RefreshBundle>>,
}

/// Every field `refresh()` fetches, bundled so the background thread that
/// does the actual fetching can send one message back instead of `App`
/// needing `Send`. Built on that thread, applied to `App` on the main
/// thread by `poll_refresh` — the exact same assignment logic `refresh()`
/// used to run inline before it became non-blocking.
struct RefreshBundle {
    status: Option<RuntimeStatus>,
    agents: Vec<AgentInfo>,
    tasks: Vec<TaskRecord>,
    workspaces: Vec<WorkspaceInfo>,
    mcp_servers: Vec<McpServerInfo>,
    mcp_gateway_enabled: Option<bool>,
    lsp_servers: Vec<LspServerSpec>,
    plugins: Vec<PluginSpec>,
    tools: Vec<ToolSpec>,
    providers: Vec<ProviderSpec>,
    secret_names: Vec<String>,
    accounts: Vec<AccountProfileInfo>,
    kg_entity_count: Option<usize>,
    /// `None` when the `CacheStatus` request itself errored — kept as
    /// "no change" rather than resetting to `(false, false)`, same as the
    /// synchronous `refresh()` this replaced.
    cache: Option<(bool, bool)>,
    vector: Option<(bool, bool)>,
    error: Option<String>,
}

impl App {
    pub fn new(dirs: SingleDirs) -> Self {
        let socket_path = dirs.socket_path();
        let mut app = Self {
            socket_path,
            dirs,
            tab: Tab::Agents,
            selected: 0,
            status: None,
            agents: Vec::new(),
            goals: None,
            goals_loading: false,
            goals_rx: None,
            tasks: Vec::new(),
            workspaces: Vec::new(),
            task_view: TaskView::Workspaces,
            mcp_servers: Vec::new(),
            mcp_gateway_enabled: false,
            lsp_servers: Vec::new(),
            plugins: Vec::new(),
            tools: Vec::new(),
            providers: Vec::new(),
            secret_names: Vec::new(),
            accounts: Vec::new(),
            usage: None,
            usage_loading: false,
            usage_rx: None,
            pool_status: None,
            pool_status_loading: false,
            pool_status_rx: None,
            provider_key_statuses: None,
            provider_key_statuses_loading: false,
            provider_key_statuses_rx: None,
            kg_entity_count: None,
            cache_configured: false,
            cache_reachable: false,
            vector_configured: false,
            vector_reachable: false,
            error: None,
            install: InstallFlow::Idle,
            provider_add: ProviderAddFlow::Idle,
            task_add: TaskAddFlow::Idle,
            quick_add: QuickAddFlow::Idle,
            task_detail: TaskDetailFlow::Idle,
            backup: BackupFlow::Idle,
            last_refresh: Instant::now(),
            loading: true,
            started_at: Instant::now(),
            refresh_rx: None,
        };
        app.refresh();
        app
    }

    /// A slowly-cycling braille spinner frame, derived from elapsed time
    /// rather than a counter `ui.rs` would need `&mut App` to advance.
    pub fn spinner_frame(&self) -> char {
        const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let idx = (self.started_at.elapsed().as_millis() / 80) as usize % FRAMES.len();
        FRAMES[idx]
    }

    /// Kicks off a fresh fetch of every tab's data on a background thread
    /// and returns immediately — never blocks the render loop, including
    /// on the very first call from `new()`. See `RefreshBundle` and
    /// `poll_refresh`, which applies the result once it arrives. A refresh
    /// already in flight is left to finish rather than piling another one
    /// on top of it (the common case: several key presses each calling
    /// `refresh()` before the daemon has answered the first one).
    pub fn refresh(&mut self) {
        self.error = None;
        if self.refresh_rx.is_some() {
            return;
        }
        let socket_path = self.socket_path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // Every request below is an independent UnixStream round trip
            // (see `client::call`) with no shared connection state, so
            // there's no reason to wait on them one at a time — fired
            // concurrently and joined before building the bundle sent
            // back to the main thread. (The daemon does its own
            // parallelizing of AgentList/Status's per-agent discovery —
            // see `single-runtime`'s `cached_discover` — so this and that
            // combine rather than duplicate effort.)
            let requests = [
                Request::Status,
                Request::AgentList,
                Request::TaskList,
                Request::WorkspaceList,
                Request::McpList,
                Request::McpGatewayStatus,
                Request::LspList,
                Request::PluginList,
                Request::ToolList,
                Request::ConfiguredProviderList,
                Request::SecretList,
                Request::AccountList { agent: None },
                Request::KgReadGraph,
                Request::CacheStatus,
                Request::VectorStatus,
            ];
            let responses: Vec<_> = std::thread::scope(|scope| {
                let handles: Vec<_> = requests
                    .into_iter()
                    .map(|req| {
                        let socket_path = socket_path.clone();
                        scope.spawn(move || call(&socket_path, &req))
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });

            // Unpacked in the same order `requests` was built.
            let mut responses = responses.into_iter();
            let mut error: Option<String> = None;
            let mut next = |error: &mut Option<String>| -> Option<ResponseData> {
                match responses.next().expect("one response per request") {
                    Ok(Response::Ok { data }) => Some(data),
                    Ok(Response::Error { message }) => {
                        *error = Some(message);
                        None
                    }
                    Err(e) => {
                        *error = Some(e.to_string());
                        None
                    }
                }
            };

            let status = next(&mut error).and_then(|d| match d { ResponseData::Status(s) => Some(s), _ => None });
            let agents = next(&mut error).and_then(|d| match d { ResponseData::Agents(a) => Some(a), _ => None }).unwrap_or_default();
            let tasks = next(&mut error).and_then(|d| match d { ResponseData::Tasks(t) => Some(t), _ => None }).unwrap_or_default();
            let workspaces = next(&mut error).and_then(|d| match d { ResponseData::Workspaces(w) => Some(w), _ => None }).unwrap_or_default();
            let mcp_servers = next(&mut error).and_then(|d| match d { ResponseData::McpServers(s) => Some(s), _ => None }).unwrap_or_default();
            let mcp_gateway_enabled = match next(&mut error) {
                Some(ResponseData::McpGatewayMode(enabled)) => Some(enabled),
                _ => None,
            };
            let lsp_servers = next(&mut error).and_then(|d| match d { ResponseData::LspServers(s) => Some(s), _ => None }).unwrap_or_default();
            let plugins = next(&mut error).and_then(|d| match d { ResponseData::Plugins(p) => Some(p), _ => None }).unwrap_or_default();
            let tools = next(&mut error).and_then(|d| match d { ResponseData::Tools(t) => Some(t), _ => None }).unwrap_or_default();
            // Configured-only, not the full preset catalog: `providers.toml`
            // carries every built-in preset unconditionally (ProviderSpec has
            // no `enabled` field, unlike MCP/LSP/Tools), so plain
            // Request::ProviderList can't tell "known preset name" apart from
            // "actually has a key stored" — the full catalog is still what
            // the [a] add-provider flow shows (ProviderPresetList, a
            // separate fetch, unaffected by this).
            let providers = next(&mut error).and_then(|d| match d { ResponseData::Providers(p) => Some(p), _ => None }).unwrap_or_default();
            // Cross-referenced against each ProviderSpec's `secret_name`
            // in `draw_providers` to distinguish "known preset, never
            // keyed" from "actually has a key stored" — see the doc
            // comment on `providers` above for why ProviderSpec alone
            // can't tell those apart. Never carries key *values*, only
            // the OS-keychain entry names (`SecretList`'s own contract).
            let secret_names = next(&mut error).and_then(|d| match d { ResponseData::SecretNames(n) => Some(n), _ => None }).unwrap_or_default();
            let accounts = next(&mut error).and_then(|d| match d { ResponseData::AccountProfiles(p) => Some(p), _ => None }).unwrap_or_default();
            let kg_entity_count = next(&mut error).and_then(|d| match d { ResponseData::KgGraph(g) => Some(g), _ => None }).map(|g| g.entities.len());
            let cache = match next(&mut error) {
                Some(ResponseData::CacheStatus { configured, reachable, .. }) => Some((configured, reachable)),
                _ => None,
            };
            let vector = match next(&mut error) {
                Some(ResponseData::VectorStatus { configured, reachable, .. }) => Some((configured, reachable)),
                _ => None,
            };

            let _ = tx.send(RefreshBundle {
                status,
                agents,
                tasks,
                workspaces,
                mcp_servers,
                mcp_gateway_enabled,
                lsp_servers,
                plugins,
                tools,
                providers,
                secret_names,
                accounts,
                kg_entity_count,
                cache,
                vector,
                error,
            });
        });
        self.refresh_rx = Some(rx);
    }

    /// Applies a completed background `refresh()`, if one has arrived.
    /// Call every tick, same as `poll_install`/`poll_provider_add`/etc.
    /// Returns true if a redraw-worthy state change happened.
    pub fn poll_refresh(&mut self) -> bool {
        let Some(rx) = &self.refresh_rx else { return false };
        let Ok(bundle) = rx.try_recv() else { return false };

        self.status = bundle.status;
        self.agents = bundle.agents;
        self.tasks = bundle.tasks;
        self.workspaces = bundle.workspaces;
        self.mcp_servers = bundle.mcp_servers;
        if let Some(enabled) = bundle.mcp_gateway_enabled {
            self.mcp_gateway_enabled = enabled;
        }
        self.lsp_servers = bundle.lsp_servers;
        self.plugins = bundle.plugins;
        self.tools = bundle.tools;
        self.providers = bundle.providers;
        self.secret_names = bundle.secret_names;
        self.accounts = bundle.accounts;
        self.kg_entity_count = bundle.kg_entity_count;
        if let Some((configured, reachable)) = bundle.cache {
            self.cache_configured = configured;
            self.cache_reachable = reachable;
        }
        if let Some((configured, reachable)) = bundle.vector {
            self.vector_configured = configured;
            self.vector_reachable = reachable;
        }
        if bundle.error.is_some() {
            self.error = bundle.error;
        }
        // Real billing-API calls are comparatively slow/rate-limited (the
        // Anthropic Usage & Cost API's own docs recommend at most once a
        // minute) and, once a billing key is configured, involve a live
        // HTTP round trip the daemon makes on this request's behalf —
        // unlike every other tab's data, this is never fetched as part of
        // the bundle above. It's kicked off on its own background thread
        // (see begin_usage_fetch/poll_usage) so a slow provider API can't
        // hold up the rest of the tabs' data the way bundling it in would.
        if self.tab == Tab::Usage {
            self.begin_usage_fetch();
        }
        // Pool data is also polled independently: benched-key state and
        // per-key headroom involve daemon-side RPD table reads that are
        // cheap but orthogonal to the main bundle — kicked off only when
        // the Pool tab is visible, same pattern as Usage.
        if self.tab == Tab::Pool {
            self.begin_pool_fetch();
        }
        // Agents and Usage classify/annotate against the same per-provider
        // statuses (single_core::auth_class), so they need them too.
        if matches!(self.tab, Tab::Pool | Tab::Agents | Tab::Usage) {
            self.begin_provider_key_status_fetch();
        }
        if self.tab == Tab::Goals {
            self.begin_goals_fetch();
        }
        self.last_refresh = Instant::now();
        self.loading = false;
        self.refresh_rx = None;
        self.clamp_selection();
        true
    }

    fn fetch<T>(&mut self, req: Request, extract: impl FnOnce(ResponseData) -> Option<T>) -> Option<T> {
        match self.raw(req) {
            Some(data) => extract(data),
            None => None,
        }
    }

    fn raw(&mut self, req: Request) -> Option<ResponseData> {
        match call(&self.socket_path, &req) {
            Ok(Response::Ok { data }) => Some(data),
            Ok(Response::Error { message }) => {
                self.error = Some(message);
                None
            }
            Err(e) => {
                self.error = Some(e.to_string());
                None
            }
        }
    }

    /// The tasks belonging to whichever workspace is currently drilled
    /// into (Tasks tab, `TaskView::Tasks`) — `self.tasks` itself always
    /// holds every task ever run, unfiltered.
    pub fn visible_tasks(&self) -> Vec<&TaskRecord> {
        let TaskView::Tasks { workspace_id } = &self.task_view else { return Vec::new() };
        self.tasks.iter().filter(|t| &t.workspace_id == workspace_id).collect()
    }

    pub fn current_len(&self) -> usize {
        match self.tab {
            Tab::Agents => self.agents.len(),
            Tab::Goals => self.goals.as_ref().map(Vec::len).unwrap_or(0),
            Tab::Tasks => match &self.task_view {
                TaskView::Workspaces => self.workspaces.len(),
                TaskView::Tasks { .. } => self.visible_tasks().len(),
            },
            Tab::Mcp => self.mcp_servers.len(),
            Tab::Lsp => self.lsp_servers.len(),
            Tab::Plugins => self.plugins.len(),
            Tab::Tools => self.tools.len(),
            Tab::Providers => self.providers.len(),
            Tab::Accounts => self.accounts.len(),
            Tab::Usage | Tab::Pool | Tab::Backup | Tab::Memory | Tab::Help => 0,
        }
    }

    fn clamp_selection(&mut self) {
        let len = self.current_len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    pub fn next_tab(&mut self) {
        self.tab = self.tab.next();
        self.selected = 0;
        self.task_view = TaskView::Workspaces;
        if self.tab == Tab::Usage {
            self.begin_usage_fetch();
        }
        if self.tab == Tab::Pool {
            self.begin_pool_fetch();
            self.begin_provider_key_status_fetch();
        }
        if self.tab == Tab::Goals {
            self.begin_goals_fetch();
        }
    }

    pub fn prev_tab(&mut self) {
        self.tab = self.tab.prev();
        self.selected = 0;
        self.task_view = TaskView::Workspaces;
        if self.tab == Tab::Usage {
            self.begin_usage_fetch();
        }
        if self.tab == Tab::Pool {
            self.begin_pool_fetch();
            self.begin_provider_key_status_fetch();
        }
        if self.tab == Tab::Goals {
            self.begin_goals_fetch();
        }
    }

    /// Drills into the selected workspace's own task list (Tasks tab,
    /// `Enter` at the workspace-list level).
    pub fn enter_workspace(&mut self) {
        if self.tab != Tab::Tasks || self.task_view != TaskView::Workspaces {
            return;
        }
        let Some(workspace) = self.workspaces.get(self.selected) else { return };
        self.task_view = TaskView::Tasks { workspace_id: workspace.id.clone() };
        self.selected = 0;
    }

    /// Backs out of a workspace's task list to the workspace list (Tasks
    /// tab, `Esc` while drilled in).
    pub fn exit_workspace(&mut self) {
        self.task_view = TaskView::Workspaces;
        self.selected = 0;
    }

    pub fn move_selection(&mut self, delta: i32) {
        let len = self.current_len();
        if len == 0 {
            return;
        }
        let current = self.selected as i32;
        let new = (current + delta).rem_euclid(len as i32);
        self.selected = new as usize;
    }

    /// Begins the interactive install flow for the currently selected
    /// agent, if it's a real agent with a bootstrap command and isn't
    /// already installed.
    pub fn begin_install(&mut self) {
        if self.tab != Tab::Agents {
            return;
        }
        let Some(agent) = self.agents.get(self.selected) else { return };
        if agent.detected {
            self.error = Some(format!("{} is already installed", agent.name));
            return;
        }
        let Some(install) = &agent.bootstrap_install else {
            self.error = Some(format!("{} has no verified install command", agent.name));
            return;
        };
        self.install = InstallFlow::Confirming { agent: agent.name.clone(), command: install.command.clone(), source: install.source.clone() };
    }

    pub fn confirm_install(&mut self) {
        let InstallFlow::Confirming { agent, .. } = &self.install else { return };
        let agent = agent.clone();
        let socket_path = self.socket_path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = call(&socket_path, &Request::AgentInstall { name: agent.clone(), dry_run: false }).and_then(|resp| match resp {
                Response::Ok { data: ResponseData::AgentInstallResult(action) } => Ok(action),
                Response::Ok { .. } => Err(anyhow::anyhow!("unexpected response")),
                Response::Error { message } => Err(anyhow::anyhow!(message)),
            });
            let _ = tx.send(result);
        });
        let agent_name = if let InstallFlow::Confirming { agent, .. } = &self.install { agent.clone() } else { unreachable!() };
        self.install = InstallFlow::Running { agent: agent_name, started_at: Instant::now(), rx };
    }

    pub fn cancel_install(&mut self) {
        self.install = InstallFlow::Idle;
    }

    /// Polls the background install thread; call every tick. Returns true
    /// if a redraw-worthy state change happened (install just finished).
    pub fn poll_install(&mut self) -> bool {
        if let InstallFlow::Running { agent, rx, .. } = &self.install {
            if let Ok(result) = rx.try_recv() {
                self.install = match result {
                    Ok(action) => InstallFlow::Done { agent: agent.clone(), action },
                    Err(e) => InstallFlow::Failed { agent: agent.clone(), error: e.to_string() },
                };
                self.refresh();
                return true;
            }
        }
        false
    }

    pub fn install_elapsed(&self) -> Option<Duration> {
        match &self.install {
            InstallFlow::Running { started_at, .. } => Some(started_at.elapsed()),
            _ => None,
        }
    }

    /// Opens the preset picker (spec: "add providers, configuring them
    /// using the TUI, for providers like OpenAI, Anthropic, OpenCode Zen,
    /// NVIDIA AI API").
    pub fn begin_add_provider(&mut self) {
        if self.tab != Tab::Providers {
            return;
        }
        let presets = match call(&self.socket_path, &Request::ProviderPresetList) {
            Ok(Response::Ok { data: ResponseData::ProviderPresets(p) }) => p,
            Ok(Response::Error { message }) => {
                self.error = Some(message);
                return;
            }
            _ => {
                self.error = Some("failed to load provider presets".into());
                return;
            }
        };
        self.provider_add = ProviderAddFlow::PickingPreset { presets, selected: 0 };
    }

    pub fn provider_picker_move(&mut self, delta: i32) {
        if let ProviderAddFlow::PickingPreset { presets, selected } = &mut self.provider_add {
            if !presets.is_empty() {
                let len = presets.len() as i32;
                *selected = ((*selected as i32 + delta).rem_euclid(len)) as usize;
            }
        }
    }

    pub fn provider_picker_confirm(&mut self) {
        if let ProviderAddFlow::PickingPreset { presets, selected } = &self.provider_add {
            if let Some(preset) = presets.get(*selected).cloned() {
                self.provider_add = ProviderAddFlow::EnteringKey { preset, input: String::new() };
            }
        }
    }

    pub fn provider_key_input(&mut self, c: char) {
        if let ProviderAddFlow::EnteringKey { input, .. } = &mut self.provider_add {
            input.push(c);
        }
    }

    pub fn provider_key_backspace(&mut self) {
        if let ProviderAddFlow::EnteringKey { input, .. } = &mut self.provider_add {
            input.pop();
        }
    }

    pub fn provider_key_submit(&mut self) {
        let ProviderAddFlow::EnteringKey { preset, input } = &self.provider_add else { return };
        if input.is_empty() {
            self.error = Some("API key cannot be empty".into());
            return;
        }
        let preset_name = preset.name.clone();
        let value = input.clone();
        let socket_path = self.socket_path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                match call(&socket_path, &Request::ProviderAddPreset { name: preset_name.clone() })? {
                    Response::Ok { .. } => {}
                    Response::Error { message } => anyhow::bail!(message),
                }
                match call(&socket_path, &Request::ProviderSetKey { name: preset_name.clone(), value })? {
                    Response::Ok { .. } => Ok(()),
                    Response::Error { message } => anyhow::bail!(message),
                }
            })();
            let _ = tx.send(result);
        });
        self.provider_add = ProviderAddFlow::Submitting { preset_name: preset.name.clone(), rx };
    }

    pub fn cancel_provider_add(&mut self) {
        self.provider_add = ProviderAddFlow::Idle;
    }

    pub fn poll_provider_add(&mut self) -> bool {
        if let ProviderAddFlow::Submitting { preset_name, rx } = &self.provider_add {
            if let Ok(result) = rx.try_recv() {
                self.provider_add = match result {
                    Ok(()) => ProviderAddFlow::Done { preset_name: preset_name.clone() },
                    Err(e) => ProviderAddFlow::Failed { preset_name: preset_name.clone(), error: e.to_string() },
                };
                self.refresh();
                return true;
            }
        }
        false
    }

    /// Opens the task-creation flow (spec: "in the TUI, i want to be able
    /// to create tasks, specify workspace paths, agent(s), and so on").
    pub fn begin_add_task(&mut self) {
        if self.tab != Tab::Tasks {
            return;
        }
        self.task_add = TaskAddFlow::EnteringDescription { input: String::new() };
    }

    pub fn task_desc_input(&mut self, c: char) {
        if let TaskAddFlow::EnteringDescription { input } = &mut self.task_add {
            input.push(c);
        }
    }

    pub fn task_desc_backspace(&mut self) {
        if let TaskAddFlow::EnteringDescription { input } = &mut self.task_add {
            input.pop();
        }
    }

    pub fn task_desc_submit(&mut self) {
        let TaskAddFlow::EnteringDescription { input } = &self.task_add else { return };
        if input.is_empty() {
            self.error = Some("description cannot be empty".into());
            return;
        }
        self.task_add = TaskAddFlow::EnteringCwd { description: input.clone(), input: ".".to_string() };
    }

    pub fn task_cwd_input(&mut self, c: char) {
        if let TaskAddFlow::EnteringCwd { input, .. } = &mut self.task_add {
            input.push(c);
        }
    }

    pub fn task_cwd_backspace(&mut self) {
        if let TaskAddFlow::EnteringCwd { input, .. } = &mut self.task_add {
            input.pop();
        }
    }

    pub fn task_cwd_submit(&mut self) {
        let TaskAddFlow::EnteringCwd { description, input } = &self.task_add else { return };
        let cwd = if input.is_empty() { ".".to_string() } else { input.clone() };
        let cwd = std::fs::canonicalize(&cwd).map(|p| p.display().to_string()).unwrap_or(cwd);
        let agent_names: Vec<String> = self.agents.iter().filter(|a| a.detected).map(|a| a.name.clone()).collect();
        let chosen = vec![false; agent_names.len()];
        self.task_add = TaskAddFlow::PickingAgents { description: description.clone(), cwd, agent_names, chosen, cursor: 0, real_home: false };
    }

    pub fn task_agents_move(&mut self, delta: i32) {
        if let TaskAddFlow::PickingAgents { agent_names, cursor, .. } = &mut self.task_add {
            if !agent_names.is_empty() {
                let len = agent_names.len() as i32;
                *cursor = ((*cursor as i32 + delta).rem_euclid(len)) as usize;
            }
        }
    }

    pub fn task_agents_toggle(&mut self) {
        if let TaskAddFlow::PickingAgents { chosen, cursor, .. } = &mut self.task_add {
            if let Some(c) = chosen.get_mut(*cursor) {
                *c = !*c;
            }
        }
    }

    /// Toggles whether this task runs against the real, ambient $HOME
    /// instead of SingleCLI's isolated one — for tasks that need to
    /// actually modify the real system (see `Request::TaskRun::real_home`
    /// docs). Off by default.
    pub fn task_toggle_real_home(&mut self) {
        if let TaskAddFlow::PickingAgents { real_home, .. } = &mut self.task_add {
            *real_home = !*real_home;
        }
    }

    pub fn task_agents_submit(&mut self) {
        let TaskAddFlow::PickingAgents { description, cwd, agent_names, chosen, real_home, .. } = &self.task_add else { return };
        let selected: Vec<String> = agent_names.iter().zip(chosen).filter(|(_, on)| **on).map(|(n, _)| n.clone()).collect();
        if selected.is_empty() {
            self.error = Some("pick at least one agent (space to toggle)".into());
            return;
        }
        let description = description.clone();
        let cwd = cwd.clone();
        let real_home = *real_home;
        let socket_path = self.socket_path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<usize> {
                if selected.len() == 1 {
                    match call(&socket_path, &Request::TaskRun { description, agent: selected[0].clone(), cwd, use_worktree: false, account: None, real_home, no_memory_context: false, timeout_secs: 300, background: false, allow_fallback: false, usage_json: false })? {
                        Response::Ok { .. } => Ok(1),
                        Response::Error { message } => anyhow::bail!(message),
                    }
                } else {
                    match call(&socket_path, &Request::Orchestrate { goal: description, agents: selected.clone(), cwd, use_worktree: true, real_home, timeout_secs: 300 })? {
                        Response::Ok { .. } => Ok(selected.len()),
                        Response::Error { message } => anyhow::bail!(message),
                    }
                }
            })();
            let _ = tx.send(result);
        });
        self.task_add = TaskAddFlow::Submitting { rx };
    }

    pub fn cancel_task_add(&mut self) {
        self.task_add = TaskAddFlow::Idle;
    }

    pub fn poll_task_add(&mut self) -> bool {
        if let TaskAddFlow::Submitting { rx } = &self.task_add {
            if let Ok(result) = rx.try_recv() {
                self.task_add = match result {
                    Ok(count) => TaskAddFlow::Done { count },
                    Err(e) => TaskAddFlow::Failed { error: e.to_string() },
                };
                self.refresh();
                return true;
            }
        }
        false
    }

    /// Opens the quick-add flow for whichever of MCP/LSP/Plugins/Tools tab
    /// is active.
    pub fn begin_quick_add(&mut self) {
        let kind = match self.tab {
            Tab::Mcp => QuickAddKind::Mcp,
            Tab::Lsp => QuickAddKind::Lsp,
            Tab::Plugins => QuickAddKind::Plugin,
            Tab::Tools => QuickAddKind::Tool,
            _ => return,
        };
        self.quick_add = QuickAddFlow::EnteringLine { kind, input: String::new() };
    }

    pub fn quick_add_input(&mut self, c: char) {
        if let QuickAddFlow::EnteringLine { input, .. } = &mut self.quick_add {
            input.push(c);
        }
    }

    pub fn quick_add_backspace(&mut self) {
        if let QuickAddFlow::EnteringLine { input, .. } = &mut self.quick_add {
            input.pop();
        }
    }

    pub fn quick_add_submit(&mut self) {
        let QuickAddFlow::EnteringLine { kind, input } = &self.quick_add else { return };
        let kind = *kind;
        let parts: Vec<&str> = input.split('|').map(|s| s.trim()).collect();
        let request = match build_quick_add_request(kind, &parts) {
            Ok(req) => req,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let socket_path = self.socket_path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = match call(&socket_path, &request) {
                Ok(Response::Ok { .. }) => Ok(()),
                Ok(Response::Error { message }) => Err(anyhow::anyhow!(message)),
                Err(e) => Err(e),
            };
            let _ = tx.send(result);
        });
        self.quick_add = QuickAddFlow::Submitting { kind, rx };
    }

    pub fn cancel_quick_add(&mut self) {
        self.quick_add = QuickAddFlow::Idle;
    }

    pub fn poll_quick_add(&mut self) -> bool {
        if let QuickAddFlow::Submitting { kind, rx } = &self.quick_add {
            if let Ok(result) = rx.try_recv() {
                let kind = *kind;
                self.quick_add = match result {
                    Ok(()) => QuickAddFlow::Done { kind },
                    Err(e) => QuickAddFlow::Failed { kind, error: e.to_string() },
                };
                self.refresh();
                return true;
            }
        }
        false
    }

    /// Removes the currently selected entry (MCP/LSP/Plugins/Tools/
    /// Accounts/Providers tab — whichever has a real Remove request).
    pub fn delete_selected(&mut self) {
        let request = match self.tab {
            Tab::Mcp => self.mcp_servers.get(self.selected).map(|s| Request::McpRemove { name: s.name.clone() }),
            Tab::Lsp => self.lsp_servers.get(self.selected).map(|s| Request::LspRemove { name: s.name.clone() }),
            Tab::Plugins => self.plugins.get(self.selected).map(|p| Request::PluginRemove { name: p.name.clone() }),
            Tab::Tools => None, // tools are built-in metadata; disable instead of remove (see toggle_selected)
            Tab::Providers => self.providers.get(self.selected).map(|p| Request::ProviderRemove { name: p.name.clone() }),
            Tab::Accounts => self.accounts.get(self.selected).map(|a| Request::AccountRemove { agent: a.agent.clone(), name: a.name.clone() }),
            _ => None,
        };
        let Some(request) = request else { return };
        self.raw(request);
        self.refresh();
    }

    /// Toggles enabled/disabled for the selected MCP server or tool (the
    /// two registries with a real Enable/Disable request).
    pub fn toggle_selected(&mut self) {
        let request = match self.tab {
            Tab::Mcp => self.mcp_servers.get(self.selected).map(|s| {
                if s.enabled { Request::McpDisable { name: s.name.clone() } } else { Request::McpEnable { name: s.name.clone() } }
            }),
            Tab::Tools => self.tools.get(self.selected).map(|t| {
                if t.enabled { Request::ToolDisable { name: t.name.clone() } } else { Request::ToolEnable { name: t.name.clone() } }
            }),
            _ => None,
        };
        let Some(request) = request else { return };
        self.raw(request);
        self.refresh();
    }

    /// Flips `single-mcp`'s dynamic gateway on/off (Mcp tab). Only changes
    /// the stored setting — like the CLI's `single mcp gateway enable`, it
    /// takes effect on the next `single install-integrations`, not
    /// retroactively, so this doesn't touch any agent's synced config.
    pub fn toggle_mcp_gateway(&mut self) {
        if self.tab != Tab::Mcp {
            return;
        }
        self.raw(Request::McpGatewaySetEnabled { enabled: !self.mcp_gateway_enabled });
        self.refresh();
    }

    /// Syncs the selected plugin into every registered agent (Plugins tab).
    pub fn sync_selected_plugin(&mut self) {
        if self.tab != Tab::Plugins {
            return;
        }
        let Some(plugin) = self.plugins.get(self.selected) else { return };
        self.raw(Request::PluginSync { name: plugin.name.clone(), agents: Vec::new(), dry_run: false, real_home: false });
        self.refresh();
    }

    /// `Enter` on the Tasks tab: drills into the selected workspace at the
    /// workspace-list level, or opens the task-detail viewer once already
    /// drilled into one workspace's task list.
    pub fn tasks_enter(&mut self) {
        if self.tab != Tab::Tasks {
            return;
        }
        match self.task_view {
            TaskView::Workspaces => self.enter_workspace(),
            TaskView::Tasks { .. } => self.begin_view_task(),
        }
    }

    /// Opens the task-detail viewer for the selected row within the
    /// currently drilled-into workspace (Tasks tab).
    fn begin_view_task(&mut self) {
        let Some(task) = self.visible_tasks().get(self.selected).map(|t| (*t).clone()) else { return };
        let output = self.read_task_output(&task);
        self.task_detail = TaskDetailFlow::Viewing { task, output, last_polled: Instant::now() };
    }

    pub fn cancel_task_detail(&mut self) {
        self.task_detail = TaskDetailFlow::Idle;
    }

    fn read_task_output(&self, task: &TaskRecord) -> String {
        let path = if task.status == TaskStatus::Running {
            self.dirs.task_live_output_path(task.id)
        } else {
            self.dirs.task_artifact_path(task.id)
        };
        std::fs::read_to_string(&path).unwrap_or_else(|_| "(no output yet)".to_string())
    }

    /// Re-fetches the viewed task and its output while it's still
    /// running, throttled so this doesn't hit the socket/disk every
    /// single event-loop tick. Stops polling once the task finishes —
    /// there's nothing left to change.
    pub fn poll_task_detail(&mut self) -> bool {
        let TaskDetailFlow::Viewing { task, last_polled, .. } = &self.task_detail else { return false };
        if task.status != TaskStatus::Running || last_polled.elapsed() < Duration::from_millis(500) {
            return false;
        }
        let id = task.id;
        let Some(updated) = self.fetch(Request::TaskInspect { id }, |d| match d { ResponseData::Task(t) => Some(t), _ => None }) else {
            return false;
        };
        let output = self.read_task_output(&updated);
        self.task_detail = TaskDetailFlow::Viewing { task: updated, output, last_polled: Instant::now() };
        true
    }

    pub fn begin_backup_export(&mut self) {
        if self.tab != Tab::Backup {
            return;
        }
        self.backup = BackupFlow::EnteringPath { mode: BackupMode::Export, input: String::new() };
    }

    pub fn begin_backup_import(&mut self) {
        if self.tab != Tab::Backup {
            return;
        }
        self.backup = BackupFlow::EnteringPath { mode: BackupMode::Import, input: String::new() };
    }

    pub fn cancel_backup(&mut self) {
        self.backup = BackupFlow::Idle;
    }

    pub fn backup_path_input(&mut self, c: char) {
        if let BackupFlow::EnteringPath { input, .. } = &mut self.backup {
            input.push(c);
        }
    }

    pub fn backup_path_backspace(&mut self) {
        if let BackupFlow::EnteringPath { input, .. } = &mut self.backup {
            input.pop();
        }
    }

    pub fn backup_path_submit(&mut self) {
        let BackupFlow::EnteringPath { mode, input } = &self.backup else { return };
        if input.is_empty() {
            self.error = Some("path cannot be empty".into());
            return;
        }
        self.backup = BackupFlow::EnteringPassphrase { mode: *mode, path: input.clone(), input: String::new() };
    }

    pub fn backup_passphrase_input(&mut self, c: char) {
        match &mut self.backup {
            BackupFlow::EnteringPassphrase { input, .. } | BackupFlow::ConfirmingPassphrase { input, .. } => input.push(c),
            _ => {}
        }
    }

    pub fn backup_passphrase_backspace(&mut self) {
        match &mut self.backup {
            BackupFlow::EnteringPassphrase { input, .. } | BackupFlow::ConfirmingPassphrase { input, .. } => {
                input.pop();
            }
            _ => {}
        }
    }

    /// For export, the first passphrase entry advances to a confirmation
    /// step (typo protection — nothing catches a mistyped passphrase
    /// afterward, since encryption always "succeeds" even with a typo,
    /// it just produces an archive nobody can decrypt). Import only asks
    /// once — decryption itself is the check.
    pub fn backup_passphrase_submit(&mut self) {
        match &self.backup {
            BackupFlow::EnteringPassphrase { mode: BackupMode::Export, path, input } => {
                if input.is_empty() {
                    self.error = Some("passphrase cannot be empty".into());
                    return;
                }
                self.backup = BackupFlow::ConfirmingPassphrase { path: path.clone(), first: input.clone(), input: String::new() };
            }
            BackupFlow::EnteringPassphrase { mode: BackupMode::Import, path, input } => {
                if input.is_empty() {
                    self.error = Some("passphrase cannot be empty".into());
                    return;
                }
                self.start_backup_import(path.clone(), input.clone());
            }
            BackupFlow::ConfirmingPassphrase { path, first, input } => {
                if first != input {
                    self.error = Some("passphrases didn't match — try again".into());
                    self.backup = BackupFlow::EnteringPassphrase { mode: BackupMode::Export, path: path.clone(), input: String::new() };
                    return;
                }
                self.start_backup_export(path.clone(), first.clone());
            }
            _ => {}
        }
    }

    fn start_backup_export(&mut self, path: String, passphrase: String) {
        let dirs = self.dirs.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = single_core::backup::export(&dirs, std::path::Path::new(&path), &age::secrecy::SecretString::from(passphrase))
                .map(|warnings| BackupOutcome::Exported { path: path.clone(), warnings });
            let _ = tx.send(result);
        });
        self.backup = BackupFlow::Submitting { mode: BackupMode::Export, rx };
    }

    fn start_backup_import(&mut self, path: String, passphrase: String) {
        let dirs = self.dirs.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // Deliberately always a dry-run preview from the TUI —
            // actually restoring files and re-inserting keychain secrets
            // is a real, hard-to-fully-undo action, and this flow has no
            // in-TUI equivalent of the CLI's explicit `--yes` gate yet.
            // The Done screen tells the user to run `single backup
            // import <path> --yes` to actually apply it — an honest v1
            // scope limit, not an oversight.
            let result = single_core::backup::import(&dirs, std::path::Path::new(&path), &age::secrecy::SecretString::from(passphrase), true)
                .map(|report| BackupOutcome::Imported { report });
            let _ = tx.send(result);
        });
        self.backup = BackupFlow::Submitting { mode: BackupMode::Import, rx };
    }

    pub fn poll_backup(&mut self) -> bool {
        if let BackupFlow::Submitting { mode, rx } = &self.backup {
            if let Ok(result) = rx.try_recv() {
                self.backup = match result {
                    Ok(outcome) => BackupFlow::Done { mode: *mode, outcome },
                    Err(e) => BackupFlow::Failed { mode: *mode, error: e.to_string() },
                };
                return true;
            }
        }
        false
    }

    /// Fetches the Usage tab's data on a background thread — never inline
    /// on the event-loop thread, since once a real billing admin key is
    /// configured this involves a live HTTP round trip to a provider's
    /// API (see `single_runtime::billing`), which must not be able to
    /// freeze keyboard input / redraws the way a blocking call here
    /// would. Safe to call repeatedly (e.g. re-entering the tab while a
    /// previous fetch is still in flight): a fetch already running is
    /// left alone rather than piling up a second one.
    fn begin_usage_fetch(&mut self) {
        if self.usage_loading {
            return;
        }
        self.usage_loading = true;
        let socket_path = self.socket_path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = match call(&socket_path, &Request::UsageShow { provider: None }) {
                Ok(Response::Ok { data: ResponseData::Usage(u) }) => Some(u),
                _ => None,
            };
            let _ = tx.send(result);
        });
        self.usage_rx = Some(rx);
    }

    /// Returns true if new usage data arrived this tick (so the caller
    /// knows to redraw immediately, same convention as every other
    /// `poll_*` here).
    pub fn poll_usage(&mut self) -> bool {
        let Some(rx) = &self.usage_rx else { return false };
        let Ok(result) = rx.try_recv() else { return false };
        self.usage = result;
        self.usage_loading = false;
        self.usage_rx = None;
        true
    }

    /// Fetches the Goals tab's list on a background thread. Same
    /// tab-gated, one-in-flight, non-blocking pattern as
    /// `begin_pool_fetch`/`begin_usage_fetch`.
    fn begin_goals_fetch(&mut self) {
        if self.goals_loading {
            return;
        }
        self.goals_loading = true;
        let socket_path = self.socket_path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = match call(&socket_path, &Request::GoalList { session_id: None }) {
                Ok(Response::Ok { data: ResponseData::Goals(gs) }) => Some(gs),
                _ => None,
            };
            let _ = tx.send(result);
        });
        self.goals_rx = Some(rx);
    }

    /// Returns true if a new goals list arrived this tick.
    pub fn poll_goals(&mut self) -> bool {
        let Some(rx) = &self.goals_rx else { return false };
        let Ok(result) = rx.try_recv() else { return false };
        self.goals = result;
        self.goals_loading = false;
        self.goals_rx = None;
        true
    }

    /// Fetches the Pool tab's health snapshot on a background thread.
    /// Mirrors `begin_usage_fetch`: only one in-flight fetch at a time,
    /// returns immediately without blocking the event loop. The daemon
    /// reads the benched-key table and computes `healthy_ratio` on its
    /// own side (`Request::PoolStatus`), so this is just a socket round
    /// trip, not a live HTTP call — but it's still kept off the main
    /// thread for consistency with the other tab-gated fetches.
    fn begin_pool_fetch(&mut self) {
        if self.pool_status_loading {
            return;
        }
        self.pool_status_loading = true;
        let socket_path = self.socket_path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = match call(&socket_path, &Request::PoolStatus) {
                Ok(Response::Ok { data: ResponseData::PoolStatus(s) }) => Some(s),
                _ => None,
            };
            let _ = tx.send(result);
        });
        self.pool_status_rx = Some(rx);
    }

    /// Returns true if a new pool-status snapshot arrived this tick.
    pub fn poll_pool(&mut self) -> bool {
        let Some(rx) = &self.pool_status_rx else { return false };
        let Ok(result) = rx.try_recv() else { return false };
        self.pool_status = result;
        self.pool_status_loading = false;
        self.pool_status_rx = None;
        true
    }

    /// Fetches per-platform free-pool key statuses on a background thread
    /// (`Request::ProviderKeyStatus { platform: None }` — all platforms).
    /// Same shape as `begin_pool_fetch`/`poll_pool`: one in-flight fetch,
    /// non-blocking, Pool-tab-gated.
    fn begin_provider_key_status_fetch(&mut self) {
        if self.provider_key_statuses_loading {
            return;
        }
        self.provider_key_statuses_loading = true;
        let socket_path = self.socket_path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = match call(&socket_path, &Request::ProviderKeyStatus { platform: None }) {
                Ok(Response::Ok { data: ResponseData::PoolKeyStatuses(statuses) }) => Some(statuses),
                _ => None,
            };
            let _ = tx.send(result);
        });
        self.provider_key_statuses_rx = Some(rx);
    }

    /// Returns true if new provider-key status data arrived this tick.
    pub fn poll_provider_key_status(&mut self) -> bool {
        let Some(rx) = &self.provider_key_statuses_rx else { return false };
        let Ok(result) = rx.try_recv() else { return false };
        self.provider_key_statuses = result;
        self.provider_key_statuses_loading = false;
        self.provider_key_statuses_rx = None;
        true
    }
}

fn build_quick_add_request(kind: QuickAddKind, parts: &[&str]) -> Result<Request, String> {
    let bad_format = || format!("expected: {}", kind.format_hint());
    match kind {
        QuickAddKind::Mcp => {
            let name = parts.first().filter(|s| !s.is_empty()).ok_or_else(bad_format)?;
            let command = parts.get(1).filter(|s| !s.is_empty()).ok_or_else(bad_format)?;
            let args = parts.get(2).map(|s| s.split(',').filter(|a| !a.is_empty()).map(|a| a.to_string()).collect()).unwrap_or_default();
            Ok(Request::McpAdd { server: single_protocol::McpServerSpec { name: name.to_string(), command: command.to_string(), args, env: Default::default(), secret_env: Default::default(), enabled: true } })
        }
        QuickAddKind::Lsp => {
            let name = parts.first().filter(|s| !s.is_empty()).ok_or_else(bad_format)?;
            let command = parts.get(1).filter(|s| !s.is_empty()).ok_or_else(bad_format)?;
            let args = parts.get(2).map(|s| s.split(',').filter(|a| !a.is_empty()).map(|a| a.to_string()).collect()).unwrap_or_default();
            let extensions = parts.get(3).map(|s| s.split(',').filter(|a| !a.is_empty()).map(|a| a.to_string()).collect()).unwrap_or_default();
            Ok(Request::LspAdd { server: single_protocol::LspServerSpec { name: name.to_string(), command: command.to_string(), args, extensions, enabled: true } })
        }
        QuickAddKind::Plugin => {
            let name = parts.first().filter(|s| !s.is_empty()).ok_or_else(bad_format)?;
            let target = parts.get(1).filter(|s| !s.is_empty()).ok_or_else(bad_format)?;
            let opencode_module = parts.get(2).filter(|s| !s.is_empty()).map(|s| s.to_string());
            Ok(Request::PluginAdd { plugin: single_protocol::PluginSpec { name: name.to_string(), target: target.to_string(), opencode_module } })
        }
        QuickAddKind::Tool => {
            let name = parts.first().filter(|s| !s.is_empty()).ok_or_else(bad_format)?;
            let description = parts.get(1).filter(|s| !s.is_empty()).ok_or_else(bad_format)?;
            let risk_level = match parts.get(2).copied().unwrap_or("low") {
                "low" => single_protocol::RiskLevel::Low,
                "medium" => single_protocol::RiskLevel::Medium,
                "high" => single_protocol::RiskLevel::High,
                other => return Err(format!("unknown risk level '{other}' (expected low/medium/high)")),
            };
            Ok(Request::ToolAdd { tool: single_protocol::ToolSpec { name: name.to_string(), description: description.to_string(), risk_level, enabled: true } })
        }
    }
}
