//! The gateway `ServerHandler`: exposes four tools
//! (`list_available_mcp_tools`, `invoke_mcp`, `notes_leave`, `notes_read`)
//! over stdio to whichever agent CLI has this binary registered as its one
//! MCP server, and lazily proxies to the real MCP servers listed in
//! SingleCLI's `mcp.toml` registry — spawned as child processes only on
//! first use, then reused for the rest of this gateway process's lifetime
//! (which is exactly the agent session's lifetime, since the agent's own
//! MCP client keeps this stdio child alive for as long as it's talking to
//! it). This deliberately holds no state in `divisid` itself — the
//! statefulness lives here, in a process the agent already owns.
//!
//! Discovery is two-step by design, not "list every tool from every
//! server upfront": `list_available_mcp_tools` returns only the *servers*
//! in the registry (name/command, no process spawned) — actually listing
//! one server's own tools means spawning it, which `invoke_mcp` does when
//! called with `tool: null`. This is a smaller, honest scope: probing
//! every registered server just to build an upfront catalog would spawn
//! exactly the N processes this design exists to avoid.
//!
//! **Idle eviction**: a spawned server stays in `sessions` (and its child
//! process alive) only while something's actually using it — a background
//! sweep (`spawn_idle_sweeper`) drops any session untouched for longer than
//! the idle timeout, so a task that used `postgres` for ten minutes and moved
//! on doesn't keep that process (and every other server it ever touched)
//! resident for the rest of the agent's session. Eviction is just removing
//! the map entry: once nothing else holds the `Arc<ChildSession>`,
//! `RunningService`'s own `Drop` cancels its background task and closes the
//! child process asynchronously (see rmcp's `service.rs` — this is its
//! documented cleanup path, not a shortcut taken here). A later call for
//! the same server just respawns it, same as first use.
//!
//! **`notes_leave`/`notes_read` (v0.1.17)** are not proxied to anything —
//! implemented directly here against `divisi_core::notes`, the same
//! SQLite-backed inbox `task::run`'s prompt preamble already reads from at
//! the start of every run. Since this gateway process stays alive for an
//! agent's whole session, these give the agent genuine mid-session
//! messaging: it can call `notes_read` at any point to check for new
//! notes, or `notes_leave` to tell a sibling agent something, without
//! waiting for its own run to end — the most honest version of "live"
//! coordination achievable given these agent CLIs don't expose real
//! process-level IPC (see `orchestrate.rs`'s module doc for why that part
//! stays out of scope). The gateway has no ambient identity for which
//! agent is calling it (it's spawned by whatever CLI configured it, with
//! no distinguishing env var SingleCLI controls), so `from_agent` is a
//! required argument, same as the `single note leave --from` CLI command.

use anyhow::{Context, Result};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData as McpError, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, RoleServer, ServerHandler, ServiceExt};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

type ChildSession = rmcp::service::RunningService<RoleClient, ()>;

/// How long a spawned server may sit unused before the sweep evicts it.
/// Generous enough that a task pausing between tool calls doesn't thrash
/// respawns, short enough that an agent's whole session doesn't keep every
/// server it ever touched resident. `SINGLE_MCP_IDLE_TIMEOUT_SECS`
/// overrides it — mainly so the lazy-spawn / reuse / eviction cycle can
/// be exercised end-to-end without a ten-minute wait.
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 600;
/// How often the sweep checks for idle sessions.
/// `SINGLE_MCP_SWEEP_INTERVAL_SECS` overrides it, same rationale.
const DEFAULT_SWEEP_INTERVAL_SECS: u64 = 60;

fn duration_from_env(var: &str, default_secs: u64) -> Duration {
    let secs = std::env::var(var).ok().and_then(|v| v.parse::<u64>().ok()).filter(|s| *s > 0).unwrap_or(default_secs);
    Duration::from_secs(secs)
}

fn idle_timeout() -> Duration {
    duration_from_env("SINGLE_MCP_IDLE_TIMEOUT_SECS", DEFAULT_IDLE_TIMEOUT_SECS)
}

fn sweep_interval() -> Duration {
    duration_from_env("SINGLE_MCP_SWEEP_INTERVAL_SECS", DEFAULT_SWEEP_INTERVAL_SECS)
}

