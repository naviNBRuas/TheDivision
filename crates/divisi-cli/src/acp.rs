//! Native `divisi acp` — an Agent Client Protocol (ACP) stdio server that
//! fronts the divisi coordinator (spec E27.02 §7). Newline-delimited
//! JSON-RPC 2.0 over stdin/stdout, protocol version 1.
//!
//! This is a **bridge, not an agent**: it consumes no development agent
//! itself. `/`-commands run against the socket; status-y prompts answer
//! from `CoordinatorStatus` / `GoalStatus`; everything else becomes a
//! `GoalSubmit` and the coordinator's `coordinator_events` are long-polled
//! and translated into ACP `session/update` notifications. The coordinator
//! does all planning / dispatch / integration.
//!
//! Replaces the Python prototype `nbr-workspace/tools/single-acp`; its
//! routing/streaming shape is the reference.

use anyhow::Result;
use serde_json::{json, Value};
use divisi_protocol::{Request, Response, ResponseData};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const PROTOCOL_VERSION: u64 = 1;
const POLL_INTERVAL: Duration = Duration::from_millis(1200);
/// how long to wait for the client's answer to a `session/request_permission`
/// before falling back to a plain message + `end_turn`.
const PERMISSION_TIMEOUT: Duration = Duration::from_secs(180);

/// One step of answering a chat turn, derived from the events the daemon returned.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TurnAction {
    /// Text for the Zed thread.
    Say(String),
    /// A risky action waiting for a yes or no.
    Confirm { approval_id: i64, summary: String },
    /// A goal the reply started, whose progress should stream into the thread.
    Goal(String),
}

/// Turns the events a `ChatSend` or `ChatConfirm` appended into actions. Your own message and the
/// bare result lines are skipped: Zed already shows the first, and the reply covers the second.
fn plan_actions(events: &[divisi_protocol::CoordinatorEvent]) -> Vec<TurnAction> {
    let mut out = Vec::new();
    for e in events {
        let Some(line) = divisi_protocol::chat_line(&e.kind, &e.body) else { continue };
        match line.role {
            divisi_protocol::ChatRole::Divisi => {
                let note = if line.degraded { "\n_(rules-only mode: no model was reachable)_" } else { "" };
                out.push(TurnAction::Say(format!("{}{note}", line.text)));
                let v: Value = serde_json::from_str(&e.body).unwrap_or(Value::Null);
                for g in v.get("goal_ids").and_then(|g| g.as_array()).into_iter().flatten().filter_map(|g| g.as_str()) {
                    out.push(TurnAction::Goal(g.to_owned()));
                }
            }
            divisi_protocol::ChatRole::Confirm => {
                if let Some(approval_id) = line.approval_id {
                    out.push(TurnAction::Confirm { approval_id, summary: line.text });
                }
            }
            divisi_protocol::ChatRole::You | divisi_protocol::ChatRole::Result => {}
        }
    }
    out
}

/// True when the daemon predates chat (it cannot parse `ChatSend`), so the bridge falls back to
/// submitting a goal directly. Any other failure is a real error to show.
fn is_unsupported(e: &anyhow::Error) -> bool {
    let m = e.to_string();
    m.contains("invalid request") && m.contains("unknown variant")
}

struct AcpSession {
    /// coordinator `sess_…` id this ACP session is bound to.
    coord_id: String,
    mode: String,
    /// highest `coordinator_events` id already translated for this session.
    last_event_id: i64,
    cancel: Arc<AtomicBool>,
    /// E29: `/agent <name>` override for this session's goal submissions.
    /// `None` means the ACP default (`single-pool`) — see `run_turn`'s
    /// `GoalSubmit` construction.
    agent_override: Option<String>,
}

pub struct Acp {
    socket_path: PathBuf,
    sessions: Mutex<HashMap<String, AcpSession>>,
    /// server→client request id → a channel the stdin loop forwards the
    /// matching response onto (used by `session/request_permission`).
    pending: Mutex<HashMap<String, mpsc::Sender<Value>>>,
    out: Mutex<std::io::Stdout>,
    seq: AtomicU64,
    srv_seq: AtomicU64,
}

/// Entry point for `divisi acp`. Blocks reading stdin until it closes.
pub fn run(socket_path: PathBuf) -> Result<()> {
    let acp = Arc::new(Acp {
        socket_path,
        sessions: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        out: Mutex::new(std::io::stdout()),
        seq: AtomicU64::new(0),
        srv_seq: AtomicU64::new(0),
    });
    log("=== divisi acp start ===");

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        log(&format!("IN  {}", &line[..line.len().min(300)]));
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            log("bad json");
            continue;
        };

        // A response to one of our own outbound requests (has `id`, no
        // `method`) → route it to whoever is waiting.
        if msg.get("method").is_none() && msg.get("id").is_some() {
            let id = id_key(&msg["id"]);
            if let Some(tx) = acp.pending.lock().unwrap().remove(&id) {
                let _ = tx.send(msg);
            }
            continue;
        }

        let acp2 = Arc::clone(&acp);
        acp.dispatch(acp2, msg);
    }
    log("=== stdin closed ===");
    Ok(())
}

