//! Best-effort detection, from a finished task's output, that the agent
//! didn't answer because it was rate-limited or the upstream was
//! transiently overloaded — either way `--allow-fallback` should hop to
//! the next agent. Deliberately generic rather than a per-agent table of
//! exact error strings: this project only claims specifics it has
//! directly verified (see e.g. `registry.rs`'s bootstrap-install
//! commands), and nobody has gone through all 20+ supported agent CLIs'
//! real output to confirm exact wording. A small set of case-insensitive
//! substrings that commonly show up across HTTP APIs and CLI tools is a
//! genuinely honest middle ground: it will miss some (an agent phrasing
//! it unusually) and it's not infallible, but it won't fabricate
//! confidence it doesn't have. Paired with
//! `single_core::account::AccountStatus::RateLimited`, which is always
//! authoritative when a user (or a previous detection) has already set it.

const SIGNALS: &[&str] = &[
    "rate limit",
    "rate_limit",
    "ratelimit",
    "429",
    "quota exceeded",
    "quota_exceeded",
    "too many requests",
    "usage limit",
    // Kiro CLI's phrasing for hitting its monthly cap ("Monthly request
    // limit reached · Upgrade your plan ... limits reset on 10/01") —
    // live-verified 2026-09-17, previously unmatched (note: this CLI also
    // exits 0 on this message, a separate bug fixed in task.rs's success
    // path, not just a missing signal here).
    "request limit",
    "limit reached",
    // Claude Code's own phrasing for hitting a subscription cap
    // ("You've hit your session limit · resets 2am") — live-verified
    // 2026-09-12, previously unmatched by every signal above, so an
    // exhausted claude session kept getting re-selected instead of
    // falling over.
    "session limit",
    // Anthropic returns HTTP 529 `overloaded_error` when its own capacity
    // is saturated — not a per-user quota, but still "this agent can't
    // answer right now", so fallback should treat it the same.
    "529",
    "overloaded",
];

/// Scans a failed/timed-out run's combined output for a rate-limit signal.
pub fn looks_like_rate_limit(text: &str) -> bool {
    let lower = text.to_lowercase();
    SIGNALS.iter().any(|signal| lower.contains(signal))
}

// Live-verification finding: an agent whose CLI is on `$PATH` but not
// actually authenticated (`claude` here — confirmed live: `single agent
// login claude` reported success but didn't persist credentials into
// SingleCLI's isolated home) gets endlessly re-selected for planning/
// dispatch, since only a rate-limit signal excludes an agent from
// `PoolHealth::usable()` or triggers `maybe_fail_over`'s hop to the next
// candidate — an auth failure did neither, so a broken-auth agent could
// burn every planning attempt for a goal with no fallback ever
// triggering. Auth failures get the identical treatment (same 15-minute
// exclusion window, same fallback hop) since the needed response is
// identical: stop retrying this agent right now, try the next one.
const AUTH_FAILURE_SIGNALS: &[&str] = &[
    "not logged in",
    "please run /login",
    "please log in",
    "please login",
    "authentication required",
    "unauthorized",
    "401 unauthorized",
];

/// `looks_like_rate_limit`, broadened to also flag an authentication
/// failure — see `AUTH_FAILURE_SIGNALS`'s doc comment for why the two are
/// treated the same downstream (temporary exclusion + fallback hop).
pub fn looks_like_unavailable(text: &str) -> bool {
    if looks_like_rate_limit(text) {
        return true;
    }
    let lower = text.to_lowercase();
    AUTH_FAILURE_SIGNALS.iter().any(|signal| lower.contains(signal))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_rate_limit_phrasing_case_insensitively() {
        assert!(looks_like_rate_limit("Error: Rate limit exceeded, please try again later"));
        assert!(looks_like_rate_limit("HTTP 429 Too Many Requests"));
        assert!(looks_like_rate_limit("QUOTA_EXCEEDED for this billing period"));
        assert!(looks_like_rate_limit("API error 529: Overloaded"));
    }

    #[test]
    fn ordinary_failures_are_not_flagged() {
        assert!(!looks_like_rate_limit("error: file not found"));
        assert!(!looks_like_rate_limit("panic: index out of bounds"));
        assert!(!looks_like_rate_limit(""));
    }

    #[test]
    fn looks_like_unavailable_detects_auth_failures() {
        assert!(looks_like_unavailable("Not logged in · Please run /login"));
        assert!(looks_like_unavailable("Error: authentication required"));
        assert!(looks_like_unavailable("401 Unauthorized"));
    }

    #[test]
    fn looks_like_unavailable_still_detects_rate_limits() {
        assert!(looks_like_unavailable("HTTP 429 Too Many Requests"));
    }

    #[test]
    fn looks_like_unavailable_does_not_flag_ordinary_failures() {
        assert!(!looks_like_unavailable("error: file not found"));
        assert!(!looks_like_unavailable("panic: index out of bounds"));
        assert!(!looks_like_unavailable(""));
    }
}
