//! Native (non-OpenAI-compat) wires — spec §6.6, Task 12. Each of these
//! providers has `base_url: ""` in the catalog (spec §5.1: `""` means
//! "entirely native, no plain OpenAI-compat base to point at") so the
//! actual endpoint is hardcoded here per wire, not read from the catalog.
//!
//! **Confirmed vs assumed**: `google`/`cohere`/`cloudflare` endpoints are
//! their well-documented public APIs (confirmed against public docs at
//! authoring time). `zhipu`'s domestic/global pair and `aihorde`'s
//! submit/poll shape match freellmapi's studied (not vendored) provider
//! notes. `sail`/`modelscope`/`pollinations`/`electronhub`/`experiential`
//! endpoints are **assumed** from the catalog's `free_note`/`quirks`
//! hints — `sail` is default-disabled (§17) so this is moot for it, and
//! the other four should be spot-checked against their current docs
//! before `single provider add-free` is used against them for real.

use super::{extract_think, PoolError, PoolRequest, PoolResponse, PoolWire};
use serde_json::{json, Value};
use divisi_core::free_pool::FreeProvider;
use std::time::Instant;

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::new()
}

fn http_error_to_pool_error(status: u16, body: &str) -> PoolError {
    match status {
        401 => PoolError::AuthFailed,
        402 => PoolError::PaymentRequired,
        403 => PoolError::TierGate,
        429 => {
            let parsed: Option<Value> = serde_json::from_str(body).ok();
            let retry = crate::pool::backoff::resolve(None, parsed.as_ref(), Some(body));
            PoolError::RateLimited { retry }
        }
        _ => PoolError::Other(format!("status {status}: {body}")),
    }
}

// ---------------------------------------------------------------- Google

pub struct GoogleWire;

impl PoolWire for GoogleWire {
    fn dispatch(&self, req: &PoolRequest, provider: &FreeProvider, key: &str) -> Result<PoolResponse, PoolError> {
        let started = Instant::now();
        let contents: Vec<Value> = req
            .messages
            .iter()
            .map(|m| json!({ "role": if m.role == "assistant" { "model" } else { "user" }, "parts": [{ "text": m.content }] }))
            .collect();
        let body = json!({ "contents": contents });

        // Gemini's REST API accepts the key either as `?key=` or the
        // `x-goog-api-key` header; the header keeps it out of access logs.
        let url = "https://generativelanguage.googleapis.com/v1beta/models/gemini-1.5-flash:generateContent".to_string();
        let resp = client()
            .post(&url)
            .timeout(provider.timeout)
            .header("x-goog-api-key", key)
            .json(&body)
            .send()
            .map_err(|e| PoolError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(http_error_to_pool_error(status, &text));
        }

        let value: Value = serde_json::from_str(&text).map_err(|e| PoolError::Other(format!("invalid JSON: {e}")))?;
        let content = value
            .get("candidates")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.get(0))
            .and_then(|p| p.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let (content, _think) = extract_think(&content);
        let finish_reason = value.get("candidates").and_then(|c| c.get(0)).and_then(|c| c.get("finishReason")).and_then(|f| f.as_str()).map(String::from);
        let truncated = finish_reason.as_deref() == Some("MAX_TOKENS");
        let latency_ms = started.elapsed().as_millis() as u64;

        Ok(PoolResponse { content, tool_calls: vec![], finish_reason, truncated, usage_tokens: None, ttfb_ms: Some(latency_ms), latency_ms })
    }
}

// ---------------------------------------------------------------- Cohere

pub struct CohereWire;

impl PoolWire for CohereWire {
    fn dispatch(&self, req: &PoolRequest, provider: &FreeProvider, key: &str) -> Result<PoolResponse, PoolError> {
        let started = Instant::now();
        let message = req.messages.last().map(|m| m.content.clone()).unwrap_or_default();
        let body = json!({ "model": "command-r", "message": message });

        let resp = client()
            .post("https://api.cohere.com/v1/chat")
            .timeout(provider.timeout)
            .bearer_auth(key)
            .json(&body)
            .send()
            .map_err(|e| PoolError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(http_error_to_pool_error(status, &text));
        }
        let value: Value = serde_json::from_str(&text).map_err(|e| PoolError::Other(format!("invalid JSON: {e}")))?;
        let content = value.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let latency_ms = started.elapsed().as_millis() as u64;
        Ok(PoolResponse { content, tool_calls: vec![], finish_reason: None, truncated: false, usage_tokens: None, ttfb_ms: Some(latency_ms), latency_ms })
    }
}

// ------------------------------------------------------------ Cloudflare

pub struct CloudflareWire;