impl Acp {
    fn dispatch(&self, acp: Arc<Acp>, msg: Value) {
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
        let id = msg.get("id").cloned();
        let p = msg.get("params").cloned().unwrap_or(json!({}));

        match method.as_str() {
            "initialize" => self.respond(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "agentInfo": { "name": "single-acp", "version": env!("CARGO_PKG_VERSION") },
                    "agentCapabilities": {
                        "loadSession": true,
                        "promptCapabilities": { "image": false, "audio": false, "embeddedContext": true },
                        "mcpCapabilities": { "http": false, "sse": false }
                    },
                    "authMethods": []
                }),
            ),
            "authenticate" => self.respond(id, json!({})),
            "session/new" => self.session_new(id, &p),
            "session/load" => self.session_load(acp.clone(), id, &p),
            "session/set_mode" => {
                let sid = p["sessionId"].as_str().unwrap_or_default().to_string();
                let mode = p["modeId"].as_str().unwrap_or("auto").to_string();
                if let Some(s) = self.sessions.lock().unwrap().get_mut(&sid) {
                    s.mode = mode.clone();
                }
                self.session_update(&sid, json!({ "sessionUpdate": "current_mode_update", "currentModeId": mode }));
                self.respond(id, json!({}));
            }
            "session/prompt" => {
                let sid = p["sessionId"].as_str().unwrap_or_default().to_string();
                let text = prompt_text(&p);
                if !self.sessions.lock().unwrap().contains_key(&sid) {
                    self.rpc_error(id, -32602, &format!("unknown session {sid}"));
                    return;
                }
                if text.trim().is_empty() {
                    self.respond(id, json!({ "stopReason": "end_turn" }));
                    return;
                }
                if let Some(s) = self.sessions.lock().unwrap().get(&sid) {
                    s.cancel.store(false, Ordering::SeqCst);
                }
                std::thread::spawn(move || {
                    acp.run_turn(&sid, id, &text);
                });
            }
            "session/cancel" => {
                let sid = p["sessionId"].as_str().unwrap_or_default().to_string();
                if let Some(s) = self.sessions.lock().unwrap().get(&sid) {
                    s.cancel.store(true, Ordering::SeqCst);
                }
                // best-effort: also cancel the coordinator goal in flight.
                if let Ok(Some(gid)) = self.active_goal(&sid) {
                    let _ = self.socket(Request::GoalCancel { goal_id: gid });
                }
            }
            "$/cancel_request" => {}
            _ => {
                if id.is_some() {
                    self.rpc_error(id, -32601, &format!("method not found: {method}"));
                }
            }
        }
    }

    // ---- session lifecycle ------------------------------------------------

    fn session_new(&self, id: Option<Value>, p: &Value) {
        let cwd = p["cwd"].as_str().map(str::to_string).unwrap_or_else(default_cwd);
        let coord_id = match self.socket(Request::SessionNew { cwd: cwd.clone() }) {
            Ok(ResponseData::Session(s)) => s.id,
            Ok(other) => {
                self.rpc_error(id, -32603, &format!("unexpected SessionNew response: {other:?}"));
                return;
            }
            Err(e) => {
                self.rpc_error(id, -32603, &format!("SessionNew failed: {e}"));
                return;
            }
        };
        // The ACP session id IS the coordinator session id — so a fresh
        // `divisi acp` process can resume a thread on `session/load`
        // (E27.03). Zed persists this string.
        let acp_sid = coord_id.clone();
        self.sessions.lock().unwrap().insert(
            acp_sid.clone(),
            AcpSession { coord_id, mode: "auto".into(), last_event_id: 0, cancel: Arc::new(AtomicBool::new(false)), agent_override: None },
        );
        self.respond(id, json!({ "sessionId": acp_sid, "modes": modes_block("auto") }));
        self.session_update(&acp_sid, json!({ "sessionUpdate": "available_commands_update", "availableCommands": commands() }));
    }

    fn session_load(&self, acp: Arc<Acp>, id: Option<Value>, p: &Value) {
        let cwd = p["cwd"].as_str().map(str::to_string).unwrap_or_else(default_cwd);
        let given = p["sessionId"].as_str().map(str::to_string).unwrap_or_default();

        // The id is a coordinator session id. If the daemon knows it, bind
        // to it and replay; if not (GC'd, wrong machine), start fresh.
        let (acp_sid, replay): (String, Vec<divisi_protocol::CoordinatorEvent>) = if given.starts_with("sess_") {
            match self.socket(Request::SessionEvents { session_id: given.clone(), since_event_id: 0 }) {
                Ok(ResponseData::CoordinatorEvents(events)) => (given.clone(), events),
                _ => (self.fresh_session(&cwd), Vec::new()),
            }
        } else {
            (self.fresh_session(&cwd), Vec::new())
        };

        let mode = self
            .sessions
            .lock()
            .unwrap()
            .get(&acp_sid)
            .map(|s| s.mode.clone())
            .unwrap_or_else(|| "auto".into());
        self.sessions.lock().unwrap().entry(acp_sid.clone()).or_insert_with(|| AcpSession {
            coord_id: acp_sid.clone(),
            mode: mode.clone(),
            last_event_id: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            agent_override: None,
        });

        self.respond(id, json!({ "modes": modes_block(&mode) }));
        self.session_update(&acp_sid, json!({ "sessionUpdate": "available_commands_update", "availableCommands": commands() }));

        let mut max_id = 0;
        for e in &replay {
            max_id = max_id.max(e.id);
            self.chunk(&acp_sid, &format!("[{}] {}\n", e.kind, e.body), "agent_message_chunk");
        }
        if max_id > 0 {
            if let Some(s) = self.sessions.lock().unwrap().get_mut(&acp_sid) {
                s.last_event_id = max_id;
            }
        }

        // E28 spec §10 (Part F): if this session has a goal still
        // `running`/`waiting_on_capacity` (etc.) — e.g. the daemon or this
        // `divisi acp` process restarted mid-goal — re-attach its event
        // long-poll so a restarted Zed panel keeps streaming without the
        // user having to send a new prompt. No JSON-RPC response is owed
        // for this background stream (unlike `run_turn`'s prompt-driven
        // one), so the stop reason is just logged.
        if let Ok(Some(goal_id)) = self.active_goal(&acp_sid) {
            let coord_id = acp_sid.clone();
            let acp_sid = acp_sid.clone();
            std::thread::spawn(move || {
                let stop = acp.stream_goal(&acp_sid, &coord_id, &goal_id);
                log(&format!("session/load re-attach for {acp_sid} ended: {stop}"));
            });
        }
    }

    /// makes a brand-new coordinator session and registers it, returning
    /// its id — the fallback when `session/load` can't resolve the given id.
    fn fresh_session(&self, cwd: &str) -> String {
        let coord_id = match self.socket(Request::SessionNew { cwd: cwd.to_string() }) {
            Ok(ResponseData::Session(s)) => s.id,
            _ => format!("sess_local_{}", self.seq.fetch_add(1, Ordering::Relaxed)),
        };
        self.sessions.lock().unwrap().insert(
            coord_id.clone(),
            AcpSession { coord_id: coord_id.clone(), mode: "auto".into(), last_event_id: 0, cancel: Arc::new(AtomicBool::new(false)), agent_override: None },
        );
        coord_id
    }

    // ---- one prompt turn -----------------------------------------------

    fn run_turn(&self, acp_sid: &str, rid: Option<Value>, text: &str) {
        log(&format!("turn start sid={acp_sid} text={:?}", &text[..text.len().min(60)]));
        let trimmed = text.trim_start();
        if let Some(rest) = trimmed.strip_prefix('/') {
            let mut parts = rest.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("help").to_lowercase();
            let arg = parts.next().unwrap_or("").trim();
            let body = self.run_slash(acp_sid, &name, arg);
            self.chunk(acp_sid, &body, "agent_message_chunk");
            self.respond(rid, json!({ "stopReason": "end_turn" }));
            return;
        }

        let (coord_id, mode, agent) = {
            let map = self.sessions.lock().unwrap();
            let Some(s) = map.get(acp_sid) else {
                self.rpc_error(rid, -32602, "unknown session");
                return;
            };
            (s.coord_id.clone(), s.mode.clone(), s.agent_override.clone())
        };

        // Plain language: the daemon interprets it. Only an older daemon without chat falls through
        // to the direct path below.
        if let Some(stop) = self.chat_turn(acp_sid, &coord_id, &mode, agent.clone(), text) {
            self.respond(rid, json!({ "stopReason": stop }));
            return;
        }

        if let Some(reply) = self.answer_status_query(acp_sid, text) {
            self.chunk(acp_sid, &reply, "agent_message_chunk");
            self.respond(rid, json!({ "stopReason": "end_turn" }));
            return;
        }

        // otherwise: submit a goal and stream the coordinator's progress.
        self.chunk(acp_sid, "planning…\n", "agent_thought_chunk");
        // Live-verification finding (E29's `single-pool`-by-default choice,
        // reverted): `plan_goal` force-overrides EVERY node in the graph to
        // whatever `agent` names, not just the planner step — so this
        // default sent every Zed-submitted goal's entire task graph
        // (code, test, review, everything) through `single-pool`, which
        // has no real tool/file/command execution (confirmed live:
        // fabricated a plausible but entirely fictional cargo test run,
        // and separately leaked a raw `<tool_call>` token into its output
        // when a different `single-agent run` wrapper tried to use a
        // tool). `agent_override` (`/agent <name>`) still works exactly
        // as before for a user who deliberately wants one agent for
        // everything; absent that, leave `agent` as `None` so the
        // coordinator's normal per-node-kind routing (routing.toml)
        // picks a real tool-capable agent per step, same as every
        // goal submitted directly via `divisi goal submit` already does.
        let goal_id = match self.socket(Request::GoalSubmit {
            session_id: coord_id.clone(),
            text: text.to_string(),
            mode: Some(mode),
            max_dispatches: None,
            max_minutes: None,
            agent,
        }) {
            Ok(ResponseData::GoalId(g)) => g,
            Ok(other) => {
                self.chunk(acp_sid, &format!("[submit failed: {other:?}]\n"), "agent_message_chunk");
                self.respond(rid, json!({ "stopReason": "end_turn" }));
                return;
            }
            Err(e) => {
                self.chunk(acp_sid, &format!("[submit failed: {e}]\n"), "agent_message_chunk");
                self.respond(rid, json!({ "stopReason": "end_turn" }));
                return;
            }
        };

        let stop = self.stream_goal(acp_sid, &coord_id, &goal_id);
        self.respond(rid, json!({ "stopReason": stop }));
    }

    /// Natural language goes to the daemon, which interprets it (rules first, a pool model for the
    /// rest), answers questions, starts goals and asks before anything risky. Returns `None` when the
    /// daemon predates chat, so the caller can use the direct-goal path instead.
    fn chat_turn(&self, acp_sid: &str, coord_id: &str, mode: &str, agent: Option<String>, text: &str) -> Option<&'static str> {
        self.chunk(acp_sid, "thinking…\n", "agent_thought_chunk");
        let request = Request::ChatSend { session: Some(coord_id.to_owned()), text: text.to_owned(), surface: "zed".into(), mode: Some(mode.to_owned()), agent };
        match self.socket(request) {
            Ok(ResponseData::Chat(o)) => Some(self.apply_actions(acp_sid, coord_id, plan_actions(&o.events))),
            Ok(_) => None,
            Err(e) if is_unsupported(&e) => None,
            Err(e) => {
                self.chunk(acp_sid, &format!("[chat failed: {e}]\n"), "agent_message_chunk");
                Some("end_turn")
            }
        }
    }

    /// Plays a turn's actions into the thread: text as messages, confirmations as permission
    /// prompts (whose answer can produce more actions), goals as a live progress stream.
    fn apply_actions(&self, acp_sid: &str, coord_id: &str, actions: Vec<TurnAction>) -> &'static str {
        let mut stop = "end_turn";
        let mut queue: std::collections::VecDeque<TurnAction> = actions.into();
        while let Some(action) = queue.pop_front() {
            match action {
                TurnAction::Say(text) => self.chunk(acp_sid, &format!("{text}\n"), "agent_message_chunk"),
                TurnAction::Goal(goal_id) => stop = self.stream_goal(acp_sid, coord_id, &goal_id),
                TurnAction::Confirm { approval_id, summary } => match self.ask_confirm(acp_sid, &summary) {
                    Some(allow) => match self.socket(Request::ChatConfirm { approval_id, allow, remember: false }) {
                        Ok(ResponseData::Chat(o)) => {
                            for a in plan_actions(&o.events).into_iter().rev() {
                                queue.push_front(a);
                            }
                        }
                        Ok(_) => {}
                        Err(e) => self.chunk(acp_sid, &format!("[confirmation failed: {e}]\n"), "agent_message_chunk"),
                    },
                    None => self.chunk(
                        acp_sid,
                        &format!("Nothing was done. The confirmation stays open for a while: `divisi chat confirm {approval_id} --allow`\n"),
                        "agent_message_chunk",
                    ),
                },
            }
        }
        stop
    }

    /// Asks the Zed user yes or no through ACP's permission request. `None` on timeout.
    fn ask_confirm(&self, acp_sid: &str, summary: &str) -> Option<bool> {
        let req_id = format!("srv-{}", self.srv_seq.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(req_id.clone(), tx);
        self.send_raw(json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": "session/request_permission",
            "params": {
                "sessionId": acp_sid,
                "toolCall": { "toolCallId": req_id.clone(), "title": format!("Confirm: {summary}") },
                "options": [
                    { "optionId": "allow", "name": "Yes, do it", "kind": "allow_once" },
                    { "optionId": "deny", "name": "No", "kind": "reject_once" }
                ]
            }
        }));
        let choice = rx.recv_timeout(PERMISSION_TIMEOUT).ok().and_then(|m| {
            m.get("result").and_then(|r| r.get("outcome")).and_then(|o| o.get("optionId").or_else(|| o.get("option"))).and_then(|v| v.as_str()).map(str::to_string)
        });
        self.pending.lock().unwrap().remove(&req_id);
        match choice.as_deref() {
            Some("allow") => Some(true),
            Some("deny") => Some(false),
            _ => None,
        }
    }

    /// long-polls `SessionEvents` and a terminal `GoalStatus`, translating
    /// each new event into an ACP `session/update`. Returns the ACP
    /// `stopReason`.
    fn stream_goal(&self, acp_sid: &str, coord_id: &str, goal_id: &str) -> &'static str {
        let cancel = self
            .sessions
            .lock()
            .unwrap()
            .get(acp_sid)
            .map(|s| Arc::clone(&s.cancel))
            .unwrap_or_default();
        let deadline = Instant::now() + Duration::from_secs(60 * 60);

        // Live-verification finding: this used to read AND write the
        // shared `AcpSession::last_event_id` every iteration. Two goals
        // racing on the same ACP session (e.g. a `session/load` re-attach
        // overlapping a fresh `session/prompt`) shared one mutable
        // cursor — one goal's burst of events could advance it past
        // events belonging to the other goal, silently dropping them
        // from that goal's own `stream_goal` call. Read the starting
        // cursor once; after that, this loop is the sole source of truth
        // for its own progress. The shared field is only ever pushed
        // forward as a high-water mark (never read back here) so a
        // subsequent `session_load` re-attach can still skip already-
        // replayed history.
        let mut since = self.sessions.lock().unwrap().get(acp_sid).map(|s| s.last_event_id).unwrap_or(0);

        loop {
            if cancel.load(Ordering::SeqCst) {
                let _ = self.socket(Request::GoalCancel { goal_id: goal_id.to_string() });
                return "cancelled";
            }
            if Instant::now() > deadline {
                self.chunk(acp_sid, "[divisi acp] stopped tailing after 1h\n", "agent_message_chunk");
                return "max_turn_requests";
            }

            if let Ok(ResponseData::CoordinatorEvents(events)) = self.socket(Request::SessionEvents {
                session_id: coord_id.to_string(),
                since_event_id: since,
            }) {
                for e in &events {
                    if e.goal_id.as_deref() != Some(goal_id) && e.goal_id.is_some() {
                        // a different goal in the same session — skip, but
                        // still advance the cursor.
                    } else {
                        self.translate_event(acp_sid, goal_id, &e.kind, &e.body);
                    }
                    since = since.max(e.id);
                }
                if let Some(s) = self.sessions.lock().unwrap().get_mut(acp_sid) {
                    s.last_event_id = s.last_event_id.max(since);
                }
            }

            match self.goal_view(goal_id) {
                Some(v) => {
                    // refresh the ACP plan from the live node list.
                    self.emit_plan(acp_sid, &v);
                    match v.goal.status.as_str() {
                        "done" => {
                            if let Some(sum) = &v.result_summary {
                                self.chunk(acp_sid, &format!("\n{sum}\n"), "agent_message_chunk");
                            }
                            self.chunk(acp_sid, &usage_line(&v), "agent_thought_chunk");
                            return "end_turn";
                        }
                        "failed" => {
                            self.chunk(acp_sid, "\n[goal failed]\n", "agent_message_chunk");
                            return "end_turn";
                        }
                        "cancelled" => return "cancelled",
                        "blocked" => {
                            let reason = v.blocked_reason.clone().unwrap_or_else(|| "goal blocked".into());
                            return self.handle_blocked(acp_sid, goal_id, &reason);
                        }
                        _ => {}
                    }
                }
                None => return "end_turn",
            }

            std::thread::sleep(POLL_INTERVAL);
        }
    }

    fn translate_event(&self, acp_sid: &str, _goal_id: &str, kind: &str, body: &str) {
        match kind {
            "node_started" => self.chunk(acp_sid, &format!("→ {body}\n"), "agent_thought_chunk"),
            "node_output" => self.chunk(acp_sid, body, "agent_message_chunk"),
            "node_done" => self.chunk(acp_sid, &format!("✓ node {body}\n"), "agent_thought_chunk"),
            "node_failed" => self.chunk(acp_sid, &format!("✗ {body}\n"), "agent_message_chunk"),
            "supervisor" => self.chunk(acp_sid, &format!("[supervisor] {body}\n"), "agent_thought_chunk"),
            "budget" => self.chunk(acp_sid, &format!("[budget] {body}\n"), "agent_thought_chunk"),
            // E28 spec §8: a live hold instead of a stall — "all providers
            // rate-limited; holding, resumes ~14:03Z".
            "capacity_wait" => self.chunk(acp_sid, &format!("all providers rate-limited; holding — {body}\n"), "agent_thought_chunk"),
            "capacity_resumed" => self.chunk(acp_sid, &format!("capacity freed, resuming — {body}\n"), "agent_thought_chunk"),
            // E28 spec §10: the goal survived a daemon restart or a
            // manual `divisi goal resume` — a live signal instead of a
            // silent gap in the Zed panel's history.
            "session_resumed" => self.chunk(acp_sid, &format!("[resumed] {body}\n"), "agent_thought_chunk"),
            "integrated" => {} // the summary is emitted from the terminal GoalStatus
            "plan" => {}       // the plan is emitted from GoalStatus node list
            _ => {}
        }
    }

    fn emit_plan(&self, acp_sid: &str, v: &divisi_protocol::GoalView) {
        if v.nodes.is_empty() {
            return;
        }
        let entries: Vec<Value> = v
            .nodes
            .iter()
            .map(|n| {
                let status = match n.status.as_str() {
                    "running" => "in_progress",
                    "done" | "skipped" | "failed" | "blocked" => "completed",
                    _ => "pending",
                };
                json!({ "content": format!("{}: {}", n.id, n.desc), "priority": "medium", "status": status })
            })
            .collect();
        self.session_update(acp_sid, json!({ "sessionUpdate": "plan", "entries": entries }));
    }

    /// spec §7: a `blocked` goal asks the human. Sent as an ACP
    /// `session/request_permission`; the answer routes back through
    /// `GoalAmend` (raise the budget) or `GoalCancel`. Falls back to a
    /// plain message + `end_turn` if the client doesn't answer.
    fn handle_blocked(&self, acp_sid: &str, goal_id: &str, reason: &str) -> &'static str {
        let req_id = format!("srv-{}", self.srv_seq.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(req_id.clone(), tx);

        self.send_raw(json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": "session/request_permission",
            "params": {
                "sessionId": acp_sid,
                "toolCall": { "toolCallId": req_id.clone(), "title": format!("Goal blocked: {reason}") },
                "options": [
                    { "optionId": "raise", "name": "Raise budget and continue", "kind": "allow_once" },
                    { "optionId": "cancel", "name": "Cancel the goal", "kind": "reject_once" }
                ]
            }
        }));

        let choice = rx
            .recv_timeout(PERMISSION_TIMEOUT)
            .ok()
            .and_then(|m| {
                m.get("result")
                    .and_then(|r| r.get("outcome"))
                    .and_then(|o| o.get("optionId").or_else(|| o.get("option")))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
        self.pending.lock().unwrap().remove(&req_id);

        match choice.as_deref() {
            Some("raise") => {
                // a goal blocked on elapsed wall time re-blocks immediately
                // if only `max_dispatches` moves — raise whichever cap the
                // block reason actually names.
                if reason.contains("for capacity, still exhausted") {
                    let bump = reason
                        .split("waited ")
                        .nth(1)
                        .and_then(|s| s.split('h').next())
                        .and_then(|s| s.parse::<f64>().ok())
                        .map(|hours| ((hours * 60.0) as u32) + 720)
                        .unwrap_or(2880);
                    let _ = self.socket(Request::GoalAmend { goal_id: goal_id.to_string(), text: format!("capacity-minutes={bump}") });
                    self.chunk(acp_sid, &format!("[capacity-wait cap raised to {bump}m, continuing]\n"), "agent_thought_chunk");
                } else if reason.contains("time budget") {
                    // `GoalSummary` doesn't carry `max_minutes` to diff
                    // against, unlike `max_dispatches` below -- a flat
                    // +60m extension is a fine default for a human-in-the-
                    // loop bump (they can amend again for more).
                    let bump = reason
                        .rsplit("of ")
                        .next()
                        .and_then(|s| s.split_whitespace().next())
                        .and_then(|s| s.parse::<u32>().ok())
                        .map(|cap| cap + 60)
                        .unwrap_or(120);
                    let _ = self.socket(Request::GoalAmend { goal_id: goal_id.to_string(), text: format!("minutes={bump}") });
                    self.chunk(acp_sid, &format!("[time budget raised to {bump}m, continuing]\n"), "agent_thought_chunk");
                } else {
                    let bump = self
                        .goal_view(goal_id)
                        .map(|v| v.goal.max_dispatches.saturating_mul(2).max(v.goal.max_dispatches + 10))
                        .unwrap_or(50);
                    let _ = self.socket(Request::GoalAmend { goal_id: goal_id.to_string(), text: format!("budget={bump}") });
                    self.chunk(acp_sid, &format!("[budget raised to {bump}, continuing]\n"), "agent_thought_chunk");
                }
                let coord_id = self.sessions.lock().unwrap().get(acp_sid).map(|s| s.coord_id.clone()).unwrap_or_default();
                self.stream_goal(acp_sid, &coord_id, goal_id)
            }
            Some("cancel") => {
                let _ = self.socket(Request::GoalCancel { goal_id: goal_id.to_string() });
                "cancelled"
            }
            _ => {
                let hint = if reason.contains("for capacity, still exhausted") {
                    "capacity-minutes=N"
                } else if reason.contains("time budget") {
                    "minutes=N"
                } else {
                    "budget=N"
                };
                self.chunk(
                    acp_sid,
                    &format!("\n**Goal blocked:** {reason}\nRun `divisi goal amend {goal_id} {hint}` to raise the cap.\n"),
                    "agent_message_chunk",
                );
                "end_turn"
            }
        }
    }

    // ---- slash commands ------------------------------------------------

    fn run_slash(&self, acp_sid: &str, name: &str, arg: &str) -> String {
        match name {
            "status" | "queue" => {
                let coord = match self.socket(Request::CoordinatorStatus) {
                    Ok(ResponseData::CoordinatorSnapshot(s)) => format_snapshot(&s),
                    Ok(other) => format!("unexpected: {other:?}"),
                    Err(e) => format!("[error: {e}]"),
                };
                // E29: fold provider auth/exhaustion state into the same
                // command — Zed has no native status-bar/panel API to put
                // this in on its own (see the E29 design spec's Zed
                // capability research), so `/status`'s text output is the
                // whole status surface.
                let providers = shell_out(&["provider", "key-status"]);
                format!("{coord}\nprovider auth/exhaustion:\n{providers}")
            }
            "goals" => match self.socket(Request::GoalList { session_id: None }) {
                Ok(ResponseData::Goals(gs)) => {
                    if gs.is_empty() {
                        "(no goals)".into()
                    } else if arg.trim() == "all" {
                        // Full unfiltered dump, oldest-run history included
                        // -- the pre-2026-09-12 behavior, kept as an
                        // explicit opt-in for when you actually want to
                        // see everything ever run, not just what needs
                        // attention right now.
                        gs.iter().map(|g| format!("{}  [{}]  {}", g.id, g.status, g.text)).collect::<Vec<_>>().join("\n")
                    } else {
                        // Live-verification finding: the unfiltered dump
                        // makes `/goals` useless once a session has run
                        // more than a handful of goals -- everything ever
                        // submitted, done/cancelled/failed/active, all
                        // mixed together with no way to tell what still
                        // needs a decision. Default view now separates
                        // what's actually actionable (still running or
                        // recoverable) from what failed recently (so you
                        // can restart/fix it) from the rest (collapsed to
                        // a count) -- `/goals all` still gives the full
                        // list when you actually want it.
                        format_goals_summary(&gs)
                    }
                }
                Ok(other) => format!("unexpected: {other:?}"),
                Err(e) => format!("[error: {e}]"),
            },
            "agents" => shell_out(&["doctor"]),
            "usage" => shell_out(&["usage", "show"]),
            "mcp" => shell_out(&["mcp", "list"]),
            "lsp" => shell_out(&["lsp", "list"]),
            "providers" => shell_out(&["provider", "list"]),
            "dashboard" => "Open the divisi control panel: run `divisi` in a terminal, or the Zed task \"divisi: control panel\".".into(),
            "agent" => {
                if arg.is_empty() {
                    let current = self
                        .sessions
                        .lock()
                        .unwrap()
                        .get(acp_sid)
                        .and_then(|s| s.agent_override.clone())
                        .unwrap_or_else(|| "single-pool (default)".to_string());
                    format!("current session agent: {current}\nusage: /agent <name>  or  /agent default")
                } else if arg == "default" {
                    if let Some(s) = self.sessions.lock().unwrap().get_mut(acp_sid) {
                        s.agent_override = None;
                    }
                    "reset to default agent (single-pool)".to_string()
                } else {
                    if let Some(s) = self.sessions.lock().unwrap().get_mut(acp_sid) {
                        s.agent_override = Some(arg.to_string());
                    }
                    format!("session agent pinned to {arg}")
                }
            }
            "cancel" => {
                if let Ok(Some(gid)) = self.active_goal(acp_sid) {
                    let _ = self.socket(Request::GoalCancel { goal_id: gid.clone() });
                    format!("cancelled {gid}")
                } else {
                    "(no active goal to cancel)".into()
                }
            }
            _ => "divisi acp — bridge to the divisi coordinator.\n\
                  Commands: /status /goals /agents /usage /mcp /lsp /providers /dashboard /agent /cancel\n\
                  Modes: auto · plan · careful · dry\n\
                  Any other prompt becomes a coordinator goal; progress streams back here."
                .into(),
        }
        .trim_end()
        .to_string()
            + "\n"
    }

    // ---- helpers -----------------------------------------------------

    fn answer_status_query(&self, acp_sid: &str, text: &str) -> Option<String> {
        if let Some(gid) = extract_goal_id(text) {
            return match self.socket(Request::GoalStatus { goal_id: gid }) {
                Ok(ResponseData::GoalView(v)) => Some(format_goal_view(&v)),
                _ => None,
            };
        }
        if looks_like_status_query(text) {
            let _ = acp_sid;
            return match self.socket(Request::CoordinatorStatus) {
                Ok(ResponseData::CoordinatorSnapshot(s)) => Some(format_snapshot(&s)),
                _ => None,
            };
        }
        None
    }

    fn active_goal(&self, acp_sid: &str) -> Result<Option<String>> {
        let coord_id = match self.sessions.lock().unwrap().get(acp_sid) {
            Some(s) => s.coord_id.clone(),
            None => return Ok(None),
        };
        match self.socket(Request::GoalList { session_id: Some(coord_id) })? {
            ResponseData::Goals(gs) => Ok(gs
                .iter()
                // E28 spec §10/§8: `waiting_on_capacity` counts as active
                // too -- it's a live hold, not a stall, so `session/cancel`
                // can reach it and `session_load`'s re-attach finds it.
                .find(|g| matches!(g.status.as_str(), "running" | "planning" | "queued" | "blocked" | "waiting_on_capacity"))
                .map(|g| g.id.clone())),
            _ => Ok(None),
        }
    }

    fn goal_view(&self, goal_id: &str) -> Option<divisi_protocol::GoalView> {
        match self.socket(Request::GoalStatus { goal_id: goal_id.to_string() }) {
            Ok(ResponseData::GoalView(v)) => Some(v),
            _ => None,
        }
    }

    fn socket(&self, req: Request) -> Result<ResponseData> {
        match crate::client::send(&self.socket_path, req)? {
            Response::Ok { data } => Ok(data),
            Response::Error { message } => Err(anyhow::anyhow!(message)),
        }
    }

    // ---- transport -------------------------------------------------

    fn respond(&self, id: Option<Value>, result: Value) {
        let Some(id) = id else { return };
        self.send_raw(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn rpc_error(&self, id: Option<Value>, code: i64, message: &str) {
        let Some(id) = id else { return };
        self.send_raw(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }));
    }

    fn session_update(&self, session_id: &str, update: Value) {
        self.send_raw(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": { "sessionId": session_id, "update": update }
        }));
    }

    fn chunk(&self, session_id: &str, text: &str, kind: &str) {
        if text.is_empty() {
            return;
        }
        self.session_update(
            session_id,
            json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } }),
        );
    }

    fn send_raw(&self, v: Value) {
        let s = v.to_string();
        log(&format!("OUT {}", &s[..s.len().min(300)]));
        let mut out = self.out.lock().unwrap();
        let _ = writeln!(out, "{s}");
        let _ = out.flush();
    }
}

