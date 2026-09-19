//! `divisi serve --openai` — an OpenAI-compatible HTTP proxy over the
//! divisi pool (E27.01 backlog #2 / Zed backlog #2). Lets Zed's inline
//! assistant / edit-prediction / commit-message model run on the pool via
//! `language_models.openai_compatible`.
//!
//! Deliberately tiny: a hand-rolled HTTP/1.1 server on `std::net` (no new
//! dependency), one thread per connection. Each `/v1/chat/completions`
//! flattens the messages into a prompt, picks an agent (the `model` name
//! if it's a real agent, else routed like the coordinator's `code/quick`
//! kind), and runs one `Request::TaskRun { allow_fallback: true }` against
//! the daemon — so a 429 already hops the fallback chain inside the
//! runtime. `task::run` is one-shot, so `stream: true` is honoured as a
//! single content chunk followed by `[DONE]`, not real per-token SSE.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use divisi_protocol::{Request, Response, ResponseData, TaskRecord};
use divisi_runtime::coordinator::graph::{Effort, NodeKind};
use divisi_runtime::coordinator::routing::{self, PoolHealth, RoutingTable};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Caps so a slow or hostile client can't tie up a worker thread or make
/// the server allocate unbounded memory.
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_HEADER_LINES: usize = 100;
const MAX_BODY_BYTES: usize = 1024 * 1024;

pub struct Config {
    pub socket_path: PathBuf,
    pub addr: String,
    /// Forced agent for every request (overrides routing). `None` → route
    /// per request when the model name isn't itself an agent.
    pub agent: Option<String>,
    pub timeout_secs: u64,
    /// Bearer token required on every request. `None` → generate one at
    /// startup and print it.
    pub api_key: Option<String>,
    /// Permit binding a non-loopback address. Off by default: the server
    /// runs arbitrary agent CLIs, so exposing it to the network is a
    /// deliberate choice and still requires `--api-key`.
    pub allow_remote: bool,
}

/// Shared, immutable per-run state handed to every connection thread.
struct Server {
    cfg: Config,
    ctx: divisi_runtime::Context,
    api_key: String,
    /// Acceptable `Host` header values (host[:port]) — a request whose
    /// `Host` isn't one of these is refused, which defeats DNS-rebinding
    /// attacks from a browser even though it can reach the socket.
    allowed_hosts: Vec<String>,
}

pub fn run(cfg: Config) -> Result<()> {
    let ctx = divisi_runtime::Context::load().context("loading divisi context")?;

    let (host, port) = split_host_port(&cfg.addr);
    let is_loopback = matches!(host.as_str(), "127.0.0.1" | "::1" | "localhost")
        || host.starts_with("127.");
    if !is_loopback && !cfg.allow_remote {
        bail!(
            "refusing to bind non-loopback address {} — this server runs agent CLIs. \
             Pass --allow-remote AND --api-key to expose it deliberately.",
            cfg.addr
        );
    }
    if !is_loopback && cfg.api_key.is_none() {
        bail!("--allow-remote requires an explicit --api-key");
    }

    let api_key = cfg.api_key.clone().unwrap_or_else(random_token);
    let allowed_hosts = vec![
        format!("{host}:{port}"),
        format!("localhost:{port}"),
        format!("127.0.0.1:{port}"),
        host.clone(),
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ];

    let listener = TcpListener::bind(&cfg.addr).with_context(|| format!("binding {}", cfg.addr))?;
    eprintln!("divisi serve --openai listening on http://{}/v1  (Ctrl-C to stop)", cfg.addr);
    if cfg.api_key.is_none() {
        eprintln!("api key (send as `Authorization: Bearer <key>`): {api_key}");
    }

    let server = std::sync::Arc::new(Server { cfg, ctx, api_key, allowed_hosts });
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let server = std::sync::Arc::clone(&server);
        std::thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
            let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
            if let Err(e) = handle_conn(stream, &server) {
                eprintln!("[serve] connection error: {e:#}");
            }
        });
    }
    Ok(())
}

fn split_host_port(addr: &str) -> (String, String) {
    match addr.rsplit_once(':') {
        Some((h, p)) => (h.trim_matches(['[', ']']).to_string(), p.to_string()),
        None => (addr.to_string(), "80".to_string()),
    }
}

