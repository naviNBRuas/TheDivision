//! The vendored ~40-entry free-LLM provider catalog (E28 spec §5.1–5.2),
//! transcribed from freellmapi's `providers/index.ts` (studied, not
//! vendored — clean-room). Pure data + parsers: no network, no state, no
//! dependency on `single-runtime`'s pool engine (which consumes this
//! table but lives in a different crate entirely — see that plan's
//! Phase 2+). This module only answers "what free providers exist and
//! how do you talk to them," never "is this one currently usable."

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// One provider's dial-in metadata: how to reach it, how to authenticate,
/// its published (or best-guess) limits, and the quirks its wire needs to
/// work around. `base_url` is `""` for providers whose wire is entirely
/// native (no plain OpenAI-compat base to point at).
#[derive(Debug, Clone, Copy)]
pub struct FreeProvider {
    pub id: &'static str,
    pub display: &'static str,
    pub base_url: &'static str,
    pub wire: Wire,
    pub auth: Auth,
    pub signup_url: &'static str,
    pub limits: Limits,
    pub pool: Option<PoolShape>,
    pub timeout: Duration,
    pub quirks: Quirks,
    pub free_note: &'static str,
    /// Coarse 1 (smallest/least capable) - 10 (largest/most capable)
    /// rank used by the bandit's intelligence term (spec §6.4). No
    /// authoritative source this iteration (D3: no live feed) -- a
    /// hand-set heuristic, documented seam for a future signed catalog
    /// to replace with real benchmark-derived ranks.
    pub intelligence_rank: u8,
}

/// Which HTTP shape a provider speaks. `OpenAiCompat` covers the 33
/// providers that speak plain OpenAI-compatible `/chat/completions`; every
/// other variant is a native wire implemented individually in
/// `single-runtime::pool::client` (Phase 4 of the E28 plan — not built
/// here, this enum is just the catalog's dispatch key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    OpenAiCompat,
    Gemini,
    Cohere,
    Cloudflare,
    Zhipu,
    AiHorde,
    Sail,
    ModelScope,
    Pollinations,
    ElectronHub,
    Experiential,
}

