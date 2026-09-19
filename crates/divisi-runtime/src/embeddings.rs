//! Best-effort text embeddings for semantic memory search (D1). A single
//! real integration (OpenAI's `/v1/embeddings`) rather than a pluggable
//! provider abstraction — the smallest thing that makes semantic search
//! real rather than faked, matching `qdrant_backend.rs`'s own stated
//! aversion to fabricated capabilities (a hash-based pseudo-embedding
//! pretending to be semantic would be exactly that).
//!
//! Configuration: `SINGLE_EMBEDDINGS_MODEL` (defaults to
//! `text-embedding-3-small`), API key read from the secret store under
//! `embeddings:api_key` — set it with `single secret set embeddings:api_key
//! <key>`, the same keychain-backed mechanism provider API keys already
//! use (see `divisi-core::providers`). Both memory search's semantic path
//! and the auto-embed-on-write step treat a missing key as "not
//! configured" and fall back to substring search / skip silently, never as
//! an error that should propagate to the caller.
//!
//! **Not independently verified against a real OpenAI account in this
//! change** — the request/response shape matches OpenAI's documented
//! `/v1/embeddings` API, but (unlike `qdrant_backend.rs`'s REST shape,
//! captured from real `curl` calls against a running Qdrant) no live round
//! trip was made here. Flagged explicitly rather than silently claiming
//! the same verification level; the round-trip test below exercises it
//! for real whenever a key happens to be configured, and skips cleanly
//! otherwise.

use anyhow::{bail, Context, Result};
use serde_json::json;

const DEFAULT_MODEL: &str = "text-embedding-3-small";
const SECRET_NAME: &str = "embeddings:api_key";

pub fn is_configured() -> bool {
    resolve_api_key().is_ok_and(|k| k.is_some())
}

fn resolve_api_key() -> Result<Option<String>> {
    let store = divisi_core::secrets::SecretTool;
    divisi_core::secrets::SecretStore::get(&store, SECRET_NAME)
}

fn model() -> String {
    std::env::var("SINGLE_EMBEDDINGS_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

/// Embeds `text`. Errors (rather than silently returning an empty vector)
/// if no key is configured — callers wanting a silent no-op should check
/// `is_configured()` first, exactly as `MemoryStore`'s auto-embed step and
/// `MemorySearchSemantic`'s fallback do.
pub fn embed(text: &str) -> Result<Vec<f32>> {
    let Some(key) = resolve_api_key()? else {
        bail!("embeddings not configured: set an API key with `single secret set {SECRET_NAME} <key>`");
    };
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post("https://api.openai.com/v1/embeddings")
        .bearer_auth(key)
        .json(&json!({ "model": model(), "input": text }))
        .send()
        .context("calling the embeddings API")?;
    if !resp.status().is_success() {
        bail!("embeddings API returned {}: {}", resp.status(), resp.text().unwrap_or_default());
    }
    let body: serde_json::Value = resp.json().context("parsing embeddings API response")?;
    let vector = body["data"][0]["embedding"]
        .as_array()
        .context("embeddings API response missing data[0].embedding")?
        .iter()
        .map(|v| v.as_f64().unwrap_or(0.0) as f32)
        .collect();
    Ok(vector)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_defaults_when_env_var_unset() {
        std::env::remove_var("SINGLE_EMBEDDINGS_MODEL");
        assert_eq!(model(), DEFAULT_MODEL);
        std::env::set_var("SINGLE_EMBEDDINGS_MODEL", "custom-model");
        assert_eq!(model(), "custom-model");
        std::env::remove_var("SINGLE_EMBEDDINGS_MODEL");
    }

    /// Only exercises a real API call when this machine already has a key
    /// configured (via `single secret set embeddings:api_key <key>`) —
    /// never writes/deletes a secret itself, to avoid touching the real
    /// OS keychain from a test run. Skips cleanly otherwise, same
    /// discipline as `qdrant_backend`/`redis_backend`'s live-service tests.
    #[test]
    fn embed_round_trips_against_a_real_key_if_one_is_configured() {
        if !is_configured() {
            eprintln!("skipping embeddings test: no key configured (single secret set {SECRET_NAME} <key>)");
            return;
        }
        let vector = embed("hello world").unwrap();
        assert!(!vector.is_empty());
    }
}