// -------------------------------------------------------------- pure helpers

fn default_cwd() -> String {
    std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_else(|_| ".".into())
}

fn id_key(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn prompt_text(p: &Value) -> String {
    p.get("prompt")
        .and_then(|b| b.as_array())
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// spec §7: a prompt that is asking about state, not requesting work — it
/// is answered from `CoordinatorStatus` with no agent burned.
pub fn looks_like_status_query(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    if t.split_whitespace().count() > 16 {
        return false; // too long to be a quick "how's it going"
    }
    // phrases that are a status ask on their own, no question mark needed.
    const STRONG: &[&str] = &[
        "how's it going", "how is it going", "hows it going", "what's running",
        "whats running", "what is running", "where are we", "done yet", "how far along",
        "still running", "what's the status", "whats the status", "status of the",
        "coordinator status", "goal status", "any progress",
    ];
    if STRONG.iter().any(|c| t.contains(c)) {
        return true;
    }
    // otherwise it must read as a question AND mention progress/state — so a
    // work request like "add a status bar and wire it up" doesn't match.
    let is_question = t.ends_with('?') || t.starts_with("how ") || t.starts_with("what ") || t.starts_with("is it ");
    let state_word = ["status", "progress", "running", "going", "queue", "blocked"]
        .iter()
        .any(|w| t.contains(w));
    is_question && state_word
}

/// pulls a `goal_…` id out of a prompt, if it names one.
pub fn extract_goal_id(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || "\"'(),".contains(c))
        .find(|w| w.starts_with("goal_") && w.len() > 5)
        .map(str::to_string)
}

fn modes_block(current: &str) -> Value {
    // spec §7 + E27 decision: the four GoalMode values only. forced-agent
    // shortcuts wait for routing-pin (Phase 4).
    let modes = [
        ("auto", "Auto · plan, run, self-correct, integrate"),
        ("plan", "Plan · produce the task graph, don't dispatch"),
        ("careful", "Careful · iterate a node until it reports done"),
        ("dry", "Dry · plan and cost it, run nothing"),
    ];
    json!({
        "currentModeId": current,
        "availableModes": modes.iter().map(|(id, d)| json!({ "id": id, "name": d, "description": d })).collect::<Vec<_>>()
    })
}

fn commands() -> Value {
    json!([
        { "name": "status", "description": "coordinator: running/queued/blocked goals + pool + provider auth/exhaustion" },
        { "name": "goals", "description": "active goals + recent failures (add 'all' for the full unfiltered history)" },
        { "name": "agents", "description": "detected agents / auth (divisi doctor)" },
        { "name": "usage", "description": "per-agent run counts / latency" },
        { "name": "mcp", "description": "MCP servers in divisi-gateway" },
        { "name": "lsp", "description": "LSP servers divisi-lsp can route to" },
        { "name": "providers", "description": "configured LLM providers" },
        { "name": "dashboard", "description": "how to open the divisi control panel" },
        { "name": "agent", "description": "set or clear this session's pinned agent (default: single-pool)" },
        { "name": "cancel", "description": "cancel this session's active goal" }
    ])
}

fn shell_out(args: &[&str]) -> String {
    match std::process::Command::new("divisi").args(args).output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            if !o.stderr.is_empty() {
                s.push_str(&String::from_utf8_lossy(&o.stderr));
            }
            if s.trim().is_empty() {
                "(no output)".into()
            } else {
                s
            }
        }
        Err(e) => format!("[could not run `divisi {}`: {e}]", args.join(" ")),
    }
}