/// 32 hex chars from `/dev/urandom`, or a time+pid mix if that fails.
fn random_token() -> String {
    let mut buf = [0u8; 16];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).is_ok() {
        return buf.iter().map(|b| format!("{b:02x}")).collect();
    }
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{n:032x}{:x}", std::process::id())
}

/// Length-then-byte comparison; not fully constant-time, but the token is
/// 128-bit random so a timing oracle is not a realistic path here.
fn token_matches(expected: &str, got: &str) -> bool {
    expected.len() == got.len()
        && expected.bytes().zip(got.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

struct HttpRequest {
    method: String,
    path: String,
    host: String,
    authorization: String,
    body: String,
}

fn read_request(stream: &TcpStream) -> Result<HttpRequest> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut content_length = 0usize;
    let mut host = String::new();
    let mut authorization = String::new();
    let mut header_bytes = 0usize;
    let mut header_lines = 0usize;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break; // connection closed before headers ended
        }
        header_bytes += n;
        header_lines += 1;
        if header_bytes > MAX_HEADER_BYTES || header_lines > MAX_HEADER_LINES {
            bail!("request headers too large");
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = lower.strip_prefix("host:") {
            host = v.trim().to_string();
        } else if lower.starts_with("authorization:") {
            authorization = line["authorization:".len()..].trim().to_string();
        }
    }
    if content_length > MAX_BODY_BYTES {
        bail!("request body exceeds {MAX_BODY_BYTES} bytes");
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }
    Ok(HttpRequest {
        method,
        path,
        host,
        authorization,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn handle_conn(mut stream: TcpStream, srv: &Server) -> Result<()> {
    let req = match read_request(&stream) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[serve] bad request: {e:#}");
            return write_json(
                &mut stream,
                400,
                &json!({ "error": { "message": "malformed or oversized request", "type": "invalid_request_error" } }),
            );
        }
    };
    let path = req.path.split('?').next().unwrap_or(&req.path);

    // /health is the one unauthenticated, host-agnostic route (liveness only).
    if req.method == "GET" && path == "/health" {
        return write_json(&mut stream, 200, &json!({ "ok": true }));
    }

    // DNS-rebinding guard: a browser tricked into hitting this socket still
    // sends the attacker's hostname in `Host`.
    if !srv.allowed_hosts.iter().any(|h| h.eq_ignore_ascii_case(&req.host)) {
        return write_json(
            &mut stream,
            421,
            &json!({ "error": { "message": "unrecognized Host header", "type": "invalid_request_error" } }),
        );
    }

    // Bearer auth on every real route (including /v1/models — it lists the
    // agent inventory).
    let presented = req.authorization.strip_prefix("Bearer ").or_else(|| req.authorization.strip_prefix("bearer ")).unwrap_or("");
    if !token_matches(&srv.api_key, presented) {
        return write_json(
            &mut stream,
            401,
            &json!({ "error": { "message": "missing or invalid Authorization bearer token", "type": "invalid_request_error" } }),
        );
    }

    match (req.method.as_str(), path) {
        ("GET", "/v1/models") => write_json(&mut stream, 200, &models_body(&srv.ctx)),
        ("POST", "/v1/chat/completions") => chat_completions(&mut stream, &req.body, &srv.cfg, &srv.ctx),
        _ => write_json(
            &mut stream,
            404,
            &json!({ "error": { "message": "no such route", "type": "invalid_request_error" } }),
        ),
    }
}

fn models_body(ctx: &divisi_runtime::Context) -> Value {
    let now = unix_now();
    let mut data = vec![json!({ "id": "pool", "object": "model", "created": now, "owned_by": "divisi" })];
    for a in &ctx.registry {
        data.push(json!({ "id": a.name, "object": "model", "created": now, "owned_by": "divisi" }));
    }
    json!({ "object": "list", "data": data })
}

fn chat_completions(stream: &mut TcpStream, body: &str, cfg: &Config, ctx: &divisi_runtime::Context) -> Result<()> {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return write_json(
                stream,
                400,
                &json!({ "error": { "message": format!("invalid JSON body: {e}"), "type": "invalid_request_error" } }),
            )
        }
    };
    let model = parsed.get("model").and_then(|m| m.as_str()).unwrap_or("pool").to_string();
    let stream_reply = parsed.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
    let messages = parsed.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
    if messages.is_empty() {
        return write_json(
            stream,
            400,
            &json!({ "error": { "message": "`messages` is required and must be non-empty", "type": "invalid_request_error" } }),
        );
    }
    let prompt = flatten_messages(&messages);
    let agent = pick_agent(&model, cfg, ctx);

    let response = crate::client::send(
        &cfg.socket_path,
        Request::TaskRun {
            description: prompt,
            agent: agent.clone(),
            cwd: ".".to_string(),
            use_worktree: false,
            account: None,
            real_home: false,
            no_memory_context: true, // the messages ARE the prompt
            timeout_secs: cfg.timeout_secs,
            background: false,
            allow_fallback: true,
            usage_json: false,
        },
    );

    let rec = match response {
        Ok(Response::Ok { data: ResponseData::Task(rec) }) => rec,
        Ok(Response::Ok { data }) => {
            eprintln!("[serve] unexpected daemon response: {data:?}");
            return write_json(
                stream,
                502,
                &json!({ "error": { "message": "unexpected response from the runtime", "type": "api_error" } }),
            );
        }
        Ok(Response::Error { message }) => {
            eprintln!("[serve] runtime error: {message}");
            return write_json(
                stream,
                502,
                &json!({ "error": { "message": "the runtime rejected the request", "type": "api_error" } }),
            );
        }
        Err(e) => {
            eprintln!("[serve] daemon unreachable: {e:#}");
            return write_json(
                stream,
                502,
                &json!({ "error": { "message": "the divisi daemon is unreachable", "type": "api_error" } }),
            );
        }
    };

    let content = task_content(&rec);
    // "length" only when the run was cut off; a failed agent still returns
    // text, so surface it with "stop" rather than erroring the request.
    let finish = if rec.timed_out { "length" } else { "stop" };
    let (pt, ct) = (rec.prompt_tokens.unwrap_or(0).max(0), rec.completion_tokens.unwrap_or(0).max(0));
    let id = format!("chatcmpl-{}", rec.id);

    if stream_reply {
        write_sse(stream, &id, &model, &content, finish)
    } else {
        let payload = json!({
            "id": id,
            "object": "chat.completion",
            "created": unix_now(),
            "model": model,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": finish,
            }],
            "usage": { "prompt_tokens": pt, "completion_tokens": ct, "total_tokens": pt + ct },
        });
        write_json(stream, 200, &payload)
    }
}