struct SessionEntry {
    session: Arc<ChildSession>,
    last_used: Instant,
}

pub struct Gateway {
    sessions: Arc<Mutex<HashMap<String, SessionEntry>>>,
}

impl Gateway {
    pub fn new() -> Self {
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        spawn_idle_sweeper(sessions.clone());
        Gateway { sessions }
    }

    /// Registry servers, honoring `enabled` the same way the rest of
    /// SingleCLI does (`single mcp list` shows both, but only enabled
    /// servers are meant to be synced/used).
    fn registry() -> Result<Vec<divisi_protocol::McpServerSpec>> {
        let dirs = divisi_core::DivisiDirs::discover().context("resolving SingleCLI config directory")?;
        let servers = divisi_core::mcp::load(&dirs.mcp_registry_file())?;
        Ok(servers.into_iter().filter(|s| s.enabled).collect())
    }

    async fn ensure_session(&self, name: &str) -> Result<Arc<ChildSession>> {
        let mut sessions = self.sessions.lock().await;
        if let Some(existing) = sessions.get_mut(name) {
            existing.last_used = Instant::now();
            return Ok(existing.session.clone());
        }
        let servers = Self::registry()?;
        let spec = servers.into_iter().find(|s| s.name == name).with_context(|| {
            format!("no enabled mcp server named '{name}' (see `single mcp list`; disabled servers must be enabled first)")
        })?;

        let mut command = tokio::process::Command::new(&spec.command);
        command.args(&spec.args);
        for (k, v) in &spec.env {
            command.env(k, v);
        }
        // Secret-backed env vars are resolved here, at spawn time, and set
        // directly on the child's environment — never written to mcp.toml
        // or any other file. This is the one path in SingleCLI that
        // actually honors McpServerSpec::secret_env (see its doc comment).
        let secret_store = divisi_core::secrets::SecretTool;
        for (env_var, secret_key) in &spec.secret_env {
            let value = divisi_core::secrets::SecretStore::get(&secret_store, secret_key)?
                .with_context(|| format!("mcp server '{name}' needs secret '{secret_key}' (set with `single secret set {secret_key} <value>`)"))?;
            command.env(env_var, value);
        }
        let transport = TokioChildProcess::new(command.configure(|_| {})).with_context(|| format!("spawning mcp server '{name}'"))?;
        let session = ().serve(transport).await.with_context(|| format!("initializing mcp server '{name}'"))?;
        let session = Arc::new(session);
        sessions.insert(name.to_string(), SessionEntry { session: session.clone(), last_used: Instant::now() });
        Ok(session)
    }

    async fn list_available_mcp_tools(&self) -> Result<Value> {
        let servers = Self::registry()?;
        let catalog: Vec<Value> = servers
            .into_iter()
            .map(|s| json!({ "server": s.name, "command": format!("{} {}", s.command, s.args.join(" ")).trim() }))
            .collect();
        Ok(json!({
            "servers": catalog,
            "note": "each entry is a registered MCP server, not yet its individual tools \
                     (that would mean spawning every one up front) — call invoke_mcp with \
                     this server name and tool: null to lazily spawn it and list its real tools."
        }))
    }

    async fn invoke_mcp(&self, arguments: &Map<String, Value>) -> Result<Value> {
        let server = arguments.get("server").and_then(Value::as_str).context("invoke_mcp needs a \"server\" argument")?;
        let tool = arguments.get("tool").and_then(Value::as_str);
        let call_args = arguments.get("arguments").and_then(Value::as_object).cloned();

        match tool {
            None => {
                // Listing a server's own tools is read-only discovery, not
                // an action with consequences — not gated by permissions.
                let session = self.ensure_session(server).await?;
                let tools = session.list_all_tools().await.with_context(|| format!("listing tools on mcp server '{server}'"))?;
                Ok(json!({ "server": server, "tools": tools }))
            }
            Some(tool_name) => {
                // Checked before spawning anything: a denied/pending call
                // shouldn't pay for (or trigger) starting the underlying
                // server process at all.
                let resource = format!("mcp:{server}:{tool_name}");
                match check_permission(&resource)? {
                    divisi_core::preferences::Verdict::Deny => {
                        return Ok(json!({
                            "server": server, "tool": tool_name, "denied": true,
                            "reason": "blocked by permission policy (permissions.toml or a learned preference)"
                        }));
                    }
                    divisi_core::preferences::Verdict::PendingApproval(id) => {
                        return Ok(json!({
                            "server": server, "tool": tool_name, "pending_approval": id,
                            "message": format!(
                                "this action needs your approval first — run `single approval resolve {id} --allow` (or --deny), then retry"
                            )
                        }));
                    }
                    divisi_core::preferences::Verdict::Allow => {}
                }

                let session = self.ensure_session(server).await?;
                let mut params = CallToolRequestParams::new(tool_name.to_string());
                if let Some(args) = call_args {
                    params = params.with_arguments(args);
                }
                let result = session.call_tool(params).await.with_context(|| format!("calling '{tool_name}' on mcp server '{server}'"))?;
                Ok(json!({ "server": server, "tool": tool_name, "result": result }))
            }
        }
    }

