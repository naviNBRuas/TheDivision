//! The `single-pool` agent adapter — spec §7. Implements the same run
//! contract `task::execute` expects from a shelled CLI agent, but never
//! shells anything: it picks a `(platform, model, key_id)` via the
//! bandit, dispatches over HTTP through `pool::client`, and records
//! ledger/cooldown/bandit state — all against the runtime's own SQLite
//! connection, which is why this lives in `single-runtime` rather than as
//! a normal `AgentAdapter` impl in `single-agent-sdk` (that trait's
//! `run_prompt` has no `&Connection` parameter — see `task.rs`'s
//! `execute()`, which special-cases `agent == "single-pool"` before ever
//! reaching the adapter dispatch, per Task 14).
//!
//! **Model selection seam**: the vendored catalog (Part A) has no
//! per-provider model list — there is no live model-catalog feed this
//! iteration (spec §2 non-goal, §17). Each provider is therefore treated
//! as offering exactly one nominal "model" identified by its own
//! `provider.id` (matching what `pool::client::build_body` already sends
//! as the `model` field). A future live catalog would replace this with
//! real per-provider model lists without changing `execute`'s shape.

use crate::pool::client::{ChatMessage, PoolError, PoolRequest, PoolResponse};
use crate::pool::handoff::HandoffStore;
use crate::pool::{bandit, cooldown, handoff, ledger};
use anyhow::Result;
use rusqlite::Connection;
use single_core::free_pool::FreeProvider;
use single_protocol::RunOutcome;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum PoolAgentOutcome {
    Ok(PoolResponse),
    /// Every eligible `(platform, model, key_id)` candidate is currently
    /// benched or lacks a valid key. **Not** a plain error — Phase 6's
    /// `waiting_on_capacity` auto-continue consumes this shape directly.
    Exhausted { earliest_recovery_ms: i64 },
}

/// A dispatch call, injected so `execute`'s tests never touch the
/// network (Task 13's explicit requirement — fake dispatcher, no real
/// HTTP). Production callers pass `pool::client::native::dispatch_for_wire`.
pub type DispatchFn<'a> = dyn Fn(&PoolRequest, &FreeProvider, &str) -> Result<PoolResponse, PoolError> + 'a;

/// Resolves a `(platform, key_id)` pair to its actual secret value.
/// Injected for the same testability reason as `DispatchFn` — production
/// callers pass a closure over `single_core::secrets::SecretStore::get`.
pub type SecretFn<'a> = dyn Fn(&str, &str) -> Option<String> + 'a;