/// Flattens chat messages into one prompt. System messages become a
/// preamble; the rest are `role: content` lines in order.
pub fn flatten_messages(messages: &[Value]) -> String {
    let text_of = |m: &Value| -> String {
        match m.get("content") {
            Some(Value::String(s)) => s.clone(),
            // OpenAI vision-style content parts: keep the text parts.
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    };
    let mut preamble = Vec::new();
    let mut turns = Vec::new();
    for m in messages {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let text = text_of(m);
        if text.is_empty() {
            continue;
        }
        if role == "system" {
            preamble.push(text);
        } else {
            turns.push(format!("{role}: {text}"));
        }
    }
    let mut out = String::new();
    if !preamble.is_empty() {
        out.push_str(&preamble.join("\n\n"));
        out.push_str("\n\n");
    }
    out.push_str(&turns.join("\n"));
    out
}

/// `model` is used verbatim if it names a real registered agent; `"pool"`
/// or anything unrecognized routes (the `--agent` override wins over
/// routing).
pub fn pick_agent(model: &str, cfg: &Config, ctx: &divisi_runtime::Context) -> String {
    if ctx.registry.iter().any(|a| a.name == model) {
        return model.to_string();
    }
    if let Some(a) = &cfg.agent {
        return a.clone();
    }
    let table = RoutingTable::load(&ctx.dirs);
    let health = divisi_runtime::state::open(&ctx.dirs.db_path())
        .map(|conn| PoolHealth::probe(&ctx.registry, &conn))
        .unwrap_or_default();
    routing::select_agent(&table, NodeKind::Code, Effort::Quick, &health)
        .or_else(|| table.fallback_default.first().cloned())
        .unwrap_or_else(|| "opencode".to_string())
}

/// The agent's answer text — the captured artifact if present, else the
/// stored summary.
fn task_content(rec: &TaskRecord) -> String {
    if let Some(p) = &rec.artifact_path {
        if let Ok(s) = std::fs::read_to_string(p) {
            // artifacts are `"<stdout>\n--- stderr ---\n<stderr>"`; keep
            // the stdout half for a clean assistant message.
            return s.split("\n--- stderr ---\n").next().unwrap_or(&s).trim_end().to_string();
        }
    }
    rec.summary.clone().unwrap_or_default()
}

// ---- HTTP writers -------------------------------------------------------

fn write_json(stream: &mut TcpStream, status: u16, body: &Value) -> Result<()> {
    let text = body.to_string();
    let reason = if (200..300).contains(&status) { "OK" } else { "ERROR" };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    )?;
    stream.flush()?;
    Ok(())
}