    /// Opens a fresh connection to the shared state db for every call
    /// (this gateway holds no long-lived db handle, matching its module
    /// doc's "no state in divisid" — the one place state lives is
    /// the db file itself). WAL + busy_timeout mirror
    /// `divisi-runtime::state::open`'s fix for the same real concurrency
    /// this introduces: an agent's mid-session note write racing the
    /// daemon's own writes to the same file.
    fn notes_conn() -> Result<rusqlite::Connection> {
        let dirs = divisi_core::DivisiDirs::discover().context("resolving SingleCLI config directory")?;
        let db_path = dirs.db_path();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = rusqlite::Connection::open(&db_path).with_context(|| format!("opening {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL").context("setting WAL journal mode")?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).context("setting busy timeout")?;
        divisi_core::notes::ensure_schema(&conn)?;
        Ok(conn)
    }

    /// Best-effort project scope from this gateway process's own cwd —
    /// there's no separate "which project is this session for" signal to
    /// read, so this mirrors how `task::run` resolves project scope from
    /// the directory it's actually working in.
    fn current_project() -> Option<String> {
        std::env::current_dir().ok().and_then(|cwd| divisi_core::project_context::resolve(&cwd).repo_root)
    }

    async fn notes_leave(&self, arguments: &Map<String, Value>) -> Result<Value> {
        let from_agent = arguments.get("from_agent").and_then(Value::as_str).context("notes_leave needs a \"from_agent\" argument")?;
        let to_agent = arguments.get("to_agent").and_then(Value::as_str);
        let topic = arguments.get("topic").and_then(Value::as_str).context("notes_leave needs a \"topic\" argument")?;
        let content = arguments.get("content").and_then(Value::as_str).context("notes_leave needs a \"content\" argument")?;

        let conn = Self::notes_conn()?;
        let project = Self::current_project();
        let id = divisi_core::notes::leave(&conn, project, from_agent, to_agent, topic, content)?;
        Ok(json!({ "note_id": id, "from": from_agent, "to": to_agent, "topic": topic }))
    }

    async fn notes_read(&self, arguments: &Map<String, Value>) -> Result<Value> {
        let to_agent = arguments.get("to_agent").and_then(Value::as_str).context("notes_read needs a \"to_agent\" argument (which agent's inbox to read)")?;
        let unread_only = arguments.get("unread_only").and_then(Value::as_bool).unwrap_or(true);

        let conn = Self::notes_conn()?;
        let project = Self::current_project();
        let notes = divisi_core::notes::inbox(&conn, project.as_deref(), to_agent, unread_only)?;
        for n in &notes {
            let _ = divisi_core::notes::mark_read(&conn, n.id);
        }
        Ok(json!({ "notes": notes }))
    }
}

/// True once `last_used` is far enough in the past to evict — split out
/// from the sweep loop so the threshold logic is testable without a real
/// timer/tokio runtime.
fn is_idle(last_used: Instant, now: Instant, timeout: Duration) -> bool {
    now.saturating_duration_since(last_used) >= timeout
}

/// Runs for the lifetime of the gateway process, dropping any spawned
/// server's session once it's been unused for the idle timeout. Holds its own
/// `Arc` clone of the map rather than a reference to `Gateway`, so it can be
/// started from `Gateway::new()` before the `Gateway` itself is handed to
/// `.serve()`.
fn spawn_idle_sweeper(sessions: Arc<Mutex<HashMap<String, SessionEntry>>>) {
    tokio::spawn(async move {
        let (sweep, timeout) = (sweep_interval(), idle_timeout());
        loop {
            tokio::time::sleep(sweep).await;
            let now = Instant::now();
            let mut sessions = sessions.lock().await;
            sessions.retain(|_, entry| !is_idle(entry.last_used, now, timeout));
        }
    });
}