fn format_snapshot(s: &divisi_protocol::CoordinatorSnapshot) -> String {
    let mut out = format!("coordinator — max_parallel {}\n", s.max_parallel);
    let mut section = |label: &str, goals: &[divisi_protocol::GoalSummary]| {
        if !goals.is_empty() {
            out.push_str(&format!("{label}:\n"));
            for g in goals {
                out.push_str(&format!("  {}  {}\n", g.id, g.text));
            }
        }
    };
    section("running", &s.running_goals);
    section("queued", &s.queued_goals);
    section("blocked", &s.blocked_goals);
    let busy: Vec<_> = s.pool.iter().filter(|p| p.running > 0 || p.rate_limited).collect();
    if !busy.is_empty() {
        out.push_str("pool:\n");
        for p in busy {
            let rl = if p.rate_limited { " (rate-limited)" } else { "" };
            out.push_str(&format!("  {} {}{}\n", p.agent, p.running, rl));
        }
    }
    out
}

/// `/goals`'s default (non-`all`) view: everything still actionable
/// (non-terminal statuses) up top, then the most recent failures (so you
/// can restart/fix them), then a one-line count of the rest so the list
/// doesn't quietly imply "there's nothing else" while still not drowning
/// the actionable items in old history. "Recent" is the last
/// `RECENT_FAILURES_SHOWN` failures by creation order rather than a
/// wall-clock window -- `created_at` is RFC3339 UTC, which sorts
/// correctly as a plain string, so this needs no date-math/chrono
/// dependency in this crate (deliberately kept out, see `chrono_now`'s
/// doc comment above).
const RECENT_FAILURES_SHOWN: usize = 10;