/// Core pool-agent loop (spec §7's numbered steps 1-8, minus the
/// `handoff::inject`/prompt-preamble step which the caller does before
/// calling this — see `run_as_task` for the production wiring):
///
/// 1. pick `(platform, model, key_id)` via the bandit, excluding anything
///    already attempted in this call,
/// 2. inject a handoff message if the picked provider differs from the
///    session's last one,
/// 3. acquire a ledger lease,
/// 4. dispatch,
/// 5. success -> record usage + a positive bandit outcome, return `Ok`,
/// 6. rate-limited/5xx -> bench the candidate, record a negative bandit
///    outcome, try the next candidate,
/// 7. every candidate exhausted -> `Exhausted { earliest_recovery_ms }`
///    (the earliest `until_ms` seen across every benched attempt).
///
/// Signature matches the plan's specified interface exactly (same
/// rationale as `ledger::admit`/`bandit::record_outcome`).
#[allow(clippy::too_many_arguments)]
pub fn execute(
    conn: &Connection,
    prompt: &str,
    strategy: &bandit::Strategy,
    candidates: &[(String, String, String)],
    session_key: Option<&str>,
    handoff_store: &HandoffStore,
    resolve_secret: &SecretFn,
    dispatch: &DispatchFn,
) -> Result<PoolAgentOutcome> {
    let key = handoff::session_key(session_key, prompt);
    let mut attempted: Vec<(String, String, String)> = Vec::new();
    let mut earliest_recovery_ms: Option<i64> = None;

    loop {
        let now = ledger::now_ms();
        let remaining: Vec<(String, String, String)> = candidates.iter().filter(|c| !attempted.contains(c)).cloned().collect();

        let Some((platform, model, key_id)) = bandit::pick(conn, strategy, &remaining, now) else {
            break;
        };

        let Some(provider) = single_core::free_pool::by_id(&platform) else {
            attempted.push((platform, model, key_id));
            continue;
        };

        let Some(secret) = resolve_secret(&platform, &key_id) else {
            // No usable secret for this candidate -- skip it without
            // benching (a missing key isn't a provider health signal).
            attempted.push((platform.clone(), model.clone(), key_id.clone()));
            continue;
        };

        let est_tokens = estimate_tokens(prompt);
        let lease = ledger::acquire_lease(conn, &platform, &model, &key_id, est_tokens)?;

        let mut messages = vec![ChatMessage { role: "user".to_string(), content: prompt.to_string() }];
        handoff::inject(handoff_store, &key, &platform, &model, &mut messages);
        let req = PoolRequest { messages, ..Default::default() };

        let started = Instant::now();
        let result = dispatch(&req, provider, &secret);
        ledger::release_lease(conn, &lease.lease_id)?;

        match result {
            Ok(mut resp) => {
                resp.latency_ms = resp.latency_ms.max(started.elapsed().as_millis() as u64);
                ledger::record(conn, &platform, &model, &key_id, ledger::UsageKind::Request, 1, now)?;
                if let Some(tokens) = resp.usage_tokens {
                    ledger::record(conn, &platform, &model, &key_id, ledger::UsageKind::Tokens, tokens, now)?;
                }
                cooldown::clear_hits(conn, &platform, &model, &key_id)?;
                bandit::record_outcome(conn, &platform, &model, &key_id, true, resp.latency_ms, resp.usage_tokens.unwrap_or(0), now)?;
                // Live-verification finding: a real successful dispatch
                // never updated `pool_provider_keys` at all, so
                // `single provider key-status` kept reporting "keyed,
                // unvalidated" forever for a key that had already served
                // real, successful requests -- only the one-time
                // best-effort probe at `add-free` time ever called
                // `mark_validated`. Real usage is strictly stronger
                // evidence than that probe, so record it here too.
                single_core::pool_keys::mark_validated(conn, &platform, &key_id, true).ok();
                return Ok(PoolAgentOutcome::Ok(resp));
            }
            Err(err) => {
                let latency_ms = started.elapsed().as_millis() as u64;
                bandit::record_outcome(conn, &platform, &model, &key_id, false, latency_ms, 0, now)?;

                let bench_kind = match &err {
                    PoolError::RateLimited { retry: Some(d) } => cooldown::BenchKind::Authoritative(*d),
                    PoolError::RateLimited { retry: None } => cooldown::BenchKind::Escalated,
                    PoolError::PaymentRequired => cooldown::BenchKind::PaymentRequired,
                    PoolError::TierGate => cooldown::BenchKind::TierGate,
                    PoolError::AuthFailed => {
                        // A real auth rejection is authoritative evidence
                        // the key itself is bad, not just rate-limited or
                        // unlucky -- reflect that in key-status too, same
                        // reasoning as the success-path mark_validated
                        // call above.
                        single_core::pool_keys::mark_validated(conn, &platform, &key_id, false).ok();
                        cooldown::BenchKind::AuthBenched
                    }
                    PoolError::Transport(_) => cooldown::BenchKind::Local,
                    // HedgeAbort is explicitly not a health signal (spec
                    // §6.6) -- no bench call at all for it.
                    PoolError::HedgeAbort => {
                        attempted.push((platform, model, key_id));
                        continue;
                    }
                    PoolError::ToolsUnsupported | PoolError::Other(_) => cooldown::BenchKind::Local,
                };
                let until_ms = cooldown::bench(conn, &platform, &model, &key_id, bench_kind, now)?;
                earliest_recovery_ms = Some(earliest_recovery_ms.map_or(until_ms, |e: i64| e.min(until_ms)));
                attempted.push((platform, model, key_id));
            }
        }
    }

    Ok(PoolAgentOutcome::Exhausted { earliest_recovery_ms: earliest_recovery_ms.unwrap_or(ledger::now_ms()) })
}

/// `chars/4` estimate, matching E27's `parse_or_estimate_tokens` seam
/// (spec §6.1) — refined post-response from real usage where reported.
fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64 / 4).max(1)
}

