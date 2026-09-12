//! Routes an open document's URI to the SingleCLI LSP registry entry that
//! should handle it, and manages the spawned backend processes.
//!
//! Claude Code spawns exactly one `single-lsp` process for the whole plugin
//! manifest, so a single session mixes documents from many languages. Each
//! document is routed by file extension to the registry entry that owns it;
//! the backing language server is spawned on first use and reused for every
//! later document of the same language, then evicted once it has sat idle too
//! long — see `Multiplexer::evict_idle_backends`.
//!
//! # Threading and lock discipline
//!
//! One thread reads client messages from stdin and drives
//! [`Multiplexer::handle_client_message`]. Each spawned backend gets its own
//! reader thread. Everything bound for the client funnels through a single
//! `mpsc` channel to one writer thread, so concurrent backends can never
//! interleave halves of a framed message on stdout.
//!
//! That channel's `Sender` is shared behind `&self` by every one of those
//! threads, which is sound because `std::sync::mpsc::Sender<T>` is `Sync` for
//! `T: Send` — true since std's mpsc was rebuilt on crossbeam, and load-bearing
//! enough that `multiplexer_is_shareable_across_threads` asserts it at compile
//! time rather than leaving it to be rediscovered.
//!
//! No lock in this type is ever held while acquiring another *except*
//! `backends`, which may be held while taking `next_id`, `client_init_params`
//! or `live_readers`. Those are leaf locks — acquired, used and released
//! within a single expression, never wrapping another acquisition — so no
//! cycle exists and the design is deadlock-free. In particular shutdown
//! releases `backends` before waiting on `readers_drained`, because the reader
//! threads it is waiting for take `backends` on their way out.

use crate::framing::{read_message, write_message};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use single_protocol::LspServerSpec;
use std::collections::HashMap;
use std::io::BufReader;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub struct Router {
    by_file_name: HashMap<String, LspServerSpec>,
    by_extension: HashMap<String, LspServerSpec>,
}

impl Router {
    pub fn from_registry(specs: Vec<LspServerSpec>) -> Self {
        let mut by_file_name = HashMap::new();
        let mut by_extension = HashMap::new();
        for spec in specs.into_iter().filter(|s| s.enabled) {
            for ext in &spec.extensions {
                // The registry keys some presets by an exact file name rather
                // than by suffix — `Dockerfile`, `CMakeLists.txt`, `Justfile`,
                // `meson.build`, `nginx.conf`, `docker-compose.yml`. A leading
                // dot is what tells the two kinds of key apart.
                let table = if ext.starts_with('.') {
                    &mut by_extension
                } else {
                    &mut by_file_name
                };
                // First registered preset for a given key wins; SingleCLI's
                // own registry is the source of truth for which preset is
                // "the" handler for an extension, same as `single lsp list`
                // shows only one row per extension in practice.
                table.entry(ext.to_ascii_lowercase()).or_insert_with(|| spec.clone());
            }
        }
        Self {
            by_file_name,
            by_extension,
        }
    }

    pub fn route(&self, uri: &str) -> Option<&LspServerSpec> {
        // An exact file name is the more specific match, so it is tried first:
        // `docker-compose.yml` is a compose file before it is a generic `.yml`.
        if let Some(spec) = self.by_file_name.get(&file_name_of(uri).to_ascii_lowercase()) {
            return Some(spec);
        }
        self.by_extension.get(&extension_of(uri)?)
    }
}

/// The last path segment of `uri` — the file's own name. Only this segment can
/// carry an extension; a dot in a *directory* name is not one.
fn file_name_of(uri: &str) -> &str {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    match path.rfind('/') {
        Some(slash) => &path[slash + 1..],
        None => path,
    }
}

/// The extension of the last path segment of `uri`, lowercased and including
/// the leading dot. Leading-dot names (`.gitignore`) count as extensionless.
fn extension_of(uri: &str) -> Option<String> {
    let name = file_name_of(uri);
    let dot = name.rfind('.')?;
    if dot == 0 || dot + 1 == name.len() {
        return None;
    }
    Some(name[dot..].to_ascii_lowercase())
}

pub fn load_registry() -> Result<Vec<LspServerSpec>> {
    let dirs = single_core::SingleDirs::discover().context("resolving SingleCLI config directory")?;
    single_core::lsp::load(&dirs.lsp_registry_file())
}

struct Backend {
    _child: Child,
    stdin: ChildStdin,
}

pub struct Multiplexer {
    router: Router,
    backends: Mutex<HashMap<String, Backend>>,
    uri_to_backend: Mutex<HashMap<String, String>>,
    next_id: Mutex<i64>,
    /// Requests the client made: proxy-assigned id -> (client's own id, owning backend).
    pending: Mutex<HashMap<i64, (Value, String)>>,
    /// Requests a *backend* made: proxy-assigned id -> (backend's own id, owning backend).
    /// Backends number their requests independently, so two of them can easily
    /// both pick id `1`; re-issuing them in the proxy's id space keeps the
    /// client's replies routable back to the right backend.
    pending_server: Mutex<HashMap<i64, (Value, String)>>,
    /// The client's `initialize` params, replayed to every backend we spawn so
    /// each one learns the workspace root it is supposed to index.
    client_init_params: Mutex<Option<Value>>,
    /// When each backend last received a request/notification from the client,
    /// used by the reaper to evict backends that have been idle too long — see
    /// `evict_idle_backends`. A long-lived session would otherwise hold every
    /// language server it ever spawned resident until the proxy exits.
    last_activity: Mutex<HashMap<String, Instant>>,
    /// How many backend reader threads are still running and could still put a
    /// message on `client_out`. Paired with `readers_drained` so shutdown can
    /// wait for them to go quiet — see `close_client_output`.
    live_readers: Mutex<usize>,
    readers_drained: Condvar,
    /// The single funnel to the writer thread — see the module docs.
    client_out: Sender<Value>,
}