fn format_goals_summary(goals: &[divisi_protocol::GoalSummary]) -> String {
    let is_active = |status: &str| matches!(status, "planning" | "running" | "queued" | "waiting_on_capacity" | "paused");

    let mut active: Vec<_> = goals.iter().filter(|g| is_active(&g.status)).collect();
    let mut failed: Vec<_> = goals.iter().filter(|g| g.status == "failed").collect();
    // Most recently created first within each section -- the ones you're
    // most likely to want to act on right now.
    active.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    failed.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let recent_failures = &failed[..failed.len().min(RECENT_FAILURES_SHOWN)];

    let mut out = String::new();
    let section = |out: &mut String, label: &str, list: &[&divisi_protocol::GoalSummary]| {
        if list.is_empty() {
            return;
        }
        out.push_str(&format!("{label}:\n"));
        for g in list {
            let extra = match (&g.capacity_reason, &g.capacity_eta) {
                (Some(reason), Some(eta)) => format!("  ({reason}, retry {eta})"),
                _ => String::new(),
            };
            out.push_str(&format!("  {}  [{}]  {}{}\n", g.id, g.status, g.text, extra));
        }
    };
    section(&mut out, "active", &active);
    section(&mut out, "recently failed", recent_failures);

    let shown: std::collections::HashSet<&str> = active.iter().chain(recent_failures.iter()).map(|g| g.id.as_str()).collect();
    let rest = goals.len() - shown.len();
    if rest > 0 {
        out.push_str(&format!("...and {rest} older/terminal goal(s) — `/goals all` to see everything.\n"));
    }
    if out.is_empty() {
        out.push_str("(no active goals, no failures)\n");
    }
    out
}