/// Every keyed, non-disabled pool provider becomes one candidate model
/// (see the model-selection seam note at the top of this file).
///
/// Deliberately does **not** require `key.valid` — that flag only ever
/// gets set by `add-free`'s validation probe, which needs a provider's
/// `validate_url` quirk to exist at all. A provider like `groq` has none
/// (spec §5.2's table lists no validate path for it), so its key would
/// sit at `valid = false` forever and never become a candidate even
/// though it's perfectly usable — confirmed live: this filter used to
/// include `k.valid` and silently produced zero candidates (an instant,
/// no-network "Exhausted") for every unvalidated-but-real key. A bad key
/// still gets caught for real, just one dispatch attempt later: a 401
/// benches it via `AuthBenched` in `execute`'s error-mapping match, same
/// as any other real dispatch failure.
pub fn candidates_from_keys(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let keys = single_core::pool_keys::list(conn, None)?;
    Ok(keys
        .into_iter()
        .filter(|k| !k.disabled)
        .filter_map(|k| single_core::free_pool::by_id(&k.platform).map(|p| (k.platform.clone(), p.id.to_string(), k.key_id.clone())))
        .collect())
}

/// One process-wide handoff store, since a session's "last provider/model"
/// state needs to persist across separate `single task run` invocations
/// within the daemon's lifetime, not just within one `execute` call.
static HANDOFF_STORE: std::sync::OnceLock<HandoffStore> = std::sync::OnceLock::new();

pub fn global_handoff_store() -> &'static HandoffStore {
    HANDOFF_STORE.get_or_init(HandoffStore::default)
}

