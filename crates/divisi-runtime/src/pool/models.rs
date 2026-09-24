//! Real model ids for the pool's OpenAI-compatible providers.
//!
//! Live-verification finding (2026-09-24): every OpenAI-compatible request sent the provider's own
//! id as the model (`"model": "groq"`), which no provider serves, so ~20 keyed providers were benched
//! on their first call and the pool ran almost entirely on Cloudflare's fixed model. Models are now
//! discovered from each provider's `GET /models`, ranked by what their names say about chat ability
//! and size, and cached for a day; the top few become separate bandit candidates so the pool learns
//! which one actually answers well. Nothing is hardcoded per provider.

use crate::pool::client::apply_auth;
use anyhow::{Context, Result};
use divisi_core::free_pool::{FreeProvider, Wire};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::time::Duration;

/// How long a discovered list is trusted.
const FRESH_MS: i64 = 24 * 3600 * 1000;
/// How long before a provider whose `/models` failed is asked again.
const RETRY_MS: i64 = 3600 * 1000;
/// Models per provider offered to the bandit.
pub const PER_PROVIDER: usize = 2;

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS pool_models (platform TEXT PRIMARY KEY, models TEXT NOT NULL, fetched_ms INTEGER NOT NULL)",
        (),
    )?;
    Ok(())
}

/// Ids that are clearly not chat models.
fn not_chat(id: &str) -> bool {
    const SKIP: [&str; 22] = [
        "embed", "whisper", "tts", "audio", "speech", "transcri", "image", "dall", "flux", "stable-diffusion", "sdxl",
        "guard", "moderation", "rerank", "ocr", "vision-preview", "clip", "bge", "e5-", "safety", "realtime", "search",
    ];
    let l = id.to_lowercase();
    SKIP.iter().any(|s| l.contains(s))
}

/// Parameter count in billions if the id states one (`llama-3.3-70b`, `gpt-oss:120b`, `8x7b`).
fn size_b(id: &str) -> Option<f64> {
    let l = id.to_lowercase();
    let bytes = l.as_bytes();
    let mut best: Option<f64> = None;
    for (i, _) in l.match_indices('b') {
        let mut j = i;
        while j > 0 && (bytes[j - 1].is_ascii_digit() || bytes[j - 1] == b'.') {
            j -= 1;
        }
        if j == i || (i + 1 < bytes.len() && bytes[i + 1].is_ascii_alphanumeric()) {
            continue;
        }
        let mut n: f64 = l[j..i].parse().unwrap_or(0.0);
        if j >= 2 && bytes[j - 1] == b'x' {
            let mut k = j - 1;
            while k > 0 && bytes[k - 1].is_ascii_digit() {
                k -= 1;
            }
            n *= l[k..j - 1].parse::<f64>().unwrap_or(1.0);
        }
        best = Some(best.map_or(n, |b: f64| b.max(n)));
    }
    best
}

/// Higher is a better agent model. Name heuristics only: size, coding/agentic families, and
/// penalties for small or preview variants.
pub fn score(id: &str) -> f64 {
    let l = id.to_lowercase();
    let mut s = 0.0;
    if let Some(b) = size_b(&l) {
        s += (b.min(400.0) / 400.0) * 4.0 + if b < 14.0 { -2.0 } else { 0.0 };
    }
    for (k, w) in [
        ("coder", 2.0), ("devstral", 2.5), ("codestral", 1.5), ("gpt-oss", 2.0), ("qwen3", 2.0), ("deepseek", 2.0),
        ("kimi", 2.0), ("glm-4", 1.5), ("glm-5", 2.0), ("llama-4", 1.5), ("llama-3.3", 1.0), ("mistral-large", 2.0),
        ("mistral-medium", 1.5), ("gemini-2.5-pro", 2.5), ("gemini-3", 2.5), ("gemini-2.5-flash", 1.5), ("command-a", 1.5),
        ("large", 1.0), ("pro", 0.5), ("instruct", 0.3), ("latest", 0.2),
    ] {
        if l.contains(k) {
            s += w;
        }
    }
    for (k, w) in [("mini", 1.5), ("small", 1.5), ("tiny", 3.0), ("nano", 3.0), ("lite", 1.5), ("preview", 0.3), ("-exp", 0.3), ("distill", 1.0)] {
        if l.contains(k) {
            s -= w;
        }
    }
    s
}

/// Chat model ids from a `/models` body, best first. OpenRouter-style catalogues only offer their
/// `:free` variants.
pub fn rank(body: &Value, platform: &str) -> Vec<String> {
    let list = body.get("data").or_else(|| body.get("models")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut ids: Vec<String> = list
        .iter()
        .filter_map(|m| m.get("id").or_else(|| m.get("name")).and_then(|v| v.as_str()))
        .map(|id| id.strip_prefix("models/").unwrap_or(id).to_string())
        .filter(|id| !not_chat(id))
        .collect();
    let free_marked = ids.iter().any(|id| id.ends_with(":free"));
    if platform == "openrouter" || free_marked {
        ids.retain(|id| id.ends_with(":free"));
    }
    ids.sort_by(|a, b| score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal).then(a.len().cmp(&b.len())));
    ids.dedup();
    ids
}

