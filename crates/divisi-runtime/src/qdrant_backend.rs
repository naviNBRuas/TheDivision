//! Optional Qdrant-backed vector store — the storage/search half of RAG
//! (spec section 9's "vector search through configurable providers").
//!
//! **Scope note**: this module itself stores and searches pre-computed
//! vectors only — it does not turn text into a vector. That's
//! `embeddings.rs`'s job (a real call to OpenAI's `/v1/embeddings`), which
//! *is* wired up as of v0.1.9: `MemoryStore` best-effort embeds and
//! upserts every memory entry into the `single_memory` collection here,
//! and `MemorySearchSemantic` embeds the query and searches it, falling
//! back to substring search if either the embeddings key or
//! `DIVISI_QDRANT_URL` isn't configured — see `handlers.rs`'s
//! `MemoryStore`/`MemorySearchSemantic` arms and `embeddings.rs`'s module
//! docs. This module's own direct callers (`single memory vector
//! upsert/search`) still take pre-computed vectors — the text→vector step
//! only happens in the memory-entry path described above, not as a
//! generic feature of this module.
//!
//! The REST API shape used here (`PUT /collections/{name}`, `PUT
//! .../points`, `POST .../points/search`, `POST .../points/delete`) was
//! verified directly against a real local Qdrant instance (`docker run
//! qdrant/qdrant`, v1.19.0) during development — request/response JSON
//! captured from real `curl` calls, not assumed from documentation.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use divisi_protocol::VectorHit;

pub fn resolve_url() -> Option<String> {
    std::env::var("DIVISI_QDRANT_URL").ok()
}

pub fn ping(url: &str) -> Result<()> {
    let client = reqwest::blocking::Client::new();
    let resp = client.get(url).send().context("connecting to qdrant")?;
    if !resp.status().is_success() {
        bail!("qdrant returned {}", resp.status());
    }
    Ok(())
}

pub fn ensure_collection(url: &str, collection: &str, vector_size: u64) -> Result<()> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .put(format!("{url}/collections/{collection}"))
        .json(&json!({ "vectors": { "size": vector_size, "distance": "Cosine" } }))
        .send()
        .context("creating qdrant collection")?;
    if !resp.status().is_success() {
        bail!("qdrant collection create failed: {} {}", resp.status(), resp.text().unwrap_or_default());
    }
    Ok(())
}

/// Creates the collection first if it doesn't exist yet (sized to
/// `vector.len()`), then upserts the point — a convenience so callers
/// don't need a separate provisioning step for a new collection name.
pub fn upsert_point(url: &str, collection: &str, id: u64, vector: &[f32], payload: Value) -> Result<()> {
    let _ = ensure_collection(url, collection, vector.len() as u64); // idempotent; ignore "already exists"
    let client = reqwest::blocking::Client::new();
    let resp = client
        .put(format!("{url}/collections/{collection}/points"))
        .json(&json!({ "points": [{ "id": id, "vector": vector, "payload": payload }] }))
        .send()
        .context("upserting qdrant point")?;
    if !resp.status().is_success() {
        bail!("qdrant upsert failed: {} {}", resp.status(), resp.text().unwrap_or_default());
    }
    Ok(())
}

pub fn search(url: &str, collection: &str, vector: &[f32], limit: u64) -> Result<Vec<VectorHit>> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{url}/collections/{collection}/points/search"))
        .json(&json!({ "vector": vector, "limit": limit, "with_payload": true }))
        .send()
        .context("searching qdrant")?;
    if !resp.status().is_success() {
        bail!("qdrant search failed: {} {}", resp.status(), resp.text().unwrap_or_default());
    }
    let body: Value = resp.json().context("parsing qdrant search response")?;
    let results = body["result"].as_array().cloned().unwrap_or_default();
    Ok(results
        .into_iter()
        .filter_map(|r| {
            Some(VectorHit {
                id: r["id"].as_u64()?,
                score: r["score"].as_f64()? as f32,
                payload: r["payload"].clone(),
            })
        })
        .collect())
}

pub fn delete_point(url: &str, collection: &str, id: u64) -> Result<()> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{url}/collections/{collection}/points/delete"))
        .json(&json!({ "points": [id] }))
        .send()
        .context("deleting qdrant point")?;
    if !resp.status().is_success() {
        bail!("qdrant delete failed: {} {}", resp.status(), resp.text().unwrap_or_default());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This project's own Qdrant integration was developed and tested
    /// against `docker run -d -p 16333:6333 qdrant/qdrant`. Tests skip
    /// cleanly (not fail) when nothing answers there.
    const TEST_URL: &str = "http://127.0.0.1:16333";

    fn skip_if_unavailable() -> Option<()> {
        if ping(TEST_URL).is_err() {
            eprintln!("skipping qdrant_backend test: no qdrant reachable at {TEST_URL}");
            return None;
        }
        Some(())
    }

    #[test]
    fn upsert_and_search_round_trip_against_real_qdrant() {
        let Some(()) = skip_if_unavailable() else { return };
        let collection = "singlecli_test_roundtrip";
        upsert_point(TEST_URL, collection, 1, &[0.1, 0.2, 0.3, 0.4], json!({"text": "hello world"})).unwrap();
        upsert_point(TEST_URL, collection, 2, &[0.9, 0.8, 0.7, 0.6], json!({"text": "unrelated"})).unwrap();

        let hits = search(TEST_URL, collection, &[0.1, 0.2, 0.3, 0.4], 5).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].id, 1);
        assert_eq!(hits[0].payload["text"], "hello world");

        delete_point(TEST_URL, collection, 1).unwrap();
        delete_point(TEST_URL, collection, 2).unwrap();
    }

    #[test]
    fn search_on_nonexistent_collection_errors_cleanly() {
        let Some(()) = skip_if_unavailable() else { return };
        let result = search(TEST_URL, "singlecli_test_does_not_exist", &[0.1, 0.2], 5);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_url_reads_env_var() {
        std::env::set_var("DIVISI_QDRANT_URL", "http://example.invalid:6333");
        assert_eq!(resolve_url().as_deref(), Some("http://example.invalid:6333"));
        std::env::remove_var("DIVISI_QDRANT_URL");
        assert_eq!(resolve_url(), None);
    }

    #[test]
    fn connecting_to_unreachable_host_errors_not_panics() {
        assert!(ping("http://127.0.0.1:1").is_err());
    }
}