/// How long shutdown waits for backend reader threads to finish forwarding
/// before giving up on them and closing client output anyway.
const READER_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a backend can go without any client traffic before the reaper
/// evicts it. Kept generous on purpose: eviction means re-spawning (and for a
/// heavy server, re-indexing) on next use, so the win only pays off when the
/// server would otherwise sit resident through a long idle stretch of the
/// session.
const IDLE_EVICT_TIMEOUT: Duration = Duration::from_secs(300);

/// How often the reaper thread scans for idle backends.
const IDLE_EVICT_POLL: Duration = Duration::from_secs(30);

impl Multiplexer {
    pub fn new(router: Router, client_out: Sender<Value>) -> Arc<Self> {
        Arc::new(Self {
            router,
            backends: Mutex::new(HashMap::new()),
            uri_to_backend: Mutex::new(HashMap::new()),
            next_id: Mutex::new(1),
            pending: Mutex::new(HashMap::new()),
            pending_server: Mutex::new(HashMap::new()),
            client_init_params: Mutex::new(None),
            last_activity: Mutex::new(HashMap::new()),
            live_readers: Mutex::new(0),
            readers_drained: Condvar::new(),
            client_out,
        })
    }

    fn allocate_id(&self) -> i64 {
        let mut next = self.next_id.lock().unwrap();
        let id = *next;
        *next += 1;
        id
    }

    fn send_to_client(&self, message: Value) {
        let _ = self.client_out.send(message);
    }

    /// Signals the writer thread that no more output is coming, so `main` can
    /// join it and know everything queued actually reached stdout instead of
    /// racing the last few replies against process teardown.
    ///
    /// A bare JSON `null` is never a valid LSP message — and a backend that
    /// sends one is dropped as malformed rather than forwarded — so it is
    /// unambiguous as the end-of-output sentinel. It goes through the same
    /// `Sender` as every other message, so mpsc's per-sender FIFO ordering
    /// guarantees it is dequeued last.
    ///
    /// The sentinel alone would only narrow the loss window, not close it: a
    /// backend reader thread can still enqueue a message *after* it and have
    /// the writer drop it. So this first drops every backend's stdin — giving
    /// each language server EOF, the second exit signal after the `exit`
    /// notification already broadcast — and then waits for every reader thread
    /// to finish. A reader decrements `live_readers` only after its last send,
    /// so observing zero means nothing more can be enqueued and the sentinel
    /// really is last.
    ///
    /// The wait is bounded rather than a `JoinHandle::join`: a language server
    /// that ignores both `exit` and stdin EOF would otherwise hang the proxy
    /// forever on shutdown, turning a dropped message into a stuck process.
    /// On timeout we fall back to the sentinel-only behaviour.
    pub fn close_client_output(&self) {
        // Not held across the wait below — `retire_backend` takes this lock.
        self.backends.lock().unwrap().clear();

        let live = self.live_readers.lock().unwrap();
        let _ = self
            .readers_drained
            .wait_timeout_while(live, READER_DRAIN_TIMEOUT, |live| *live > 0);

        self.send_to_client(Value::Null);
    }

