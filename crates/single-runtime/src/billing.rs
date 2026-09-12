//! Real calls to each billing-supported provider's usage/cost API (see
//! `single_core::billing` for the registry and per-provider verification
//! status). Structurally parallel to `embeddings.rs`: real HTTP against
//! the vendor's documented endpoint, using `reqwest::blocking` (this
//! runtime already dispatches requests synchronously — see
//! `handlers::dispatch`), with an honest doc comment about what has and
//! hasn't been execution-confirmed rather than assumed.
//!
//! **Not independently verified against a live account in this change**
//! for any of the three supported providers — same caveat `embeddings.rs`
//! already carries for its own OpenAI call. The request/response shapes
//! below match each vendor's own current API reference (fetched directly
//! during implementation), not memory or assumption. `single_core::billing`
//! registry entries stay `verified: false` until someone actually runs
//! this against a real admin/API key and confirms the shape.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use single_protocol::UsageRecord;

pub fn fetch_usage(provider: &str, admin_key: &str, since: DateTime<Utc>) -> Result<Vec<UsageRecord>> {
    match provider {
        "anthropic" => fetch_anthropic(admin_key, since),
        "openai" => fetch_openai(admin_key, since),
        "openrouter" => fetch_openrouter(admin_key),
        other => anyhow::bail!("{other} has no billing API integration (see single_core::billing registry)"),
    }
}

/// `GET /v1/organizations/cost_report` — Anthropic's Usage & Cost Admin
/// API. `amount` is documented as a decimal string; the vendor's own
/// example response shows values like `"123.78912"` directly as dollars
/// (not cents, despite the endpoint's own prose description saying
/// "lowest currency units" — the example contradicts that, so this
/// parses it as dollars, matching the example, and that assumption is
/// exactly the kind of thing a real verification pass against a live
/// account needs to confirm or correct).
fn fetch_anthropic(admin_key: &str, since: DateTime<Utc>) -> Result<Vec<UsageRecord>> {
    #[derive(Deserialize)]
    struct CostReport {
        data: Vec<CostBucket>,
    }
    #[derive(Deserialize)]
    struct CostBucket {
        starting_at: String,
        ending_at: String,
        results: Vec<CostResult>,
    }
    #[derive(Deserialize)]
    struct CostResult {
        amount: String,
        model: Option<String>,
    }

    let client = reqwest::blocking::Client::new();
    let resp = client
        .get("https://api.anthropic.com/v1/organizations/cost_report")
        .header("anthropic-version", "2023-06-01")
        .header("x-api-key", admin_key)
        .query(&[("starting_at", since.to_rfc3339()), ("group_by[]", "description".into())])
        .send()
        .context("calling Anthropic's cost_report API")?;
    if !resp.status().is_success() {
        anyhow::bail!("Anthropic cost_report API returned {}: {}", resp.status(), resp.text().unwrap_or_default());
    }
    let report: CostReport = resp.json().context("parsing Anthropic cost_report response")?;

    let mut records = Vec::new();
    for bucket in report.data {
        for result in bucket.results {
            let cost_usd: f64 = result.amount.parse().unwrap_or(0.0);
            records.push(UsageRecord {
                provider: "anthropic".into(),
                key_label: None,
                agent: None,
                model: result.model,
                cost_usd,
                input_tokens: 0,
                output_tokens: 0,
                period_start: bucket.starting_at.clone(),
                period_end: bucket.ending_at.clone(),
            });
        }
    }
    Ok(records)
}

/// `GET /v1/organization/costs` — OpenAI's Admin API. `amount` is
/// `{ value: f64, currency: "usd" }` in decimal dollars.
fn fetch_openai(admin_key: &str, since: DateTime<Utc>) -> Result<Vec<UsageRecord>> {
    #[derive(Deserialize)]
    struct CostsResponse {
        data: Vec<CostBucket>,
    }
    #[derive(Deserialize)]
    struct CostBucket {
        start_time: i64,
        end_time: i64,
        results: Vec<CostResult>,
    }
    #[derive(Deserialize)]
    struct CostResult {
        amount: Amount,
        #[serde(default)]
        api_key_id: Option<String>,
    }
    #[derive(Deserialize)]
    struct Amount {
        value: f64,
    }

    let client = reqwest::blocking::Client::new();
    let resp = client
        .get("https://api.openai.com/v1/organization/costs")
        .bearer_auth(admin_key)
        .query(&[("start_time", since.timestamp().to_string())])
        .send()
        .context("calling OpenAI's organization costs API")?;
    if !resp.status().is_success() {
        anyhow::bail!("OpenAI costs API returned {}: {}", resp.status(), resp.text().unwrap_or_default());
    }
    let report: CostsResponse = resp.json().context("parsing OpenAI costs response")?;

    let mut records = Vec::new();
    for bucket in report.data {
        let period_start = DateTime::from_timestamp(bucket.start_time, 0).map(|d| d.to_rfc3339()).unwrap_or_default();
        let period_end = DateTime::from_timestamp(bucket.end_time, 0).map(|d| d.to_rfc3339()).unwrap_or_default();
        for result in bucket.results {
            records.push(UsageRecord {
                provider: "openai".into(),
                // OpenAI's api_key_id is an opaque platform ID, not the
                // human label a ProviderKeySpec carries — real attribution
                // to a label needs the caller to have separately recorded
                // which api_key_id corresponds to which local label
                // (not tracked anywhere yet; left None rather than
                // fabricating a match).
                key_label: None,
                agent: None,
                model: None,
                cost_usd: result.amount.value,
                input_tokens: 0,
                output_tokens: 0,
                period_start: period_start.clone(),
                period_end: period_end.clone(),
            });
            let _ = &result.api_key_id;
        }
    }
    Ok(records)
}

/// `GET /api/v1/key` — OpenRouter. No date-range query support: this
/// returns fixed snapshot totals (all-time / current UTC day / week /
/// month) for whichever key is used to authenticate, so `key` here is
/// simultaneously the auth credential and the thing being measured —
/// unlike anthropic/openai's separate-admin-key model. One call reports
/// one key's usage; multiple labeled keys need one call each.
fn fetch_openrouter(key: &str) -> Result<Vec<UsageRecord>> {
    #[derive(Deserialize)]
    struct KeyResponse {
        data: KeyData,
    }
    #[derive(Deserialize)]
    struct KeyData {
        label: Option<String>,
        usage: f64,
    }

    let client = reqwest::blocking::Client::new();
    let resp = client.get("https://openrouter.ai/api/v1/key").bearer_auth(key).send().context("calling OpenRouter's key API")?;
    if !resp.status().is_success() {
        anyhow::bail!("OpenRouter key API returned {}: {}", resp.status(), resp.text().unwrap_or_default());
    }
    let parsed: KeyResponse = resp.json().context("parsing OpenRouter key response")?;

    let now = Utc::now().to_rfc3339();
    Ok(vec![UsageRecord {
        provider: "openrouter".into(),
        key_label: parsed.data.label,
        agent: None,
        model: None,
        cost_usd: parsed.data.usage,
        input_tokens: 0,
        output_tokens: 0,
        period_start: "all-time".into(),
        period_end: now,
    }])
}
