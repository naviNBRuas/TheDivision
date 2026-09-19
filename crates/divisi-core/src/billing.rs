//! The billing-provider registry (Usage page): which providers SingleCLI
//! knows how to pull real `$` usage data from, and how honest that
//! support actually is — same discipline as `registry::builtin_registry`
//! (every entry documents whether it's been confirmed against the real
//! API or is doc-sourced/unsupported, not silently assumed uniform).
//!
//! Verified during implementation by reading each vendor's own current API
//! docs directly (not assumed from memory):
//!
//! - **anthropic**: real Admin API (`/v1/organizations/cost_report`),
//!   needs a separate Admin API key (`sk-ant-admin01-...`), distinct from
//!   any inference key — see `admin_key_env_hint`. Supports arbitrary
//!   `starting_at`/`ending_at` date ranges and an `api_key_ids[]` filter,
//!   so per-labeled-key attribution is possible if the org has issued a
//!   separate key per agent.
//! - **openai**: real Admin API (`/v1/organization/costs`), needs a
//!   separate Admin Key from the organization settings page, Bearer auth.
//!   Also supports arbitrary date ranges and an `api_key_ids[]` filter.
//! - **openrouter**: real (`GET /api/v1/key`), but a materially different
//!   shape from the two above — no separate admin key (same inference key
//!   used for calls), and no arbitrary date-range query at all: it only
//!   reports fixed snapshot totals (all-time / current UTC day / week /
//!   month), one key at a time. `fetch_usage`'s `since` parameter is
//!   approximated to the nearest supported bucket for this provider, not
//!   honored exactly.
//! - **xai**: **no aggregate usage/billing API exists at all** — xAI only
//!   returns a `cost_in_usd_ticks` field on each individual chat
//!   completion response. Capturing that would mean SingleCLI parsing
//!   response bodies from every request it proxies for grok, a completely
//!   different mechanism than "call a billing endpoint" and out of scope
//!   here. Marked `verified: false` / `unsupported: true` rather than
//!   guessed — this is a real, confirmed gap, not an oversight.

#[derive(Debug, Clone)]
pub struct BillingProviderDefinition {
    pub provider: &'static str,
    /// `true` only once this provider's real usage endpoint has actually
    /// been called successfully against a live account — not assumed
    /// from reading its docs. Left `false` until that verification pass
    /// happens; the shape below is still real (sourced from the vendor's
    /// current API reference), just not yet execution-confirmed.
    pub verified: bool,
    /// Whether this provider has a usage/billing API path at all. `xai`
    /// is the one honest `false` here.
    pub supported: bool,
    pub admin_key_env_hint: &'static str,
    pub notes: &'static str,
}

pub fn builtin_registry() -> Vec<BillingProviderDefinition> {
    vec![
        BillingProviderDefinition {
            provider: "anthropic",
            verified: false,
            supported: true,
            admin_key_env_hint: "ANTHROPIC_ADMIN_KEY",
            notes: "Admin API key required (sk-ant-admin01-...), distinct from any inference key. Supports arbitrary date ranges and per-API-key filtering via api_key_ids[].",
        },
        BillingProviderDefinition {
            provider: "openai",
            verified: false,
            supported: true,
            admin_key_env_hint: "OPENAI_ADMIN_KEY",
            notes: "Organization Admin Key required, distinct from any inference key. Supports arbitrary date ranges and per-API-key filtering via api_key_ids[].",
        },
        BillingProviderDefinition {
            provider: "openrouter",
            verified: false,
            supported: true,
            admin_key_env_hint: "OPENROUTER_API_KEY",
            notes: "No separate admin key — uses the same inference key as the agent it's billing for. Only reports fixed snapshot totals (all-time/daily/weekly/monthly), not an arbitrary date range.",
        },
        BillingProviderDefinition {
            provider: "xai",
            verified: false,
            supported: false,
            admin_key_env_hint: "",
            notes: "No aggregate usage/billing API exists. xAI only exposes a cost_in_usd_ticks field on individual response bodies, which would require intercepting every request rather than calling a report endpoint — out of scope for this registry.",
        },
    ]
}

pub fn find(provider: &str) -> Option<BillingProviderDefinition> {
    builtin_registry().into_iter().find(|p| p.provider == provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xai_is_honestly_marked_unsupported_not_guessed() {
        let xai = find("xai").expect("xai should be a registry entry, even though unsupported");
        assert!(!xai.supported);
        assert!(!xai.notes.is_empty(), "an unsupported entry must explain why, not just say false");
    }

    #[test]
    fn supported_providers_have_a_real_admin_key_hint() {
        for provider in builtin_registry().into_iter().filter(|p| p.supported) {
            assert!(!provider.admin_key_env_hint.is_empty(), "{} is supported but has no admin key hint", provider.provider);
        }
    }

    #[test]
    fn registry_has_unique_provider_names() {
        let mut names: Vec<&str> = builtin_registry().iter().map(|p| p.provider).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before, "duplicate billing provider name");
    }
}