    /// Spawns the background reaper that evicts idle backends. The process
    /// itself is torn down from `main` on `exit`, which kills this thread too,
    /// so it needs no shutdown signal of its own.
    pub fn start_reaper(self: &Arc<Self>) {
        let this = self.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(IDLE_EVICT_POLL);
            this.evict_idle_backends();
        });
    }

    /// Terminates every backend that has had no client traffic for longer than
    /// `IDLE_EVICT_TIMEOUT`. Runs on the reaper thread; exposed as a plain
    /// method so tests can drive it deterministically instead of sleeping.
    fn evict_idle_backends(&self) {
        let now = Instant::now();
        let idle: Vec<String> = {
            let backends = self.backends.lock().unwrap();
            let activity = self.last_activity.lock().unwrap();
            backends
                .keys()
                .filter(|name| {
                    activity
                        .get(*name)
                        .map(|last| now.duration_since(*last) > IDLE_EVICT_TIMEOUT)
                        .unwrap_or(true)
                })
                .cloned()
                .collect()
        };
        for name in idle {
            self.evict_backend(&name);
        }
    }

    /// Kills one backend and lets its reader thread retire it. The reader
    /// observes EOF on the broken stdout pipe and runs `retire_backend`, which
    /// clears `uri_to_backend`/`pending`/`pending_server` and fails any
    /// outstanding requests — so a process being evicted cannot strand a
    /// client waiting on a reply. The `shutdown`/`exit` pair is the graceful
    /// first step LSP servers expect before their stdin closes and they are
    /// forced down.
    fn evict_backend(&self, name: &str) {
        let mut backends = self.backends.lock().unwrap();
        let Some(mut backend) = backends.remove(name) else {
            return;
        };
        self.last_activity.lock().unwrap().remove(name);
        {
            let mut stdin = &backend.stdin;
            let _ = write_message(&mut stdin, &json!({ "jsonrpc": "2.0", "method": "shutdown", "params": {} }));
            let _ = write_message(&mut stdin, &json!({ "jsonrpc": "2.0", "method": "exit", "params": {} }));
        }
        // Close the child's stdin, the standard LSP "quit" signal, before the
        // hard kill below — best-effort grace that costs nothing if the kill
        // lands first.
        drop(backend.stdin);
        let pid = backend._child.id();
        let _ = backend._child.kill();
        #[cfg(unix)]
        kill_process_group(pid);
    }

    fn extract_uri(message: &Value) -> Option<String> {
        message
            .pointer("/params/textDocument/uri")
            .or_else(|| message.pointer("/params/uri"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn send_to_backend(&self, backend_name: &str, message: &Value) -> Result<()> {
        let backends = self.backends.lock().unwrap();
        let backend = backends
            .get(backend_name)
            .with_context(|| format!("language server `{backend_name}` is not running"))?;
        // `impl Write for &ChildStdin` — writing takes a shared borrow, and
        // only the client thread ever writes to a backend.
        let mut stdin = &backend.stdin;
        write_message(&mut stdin, message)
    }

    /// Spawns (or reuses) the backend for `spec`, starting its reader thread on
    /// first spawn. The `backends` lock is held across the whole spawn, so two
    /// concurrent callers for the same spec can never start two processes.
    fn ensure_backend(self: &Arc<Self>, spec: &LspServerSpec) -> Result<()> {
        let mut backends = self.backends.lock().unwrap();
        if backends.contains_key(&spec.name) {
            return Ok(());
        }
        let mut cmd = Command::new(&spec.command);
        cmd.args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Spawn the server as its own process-group leader so eviction can
        // `killpg` the whole group — a server's helper processes would
        // otherwise survive a plain child kill and keep the stdout pipe open,
        // stranding the reader thread and its pending requests.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning language server `{}`", spec.command))?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        // A real language server rejects everything until it has been
        // initialized, so the proxy performs that handshake itself, replaying
        // the client's own `initialize` params. The request carries a proxy id
        // that is deliberately *not* registered in `pending`, so the reader
        // thread drops the reply instead of leaking it to the client. The two
        // messages are pipelined: a backend reads its stdin in order, so it
        // always sees `initialize` first. Done before the backend is published
        // into `backends` so a handshake failure leaves no half-live entry.
        let init_params = self
            .client_init_params
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| json!({ "processId": Value::Null, "rootUri": Value::Null, "capabilities": {} }));
        write_message(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": self.allocate_id(),
                "method": "initialize",
                "params": init_params,
            }),
        )
        .with_context(|| format!("initializing language server `{}`", spec.name))?;
        write_message(&mut stdin, &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }))
            .with_context(|| format!("initializing language server `{}`", spec.name))?;

        backends.insert(spec.name.clone(), Backend { _child: child, stdin });
        drop(backends);

        let this = self.clone();
        let backend_name = spec.name.clone();
        // Counted before the thread exists, so shutdown can never observe zero
        // live readers while one is still starting up.
        *self.live_readers.lock().unwrap() += 1;
        std::thread::spawn(move || this.read_from_backend(backend_name, stdout));
        Ok(())
    }

    fn read_from_backend(self: Arc<Self>, backend_name: String, stdout: ChildStdout) {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut message = match read_message(&mut reader) {
                Ok(Some(message)) => message,
                Ok(None) | Err(_) => break, // backend exited or its pipe broke
            };
            // A `method` is what separates a request/notification *from* the
            // server from a response *to* one of ours — both carry an `id`.
            let has_method = message.get("method").is_some();
            let id = message.get("id").cloned().filter(|value| !value.is_null());

            let forwarded = match (has_method, id) {
                (true, Some(server_id)) => {
                    // Server→client request: re-issue it in the proxy's id
                    // space and remember where the client's answer goes.
                    let proxy_id = self.allocate_id();
                    self.pending_server
                        .lock()
                        .unwrap()
                        .insert(proxy_id, (server_id, backend_name.clone()));
                    message["id"] = Value::from(proxy_id);
                    self.client_out.send(message)
                }
                // Notification (e.g. publishDiagnostics) — forward as-is.
                (true, None) => self.client_out.send(message),
                (false, Some(id)) => {
                    // Response to a request we forwarded. Proxy ids are always
                    // integers, so anything else is not ours.
                    let client_id = id.as_i64().and_then(|id| {
                        let mut pending = self.pending.lock().unwrap();
                        match pending.get(&id) {
                            Some((_, owner)) if owner == &backend_name => {
                                pending.remove(&id).map(|(client_id, _)| client_id)
                            }
                            // Unknown id — the reply to our own `initialize`
                            // handshake, or an id this backend does not own.
                            // Leave any entry in place rather than evicting
                            // another backend's pending request.
                            _ => None,
                        }
                    });
                    match client_id {
                        Some(client_id) => {
                            message["id"] = client_id;
                            self.client_out.send(message)
                        }
                        None => Ok(()),
                    }
                }
                (false, None) => Ok(()), // malformed; nothing to do with it
            };
            if forwarded.is_err() {
                break; // writer thread is gone, so the client is too
            }
        }
        self.retire_backend(&backend_name);

        // Strictly after every send this thread will ever make, so a shutdown
        // that observes zero knows nothing more can reach `client_out`.
        let mut live = self.live_readers.lock().unwrap();
        *live -= 1;
        if *live == 0 {
            self.readers_drained.notify_all();
        }
    }

    /// Cleans up after a backend that exited: forget it so the next document
    /// for that language spawns a fresh one, and fail its outstanding requests
    /// rather than leaving the client waiting forever.
    fn retire_backend(&self, backend_name: &str) {
        self.backends.lock().unwrap().remove(backend_name);
        self.uri_to_backend.lock().unwrap().retain(|_, name| name != backend_name);
        self.pending_server
            .lock()
            .unwrap()
            .retain(|_, (_, owner)| owner != backend_name);

        let orphaned: Vec<Value> = {
            let mut pending = self.pending.lock().unwrap();
            let ids: Vec<i64> = pending
                .iter()
                .filter(|(_, (_, owner))| owner == backend_name)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id).map(|(client_id, _)| client_id))
                .collect()
        };
        for client_id in orphaned {
            self.send_to_client(error_response(
                client_id,
                INTERNAL_ERROR,
                &format!("language server `{backend_name}` exited"),
            ));
        }
    }

    /// Handles one message received from the client (Claude Code).
    ///
    /// Never fails the process over a single message: anything unroutable is
    /// answered with a JSON-RPC error (for requests) or dropped (for
    /// notifications), so one unsupported file type cannot take down a session
    /// covering 150+ extensions.
    pub fn handle_client_message(self: &Arc<Self>, message: Value) -> Result<()> {
        let method = message.get("method").and_then(Value::as_str).map(str::to_string);
        let client_id = message.get("id").cloned().filter(|value| !value.is_null());

        let Some(method) = method else {
            // No method: this is the client answering a request a backend made.
            if let Some(id) = client_id.as_ref().and_then(Value::as_i64) {
                let route = self.pending_server.lock().unwrap().remove(&id);
                if let Some((server_id, backend_name)) = route {
                    let mut outgoing = message;
                    outgoing["id"] = server_id;
                    let _ = self.send_to_backend(&backend_name, &outgoing);
                }
            }
            return Ok(());
        };

        if method == "initialize" {
            // The proxy owns the handshake with the client: it cannot delegate
            // it to a backend, because at this point no document has been
            // opened and there is nothing to route on. Each backend gets these
            // same params replayed when it is spawned (see `ensure_backend`).
            *self.client_init_params.lock().unwrap() = message.get("params").cloned();
            if let Some(id) = client_id {
                self.send_to_client(json!({ "jsonrpc": "2.0", "id": id, "result": initialize_result() }));
            }
            return Ok(());
        }

        if method == "shutdown" || method == "exit" {
            let names: Vec<String> = self.backends.lock().unwrap().keys().cloned().collect();
            for name in names {
                let mut outgoing = message.clone();
                if outgoing.get("id").is_some() {
                    // Unregistered proxy id: each backend's reply is dropped,
                    // and the client is answered once, below.
                    outgoing["id"] = Value::from(self.allocate_id());
                }
                let _ = self.send_to_backend(&name, &outgoing);
            }
            if let Some(id) = client_id {
                self.send_to_client(json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null }));
            }
            return Ok(());
        }

        let Some(uri) = Self::extract_uri(&message) else {
            // Nothing to route on. Answering requests with an error beats
            // leaving the client blocked on a reply that will never come.
            if let Some(id) = client_id {
                self.send_to_client(error_response(
                    id,
                    METHOD_NOT_FOUND,
                    &format!("single-lsp cannot route `{method}`: no document URI in its params"),
                ));
            }
            return Ok(());
        };

        // Keyed by preset name, not by URI — two `.rs` files share one
        // `rust-analyzer`. Note the map is not held across `ensure_backend`,
        // so `uri_to_backend` and `backends` are never nested.
        let known = self.uri_to_backend.lock().unwrap().get(&uri).cloned();
        let backend_name = match known {
            Some(name) => name,
            None => {
                let Some(spec) = self.router.route(&uri).cloned() else {
                    if let Some(id) = client_id {
                        self.send_to_client(error_response(
                            id,
                            METHOD_NOT_FOUND,
                            &format!("no LSP server registered for {uri}"),
                        ));
                    }
                    return Ok(());
                };
                if let Err(err) = self.ensure_backend(&spec) {
                    if let Some(id) = client_id {
                        self.send_to_client(error_response(id, INTERNAL_ERROR, &format!("{err:#}")));
                    }
                    return Ok(());
                }
                self.uri_to_backend.lock().unwrap().insert(uri.clone(), spec.name.clone());
                spec.name
            }
        };

        // Any traffic resets the idle clock for this backend — a request or
        // notification is evidence the language server is still in active use.
        self.last_activity.lock().unwrap().insert(backend_name.clone(), Instant::now());

        if method == "textDocument/didClose" {
            self.uri_to_backend.lock().unwrap().remove(&uri);
        }

        let mut outgoing = message;
        let mut proxy_id = None;
        if let Some(id) = client_id {
            let assigned = self.allocate_id();
            self.pending.lock().unwrap().insert(assigned, (id, backend_name.clone()));
            outgoing["id"] = Value::from(assigned);
            proxy_id = Some(assigned);
        }

        if let Err(err) = self.send_to_backend(&backend_name, &outgoing) {
            let stranded = proxy_id.and_then(|id| self.pending.lock().unwrap().remove(&id));
            if let Some((client_id, _)) = stranded {
                self.send_to_client(error_response(client_id, INTERNAL_ERROR, &format!("{err:#}")));
            }
        }
        Ok(())
    }
}