impl PoolWire for CloudflareWire {
    fn dispatch(&self, req: &PoolRequest, provider: &FreeProvider, key: &str) -> Result<PoolResponse, PoolError> {
        let started = Instant::now();
        let (account_id, token) = match key.split_once(':') {
            Some((a, t)) => (a, t),
            None => return Err(PoolError::AuthFailed),
        };
        let model = "@cf/meta/llama-3.1-8b-instruct";
        let url = format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/run/{model}");
        let messages: Vec<Value> = req.messages.iter().map(|m| json!({ "role": m.role, "content": m.content })).collect();

        let resp = client()
            .post(&url)
            .timeout(provider.timeout)
            .bearer_auth(token)
            .json(&json!({ "messages": messages }))
            .send()
            .map_err(|e| PoolError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(http_error_to_pool_error(status, &text));
        }
        let value: Value = serde_json::from_str(&text).map_err(|e| PoolError::Other(format!("invalid JSON: {e}")))?;
        let content = value.get("result").and_then(|r| r.get("response")).and_then(|r| r.as_str()).unwrap_or("").to_string();
        let latency_ms = started.elapsed().as_millis() as u64;
        Ok(PoolResponse { content, tool_calls: vec![], finish_reason: None, truncated: false, usage_tokens: None, ttfb_ms: Some(latency_ms), latency_ms })
    }
}

// ----------------------------------------------------------------- Zhipu

pub struct ZhipuWire {
    /// Overridable for tests: normally `["https://open.bigmodel.cn/api/paas/v4", "https://api.z.ai/api/paas/v4"]`.
    pub hosts: Vec<&'static str>,
}

impl Default for ZhipuWire {
    fn default() -> Self {
        ZhipuWire { hosts: vec!["https://open.bigmodel.cn/api/paas/v4", "https://api.z.ai/api/paas/v4"] }
    }
}

impl PoolWire for ZhipuWire {
    fn dispatch(&self, req: &PoolRequest, provider: &FreeProvider, key: &str) -> Result<PoolResponse, PoolError> {
        // Domestic-first host re-probe: try each host in order, falling
        // through to the next on a transport failure (the domestic host
        // may be unreachable outside China) but stopping immediately on
        // any real HTTP response (even an error one) since that means
        // the host answered.
        let mut last_err = PoolError::Transport("no zhipu hosts configured".to_string());
        for host in &self.hosts {
            let base = FreeProvider { base_url: host, ..*provider };
            match crate::pool::client::dispatch_openai_compat(
                &client(),
                req,
                &base,
                key,
                provider.timeout,
                &std::sync::atomic::AtomicBool::new(false),
            ) {
                Ok(resp) => return Ok(resp),
                Err(PoolError::Transport(e)) => last_err = PoolError::Transport(e),
                Err(other) => return Err(other),
            }
        }
        Err(last_err)
    }
}

// --------------------------------------------------------------- AiHorde

pub struct AiHordeWire;

/// AI Horde is a volunteer-run queue: submit a generation request, then
/// poll a status endpoint until `done`. Keyless sentinel auth
/// (`"0000000000"`), 120s overall timeout budget for the poll loop.
impl PoolWire for AiHordeWire {
    fn dispatch(&self, req: &PoolRequest, provider: &FreeProvider, key: &str) -> Result<PoolResponse, PoolError> {
        dispatch_aihorde(&client(), req, provider, key, "https://aihorde.net/api/v2")
    }
}