fn discover(provider: &FreeProvider, key: &str) -> Result<Vec<String>> {
    let url = format!("{}/models", provider.base_url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::new();
    let resp = apply_auth(client.get(&url).timeout(Duration::from_secs(15)), provider.auth, key).send().context("listing models")?;
    let status = resp.status();
    let body: Value = resp.json().context("reading the model list")?;
    anyhow::ensure!(status.is_success(), "model list returned {status}");
    Ok(rank(&body, provider.id))
}

/// Whether this provider's model is chosen by discovery (the native wires pick their own).
pub fn discoverable(provider: &FreeProvider) -> bool {
    provider.wire == Wire::OpenAiCompat && !provider.base_url.is_empty()
}

/// The best few models for `provider`, from the cache or a fresh `/models` call. Empty when the
/// provider can't list models (the caller then keeps the provider's nominal id).
pub fn best(conn: &Connection, provider: &FreeProvider, key: &str, now_ms: i64) -> Vec<String> {
    if ensure_schema(conn).is_err() {
        return Vec::new();
    }
    let cached: Option<(String, i64)> = conn
        .query_row("SELECT models, fetched_ms FROM pool_models WHERE platform = ?1", [provider.id], |r| Ok((r.get(0)?, r.get(1)?)))
        .ok();
    if let Some((models, at)) = &cached {
        let list: Vec<String> = serde_json::from_str(models).unwrap_or_default();
        let ttl = if list.is_empty() { RETRY_MS } else { FRESH_MS };
        if now_ms - at < ttl {
            return list;
        }
    }
    let list: Vec<String> = discover(provider, key).map(|l| l.into_iter().take(PER_PROVIDER + 2).collect()).unwrap_or_default();
    let _ = conn.execute(
        "INSERT INTO pool_models (platform, models, fetched_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT(platform) DO UPDATE SET models = excluded.models, fetched_ms = excluded.fetched_ms",
        params![provider.id, serde_json::to_string(&list).unwrap_or_default(), now_ms],
    );
    list
}

/// Replaces each discoverable provider's nominal candidate with its best real models.
pub fn expand(
    conn: &Connection,
    candidates: Vec<(String, String, String)>,
    resolve_secret: &dyn Fn(&str, &str) -> Option<String>,
    now_ms: i64,
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (platform, model, key_id) in candidates {
        let Some(provider) = divisi_core::free_pool::by_id(&platform).filter(|p| discoverable(p)) else {
            out.push((platform, model, key_id));
            continue;
        };
        let models = resolve_secret(&platform, &key_id).map(|k| best(conn, provider, &k, now_ms)).unwrap_or_default();
        if models.is_empty() {
            out.push((platform, model, key_id));
        } else {
            out.extend(models.into_iter().take(PER_PROVIDER).map(|m| (platform.clone(), m, key_id.clone())));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ranks_big_agentic_models_first_and_drops_non_chat_ones() {
        let body = json!({"data": [
            {"id": "llama-3.1-8b-instant"}, {"id": "whisper-large-v3"}, {"id": "openai/gpt-oss-120b"},
            {"id": "llama-3.3-70b-versatile"}, {"id": "meta-llama/llama-guard-4-12b"}, {"id": "qwen/qwen3-32b"}
        ]});
        let r = rank(&body, "groq");
        assert_eq!(r[0], "openai/gpt-oss-120b");
        assert!(!r.iter().any(|m| m.contains("whisper") || m.contains("guard")));
        assert_eq!(r.last().unwrap(), "llama-3.1-8b-instant");
    }

    #[test]
    fn openrouter_only_offers_free_variants() {
        let body = json!({"data": [{"id": "openai/gpt-5"}, {"id": "qwen/qwen3-coder:free"}, {"id": "meta-llama/llama-3.3-70b-instruct:free"}]});
        let r = rank(&body, "openrouter");
        assert_eq!(r, vec!["qwen/qwen3-coder:free".to_string(), "meta-llama/llama-3.3-70b-instruct:free".to_string()]);
    }

    #[test]
    fn gemini_style_names_are_unprefixed() {
        let body = json!({"models": [{"name": "models/gemini-2.5-flash"}, {"name": "models/text-embedding-004"}]});
        assert_eq!(rank(&body, "google"), vec!["gemini-2.5-flash".to_string()]);
    }

    #[test]
    fn sizes_are_read_from_ids() {
        assert_eq!(size_b("llama-3.3-70b-versatile"), Some(70.0));
        assert_eq!(size_b("gpt-oss:120b"), Some(120.0));
        assert_eq!(size_b("mixtral-8x7b"), Some(56.0));
        assert_eq!(size_b("gpt-4o"), None);
    }

    #[test]
    fn a_cached_list_is_reused_and_a_failed_lookup_keeps_the_nominal_candidate() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        conn.execute("INSERT INTO pool_models VALUES ('groq', '[\"a\",\"b\",\"c\"]', 1000)", ()).unwrap();
        let out = expand(&conn, vec![("groq".into(), "groq".into(), "default".into())], &|_, _| Some("k".into()), 2000);
        assert_eq!(out, vec![("groq".into(), "a".into(), "default".into()), ("groq".into(), "b".into(), "default".into())]);
        let none = expand(&conn, vec![("groq".into(), "groq".into(), "k2".into())], &|_, _| None, 2000);
        assert_eq!(none, vec![("groq".into(), "groq".into(), "k2".into())]);
    }
}