/// How the API key gets attached to a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    Bearer,
    XApiKey,
    /// A single key value that's actually two parts glued together, e.g.
    /// Cloudflare's `account_id:token` — the string after the colon
    /// names what the compound's second half is.
    Compound(&'static str),
    /// No real key needed; the sentinel value to send (e.g. AI Horde's
    /// anonymous `"0000000000"`).
    Keyless(&'static str),
    /// A custom header name instead of `Authorization: Bearer`.
    Header(&'static str),
}

/// Published (or best-effort observed) rate limits. `None` on any field
/// means "unknown" — the pool engine skips that window at admission time
/// but caps cooldown guesses at 10 minutes for it (spec §6.1/§6.2); this
/// catalog only records the number, it never enforces anything.
#[derive(Debug, Clone, Copy, Default)]
pub struct Limits {
    pub rpm: Option<u32>,
    pub rpd: Option<u32>,
    pub tpm: Option<u32>,
    pub tpd: Option<u64>,
}

/// How a provider's quota is shared across models/keys — drives
/// `pool::pools::infer_pool_key` once that module exists (Phase 2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PoolShape {
    /// A `:free`-suffixed model tier, quota separate from paid models.
    Free,
    /// Scoped per API project (Google-style).
    Project,
    /// Scoped per account, shared across every model.
    Account,
    /// A shared credit pool with its own per-minute ceiling.
    CreditPool { rpm: u32 },
    /// A daily token allowance shared account-wide.
    DailyTokens { n: u64 },
}

/// Per-provider workarounds the HTTP client (Phase 4) needs to apply.
/// Every field defaults to the "no workaround needed" value.
#[derive(Debug, Clone, Copy, Default)]
pub struct Quirks {
    /// Drop every tool call in a response but the first — some models
    /// choke on the request shape otherwise.
    pub force_single_tool_call: bool,
    /// Strip tool definitions entirely; the provider doesn't support them.
    pub no_tools: bool,
    /// Never request SSE streaming from this provider.
    pub no_stream: bool,
    /// Send a real browser `User-Agent` header (some providers 403/1010
    /// bot-looking clients otherwise).
    pub browser_ua: bool,
    /// Bump a caller-supplied `max_tokens` up to at least this value.
    pub min_max_tokens: Option<u32>,
    /// A cheap endpoint to probe for key validation, distinct from an
    /// actual chat completion (used by `single provider add-free`).
    pub validate_url: Option<&'static str>,
    /// How long a successful validation stays trusted before re-probing.
    pub validate_cache: Option<Duration>,
    /// Registered but region-locked (needs a China account, etc.) — see
    /// `default_disabled_reason`.
    pub region_wall: bool,
    /// Registered but needs real-name/identity verification to sign up.
    pub real_name_auth: bool,
    /// The "aihorde-class" problem (2026-09 routing audit, E28 follow-up):
    /// the provider's backing models are crowdsourced/rotating and skew
    /// toward conversational or roleplay completion rather than
    /// instruction-following structured output — confirmed here by a
    /// concrete wire-level tell (`no_tools`, forced non-streaming, or a
    /// "rotating public roster" of unvetted models), not vibes. A `true`
    /// provider is excluded from the `code`/`plan`/`integrate` role
    /// routing (`pool::pools::structured_output_candidate`), which need
    /// either real tool-calling or strict single-shot JSON compliance;
    /// it stays fully usable for `research`/`docs`/generic prompts.
    pub chat_prose_only: bool,
}

/// Looks up a catalog entry by id. `O(n)` over ~40 entries — not worth a
/// map for a table this size that's read a handful of times per command.
pub fn by_id(id: &str) -> Option<&'static FreeProvider> {
    FREE_PROVIDERS.iter().find(|p| p.id == id)
}

/// Shape-checks a key against its provider's `Auth` requirement before
/// `provider add-free` stores it. Live-verification finding: a
/// `Compound` provider (currently just Cloudflare, `account_id:token`)
/// silently accepted a bare token with no error — its empty `base_url`
/// means the usual best-effort validate-URL probe never runs for it
/// either, so a malformed key looked identical to a good one ("keyed,
/// unvalidated") right up until a real dispatch hit `PoolError::AuthFailed`
/// with no clue why. `Ok(())` for every other `Auth` variant — there's
/// nothing shape-checkable about a bearer token or an API-key header.
pub fn validate_key_shape(provider: &FreeProvider, key: &str) -> Result<(), String> {
    if let Auth::Compound(second_half) = provider.auth {
        if !key.contains(':') {
            return Err(format!(
                "{} needs a compound key in `first_half:{second_half}` form (e.g. Cloudflare's `account_id:token`), \
                 got a value with no ':' separator — see the provider's signup page for what the first half is",
                provider.id
            ));
        }
    }
    Ok(())
}

/// The §17-resolved reason string for the five providers `sync-pool`
/// writes `enabled = false` regardless of whether a key is on file —
/// `sail` (needs a payment method) and the four region-walled/real-name
/// ones. `None` for every other provider, keyed or not.
pub fn default_disabled_reason(id: &str) -> Option<&'static str> {
    match id {
        "sail" => Some("needs a payment method on file (flex-only credit model)"),
        "modelscope" => Some("needs a China account + real-name verification"),
        "qianfan" => Some("needs Chinese real-name auth (Baidu Cloud)"),
        "volcengine" => Some("needs Chinese real-name auth (Volcengine)"),
        "xfyun" => Some("needs a China console account (APIPassword)"),
        _ => None,
    }
}

const S30: Duration = Duration::from_secs(30);
const S60: Duration = Duration::from_secs(60);
const S120: Duration = Duration::from_secs(120);
const S180: Duration = Duration::from_secs(180);