const METHOD_NOT_FOUND: i64 = -32601;
const INTERNAL_ERROR: i64 = -32603;

/// SIGKILLs every process in the group led by `pid` — the backend and any
/// helpers it spawned. `Command::process_group(0)` made the backend a group
/// leader, so `pgid == pid`; a negative argument to `kill` would mean the same
/// thing but `killpg` is the dedicated, clearer spelling.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    unsafe {
        libc::killpg(pid as i32, libc::SIGKILL);
    }
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// What the proxy tells the client it can do.
///
/// The proxy answers `initialize` before any backend exists, so it cannot
/// report a real backend's capabilities — it advertises the common set and
/// lets an individual backend reply with a method error if it lacks one. Sync
/// is declared `Full` (kind 1) deliberately: it is the one document-sync mode
/// every backend accepts, and a session mixes backends, so the lowest common
/// denominator is the only safe choice.
fn initialize_result() -> Value {
    json!({
        "capabilities": {
            "textDocumentSync": { "openClose": true, "change": 1 },
            "hoverProvider": true,
            "definitionProvider": true,
            "typeDefinitionProvider": true,
            "implementationProvider": true,
            "referencesProvider": true,
            "documentHighlightProvider": true,
            "documentSymbolProvider": true,
            "workspaceSymbolProvider": true,
            "codeActionProvider": true,
            "renameProvider": true,
            "documentFormattingProvider": true,
            "completionProvider": { "resolveProvider": false },
            "signatureHelpProvider": { "triggerCharacters": ["(", ","] }
        },
        "serverInfo": { "name": "single-lsp", "version": env!("CARGO_PKG_VERSION") }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, exts: &[&str]) -> LspServerSpec {
        LspServerSpec {
            name: name.to_string(),
            command: name.to_string(),
            args: Vec::new(),
            extensions: exts.iter().map(|e| e.to_string()).collect(),
            enabled: true,
        }
    }

    #[test]
    fn routes_by_extension() {
        let router = Router::from_registry(vec![
            spec("rust-analyzer", &[".rs"]),
            spec("pyright", &[".py", ".pyi"]),
        ]);
        assert_eq!(router.route("file:///project/src/main.rs").unwrap().name, "rust-analyzer");
        assert_eq!(router.route("file:///project/lib.pyi").unwrap().name, "pyright");
        assert!(router.route("file:///project/README.md").is_none());
    }

    #[test]
    fn ignores_disabled_presets() {
        let mut disabled = spec("gopls", &[".go"]);
        disabled.enabled = false;
        let router = Router::from_registry(vec![disabled]);
        assert!(router.route("file:///main.go").is_none());
    }

    #[test]
    fn first_registered_preset_wins_for_a_shared_extension() {
        let router = Router::from_registry(vec![spec("tsserver", &[".ts"]), spec("deno", &[".ts"])]);
        assert_eq!(router.route("file:///a.ts").unwrap().name, "tsserver");
    }

    #[test]
    fn routing_is_case_insensitive() {
        let router = Router::from_registry(vec![spec("rust-analyzer", &[".RS"])]);
        assert_eq!(router.route("file:///a.rs").unwrap().name, "rust-analyzer");
        assert_eq!(router.route("file:///B.Rs").unwrap().name, "rust-analyzer");
    }

    /// The registry keys several presets by exact file name rather than by
    /// suffix, and a suffix-only router leaves those permanently unroutable.
    #[test]
    fn routes_files_named_exactly_like_a_preset_key() {
        let router = Router::from_registry(vec![
            spec("dockerfile", &["Dockerfile"]),
            spec("nginx", &["nginx.conf"]),
            spec("meson", &["meson.build"]),
            spec("cmake", &["CMakeLists.txt", ".cmake"]),
        ]);
        assert_eq!(router.route("file:///app/Dockerfile").unwrap().name, "dockerfile");
        assert_eq!(router.route("file:///etc/nginx.conf").unwrap().name, "nginx");
        assert_eq!(router.route("file:///src/meson.build").unwrap().name, "meson");
        // A preset can key on both a file name and a suffix.
        assert_eq!(router.route("file:///CMakeLists.txt").unwrap().name, "cmake");
        assert_eq!(router.route("file:///cmake/utils.cmake").unwrap().name, "cmake");
    }

    /// `docker-compose.yml` must reach the compose server, not whichever
    /// generic `.yml` preset happens to be registered first.
    #[test]
    fn exact_file_name_beats_a_generic_extension() {
        let router = Router::from_registry(vec![
            spec("yaml", &[".yaml", ".yml"]),
            spec("ansible", &[".yml", ".yaml"]),
            spec("dockercompose", &["docker-compose.yml", "docker-compose.yaml"]),
        ]);
        assert_eq!(router.route("file:///p/docker-compose.yml").unwrap().name, "dockercompose");
        assert_eq!(router.route("file:///p/docker-compose.yaml").unwrap().name, "dockercompose");
        // Any other YAML still falls through to the generic preset.
        assert_eq!(router.route("file:///p/playbook.yml").unwrap().name, "yaml");
    }

    #[test]
    fn file_name_routing_is_case_insensitive_and_path_aware() {
        let router = Router::from_registry(vec![spec("dockerfile", &["Dockerfile"])]);
        assert_eq!(router.route("file:///app/dockerfile").unwrap().name, "dockerfile");
        assert_eq!(router.route("file:///my.project/DOCKERFILE").unwrap().name, "dockerfile");
        // A directory of that name is not a file of that name.
        assert!(router.route("file:///Dockerfile/notes").is_none());
    }

    #[test]
    fn extension_of_handles_file_uri_and_multi_dot_names() {
        assert_eq!(extension_of("file:///a/b.test.tsx").as_deref(), Some(".tsx"));
        assert_eq!(extension_of("file:///a/noext"), None);
    }

    #[test]
    fn extension_of_ignores_dots_outside_the_file_name() {
        assert_eq!(extension_of("file:///my.project/Makefile"), None);
        assert_eq!(extension_of("file:///my.project/src/main.rs").as_deref(), Some(".rs"));
    }

    #[test]
    fn extension_of_treats_dotfiles_as_extensionless() {
        assert_eq!(extension_of("file:///repo/.gitignore"), None);
        assert_eq!(extension_of("file:///repo/.eslintrc.json").as_deref(), Some(".json"));
    }

    #[test]
    fn extract_uri_reads_textdocument_uri() {
        let message = json!({ "method": "textDocument/hover", "params": { "textDocument": { "uri": "file:///a.rs" } } });
        assert_eq!(Multiplexer::extract_uri(&message).as_deref(), Some("file:///a.rs"));
    }

    #[test]
    fn extract_uri_falls_back_to_a_bare_params_uri() {
        let message = json!({ "method": "textDocument/documentColor", "params": { "uri": "file:///a.rs" } });
        assert_eq!(Multiplexer::extract_uri(&message).as_deref(), Some("file:///a.rs"));
    }

    #[test]
    fn extract_uri_returns_none_when_absent() {
        let message = json!({ "method": "initialize", "params": {} });
        assert!(Multiplexer::extract_uri(&message).is_none());
    }

    #[test]
    fn answers_initialize_itself_and_remembers_the_params() {
        let (tx, rx) = std::sync::mpsc::channel();
        let multiplexer = Multiplexer::new(Router::from_registry(Vec::new()), tx);
        let params = json!({ "rootUri": "file:///project", "capabilities": {} });
        multiplexer
            .handle_client_message(json!({ "jsonrpc": "2.0", "id": 7, "method": "initialize", "params": params.clone() }))
            .unwrap();

        let reply = rx.try_recv().unwrap();
        assert_eq!(reply["id"], json!(7));
        assert_eq!(reply["result"]["serverInfo"]["name"], json!("single-lsp"));
        assert_eq!(*multiplexer.client_init_params.lock().unwrap(), Some(params));
    }

    #[test]
    fn answers_unroutable_requests_with_an_error_instead_of_hanging() {
        let (tx, rx) = std::sync::mpsc::channel();
        let multiplexer = Multiplexer::new(Router::from_registry(vec![spec("rust-analyzer", &[".rs"])]), tx);

        // A request for an extension no preset covers.
        multiplexer
            .handle_client_message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "textDocument/hover",
                "params": { "textDocument": { "uri": "file:///a.md" } }
            }))
            .unwrap();
        let reply = rx.try_recv().unwrap();
        assert_eq!(reply["id"], json!(1));
        assert_eq!(reply["error"]["code"], json!(METHOD_NOT_FOUND));

        // A request with no document to route on at all.
        multiplexer
            .handle_client_message(json!({ "jsonrpc": "2.0", "id": 2, "method": "workspace/symbol", "params": {} }))
            .unwrap();
        assert_eq!(rx.try_recv().unwrap()["id"], json!(2));

        // The matching notification is dropped silently — nothing to answer.
        multiplexer
            .handle_client_message(json!({ "jsonrpc": "2.0", "method": "$/cancelRequest", "params": { "id": 2 } }))
            .unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn answers_shutdown_even_with_no_backends_running() {
        let (tx, rx) = std::sync::mpsc::channel();
        let multiplexer = Multiplexer::new(Router::from_registry(Vec::new()), tx);
        multiplexer
            .handle_client_message(json!({ "jsonrpc": "2.0", "id": 9, "method": "shutdown" }))
            .unwrap();
        let reply = rx.try_recv().unwrap();
        assert_eq!(reply["id"], json!(9));
        assert_eq!(reply["result"], Value::Null);
    }

    #[test]
    fn routes_a_client_response_back_to_the_backend_that_asked() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let multiplexer = Multiplexer::new(Router::from_registry(Vec::new()), tx);
        multiplexer
            .pending_server
            .lock()
            .unwrap()
            .insert(42, (json!("ra-1"), "rust-analyzer".to_string()));

        // No backend is actually running, so the send fails silently — what
        // matters is that the pending entry was consumed by the right route
        // rather than the response being dropped as unroutable.
        multiplexer
            .handle_client_message(json!({ "jsonrpc": "2.0", "id": 42, "result": Value::Null }))
            .unwrap();
        assert!(multiplexer.pending_server.lock().unwrap().is_empty());
    }

    #[test]
    fn retiring_a_backend_fails_its_outstanding_requests() {
        let (tx, rx) = std::sync::mpsc::channel();
        let multiplexer = Multiplexer::new(Router::from_registry(Vec::new()), tx);
        multiplexer
            .pending
            .lock()
            .unwrap()
            .insert(1, (json!(100), "rust-analyzer".to_string()));
        multiplexer
            .pending
            .lock()
            .unwrap()
            .insert(2, (json!(200), "pyright".to_string()));
        multiplexer
            .uri_to_backend
            .lock()
            .unwrap()
            .insert("file:///a.rs".to_string(), "rust-analyzer".to_string());

        multiplexer.retire_backend("rust-analyzer");

        let reply = rx.try_recv().unwrap();
        assert_eq!(reply["id"], json!(100));
        assert_eq!(reply["error"]["code"], json!(INTERNAL_ERROR));
        assert!(rx.try_recv().is_err(), "pyright's request must not be failed");
        assert!(multiplexer.uri_to_backend.lock().unwrap().is_empty());
        assert!(multiplexer.pending.lock().unwrap().contains_key(&2));
    }

    /// A stand-in language server: it emits one notification and responses for
    /// a spread of ids, so whichever id the proxy happened to assign is
    /// covered. Both sleeps matter — the first lets the caller register its
    /// pending request before any reply can arrive (a real server cannot
    /// answer a request it has not received yet, so only a canned fake needs
    /// this), and the trailing one keeps the child's stdin readable so the
    /// proxy's writes do not hit a broken pipe.
    #[cfg(unix)]
    fn fake_backend(name: &str, extension: &str, marker: &str) -> LspServerSpec {
        let mut payload = String::new();
        let mut frame = |message: Value| {
            let body = serde_json::to_string(&message).unwrap();
            payload.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        };
        frame(json!({ "jsonrpc": "2.0", "method": "window/logMessage", "params": { "type": 3, "message": marker } }));
        for id in 1..=5 {
            frame(json!({ "jsonrpc": "2.0", "id": id, "result": marker }));
        }
        LspServerSpec {
            name: name.to_string(),
            command: "sh".to_string(),
            args: vec!["-c".to_string(), format!("sleep 1; printf %s '{payload}'; sleep 5")],
            extensions: vec![extension.to_string()],
            enabled: true,
        }
    }

    /// The headline scenario: a Rust file and a Python file open at once in
    /// one process, each answered by its own backend.
    ///
    /// Both fakes reply to *every* id in the same range, including the one the
    /// other backend owns, so this also pins down the ownership check in
    /// `read_from_backend`: without it, whichever backend spoke first would
    /// consume the other's pending entry and hand the client a response from
    /// the wrong language server.
    #[cfg(unix)]
    #[test]
    fn keeps_two_languages_alive_at_once_without_crossing_their_replies() {
        use std::time::{Duration, Instant};

        let (tx, rx) = std::sync::mpsc::channel();
        let router = Router::from_registry(vec![
            fake_backend("rs-fake", ".rs", "rs"),
            fake_backend("py-fake", ".py", "py"),
        ]);
        let multiplexer = Multiplexer::new(router, tx);

        for (id, uri) in [(10, "file:///project/a.rs"), (20, "file:///project/b.py")] {
            multiplexer
                .handle_client_message(json!({
                    "jsonrpc": "2.0", "id": id, "method": "textDocument/hover",
                    "params": { "textDocument": { "uri": uri } }
                }))
                .unwrap();
        }
        assert_eq!(multiplexer.backends.lock().unwrap().len(), 2, "one backend per language");

        let mut responses: HashMap<i64, String> = HashMap::new();
        let mut notifications = 0;
        let deadline = Instant::now() + Duration::from_secs(20);
        while (responses.len() < 2 || notifications < 2) && Instant::now() < deadline {
            let Ok(message) = rx.recv_timeout(Duration::from_millis(250)) else {
                continue;
            };
            if message.get("method").is_some() {
                notifications += 1;
            } else {
                let id = message["id"].as_i64().expect("response carries the client's own id");
                let result = message["result"].as_str().expect("fake replies with a marker").to_string();
                assert!(responses.insert(id, result).is_none(), "one response per request");
            }
        }

        assert_eq!(responses.get(&10).map(String::as_str), Some("rs"));
        assert_eq!(responses.get(&20).map(String::as_str), Some("py"));
        assert_eq!(notifications, 2, "each backend's notification is forwarded as-is");
        // Responses to the proxy's own `initialize` handshake, and to ids no
        // backend owns, must not reach the client.
        assert_eq!(responses.len(), 2);
    }

    /// Shutdown must not outrun a backend that is still forwarding.
    ///
    /// The fake stays silent for a second and only then speaks, so the
    /// end-of-output sentinel would be enqueued well ahead of its notification
    /// if `close_client_output` did not wait for reader threads to drain — the
    /// writer would hit the sentinel, break, and drop the message.
    #[cfg(unix)]
    #[test]
    fn shutdown_waits_for_backend_readers_before_closing_client_output() {
        let (tx, rx) = std::sync::mpsc::channel();
        let body = serde_json::to_string(
            &json!({ "jsonrpc": "2.0", "method": "window/logMessage", "params": { "type": 3, "message": "late" } }),
        )
        .unwrap();
        let payload = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let slow = LspServerSpec {
            name: "slow-fake".to_string(),
            command: "sh".to_string(),
            // No trailing sleep: the backend exits right after speaking, so its
            // reader thread finishes and the drain barrier can be satisfied.
            args: vec!["-c".to_string(), format!("sleep 1; printf %s '{payload}'")],
            extensions: vec![".rs".to_string()],
            enabled: true,
        };

        let multiplexer = Multiplexer::new(Router::from_registry(vec![slow]), tx);
        multiplexer
            .handle_client_message(json!({
                "jsonrpc": "2.0", "method": "textDocument/didOpen",
                "params": { "textDocument": { "uri": "file:///a.rs" } }
            }))
            .unwrap();

        multiplexer.close_client_output();

        // Everything is already enqueued by the time the call returns.
        let messages: Vec<Value> = rx.try_iter().collect();
        assert_eq!(messages.len(), 2, "expected the notification and the sentinel: {messages:?}");
        assert_eq!(messages[0]["method"], json!("window/logMessage"));
        assert!(messages[1].is_null(), "the sentinel must be last");
    }

    /// A second document in an already-open language reuses the running
    /// backend rather than spawning a second copy of it.
    #[cfg(unix)]
    #[test]
    fn reuses_one_backend_for_several_documents_of_the_same_language() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let multiplexer = Multiplexer::new(Router::from_registry(vec![fake_backend("rs-fake", ".rs", "rs")]), tx);

        for uri in ["file:///a.rs", "file:///b.rs"] {
            multiplexer
                .handle_client_message(json!({
                    "jsonrpc": "2.0", "method": "textDocument/didOpen",
                    "params": { "textDocument": { "uri": uri } }
                }))
                .unwrap();
        }

        assert_eq!(multiplexer.backends.lock().unwrap().len(), 1);
        assert_eq!(multiplexer.uri_to_backend.lock().unwrap().len(), 2);
    }

    /// Lazy-spawn and idle-eviction, end to end against a real child process.
    ///
    /// The fake is a real `sh` that emits one notification (proving a process
    /// actually spawned and talked) and then sleeps for an hour — so nothing
    /// but eviction can end it. Eviction kills that process, and the only way
    /// the test can see the backend was really gone is via the reader thread's
    /// EOF-triggered retirement: routing is cleared and the outstanding
    /// request is failed. If eviction merely forgot a map entry without
    /// killing the process, the reader would still be blocked on a live pipe
    /// and none of that would happen.
    #[cfg(unix)]
    #[test]
    fn spawns_backends_lazily_and_evicts_idle_ones() {
        use std::time::{Duration as StdDuration, Instant};

        let body = serde_json::to_string(
            &json!({ "jsonrpc": "2.0", "method": "window/logMessage", "params": { "type": 3, "message": "idle-fake" } }),
        )
        .unwrap();
        let payload = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let long_lived = LspServerSpec {
            name: "idle-fake".to_string(),
            command: "sh".to_string(),
            args: vec!["-c".to_string(), format!("printf %s '{payload}'; sleep 3600")],
            extensions: vec![".rs".to_string()],
            enabled: true,
        };

        let (tx, rx) = std::sync::mpsc::channel();
        let multiplexer = Multiplexer::new(Router::from_registry(vec![long_lived]), tx);

        // Lazy: nothing is spawned before any document is routed to a backend.
        assert!(multiplexer.backends.lock().unwrap().is_empty(), "no backend before first use");

        // First use spawns a real process.
        multiplexer
            .handle_client_message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "textDocument/hover",
                "params": { "textDocument": { "uri": "file:///a.rs" } }
            }))
            .unwrap();
        assert_eq!(multiplexer.backends.lock().unwrap().len(), 1, "first use spawns a backend");
        let deadline = Instant::now() + StdDuration::from_secs(10);
        let mut saw_notification = false;
        while Instant::now() < deadline && !saw_notification {
            match rx.try_recv() {
                Ok(reply) if reply.get("method").is_some() => saw_notification = true,
                Ok(_) => {}
                Err(_) => std::thread::sleep(StdDuration::from_millis(20)),
            }
        }
        assert!(saw_notification, "the spawned process's notification reaches the client");

        // Give the evicted backend an outstanding request, which must be failed
        // when it goes away rather than left to hang the client.
        multiplexer.pending.lock().unwrap().insert(999, (json!(1), "idle-fake".to_string()));

        // Age it past the eviction threshold, then reap.
        multiplexer
            .last_activity
            .lock()
            .unwrap()
            .insert("idle-fake".to_string(), Instant::now() - IDLE_EVICT_TIMEOUT - StdDuration::from_secs(1));
        multiplexer.evict_idle_backends();
        assert!(multiplexer.backends.lock().unwrap().is_empty(), "idle backend evicted");

        // The process really died: its reader thread hit EOF, retired it,
        // cleared routing, and failed the outstanding request.
        let deadline = Instant::now() + StdDuration::from_secs(10);
        let mut saw_error = false;
        while Instant::now() < deadline && !saw_error {
            match rx.try_recv() {
                Ok(reply) if reply["error"]["code"] == json!(INTERNAL_ERROR) => saw_error = true,
                Ok(_) => {}
                Err(_) => std::thread::sleep(StdDuration::from_millis(20)),
            }
        }
        assert!(saw_error, "outstanding request to an evicted backend must be failed");
        assert!(multiplexer.uri_to_backend.lock().unwrap().is_empty(), "routing cleared on eviction");
        assert!(multiplexer.last_activity.lock().unwrap().is_empty(), "activity tracking cleared");

        // A later document lazily spawns a fresh backend rather than reusing
        // the evicted (now-dead) process.
        multiplexer
            .handle_client_message(json!({
                "jsonrpc": "2.0", "method": "textDocument/didOpen",
                "params": { "textDocument": { "uri": "file:///b.rs" } }
            }))
            .unwrap();
        assert_eq!(multiplexer.backends.lock().unwrap().len(), 1, "a fresh backend is re-spawned");
    }

    /// The whole design rests on `Arc<Multiplexer>` being movable into a
    /// backend reader thread; this fails to compile if a field stops being
    /// `Sync` (an unwrapped `mpsc::Sender` field, notably, is not).
    #[test]
    fn multiplexer_is_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Multiplexer>();
        assert_send_sync::<Arc<Multiplexer>>();
    }
}