/// The real first caller of `permissions::evaluate` (via
/// `preferences::evaluate_and_learn`) — every `invoke_mcp` tool call is
/// checked against `permissions.toml`'s `tools` rules, then learned
/// preferences, before being proxied. Opens the same SQLite db the daemon
/// uses directly (see this file's module doc for why) rather than
/// round-tripping through the socket.
fn check_permission(resource: &str) -> Result<divisi_core::preferences::Verdict> {
    let dirs = divisi_core::DivisiDirs::discover().context("resolving SingleCLI config directory")?;
    let rules = divisi_core::permissions::load(&dirs.permissions_file())?;
    let db_path = dirs.db_path();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let conn = rusqlite::Connection::open(&db_path).with_context(|| format!("opening {}", db_path.display()))?;
    divisi_core::preferences::ensure_schema(&conn)?;
    divisi_core::preferences::evaluate_and_learn(&rules.tools, &conn, resource, Some("divisi-gateway invoke_mcp"))
}

fn schema(fields: Value) -> Arc<Map<String, Value>> {
    let Value::Object(map) = fields else { unreachable!("schema() is always called with a json!({{...}}) object literal") };
    Arc::new(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `DivisiDirs::discover()` (used by `registry()`, `notes_conn`,
    /// `current_project`, `check_permission`) reads the `SINGLE_CONFIG_DIR`
    /// env var fresh on every call rather than taking an injected
    /// `DivisiDirs` — a real constraint shared with `check_permission`'s
    /// existing design, not something worth reworking just for tests. Rust
    /// runs tests in this module concurrently by default, and that env var
    /// is process-global, so any test that sets it races every other one
    /// that does. This mutex serializes just those tests.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn registry_only_returns_enabled_servers() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SINGLE_CONFIG_DIR", dir.path());
        let dirs = divisi_core::DivisiDirs::discover().unwrap();
        // default_servers() ships a mix of enabled (git, memory, fetch,
        // sequential-thinking) and disabled (filesystem, github, ...) —
        // registry() must only surface the enabled ones.
        divisi_core::mcp::save(&dirs.mcp_registry_file(), &divisi_core::mcp::default_servers()).unwrap();

        let servers = Gateway::registry().unwrap();
        assert!(servers.iter().all(|s| s.enabled));
        assert!(servers.iter().any(|s| s.name == "git"));
        assert!(!servers.iter().any(|s| s.name == "filesystem"), "disabled servers must not appear");
    }

    #[test]
    fn is_idle_evicts_only_once_the_timeout_has_actually_elapsed() {
        let last_used = Instant::now();
        let timeout = Duration::from_secs(600);
        assert!(!is_idle(last_used, last_used + Duration::from_secs(599), timeout));
        assert!(is_idle(last_used, last_used + Duration::from_secs(600), timeout));
        assert!(is_idle(last_used, last_used + Duration::from_secs(601), timeout));
    }

    #[test]
    fn schema_extracts_the_object_from_a_json_literal() {
        let obj = schema(json!({ "type": "object", "properties": {} }));
        assert_eq!(obj.get("type").and_then(Value::as_str), Some("object"));
    }

    #[tokio::test]
    async fn notes_leave_then_notes_read_round_trips_through_the_gateway() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SINGLE_CONFIG_DIR", dir.path());
        let gateway = Gateway::new();

        let leave_args = json!({ "from_agent": "claude", "to_agent": "codex", "topic": "heads up", "content": "watch the flaky test" });
        let Value::Object(leave_args) = leave_args else { unreachable!() };
        let left = gateway.notes_leave(&leave_args).await.unwrap();
        assert!(left["note_id"].as_i64().is_some());

        let read_args = json!({ "to_agent": "codex" });
        let Value::Object(read_args) = read_args else { unreachable!() };
        let result = gateway.notes_read(&read_args).await.unwrap();
        let notes = result["notes"].as_array().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0]["from_agent"], "claude");
        assert_eq!(notes[0]["content"], "watch the flaky test");

        // A second read shouldn't redeliver it — notes_read marks read.
        let result2 = gateway.notes_read(&read_args).await.unwrap();
        assert!(result2["notes"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn notes_leave_requires_from_agent_topic_and_content() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SINGLE_CONFIG_DIR", dir.path());
        let gateway = Gateway::new();

        let Value::Object(missing_from) = json!({ "topic": "t", "content": "c" }) else { unreachable!() };
        assert!(gateway.notes_leave(&missing_from).await.is_err());
    }

    #[tokio::test]
    async fn notes_read_sees_broadcast_notes() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SINGLE_CONFIG_DIR", dir.path());
        let gateway = Gateway::new();

        let Value::Object(leave_args) = json!({ "from_agent": "claude", "topic": "fyi", "content": "for everyone" }) else { unreachable!() };
        gateway.notes_leave(&leave_args).await.unwrap();

        let Value::Object(read_args) = json!({ "to_agent": "agy" }) else { unreachable!() };
        let result = gateway.notes_read(&read_args).await.unwrap();
        assert_eq!(result["notes"].as_array().unwrap().len(), 1);
    }
}