/// `static` initializers must be const-evaluable, so the table below uses
/// these instead of `Limits::default()`/`Quirks::default()` (whose
/// `#[derive(Default)]` impls aren't `const fn`).
const NO_LIMITS: Limits = Limits { rpm: None, rpd: None, tpm: None, tpd: None };
const NO_QUIRKS: Quirks = Quirks {
    force_single_tool_call: false,
    no_tools: false,
    no_stream: false,
    browser_ua: false,
    min_max_tokens: None,
    validate_url: None,
    validate_cache: None,
    region_wall: false,
    real_name_auth: false,
    chat_prose_only: false,
};

/// `true` iff `id` is safe to route for `code`/`plan`/`integrate` role work
/// — an unknown id (not in the catalog, e.g. a shelled CLI agent name like
/// `claude`/`grok`) is not this table's concern and passes through as
/// suitable. See `FreeProvider::chat_prose_only` for what disqualifies a
/// free-pool provider specifically.
pub fn structured_output_ok(id: &str) -> bool {
    by_id(id).map(|p| !p.quirks.chat_prose_only).unwrap_or(true)
}

/// The vendored table, transcribed row-for-row from E28 spec §5.2 (which
/// is itself a snapshot from freellmapi's `providers/index.ts`, 2026-09).
/// 40 entries: 35 routable text providers + `siliconflow` (media-only,
/// registered but unroutable this iteration per §17) + the 4
/// region-walled ones (`modelscope`/`qianfan`/`volcengine`/`xfyun`) that
/// are counted in the same set as the 5 default-disabled ids (`sail` is
/// the 5th, not region-walled but payment-gated).
/// 2026-09 routing-suitability audit (all ~44 entries, one pass): looked
/// for the "aihorde-class" problem — a provider whose *wire-level*
/// contract (not just flakiness) makes it a bad fit for `code`/`plan`/
/// `integrate` role work, which needs either real tool-calling or
/// strict single-shot JSON compliance. Two entries earned
/// `chat_prose_only = true`:
/// - `aihorde`: `no_tools` + forced non-streaming + a 16-token floor on a
///   kudos-based crowdsourced queue whose backing models are volunteer
///   community LLMs (historically story/roleplay-tuned, not
///   instruction-following) — the clearest case.
/// - `radeon`: `no_tools` over a "rotating public roster" of unvetted
///   models the operator swaps without notice — no tool-calling and no
///   stable model identity to reason about compliance for.
///
/// Everything else keeps `chat_prose_only = false` — each remaining
/// quirk is a capacity/availability/privacy concern, not an output-shape
/// one, and doesn't block structured roles:
/// - already excluded from routing by other means: `siliconflow` (media
///   models only, registered but unroutable this iteration),
///   `sail`/`modelscope`/`qianfan`/`volcengine`/`xfyun` (default-disabled
///   per `default_disabled_reason` — payment/region/real-name gated
///   before a request ever goes out).
/// - tight or promo-only quotas (`ovh` 2 rpm, `agnes` ~30 concurrent,
///   `opencode-zen`/`bai` trial rosters): admission just fails fast and
///   the bandit/cooldown machinery routes around it like any other
///   rate-limited candidate — doesn't corrupt output.
/// - `kilo` logs prompts for training: a data-handling concern for the
///   user to weigh via `default_disabled_reason`-style opt-out, not a
///   correctness-of-output one, so it's left routable.
/// - `pollinations`: `validate_url` quirk exists because its public
///   `/v1/models` lies, but the chat wire itself speaks normal
///   OpenAI-compat completions against named models — no `no_tools`/
///   `no_stream` tell, so left routable.
/// - the remaining ~30 plain `OpenAiCompat`/native-wire entries (groq,
///   cerebras, nvidia, mistral, openrouter, cohere,
///   cloudflare, zhipu, ollama-cloud, llm7, huggingface, reka, routeway,
///   bazaarlink, ainative, aion, requesty, navyai, nara, sea-lion,
///   orcarouter, unorouter, xkiro, anyapi, electronhub, experiential,
///   longcat, custom) have no structural tell at all — full tool-calling
///   and streaming support, named (not rotating-anonymous) models.
pub static FREE_PROVIDERS: &[FreeProvider] = &[
    FreeProvider {
        id: "google",
        display: "Google Gemini",
        base_url: "",
        wire: Wire::Gemini,
        auth: Auth::Bearer,
        signup_url: "https://aistudio.google.com/apikey",
        limits: Limits { rpm: None, rpd: None, tpm: None, tpd: None },
        pool: Some(PoolShape::Project),
        timeout: S60,
        quirks: NO_QUIRKS,
        free_note: "per-project free tier pool",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "groq",
        display: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://console.groq.com",
        limits: Limits { rpm: None, rpd: Some(1000), tpm: Some(8000), tpd: None },
        pool: Some(PoolShape::Account),
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "account pool; some models rpd 1000 / tpm 8000",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "cerebras",
        display: "Cerebras",
        base_url: "https://api.cerebras.ai/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://cloud.cerebras.ai",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "free tier, published limits vary by model",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "nvidia",
        display: "NVIDIA NIM",
        base_url: "https://integrate.api.nvidia.com/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://build.nvidia.com",
        limits: Limits { rpm: Some(40), rpd: None, tpm: None, tpd: None },
        pool: Some(PoolShape::CreditPool { rpm: 40 }),
        timeout: S180,
        quirks: Quirks { force_single_tool_call: true, ..NO_QUIRKS },
        free_note: "credit pool ~40 rpm account-wide",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "mistral",
        display: "Mistral",
        base_url: "https://api.mistral.ai/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://console.mistral.ai",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "free tier",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "openrouter",
        display: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://openrouter.ai/keys",
        limits: Limits { rpm: None, rpd: Some(1000), tpm: None, tpd: None },
        pool: Some(PoolShape::Free),
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "`:free` pool 1000/day (50/day if <10 credits)",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "cohere",
        display: "Cohere",
        base_url: "",
        wire: Wire::Cohere,
        auth: Auth::Bearer,
        signup_url: "https://dashboard.cohere.com",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "free trial key",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "cloudflare",
        display: "Cloudflare Workers AI",
        base_url: "",
        wire: Wire::Cloudflare,
        auth: Auth::Compound("token"),
        signup_url: "https://dash.cloudflare.com",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "account_id:token compound key",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "zhipu",
        display: "Zhipu (z.ai)",
        base_url: "",
        wire: Wire::Zhipu,
        auth: Auth::Bearer,
        signup_url: "https://open.bigmodel.cn",
        limits: NO_LIMITS,
        pool: None,
        timeout: S60,
        quirks: NO_QUIRKS,
        free_note: "domestic->global host re-probe",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "ollama-cloud",
        display: "Ollama Cloud",
        base_url: "https://ollama.com/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://ollama.com",
        limits: NO_LIMITS,
        pool: None,
        timeout: S120,
        quirks: NO_QUIRKS,
        free_note: "reasoning in message.reasoning",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "kilo",
        display: "Kilo",
        base_url: "https://api.kilo.ai/api/gateway/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Keyless("kilo"),
        signup_url: "https://kilo.ai",
        limits: Limits { rpm: None, rpd: None, tpm: None, tpd: None },
        pool: None,
        timeout: S30,
        quirks: Quirks { validate_url: Some("/api/gateway/models"), ..NO_QUIRKS },
        free_note: "200 req/hr per IP; prompts logged for training",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "pollinations",
        display: "Pollinations",
        base_url: "",
        wire: Wire::Pollinations,
        auth: Auth::Bearer,
        signup_url: "https://pollinations.ai",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: Quirks { validate_url: Some("/account/key"), ..NO_QUIRKS },
        free_note: "validate /account/key (public /v1/models lies)",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "llm7",
        display: "LLM7",
        base_url: "https://api.llm7.io/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://llm7.io",
        limits: Limits { rpm: None, rpd: None, tpm: None, tpd: None },
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "100 req/hr; anon key works for basic use",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "huggingface",
        display: "Hugging Face Router",
        base_url: "https://router.huggingface.co/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://huggingface.co/settings/tokens",
        limits: NO_LIMITS,
        pool: Some(PoolShape::CreditPool { rpm: 0 }),
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "$0.10/mo router credit",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "opencode-zen",
        display: "OpenCode Zen",
        base_url: "https://opencode.ai/zen/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://opencode.ai/zen",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "trial-only promo roster",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "ovh",
        display: "OVHcloud AI Endpoints",
        base_url: "https://oai.endpoints.kepler.ai.cloud.ovh.net/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Keyless("ovh"),
        signup_url: "https://endpoints.ai.cloud.ovh.net",
        limits: Limits { rpm: Some(2), rpd: None, tpm: None, tpd: None },
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "2 req/min per IP per model",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "agnes",
        display: "Agnes AI",
        base_url: "https://apihub.agnes-ai.com/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://agnes-ai.com",
        limits: NO_LIMITS,
        pool: None,
        timeout: S60,
        quirks: NO_QUIRKS,
        free_note: "~30 concurrent -> 429",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "reka",
        display: "Reka",
        base_url: "https://api.reka.ai/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://platform.reka.ai",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "recurring monthly credit, no card",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "siliconflow",
        display: "SiliconFlow",
        base_url: "https://api.siliconflow.com/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://siliconflow.com",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "media only (FLUX.1-schnell, CosyVoice2) — registered, unroutable this iteration",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "routeway",
        display: "Routeway",
        base_url: "https://api.routeway.ai/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://routeway.ai",
        limits: Limits { rpm: Some(5), rpd: None, tpm: None, tpd: None },
        pool: None,
        timeout: S30,
        quirks: Quirks { browser_ua: true, ..NO_QUIRKS },
        free_note: "~5 rpm observed (doc says 20/200)",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "bazaarlink",
        display: "BazaarLink",
        base_url: "https://bazaarlink.ai/api/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://bazaarlink.ai",
        limits: NO_LIMITS,
        pool: Some(PoolShape::Free),
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "auto:free route only",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "ainative",
        display: "AI Native Studio",
        base_url: "https://api.ainative.studio/api/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://ainative.studio",
        limits: Limits { rpm: None, rpd: None, tpm: None, tpd: Some(10_000_000) },
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "~10M tok/mo (unverified)",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "aion",
        display: "AionLabs",
        base_url: "https://api.aionlabs.ai/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://aionlabs.ai",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "no-card, 30-day account age gate",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "requesty",
        display: "Requesty",
        base_url: "https://router.requesty.ai/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://requesty.ai",
        limits: NO_LIMITS,
        pool: Some(PoolShape::Free),
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "shared free pool",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "navyai",
        display: "Navy AI",
        base_url: "https://api.navy/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://api.navy",
        limits: Limits { rpm: Some(20), rpd: None, tpm: None, tpd: Some(150_000) },
        pool: Some(PoolShape::DailyTokens { n: 150_000 }),
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "150K tok/day, 20 rpm; needs explicit User-Agent",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "nara",
        display: "Nara",
        base_url: "https://router.bynara.id/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://bynara.id",
        limits: NO_LIMITS,
        pool: Some(PoolShape::Free),
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "shared free pool; Telegram channel verification",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "sea-lion",
        display: "SEA-LION",
        base_url: "https://api.sea-lion.ai/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://sea-lion.ai",
        limits: Limits { rpm: Some(10), rpd: None, tpm: None, tpd: None },
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "10 rpm recurring; Google sign-in, no card, no region wall",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "orcarouter",
        display: "OrcaRouter",
        base_url: "https://api.orcarouter.ai/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://orcarouter.ai",
        limits: NO_LIMITS,
        pool: Some(PoolShape::Free),
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "shared $0 pool, unpublished limits; 429 is a clean quota signal",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "unorouter",
        display: "UnoRouter",
        base_url: "https://api.unorouter.com/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://unorouter.com",
        limits: NO_LIMITS,
        pool: Some(PoolShape::Free),
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "account-wide per-minute `:free` pool",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "xkiro",
        display: "xKiro",
        base_url: "https://api.xkiro.com/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://xkiro.com",
        limits: Limits { rpm: None, rpd: None, tpm: None, tpd: Some(5_000_000) },
        pool: Some(PoolShape::DailyTokens { n: 5_000_000 }),
        timeout: S30,
        quirks: Quirks { validate_url: Some("/v1/usage"), ..NO_QUIRKS },
        free_note: "5M tok/day account-wide",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "radeon",
        display: "AMD Radeon Developer",
        base_url: "https://developer.amd.com.cn/radeon/api/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://developer.amd.com.cn/radeon",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: Quirks { no_tools: true, chat_prose_only: true, ..NO_QUIRKS },
        free_note: "rotating public roster, header-reported limits; no parallel tools, 10-min gen window",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "bai",
        display: "b.ai",
        base_url: "https://api.b.ai/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://b.ai",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "limited-time 0-credit promo",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "anyapi",
        display: "AnyAPI",
        base_url: "https://api.anyapi.ai/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://anyapi.ai",
        limits: Limits { rpm: None, rpd: None, tpm: None, tpd: Some(100_000) },
        pool: Some(PoolShape::DailyTokens { n: 100_000 }),
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "100K tok/day, no published rpm",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "electronhub",
        display: "ElectronHub",
        base_url: "",
        wire: Wire::ElectronHub,
        auth: Auth::Bearer,
        signup_url: "https://electronhub.ai",
        limits: NO_LIMITS,
        pool: Some(PoolShape::CreditPool { rpm: 0 }),
        timeout: S30,
        quirks: Quirks { validate_url: Some("/v1/user/me"), ..NO_QUIRKS },
        free_note: "shared weekly credit pool",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "experiential",
        display: "Experiential",
        base_url: "",
        wire: Wire::Experiential,
        auth: Auth::Bearer,
        signup_url: "https://experientiallabs.ai",
        limits: NO_LIMITS,
        pool: Some(PoolShape::CreditPool { rpm: 0 }),
        timeout: S30,
        quirks: Quirks { validate_url: Some("/v1/models"), ..NO_QUIRKS },
        free_note: "shared monthly credit pool; authed /v1/models",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "sail",
        display: "Sail",
        base_url: "",
        wire: Wire::Sail,
        auth: Auth::Bearer,
        signup_url: "https://sail.dev",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "$5/mo credit then pay-go; flex-only models",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "modelscope",
        display: "ModelScope",
        base_url: "",
        wire: Wire::ModelScope,
        auth: Auth::Bearer,
        signup_url: "https://modelscope.cn",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: Quirks { region_wall: true, real_name_auth: true, validate_cache: Some(Duration::from_secs(86_400)), ..NO_QUIRKS },
        free_note: "magic-grain quota; Alibaba China + real-name",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "qianfan",
        display: "Baidu Qianfan",
        base_url: "https://qianfan.baidubce.com/v2",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://qianfan.baidubce.com",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: Quirks { region_wall: true, real_name_auth: true, ..NO_QUIRKS },
        free_note: "ERNIE-Speed/Lite/Tiny free; Chinese real-name auth",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "volcengine",
        display: "Volcengine Ark",
        base_url: "https://ark.cn-beijing.volces.com/api/v3",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://console.volcengine.com",
        limits: Limits { rpm: None, rpd: None, tpm: None, tpd: Some(2_000_000) },
        pool: None,
        timeout: S30,
        quirks: Quirks { region_wall: true, real_name_auth: true, ..NO_QUIRKS },
        free_note: "2M tok/day/model + 500K new-user; real-name auth",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "longcat",
        display: "LongCat",
        base_url: "https://api.longcat.chat/openai/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://longcat.chat",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: NO_QUIRKS,
        free_note: "daily free quota; email signup works outside China",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "xfyun",
        display: "iFlytek Spark",
        base_url: "https://spark-api-open.xf-yun.com/v1",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "https://xfyun.cn",
        limits: NO_LIMITS,
        pool: None,
        timeout: S30,
        quirks: Quirks { region_wall: true, real_name_auth: true, ..NO_QUIRKS },
        free_note: "Lite model free, no published ceiling; console APIPassword",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "aihorde",
        display: "AI Horde",
        base_url: "",
        wire: Wire::AiHorde,
        auth: Auth::Keyless("0000000000"),
        signup_url: "https://aihorde.net",
        limits: NO_LIMITS,
        pool: None,
        timeout: S120,
        quirks: Quirks {
            min_max_tokens: Some(16),
            no_tools: true,
            no_stream: true,
            chat_prose_only: true,
            ..NO_QUIRKS
        },
        free_note: "kudos-based queue proxy",
        intelligence_rank: 5,
    },
    FreeProvider {
        id: "custom",
        display: "Custom (llama.cpp / vLLM / LM Studio)",
        base_url: "",
        wire: Wire::OpenAiCompat,
        auth: Auth::Bearer,
        signup_url: "",
        limits: NO_LIMITS,
        pool: None,
        timeout: S120,
        quirks: NO_QUIRKS,
        free_note: "user-supplied local/self-hosted endpoint",
        intelligence_rank: 5,
    },
];