/// Production entry point for `task::execute`'s `agent == "single-pool"`
/// special case (Task 14): builds the real candidate list, resolves
/// secrets via the OS keychain, dispatches over real HTTP, and maps the
/// result onto the same `RunOutcome` shape a shelled CLI agent returns —
/// `Exhausted` becomes a failed outcome whose stderr matches
/// `single_core::ratelimit::looks_like_rate_limit` (spec: "maps to the
/// task's existing rate-limited terminal shape" until Phase 6's
/// goal-level `waiting_on_capacity` lands).
pub fn run_as_task(conn: &Connection, prompt: &str, timeout: Duration, session_key: Option<&str>, handoff_store: &HandoffStore) -> Result<RunOutcome> {
    let candidates = candidates_from_keys(conn)?;
    let strategy = bandit::Strategy::Balanced;
    let started = Instant::now();

    // E29: any `{{REDACTED:<session>:N}}` alias `redact::scan_and_replace`
    // left in the prompt gets resolved back to its real value here, at
    // the last possible moment before the outbound HTTP call — the agent
    // upstream of this point never sees the plaintext, only the wire
    // request built from `resolved_prompt` does. A prompt with no alias
    // tokens passes through unchanged; a dangling unresolvable alias is a
    // real error (see `redact::resolve`'s doc comment).
    single_core::redact::ensure_schema(conn)?;
    let redact_store = single_core::redact::RedactStore { conn };
    let secret_store = single_core::secrets::SecretTool;
    let resolved_prompt = single_core::redact::resolve(&redact_store, &secret_store, prompt)?;

    let resolve_secret = |platform: &str, key_id: &str| -> Option<String> {
        use single_core::secrets::{SecretStore, SecretTool};
        let name = single_core::pool_keys::secret_name(platform, key_id);
        SecretStore::get(&SecretTool, &name).ok().flatten()
    };
    let dispatch = |req: &PoolRequest, provider: &FreeProvider, key: &str| crate::pool::client::native::dispatch_for_wire(req, provider, key);

    let _ = timeout; // per-attempt timeouts come from each provider's own FreeProvider.timeout; an overall task timeout is enforced by task::execute's existing timeout machinery around this call.

    let outcome = execute(conn, &resolved_prompt, &strategy, &candidates, session_key, handoff_store, &resolve_secret, &dispatch)?;

    Ok(match outcome {
        PoolAgentOutcome::Ok(resp) => RunOutcome {
            success: true,
            stdout: resp.content,
            stderr: String::new(),
            exit_code: Some(0),
            timed_out: false,
            cancelled: false,
            duration_ms: started.elapsed().as_millis(),
            usage: None, // real usage wiring (TokenUsage shape) lands with Phase 6's coordinator integration.
        },
        PoolAgentOutcome::Exhausted { earliest_recovery_ms } => RunOutcome {
            success: false,
            stdout: String::new(),
            stderr: format!(
                "single-pool: rate limited — every keyed provider is exhausted or benched, earliest recovery at {earliest_recovery_ms}"
            ),
            exit_code: Some(1),
            timed_out: false,
            cancelled: false,
            duration_ms: started.elapsed().as_millis(),
            usage: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::ensure_pool_schema;
    use std::cell::RefCell;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_pool_schema(&conn).unwrap();
        conn
    }

    fn seed_key(conn: &Connection, platform: &str, key_id: &str) {
        single_core::pool_keys::add(conn, platform, key_id).unwrap();
        single_core::pool_keys::mark_validated(conn, platform, key_id, true).unwrap();
    }

    #[test]
    fn candidates_from_keys_includes_an_unvalidated_key() {
        // Regression test: a provider with no `validate_url` quirk (e.g.
        // groq) can never have its key marked `valid` by `add-free`'s
        // probe, since there's nothing to probe. Confirmed live against
        // the real daemon: the old filter (`!disabled && valid`) silently
        // produced zero candidates for a perfectly real, working key.
        let conn = test_conn();
        single_core::pool_keys::add(&conn, "groq", "default").unwrap();
        // deliberately NOT calling mark_validated -- valid stays false.
        let candidates = candidates_from_keys(&conn).unwrap();
        assert!(candidates.iter().any(|(p, _, k)| p == "groq" && k == "default"), "{candidates:?}");
    }

    #[test]
    fn candidates_from_keys_excludes_a_disabled_key() {
        let conn = test_conn();
        single_core::pool_keys::add(&conn, "groq", "default").unwrap();
        single_core::pool_keys::disable(&conn, "groq", "default").unwrap();
        let candidates = candidates_from_keys(&conn).unwrap();
        assert!(candidates.is_empty(), "{candidates:?}");
    }

    fn always_resolve(_p: &str, _k: &str) -> Option<String> {
        Some("secret".to_string())
    }

    /// E29: proves the exact sequence `run_as_task` performs — resolve
    /// then dispatch — never lets a `{{REDACTED:...}}` alias reach the
    /// wire, and does deliver the real secret value to whatever the
    /// dispatch closure represents (a real provider's HTTP call in
    /// production). `run_as_task` itself hardcodes the real network
    /// dispatcher, so this replicates its resolve-then-execute sequence
    /// with an injectable one instead of adding network I/O to a unit test.
    #[test]
    fn resolve_then_dispatch_never_leaks_the_alias_and_delivers_the_real_secret() {
        let conn = test_conn();
        single_core::redact::ensure_schema(&conn).unwrap();
        seed_key(&conn, "groq", "default");

        struct FakeKeychain(RefCell<std::collections::HashMap<String, String>>);
        impl single_core::secrets::SecretStore for FakeKeychain {
            fn set(&self, name: &str, value: &str) -> anyhow::Result<()> {
                self.0.borrow_mut().insert(name.to_string(), value.to_string());
                Ok(())
            }
            fn get(&self, name: &str) -> anyhow::Result<Option<String>> {
                Ok(self.0.borrow().get(name).cloned())
            }
            fn delete(&self, name: &str) -> anyhow::Result<bool> {
                Ok(self.0.borrow_mut().remove(name).is_some())
            }
            fn list(&self) -> anyhow::Result<Vec<String>> {
                Ok(self.0.borrow().keys().cloned().collect())
            }
        }
        let keychain = FakeKeychain(RefCell::new(std::collections::HashMap::new()));

        let redact_store = single_core::redact::RedactStore { conn: &conn };
        let (redacted_prompt, aliases) = single_core::redact::scan_and_replace(
            &redact_store,
            &keychain,
            "sess1",
            "use sk-abcdEFGH1234567890abcdEFGH1234567890abcd to call it",
        )
        .unwrap();
        assert_eq!(aliases.len(), 1);
        assert!(redacted_prompt.contains("{{REDACTED:sess1:"));

        // exactly what `run_as_task` does before calling `execute`.
        let resolved_prompt = single_core::redact::resolve(&redact_store, &keychain, &redacted_prompt).unwrap();
        assert!(resolved_prompt.contains("sk-abcdEFGH1234567890abcdEFGH1234567890abcd"));
        assert!(!resolved_prompt.contains("REDACTED"));

        let seen: RefCell<String> = RefCell::new(String::new());
        let handoff_store = HandoffStore::default();
        let dispatch = |req: &PoolRequest, _: &FreeProvider, _: &str| {
            *seen.borrow_mut() = req.messages[0].content.clone();
            Ok(PoolResponse { content: "ok".to_string(), tool_calls: vec![], finish_reason: None, truncated: false, usage_tokens: None, ttfb_ms: None, latency_ms: 1 })
        };
        let candidates = vec![("groq".to_string(), "groq".to_string(), "default".to_string())];
        let _ = execute(&conn, &resolved_prompt, &bandit::Strategy::Balanced, &candidates, None, &handoff_store, &always_resolve, &dispatch).unwrap();

        let dispatched = seen.borrow().clone();
        assert!(dispatched.contains("sk-abcdEFGH1234567890abcdEFGH1234567890abcd"));
        assert!(!dispatched.contains("REDACTED"));
    }

    #[test]
    fn execute_picks_and_dispatches_via_bandit() {
        let conn = test_conn();
        seed_key(&conn, "groq", "default");
        let candidates = vec![("groq".to_string(), "groq".to_string(), "default".to_string())];
        let handoff_store = HandoffStore::default();

        let dispatch = |_: &PoolRequest, _: &FreeProvider, _: &str| {
            Ok(PoolResponse { content: "hello".to_string(), tool_calls: vec![], finish_reason: None, truncated: false, usage_tokens: Some(10), ttfb_ms: Some(5), latency_ms: 5 })
        };

        let outcome = execute(&conn, "hi", &bandit::Strategy::Balanced, &candidates, None, &handoff_store, &always_resolve, &dispatch).unwrap();
        match outcome {
            PoolAgentOutcome::Ok(resp) => assert_eq!(resp.content, "hello"),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn execute_injects_handoff_on_model_switch() {
        let conn = test_conn();
        seed_key(&conn, "groq", "default");
        seed_key(&conn, "cerebras", "default");
        let handoff_store = HandoffStore::default();
        handoff::record_summary(&handoff_store, &handoff::session_key(Some("sess1"), "hi"), "prior summary".to_string(), "groq", "groq");

        let seen_content: RefCell<Option<String>> = RefCell::new(None);
        let dispatch = |req: &PoolRequest, _: &FreeProvider, _: &str| {
            *seen_content.borrow_mut() = Some(req.messages[0].content.clone());
            Ok(PoolResponse { content: "ok".to_string(), tool_calls: vec![], finish_reason: None, truncated: false, usage_tokens: None, ttfb_ms: None, latency_ms: 1 })
        };

        // Force cerebras to be picked by benching groq first.
        cooldown::bench(&conn, "groq", "groq", "default", cooldown::BenchKind::Escalated, ledger::now_ms()).unwrap();

        let candidates = vec![("groq".to_string(), "groq".to_string(), "default".to_string()), ("cerebras".to_string(), "cerebras".to_string(), "default".to_string())];
        let _ = execute(&conn, "hi", &bandit::Strategy::Priority, &candidates, Some("sess1"), &handoff_store, &always_resolve, &dispatch).unwrap();

        assert!(seen_content.borrow().as_ref().unwrap().starts_with("SingleCLI context handoff:"));
    }

    #[test]
    fn execute_records_outcome_on_success() {
        let conn = test_conn();
        seed_key(&conn, "groq", "default");
        let handoff_store = HandoffStore::default();
        let dispatch = |_: &PoolRequest, _: &FreeProvider, _: &str| {
            Ok(PoolResponse { content: "ok".to_string(), tool_calls: vec![], finish_reason: None, truncated: false, usage_tokens: Some(7), ttfb_ms: None, latency_ms: 1 })
        };
        let candidates = vec![("groq".to_string(), "groq".to_string(), "default".to_string())];
        let now = ledger::now_ms();
        execute(&conn, "hi", &bandit::Strategy::Balanced, &candidates, None, &handoff_store, &always_resolve, &dispatch).unwrap();

        let (alpha, beta) = bandit::posterior(&conn, "groq", "groq", "default", now + 1000);
        assert!(alpha > 1.0);
        assert_eq!(beta, 1.0);
    }

    #[test]
    fn execute_marks_the_key_validated_on_a_real_successful_dispatch() {
        // Regression test: a real successful dispatch used to update the
        // ledger and bandit but never `pool_provider_keys`, so `single
        // provider key-status` kept reporting "keyed, unvalidated"
        // forever even for keys that had already served real traffic.
        let conn = test_conn();
        seed_key(&conn, "groq", "default");

        let handoff_store = HandoffStore::default();
        let dispatch = |_: &PoolRequest, _: &FreeProvider, _: &str| {
            Ok(PoolResponse { content: "ok".to_string(), tool_calls: vec![], finish_reason: None, truncated: false, usage_tokens: None, ttfb_ms: None, latency_ms: 1 })
        };
        let candidates = vec![("groq".to_string(), "groq".to_string(), "default".to_string())];
        execute(&conn, "hi", &bandit::Strategy::Balanced, &candidates, None, &handoff_store, &always_resolve, &dispatch).unwrap();

        let key = single_core::pool_keys::list(&conn, None).unwrap().into_iter().find(|k| k.key_id == "default").unwrap();
        assert!(key.valid);
        assert!(key.last_validated_at.is_some());
    }

    #[test]
    fn execute_marks_the_key_invalid_on_a_real_auth_failure() {
        let conn = test_conn();
        seed_key(&conn, "groq", "default");
        let handoff_store = HandoffStore::default();
        let dispatch = |_: &PoolRequest, _: &FreeProvider, _: &str| Err(PoolError::AuthFailed);
        let candidates = vec![("groq".to_string(), "groq".to_string(), "default".to_string())];
        let _ = execute(&conn, "hi", &bandit::Strategy::Balanced, &candidates, None, &handoff_store, &always_resolve, &dispatch).unwrap();

        let key = single_core::pool_keys::list(&conn, None).unwrap().into_iter().find(|k| k.key_id == "default").unwrap();
        assert!(!key.valid);
        assert!(key.last_validated_at.is_some());
    }

    #[test]
    fn execute_benches_and_retries_next_candidate_on_429() {
        let conn = test_conn();
        seed_key(&conn, "groq", "default");
        seed_key(&conn, "cerebras", "default");
        let handoff_store = HandoffStore::default();

        let dispatch = |_: &PoolRequest, provider: &FreeProvider, _: &str| {
            if provider.id == "groq" {
                Err(PoolError::RateLimited { retry: None })
            } else {
                Ok(PoolResponse { content: "from cerebras".to_string(), tool_calls: vec![], finish_reason: None, truncated: false, usage_tokens: None, ttfb_ms: None, latency_ms: 1 })
            }
        };

        let candidates = vec![("groq".to_string(), "groq".to_string(), "default".to_string()), ("cerebras".to_string(), "cerebras".to_string(), "default".to_string())];
        let outcome = execute(&conn, "hi", &bandit::Strategy::Priority, &candidates, None, &handoff_store, &always_resolve, &dispatch).unwrap();

        match outcome {
            PoolAgentOutcome::Ok(resp) => assert_eq!(resp.content, "from cerebras"),
            other => panic!("expected Ok from the second candidate, got {other:?}"),
        }
        assert!(cooldown::is_benched(&conn, "groq", "groq", "default", ledger::now_ms()).unwrap().is_some());
    }

    #[test]
    fn execute_returns_exhausted_when_all_candidates_fail() {
        let conn = test_conn();
        seed_key(&conn, "groq", "default");
        let handoff_store = HandoffStore::default();
        let dispatch = |_: &PoolRequest, _: &FreeProvider, _: &str| Err(PoolError::RateLimited { retry: Some(Duration::from_secs(30)) });
        let candidates = vec![("groq".to_string(), "groq".to_string(), "default".to_string())];

        let outcome = execute(&conn, "hi", &bandit::Strategy::Balanced, &candidates, None, &handoff_store, &always_resolve, &dispatch).unwrap();
        match outcome {
            PoolAgentOutcome::Exhausted { earliest_recovery_ms } => assert!(earliest_recovery_ms > ledger::now_ms()),
            other => panic!("expected Exhausted, got {other:?}"),
        }
    }
}