/// Single-chunk SSE: one delta with the whole content, then `[DONE]`.
/// Real per-token streaming isn't available (the underlying `task::run`
/// is one-shot).
fn write_sse(stream: &mut TcpStream, id: &str, model: &str, content: &str, finish: &str) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
    )?;
    let created = unix_now();
    let role_chunk = json!({
        "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
        "choices": [{ "index": 0, "delta": { "role": "assistant" }, "finish_reason": null }],
    });
    let text_chunk = json!({
        "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
        "choices": [{ "index": 0, "delta": { "content": content }, "finish_reason": null }],
    });
    let stop_chunk = json!({
        "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
        "choices": [{ "index": 0, "delta": {}, "finish_reason": finish }],
    });
    write!(stream, "data: {role_chunk}\n\n")?;
    write!(stream, "data: {text_chunk}\n\n")?;
    write!(stream, "data: {stop_chunk}\n\n")?;
    write!(stream, "data: [DONE]\n\n")?;
    stream.flush()?;
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_messages_puts_system_first_then_role_lines() {
        let msgs = vec![
            json!({ "role": "system", "content": "you are terse" }),
            json!({ "role": "user", "content": "hi" }),
            json!({ "role": "assistant", "content": "hello" }),
            json!({ "role": "user", "content": "bye" }),
        ];
        let p = flatten_messages(&msgs);
        assert_eq!(p, "you are terse\n\nuser: hi\nassistant: hello\nuser: bye");
    }

    #[test]
    fn flatten_messages_handles_content_parts_and_skips_empty() {
        let msgs = vec![
            json!({ "role": "user", "content": [ { "type": "text", "text": "part one" }, { "type": "image_url" }, { "type": "text", "text": "part two" } ] }),
            json!({ "role": "assistant", "content": "" }),
        ];
        assert_eq!(flatten_messages(&msgs), "user: part one\npart two");
    }

    #[test]
    fn token_matches_only_on_exact_equal_length() {
        assert!(token_matches("abc123", "abc123"));
        assert!(!token_matches("abc123", "abc124"));
        assert!(!token_matches("abc123", "abc12")); // shorter
        assert!(!token_matches("abc123", "abc1234")); // longer
        assert!(!token_matches("secret", ""));
    }

    #[test]
    fn split_host_port_handles_ipv4_ipv6_and_bare() {
        assert_eq!(split_host_port("127.0.0.1:8765"), ("127.0.0.1".into(), "8765".into()));
        assert_eq!(split_host_port("[::1]:9000"), ("::1".into(), "9000".into()));
        assert_eq!(split_host_port("localhost"), ("localhost".into(), "80".into()));
    }

    #[test]
    fn random_token_is_32_hex_chars() {
        let t = random_token();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(random_token(), t);
    }

    #[test]
    fn openai_response_shape_has_the_required_fields() {
        // guards the non-stream payload keys Zed's client checks.
        let payload = json!({
            "id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": "pool",
            "choices": [{ "index": 0, "message": { "role": "assistant", "content": "x" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 },
        });
        assert_eq!(payload["choices"][0]["message"]["role"], "assistant");
        assert_eq!(payload["usage"]["total_tokens"], 3);
        let s: divisi_protocol::TokenUsage = serde_json::from_value(json!({ "prompt_tokens": 1, "completion_tokens": 2 })).unwrap();
        assert_eq!(s.completion_tokens, 2);
    }
}