/// One provider's `enabled`/`disabled_reason` state in `free-pool.toml`
/// (`single provider sync-pool`'s output, E28 spec §5.3/§17).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FreePoolEntry {
    pub enabled: bool,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct FreePoolFile {
    #[serde(default)]
    providers: BTreeMap<String, FreePoolEntry>,
}

pub fn load_pool_file(path: &Path) -> Result<BTreeMap<String, FreePoolEntry>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: FreePoolFile = toml::from_str(&text).with_context(|| format!("parsing {} as TOML", path.display()))?;
    Ok(file.providers)
}

pub fn save_pool_file(path: &Path, providers: &BTreeMap<String, FreePoolEntry>) -> Result<()> {
    let file = FreePoolFile { providers: providers.clone() };
    let rendered = toml::to_string_pretty(&file).context("serializing free-pool registry")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))
}

/// Reconciles `existing` (whatever's already in `free-pool.toml`) against
/// `FREE_PROVIDERS`, per E28 §17's resolution:
/// - The 5 default-disabled ids (`sail`/`modelscope`/`qianfan`/
///   `volcengine`/`xfyun`) are *always* written `enabled = false` with
///   their reason string, every run — an operator who wants one on has to
///   flip it by hand afterward (re-running `sync-pool` resets it, which
///   is the point: it's a deliberate, visible override, not a config the
///   sync silently respects).
/// - Every other provider already present in `existing` is left alone
///   (its `enabled` flag is an operator override once set) — only a
///   provider with no existing entry gets a fresh one, defaulted from
///   `is_keyed_and_valid`.
pub fn reconcile_pool_state(
    existing: &BTreeMap<String, FreePoolEntry>,
    is_keyed_and_valid: impl Fn(&str) -> bool,
) -> BTreeMap<String, FreePoolEntry> {
    let mut out = BTreeMap::new();
    for provider in FREE_PROVIDERS {
        if let Some(reason) = default_disabled_reason(provider.id) {
            out.insert(provider.id.to_string(), FreePoolEntry { enabled: false, disabled_reason: Some(reason.to_string()) });
            continue;
        }
        let entry = existing.get(provider.id).cloned().unwrap_or(FreePoolEntry {
            enabled: is_keyed_and_valid(provider.id),
            disabled_reason: None,
        });
        out.insert(provider.id.to_string(), entry);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_id_is_unique() {
        let mut ids: Vec<&str> = FREE_PROVIDERS.iter().map(|p| p.id).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "duplicate provider id in FREE_PROVIDERS");
    }

    #[test]
    fn validate_key_shape_rejects_a_bare_token_for_a_compound_provider() {
        let cloudflare = by_id("cloudflare").unwrap();
        assert!(validate_key_shape(cloudflare, "cfat_justatoken").is_err());
        assert!(validate_key_shape(cloudflare, "7d911b71704e00b8:cfat_justatoken").is_ok());
    }

    #[test]
    fn validate_key_shape_accepts_anything_for_non_compound_providers() {
        let groq = by_id("groq").unwrap();
        assert!(validate_key_shape(groq, "gsk-anything-at-all").is_ok());
        assert!(validate_key_shape(groq, "no-colon-either").is_ok());
    }

    #[test]
    fn every_base_url_is_a_valid_url_or_empty() {
        for p in FREE_PROVIDERS {
            if p.base_url.is_empty() {
                continue;
            }
            assert!(
                p.base_url.starts_with("https://") || p.base_url.starts_with("http://"),
                "{} has a non-empty base_url that isn't a URL: {}",
                p.id,
                p.base_url
            );
        }
    }

    #[test]
    fn region_walled_providers_have_a_default_disabled_reason() {
        let expected = ["sail", "modelscope", "qianfan", "volcengine", "xfyun"];
        for id in expected {
            assert!(default_disabled_reason(id).is_some(), "{id} should have a default-disabled reason");
        }
        for p in FREE_PROVIDERS {
            if expected.contains(&p.id) {
                continue;
            }
            assert!(default_disabled_reason(p.id).is_none(), "{} should not have a default-disabled reason", p.id);
        }
    }

    #[test]
    fn provider_count_matches_spec_table() {
        // Was 44; `1f312b1` ("chore: drop discontinued GitHub Models
        // free-pool provider") intentionally removed one row and didn't
        // update this assertion — a real drift, not a regression.
        assert_eq!(FREE_PROVIDERS.len(), 43, "spec §5.2 table has 43 rows (GitHub Models dropped, 1f312b1) — a change here should be deliberate");
    }

    #[test]
    fn by_id_finds_a_known_provider_and_none_for_unknown() {
        assert!(by_id("groq").is_some());
        assert!(by_id("does-not-exist").is_none());
    }

    #[test]
    fn pool_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("free-pool.toml");
        let mut providers = BTreeMap::new();
        providers.insert("groq".to_string(), FreePoolEntry { enabled: true, disabled_reason: None });
        save_pool_file(&path, &providers).unwrap();
        let loaded = load_pool_file(&path).unwrap();
        assert_eq!(loaded.get("groq"), Some(&FreePoolEntry { enabled: true, disabled_reason: None }));
    }

    #[test]
    fn reconcile_always_forces_the_five_default_disabled_ids_off() {
        let mut existing = BTreeMap::new();
        existing.insert("sail".to_string(), FreePoolEntry { enabled: true, disabled_reason: None });
        let out = reconcile_pool_state(&existing, |_| true);
        let sail = out.get("sail").unwrap();
        assert!(!sail.enabled);
        assert!(sail.disabled_reason.is_some());
    }

    #[test]
    fn reconcile_preserves_an_existing_operator_override() {
        let mut existing = BTreeMap::new();
        existing.insert("groq".to_string(), FreePoolEntry { enabled: false, disabled_reason: None });
        // is_keyed_and_valid says true, but the existing row already has
        // enabled=false — must not be clobbered.
        let out = reconcile_pool_state(&existing, |_| true);
        assert!(!out.get("groq").unwrap().enabled);
    }

    #[test]
    fn reconcile_defaults_a_new_entry_from_keyed_and_valid() {
        let out = reconcile_pool_state(&BTreeMap::new(), |id| id == "groq");
        assert!(out.get("groq").unwrap().enabled);
        assert!(!out.get("cerebras").unwrap().enabled);
    }
}