fn format_goal_view(v: &divisi_protocol::GoalView) -> String {
    let mut out = format!("{}  [{}]  {}\n", v.goal.id, v.goal.status, v.goal.text);
    out.push_str(&format!("dispatches {}/{}\n", v.goal.dispatches, v.goal.max_dispatches));
    if let Some(r) = &v.blocked_reason {
        out.push_str(&format!("blocked: {r}\n"));
    }
    if let Some(r) = &v.result_summary {
        out.push_str(&format!("result: {r}\n"));
    }
    if v.total_prompt_tokens > 0 || v.total_completion_tokens > 0 {
        out.push_str(&usage_line(v));
    }
    for n in &v.nodes {
        out.push_str(&format!("  {:<4} {:<9} {:<8} {}\n", n.id, n.status, n.agent, n.desc));
    }
    out
}

/// one-line token summary for a goal (E27.03).
fn usage_line(v: &divisi_protocol::GoalView) -> String {
    format!(
        "tokens ~{} in / ~{} out{}\n",
        v.total_prompt_tokens,
        v.total_completion_tokens,
        if v.any_tokens_estimated { " (estimated)" } else { "" }
    )
}

fn log(msg: &str) {
    if let Ok(path) = std::env::var("DIVISI_ACP_LOG").or_else(|_| {
        std::env::var("HOME").map(|h| format!("{h}/.cache/single-acp.log"))
    }) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{} {msg}", chrono_now());
        }
    }
}