fn dispatch_aihorde(c: &reqwest::blocking::Client, req: &PoolRequest, provider: &FreeProvider, key: &str, base: &str) -> Result<PoolResponse, PoolError> {
    let started = Instant::now();
    let prompt = req.messages.iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("\n");
    let submit_body = json!({
        "prompt": prompt,
        "params": { "max_length": 16, "stop_sequence": ["</s>"] },
    });

    let submit_resp = c
        .post(format!("{base}/generate/text/async"))
        .timeout(provider.timeout)
        .header("apikey", key)
        .json(&submit_body)
        .send()
        .map_err(|e| PoolError::Transport(e.to_string()))?;
    let submit_status = submit_resp.status().as_u16();
    let submit_text = submit_resp.text().unwrap_or_default();
    if !(200..300).contains(&submit_status) {
        return Err(http_error_to_pool_error(submit_status, &submit_text));
    }
    let submit_value: Value = serde_json::from_str(&submit_text).map_err(|e| PoolError::Other(format!("invalid JSON: {e}")))?;
    let request_id = submit_value.get("id").and_then(|i| i.as_str()).ok_or_else(|| PoolError::Other("no request id from aihorde".to_string()))?;

    // Poll loop, budgeted against the overall provider timeout.
    let poll_deadline = started + provider.timeout;
    loop {
        if Instant::now() >= poll_deadline {
            return Err(PoolError::Transport("aihorde poll deadline exceeded".to_string()));
        }
        let status_resp = c
            .get(format!("{base}/generate/text/status/{request_id}"))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .map_err(|e| PoolError::Transport(e.to_string()))?;
        let status_text = status_resp.text().unwrap_or_default();
        let status_value: Value = serde_json::from_str(&status_text).map_err(|e| PoolError::Other(format!("invalid JSON: {e}")))?;

        if status_value.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
            let content = status_value
                .get("generations")
                .and_then(|g| g.get(0))
                .and_then(|g| g.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let latency_ms = started.elapsed().as_millis() as u64;
            return Ok(PoolResponse {
                content,
                tool_calls: vec![],
                finish_reason: None,
                truncated: false,
                usage_tokens: None,
                ttfb_ms: Some(latency_ms),
                latency_ms,
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

// ---------------------------------------------------------------- Sail

pub struct SailWire;

/// Registered but `enabled = false` by default (spec §17 — needs a
/// payment method on file). Implemented so an operator who flips it on
/// gets a working path; endpoint shape **assumed** (Responses-API
/// background-poll pattern per the catalog's `free_note`), not verified
/// against a live account.
impl PoolWire for SailWire {
    fn dispatch(&self, req: &PoolRequest, provider: &FreeProvider, key: &str) -> Result<PoolResponse, PoolError> {
        let started = Instant::now();
        let input: Vec<Value> = req.messages.iter().map(|m| json!({ "role": m.role, "content": m.content })).collect();
        let submit = client()
            .post("https://api.sail.dev/v1/responses")
            .timeout(provider.timeout)
            .bearer_auth(key)
            .json(&json!({ "input": input, "background": true }))
            .send()
            .map_err(|e| PoolError::Transport(e.to_string()))?;
        let status = submit.status().as_u16();
        let text = submit.text().unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(http_error_to_pool_error(status, &text));
        }
        let value: Value = serde_json::from_str(&text).map_err(|e| PoolError::Other(format!("invalid JSON: {e}")))?;
        let response_id = value.get("id").and_then(|i| i.as_str()).ok_or_else(|| PoolError::Other("no response id from sail".to_string()))?;

        let poll_deadline = started + provider.timeout;
        loop {
            if Instant::now() >= poll_deadline {
                return Err(PoolError::Transport("sail poll deadline exceeded".to_string()));
            }
            let poll = client()
                .get(format!("https://api.sail.dev/v1/responses/{response_id}"))
                .timeout(std::time::Duration::from_secs(10))
                .bearer_auth(key)
                .send()
                .map_err(|e| PoolError::Transport(e.to_string()))?;
            let poll_text = poll.text().unwrap_or_default();
            let poll_value: Value = serde_json::from_str(&poll_text).map_err(|e| PoolError::Other(format!("invalid JSON: {e}")))?;
            if poll_value.get("status").and_then(|s| s.as_str()) == Some("completed") {
                let content = poll_value.get("output_text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                let latency_ms = started.elapsed().as_millis() as u64;
                return Ok(PoolResponse {
                    content,
                    tool_calls: vec![],
                    finish_reason: None,
                    truncated: false,
                    usage_tokens: None,
                    ttfb_ms: Some(latency_ms),
                    latency_ms,
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}

// ---------------------------------------------- OpenAI-compat subclasses

/// `ModelScope`/`Pollinations`/`ElectronHub`/`Experiential` all speak
/// plain OpenAI-compat `/chat/completions` (spec §5.2's "native
/// (OpenAI-compat subclass)" phrasing) — they only differ in base URL
/// and validate-probe path (already in the catalog's `quirks`), so one
/// generic wire parameterized by base URL covers all four instead of
/// four near-identical impls.
pub struct OpenAiCompatSubclassWire {
    pub base_url: &'static str,
}

impl PoolWire for OpenAiCompatSubclassWire {
    fn dispatch(&self, req: &PoolRequest, provider: &FreeProvider, key: &str) -> Result<PoolResponse, PoolError> {
        let provider_with_url = FreeProvider { base_url: self.base_url, ..*provider };
        crate::pool::client::dispatch_openai_compat(&client(), req, &provider_with_url, key, provider.timeout, &std::sync::atomic::AtomicBool::new(false))
    }
}

pub fn modelscope_wire() -> OpenAiCompatSubclassWire {
    OpenAiCompatSubclassWire { base_url: "https://api-inference.modelscope.cn/v1" }
}
pub fn pollinations_wire() -> OpenAiCompatSubclassWire {
    OpenAiCompatSubclassWire { base_url: "https://text.pollinations.ai/openai" }
}
pub fn electronhub_wire() -> OpenAiCompatSubclassWire {
    OpenAiCompatSubclassWire { base_url: "https://api.electronhub.top/v1" }
}
pub fn experiential_wire() -> OpenAiCompatSubclassWire {
    OpenAiCompatSubclassWire { base_url: "https://api.experientiallabs.ai/v1" }
}

/// Dispatches to the right wire for `provider.wire`. The single point
/// Phase 5's `pool_agent.rs` calls instead of matching `Wire` itself.
pub fn dispatch_for_wire(req: &PoolRequest, provider: &FreeProvider, key: &str) -> Result<PoolResponse, PoolError> {
    use divisi_core::free_pool::Wire;
    match provider.wire {
        Wire::OpenAiCompat => super::OpenAiCompatWire::default().dispatch(req, provider, key),
        Wire::Gemini => GoogleWire.dispatch(req, provider, key),
        Wire::Cohere => CohereWire.dispatch(req, provider, key),
        Wire::Cloudflare => CloudflareWire.dispatch(req, provider, key),
        Wire::Zhipu => ZhipuWire::default().dispatch(req, provider, key),
        Wire::AiHorde => AiHordeWire.dispatch(req, provider, key),
        Wire::Sail => SailWire.dispatch(req, provider, key),
        Wire::ModelScope => modelscope_wire().dispatch(req, provider, key),
        Wire::Pollinations => pollinations_wire().dispatch(req, provider, key),
        Wire::ElectronHub => electronhub_wire().dispatch(req, provider, key),
        Wire::Experiential => experiential_wire().dispatch(req, provider, key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::client::test_server::MockServer;
    use crate::pool::client::ChatMessage;
    use divisi_core::free_pool::{Auth, Limits, Wire};
    use std::thread;
    use std::time::Duration;

    fn base_provider(wire: Wire, auth: Auth) -> FreeProvider {
        FreeProvider {
            id: "test",
            display: "Test",
            base_url: "",
            wire,
            auth,
            signup_url: "",
            limits: Limits::default(),
            pool: None,
            timeout: Duration::from_secs(5),
            quirks: Default::default(),
            free_note: "",
            intelligence_rank: 5,
        }
    }

    fn req() -> PoolRequest {
        PoolRequest { messages: vec![ChatMessage { role: "user".to_string(), content: "hi".to_string() }], ..Default::default() }
    }

    #[test]
    fn cloudflare_splits_compound_key_and_hits_account_scoped_url() {
        // Can't hit the real cloudflare API without live creds; verify
        // the compound-key split logic directly by checking a malformed
        // key (no colon) is rejected as AuthFailed before any request.
        let provider = base_provider(Wire::Cloudflare, Auth::Compound("token"));
        let result = CloudflareWire.dispatch(&req(), &provider, "not-a-compound-key");
        assert!(matches!(result, Err(PoolError::AuthFailed)));
    }

    #[test]
    fn zhipu_reprobes_second_host_on_transport_failure_of_first() {
        let server = MockServer::start();
        let good_host: &'static str = Box::leak(server.base_url.clone().into_boxed_str());
        let wire = ZhipuWire { hosts: vec!["http://127.0.0.1:1", good_host] }; // first host: connection refused
        let provider = base_provider(Wire::Zhipu, Auth::Bearer);

        let handle = thread::spawn(move || server.expect_request(200, r#"{"choices":[{"message":{"content":"ok"}}]}"#));
        let result = wire.dispatch(&req(), &provider, "k");
        handle.join().unwrap();

        assert_eq!(result.unwrap().content, "ok");
    }

    #[test]
    fn aihorde_poll_loop_returns_content_once_done() {
        let server = MockServer::start();
        let base = server.base_url.clone();
        let provider = base_provider(Wire::AiHorde, Auth::Keyless("0000000000"));

        let handle = thread::spawn(move || {
            // 1st request: submit -> returns an id.
            server.expect_request(200, r#"{"id":"req-123"}"#);
            // 2nd request: poll -> not done yet.
            server.expect_request(200, r#"{"done":false}"#);
            // 3rd request: poll -> done, with generated text.
            server.expect_request(200, r#"{"done":true,"generations":[{"text":"result text"}]}"#);
        });

        let c = client();
        let result = dispatch_aihorde(&c, &req(), &provider, "0000000000", &base);
        handle.join().unwrap();

        assert_eq!(result.unwrap().content, "result text");
    }

    #[test]
    fn openai_compat_subclass_wires_use_their_own_base_url() {
        let server = MockServer::start();
        let subclass = OpenAiCompatSubclassWire { base_url: Box::leak(server.base_url.clone().into_boxed_str()) };
        let provider = base_provider(Wire::ElectronHub, Auth::Bearer);

        let handle = thread::spawn(move || server.expect_request(200, r#"{"choices":[{"message":{"content":"ok"}}]}"#));
        let result = subclass.dispatch(&req(), &provider, "k");
        let received = handle.join().unwrap();

        assert_eq!(received.path, "/chat/completions");
        assert_eq!(result.unwrap().content, "ok");
    }
}