impl ServerHandler for Gateway {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("divisi-gateway", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Dynamic MCP gateway for SingleCLI. Call list_available_mcp_tools first to see \
                 registered servers, then invoke_mcp with {server, tool: null} to discover that \
                 server's real tools, then invoke_mcp with {server, tool, arguments} to call one. \
                 Also exposes notes_leave/notes_read for messaging other agents working on this \
                 same project, live during your session — call notes_read any time to check for \
                 new notes, and notes_leave to tell another agent (or everyone) something.",
            )
    }

    async fn list_tools(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(vec![
            Tool::new(
                "list_available_mcp_tools",
                "Lists MCP servers SingleCLI has registered and enabled, without spawning any of them.",
                schema(json!({ "type": "object", "properties": {}, "additionalProperties": false })),
            ),
            Tool::new(
                "invoke_mcp",
                "Lazily spawns (or reuses) a registered MCP server and either lists its tools \
                 (tool: null) or calls one of them (tool: <name>, arguments: {...}).",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "Registered server name, from list_available_mcp_tools." },
                        "tool": { "type": "string", "description": "Tool name to call; omit to list this server's tools instead." },
                        "arguments": { "type": "object", "description": "Arguments for the tool call, if any." }
                    },
                    "required": ["server"],
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "notes_leave",
                "Leaves a note for another agent (or, with to_agent omitted, a broadcast note for \
                 every agent) working on this project. Delivered immediately to notes_read, and \
                 also to the recipient's next `single task run` prompt preamble.",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "from_agent": { "type": "string", "description": "Your own agent name, e.g. \"claude\" or \"codex\"." },
                        "to_agent": { "type": "string", "description": "Recipient agent name; omit to broadcast to every agent on this project." },
                        "topic": { "type": "string", "description": "Short topic/subject line." },
                        "content": { "type": "string", "description": "The note itself." }
                    },
                    "required": ["from_agent", "topic", "content"],
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "notes_read",
                "Reads notes left for an agent on this project (including broadcast notes), \
                 newest first. Marks returned notes as read.",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "to_agent": { "type": "string", "description": "Your own agent name — reads notes addressed to you." },
                        "unread_only": { "type": "boolean", "description": "Defaults to true; set false to also see already-read notes." }
                    },
                    "required": ["to_agent"],
                    "additionalProperties": false
                })),
            ),
        ]))
    }

    async fn call_tool(&self, request: CallToolRequestParams, _context: RequestContext<RoleServer>) -> Result<CallToolResponse, McpError> {
        let empty = Map::new();
        let arguments = request.arguments.as_ref().unwrap_or(&empty);
        let result = match request.name.as_ref() {
            "list_available_mcp_tools" => self.list_available_mcp_tools().await,
            "invoke_mcp" => self.invoke_mcp(arguments).await,
            "notes_leave" => self.notes_leave(arguments).await,
            "notes_read" => self.notes_read(arguments).await,
            other => Err(anyhow::anyhow!("unknown tool: {other}")),
        };
        match result {
            Ok(value) => Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
            )])
            .into()),
            Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))]).into()),
        }
    }
}