fn chrono_now() -> String {
    // avoid pulling chrono into divisi-cli just for a log timestamp.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(id: &str, status: &str, created_at: &str) -> divisi_protocol::GoalSummary {
        divisi_protocol::GoalSummary {
            id: id.to_string(),
            session_id: "s1".to_string(),
            text: format!("goal {id}"),
            mode: "auto".to_string(),
            status: status.to_string(),
            dispatches: 0,
            max_dispatches: 10,
            created_at: created_at.to_string(),
            capacity_reason: None,
            capacity_eta: None,
            blocked_reason: None,
        }
    }

    #[test]
    fn goals_summary_separates_active_and_failed_from_terminal_history() {
        let goals = vec![
            goal("g1", "running", "2026-01-01T00:00:00Z"),
            goal("g2", "done", "2026-01-02T00:00:00Z"),
            goal("g3", "failed", "2026-01-03T00:00:00Z"),
            goal("g4", "cancelled", "2026-01-04T00:00:00Z"),
        ];
        let out = format_goals_summary(&goals);
        assert!(out.contains("active:"));
        assert!(out.contains("g1"));
        assert!(out.contains("recently failed:"));
        assert!(out.contains("g3"));
        assert!(!out.contains("g2"), "a done goal must not appear in the default view: {out}");
        assert!(!out.contains("g4"), "a cancelled goal must not appear in the default view: {out}");
        assert!(out.contains("2 older/terminal goal"), "expected the done+cancelled pair collapsed to a count: {out}");
    }

    #[test]
    fn goals_summary_caps_recent_failures_and_orders_newest_first() {
        let mut goals = Vec::new();
        for i in 0..15 {
            goals.push(goal(&format!("f{i}"), "failed", &format!("2026-01-{:02}T00:00:00Z", i + 1)));
        }
        let out = format_goals_summary(&goals);
        assert!(out.contains("f14"), "the newest failure must be shown: {out}");
        assert!(!out.contains("f0\n") && !out.contains("f4\n"), "the oldest failures must be capped out of the default view: {out}");
        assert!(out.contains("5 older/terminal goal"));
    }

    #[test]
    fn goals_summary_reports_nothing_pending_when_all_terminal_and_old() {
        let goals = vec![goal("g1", "done", "2026-01-01T00:00:00Z")];
        let out = format_goals_summary(&goals);
        assert!(out.contains("older/terminal goal"));
        assert!(!out.contains("active:"));
        assert!(!out.contains("recently failed:"));
    }

    #[test]
    fn goal_submit_defaults_to_single_pool_when_no_override_set() {
        let agent: Option<String> = None;
        let resolved = agent.or_else(|| Some("single-pool".to_string()));
        assert_eq!(resolved.as_deref(), Some("single-pool"));
    }

    #[test]
    fn goal_submit_honors_explicit_override() {
        let agent: Option<String> = Some("opencode".to_string());
        let resolved = agent.or_else(|| Some("single-pool".to_string()));
        assert_eq!(resolved.as_deref(), Some("opencode"));
    }

    #[test]
    fn status_query_heuristic() {
        assert!(looks_like_status_query("what's the status?"));
        assert!(looks_like_status_query("how's it going"));
        assert!(looks_like_status_query("any progress on the auth work?"));
        assert!(looks_like_status_query("is it done yet"));
        assert!(!looks_like_status_query("add a status bar to the settings page and wire it to the store"));
        assert!(!looks_like_status_query("implement the login flow"));
    }

    #[test]
    fn goal_id_extraction() {
        assert_eq!(extract_goal_id("how is goal_abc123 going?").as_deref(), Some("goal_abc123"));
        assert_eq!(extract_goal_id("status of (goal_x9y)").as_deref(), Some("goal_x9y"));
        assert_eq!(extract_goal_id("no id here"), None);
        assert_eq!(extract_goal_id("goal_"), None);
    }

    #[test]
    fn prompt_text_joins_text_blocks_only() {
        let p = json!({ "prompt": [
            { "type": "text", "text": "hello " },
            { "type": "image", "data": "…" },
            { "type": "text", "text": "world" }
        ]});
        assert_eq!(prompt_text(&p), "hello world");
    }

    #[test]
    fn modes_block_lists_the_four_goal_modes() {
        let m = modes_block("plan");
        assert_eq!(m["currentModeId"], "plan");
        let ids: Vec<_> = m["availableModes"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap().to_string()).collect();
        assert_eq!(ids, vec!["auto", "plan", "careful", "dry"]);
    }

    fn ev(id: i64, kind: &str, body: Value) -> divisi_protocol::CoordinatorEvent {
        divisi_protocol::CoordinatorEvent { id, goal_id: None, ts: "t".into(), kind: kind.into(), body: body.to_string() }
    }

    #[test]
    fn chat_events_become_turn_actions_in_order() {
        let events = vec![
            ev(1, "chat_user", json!({"text": "hi", "surface": "zed"})),
            ev(2, "chat_assistant", json!({"text": "Started goal_a_1 in auto mode.", "goal_ids": ["goal_a_1"], "degraded": false})),
        ];
        assert_eq!(
            plan_actions(&events),
            vec![TurnAction::Say("Started goal_a_1 in auto mode.".into()), TurnAction::Goal("goal_a_1".into())],
            "your own message is not echoed back into Zed"
        );
    }

    #[test]
    fn a_confirmation_becomes_a_prompt_and_a_degraded_reply_says_so() {
        let events = vec![
            ev(1, "chat_confirm", json!({"approval_id": 7, "summary": "cancel goal_a_1"})),
            ev(2, "chat_assistant", json!({"text": "ok", "goal_ids": [], "degraded": true})),
            ev(3, "chat_result", json!({"approval_id": 7, "outcome": "approved"})),
        ];
        let actions = plan_actions(&events);
        assert_eq!(actions[0], TurnAction::Confirm { approval_id: 7, summary: "cancel goal_a_1".into() });
        assert_eq!(actions[1], TurnAction::Say("ok\n_(rules-only mode: no model was reachable)_".into()));
        assert_eq!(actions.len(), 2, "a result line is not repeated to the user");
    }

    #[test]
    fn only_an_old_daemon_triggers_the_legacy_fallback() {
        assert!(is_unsupported(&anyhow::anyhow!("invalid request: unknown variant `ChatSend`, expected one of `Status`")));
        assert!(!is_unsupported(&anyhow::anyhow!("that message is too long (8000 characters at most)")));
        assert!(!is_unsupported(&anyhow::anyhow!("connection refused")));
    }
}
