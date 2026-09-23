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
//! `divisi_core::account::AccountStatus::RateLimited`, which is always
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
    // Amp exits 0 with "Error: Out of Credits Add credits to keep using Amp." on stderr — live-verified
    // 2026-09-23: 139 runs were recorded as completed without doing any work.
    "out of credits",
    "insufficient credits",
    "402 payment required",
];

/// Scans a failed/timed-out run's combined output for a rate-limit signal.
pub fn looks_like_rate_limit(text: &str) -> bool {
    let lower = text.to_lowercase();
    SIGNALS.iter().any(|signal| lower.contains(signal))
}

// Live-verification finding: an agent whose CLI is on `$PATH` but not
// actually authenticated (`claude` here — confirmed live: `divisi agent
// login claude` reported success but didn't persist credentials into
// divisi's isolated home) gets endlessly re-selected for planning/
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

/// True when the text says the agent has no usable credentials. Broader than the routing
/// signals above, because it is used to *label* an agent (a probe reads real output), not to decide a
/// retry in the hot path.
pub fn looks_like_auth_failure(text: &str) -> bool {
    const EXTRA: &[&str] = &["login required", "no api key", "missing api key", "invalid api key", "api key not", "please sign in", "sign in to", "not authenticated", "credentials"];
    let lower = text.to_lowercase();
    AUTH_FAILURE_SIGNALS.iter().chain(EXTRA).any(|signal| lower.contains(signal))
}

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

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone, Utc};

/// The furthest reset time believed: monthly plan caps are real, a 900-day wait is a parse error.
const MAX_RESET_DAYS: i64 = 40;

fn month_number(word: &str) -> Option<u32> {
    let m = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let head = word.get(..3)?;
    m.iter().position(|x| *x == head).map(|i| i as u32 + 1).filter(|_| word.len() <= 9)
}

/// `"22nd"` -> 22, `"3"` -> 3.
fn day_number(word: &str) -> Option<u32> {
    let digits: String = word.chars().take_while(|c| c.is_ascii_digit()).collect();
    let rest = &word[digits.len()..];
    if digits.is_empty() || digits.len() > 2 || !["", "st", "nd", "rd", "th"].contains(&rest) {
        return None;
    }
    digits.parse().ok().filter(|d| (1..=31).contains(d))
}

/// `"1:35"` -> (1, 35), `"2"` -> (2, 0).
fn clock(word: &str) -> Option<(u32, u32)> {
    let (h, m) = word.split_once(':').unwrap_or((word, "0"));
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    ((1..=12).contains(&h) && m < 60).then_some((h, m))
}

fn to_24h(hour: u32, pm: bool) -> u32 {
    match (hour, pm) {
        (12, false) => 0,
        (12, true) => 12,
        (h, true) => h + 12,
        (h, false) => h,
    }
}

fn local_utc(date: NaiveDate, h: u32, m: u32) -> Option<DateTime<Utc>> {
    let naive = date.and_time(NaiveTime::from_hms_opt(h, m, 0)?);
    Local.from_local_datetime(&naive).earliest().map(|d| d.with_timezone(&Utc))
}

/// `sep 22nd, 2026 1:35 am` (codex).
fn absolute(words: &[&str]) -> Option<DateTime<Utc>> {
    (0..words.len().saturating_sub(4)).find_map(|i| {
        let month = month_number(words[i])?;
        let day = day_number(words[i + 1])?;
        let year: i32 = words[i + 2].parse().ok().filter(|y| (2000..=2100).contains(y))?;
        let (h, m) = clock(words[i + 3])?;
        let pm = match words[i + 4] {
            "am" => false,
            "pm" => true,
            _ => return None,
        };
        local_utc(NaiveDate::from_ymd_opt(year, month, day)?, to_24h(h, pm), m)
    })
}

/// `limits reset on 10/01` (kiro): month/day, this year or the next.
fn month_day(lower: &str, now: DateTime<Local>) -> Option<DateTime<Utc>> {
    let after = &lower[lower.find("reset")? + 5..];
    let bytes: Vec<char> = after.chars().take(40).collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let a: String = bytes[i..].iter().take_while(|c| c.is_ascii_digit()).collect();
            let j = i + a.len();
            if bytes.get(j) == Some(&'/') {
                let b: String = bytes[j + 1..].iter().take_while(|c| c.is_ascii_digit()).collect();
                if let (Ok(month), Ok(day)) = (a.parse::<u32>(), b.parse::<u32>()) {
                    let this_year = NaiveDate::from_ymd_opt(now.year(), month, day)?;
                    let at = local_utc(this_year, 0, 0)?;
                    if at > now.with_timezone(&Utc) {
                        return Some(at);
                    }
                    return local_utc(NaiveDate::from_ymd_opt(now.year() + 1, month, day)?, 0, 0);
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

/// `resets 2am` / `resets 5:30 pm` (claude): the next time the clock shows that.
fn clock_time(words: &[&str], now: DateTime<Local>) -> Option<DateTime<Utc>> {
    let at = words.iter().position(|w| w.starts_with("reset"))?;
    let rest = &words[at + 1..];
    let (h, m, pm) = rest.iter().take(3).enumerate().find_map(|(i, w)| {
        if let Some(head) = w.strip_suffix("am").or_else(|| w.strip_suffix("pm")) {
            let (h, m) = clock(head)?;
            return Some((h, m, w.ends_with("pm")));
        }
        let (h, m) = clock(w)?;
        match rest.get(i + 1).copied() {
            Some("am") => Some((h, m, false)),
            Some("pm") => Some((h, m, true)),
            _ => None,
        }
    })?;
    let today = now.date_naive();
    let candidate = local_utc(today, to_24h(h, pm), m)?;
    if candidate > now.with_timezone(&Utc) {
        Some(candidate)
    } else {
        local_utc(today.succ_opt()?, to_24h(h, pm), m)
    }
}

/// `try again in 2 hours 5 minutes`, `retry in 45 minutes`, `Retry-After: 120`.
fn relative(lower: &str, now: DateTime<Local>) -> Option<DateTime<Utc>> {
    let n = now.with_timezone(&Utc);
    if let Some(i) = lower.find("retry-after") {
        let secs: String = lower[i + 11..].chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(s) = secs.parse::<i64>() {
            return Some(n + Duration::seconds(s));
        }
    }
    let words: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let start = words.iter().position(|w| *w == "in" || *w == "after")? + 1;
    // Compact form, as agy writes it: "Resets in 55h7m23s".
    if let Some(total) = words.get(start).and_then(|w| compact_duration(w)) {
        return Some(n + total);
    }
    let mut total = Duration::zero();
    let mut i = start;
    let mut any = false;
    while i < words.len() {
        if words[i] == "and" {
            i += 1;
            continue;
        }
        let Ok(qty) = words[i].parse::<i64>() else { break };
        let unit = words.get(i + 1).copied().unwrap_or("");
        let step = match unit.trim_end_matches('s') {
            "second" | "sec" => Duration::seconds(qty),
            "minute" | "min" => Duration::minutes(qty),
            "hour" | "hr" => Duration::hours(qty),
            "day" => Duration::days(qty),
            _ => break,
        };
        total = total + step;
        any = true;
        i += 2;
    }
    any.then_some(n + total)
}

/// `55h7m23s`, `2d4h`, `90m`: digit runs each followed by one of d, h, m, s. `None` for anything else.
fn compact_duration(word: &str) -> Option<Duration> {
    let mut total = Duration::zero();
    let mut digits = String::new();
    let mut any = false;
    for c in word.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let qty: i64 = digits.parse().ok()?;
        digits.clear();
        total = total
            + match c {
                'd' => Duration::days(qty),
                'h' => Duration::hours(qty),
                'm' => Duration::minutes(qty),
                's' => Duration::seconds(qty),
                _ => return None,
            };
        any = true;
    }
    (digits.is_empty() && any).then_some(total)
}

/// When a rate-limited agent says it will work again, read from its own message. `None` when it
/// gives no usable time (the caller keeps its default short cooldown) or the time is in the past or
/// implausibly far away. Handles the phrasings verified live: codex ("try again at Sep 22nd, 2026
/// 1:35 AM"), kiro ("limits reset on 10/01"), claude ("resets 2am"), and relative waits.
pub fn reset_time(text: &str, now: DateTime<Local>) -> Option<DateTime<Utc>> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !(c.is_ascii_alphanumeric() || c == ':')).filter(|w| !w.is_empty()).collect();
    let found = absolute(&words).or_else(|| month_day(&lower, now)).or_else(|| clock_time(&words, now)).or_else(|| relative(&lower, now))?;
    let n = now.with_timezone(&Utc);
    (found > n && found <= n + Duration::days(MAX_RESET_DAYS)).then_some(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_compact_durations_like_agys() {
        let n = now().with_timezone(&Utc);
        let msg = r#"AGY_ERROR: {"short_error":"RESOURCE_EXHAUSTED (code 429): Individual quota reached. Resets in 55h7m23s."}"#;
        assert_eq!(reset_time(msg, now()), Some(n + Duration::hours(55) + Duration::minutes(7) + Duration::seconds(23)));
        assert_eq!(compact_duration("2d4h"), Some(Duration::hours(52)));
        assert_eq!(compact_duration("90m"), Some(Duration::minutes(90)));
        assert_eq!(compact_duration("soon"), None);
        assert_eq!(compact_duration("12"), None, "digits with no unit are not a duration");
    }

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

    // ---- reset_time ----------------------------------------------------------------------

    use chrono::{Datelike, Duration, Local, TimeZone, Utc};

    fn now() -> chrono::DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 20, 3, 30, 0).unwrap()
    }
    fn local(y: i32, m: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<Utc> {
        Local.with_ymd_and_hms(y, m, d, h, mi, 0).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn codex_gives_an_absolute_local_time() {
        let msg = "ERROR: You've hit your usage limit. Upgrade to Plus to continue using Codex (https://chatgpt.com/explore/plus), or try again at Sep 22nd, 2026 1:35 AM.";
        assert_eq!(reset_time(msg, now()), Some(local(2026, 9, 22, 1, 35)));
        assert_eq!(reset_time("try again at Oct 3rd, 2026 11:05 PM", now()), Some(local(2026, 10, 3, 23, 5)));
        assert_eq!(reset_time("try again at Sep 21st, 2026 12:00 AM", now()), Some(local(2026, 9, 21, 0, 0)), "12 AM is midnight");
        assert_eq!(reset_time("try again at Sep 21st, 2026 12:30 PM", now()), Some(local(2026, 9, 21, 12, 30)), "12 PM is noon");
    }

    #[test]
    fn kiro_gives_a_month_and_day() {
        let msg = "Monthly request limit reached · Upgrade your plan to continue. Your limits reset on 10/01.";
        assert_eq!(reset_time(msg, now()), Some(local(2026, 10, 1, 0, 0)));
        // a date already past this year means next year, not the past
        assert_eq!(reset_time("limits reset on 09/01", now()), None, "next year is beyond the cap, so no cooldown is claimed");
    }

    #[test]
    fn claude_gives_a_clock_time_and_means_the_next_occurrence() {
        assert_eq!(reset_time("You've hit your session limit · resets 2am", now()), Some(local(2026, 9, 21, 2, 0)), "2am already passed today");
        assert_eq!(reset_time("session limit reached, resets 5:30 am", now()), Some(local(2026, 9, 20, 5, 30)), "5:30am is still ahead today");
        assert_eq!(reset_time("resets 9pm", now()), Some(local(2026, 9, 20, 21, 0)));
    }

    #[test]
    fn relative_waits_are_summed() {
        let n = now().with_timezone(&Utc);
        assert_eq!(reset_time("Please try again in 2 hours 5 minutes.", now()), Some(n + Duration::minutes(125)));
        assert_eq!(reset_time("rate limited, retry in 45 minutes", now()), Some(n + Duration::minutes(45)));
        assert_eq!(reset_time("Retry-After: 120", now()), Some(n + Duration::seconds(120)));
        assert_eq!(reset_time("try again in 1 day and 2 hours", now()), Some(n + Duration::hours(26)));
    }

    #[test]
    fn no_time_in_the_message_means_no_claim() {
        for msg in ["Rate limit exceeded", "HTTP 429 Too Many Requests", "", "error: file not found", "resets soon"] {
            assert_eq!(reset_time(msg, now()), None, "{msg:?}");
        }
    }

    #[test]
    fn past_and_absurd_times_are_ignored() {
        assert_eq!(reset_time("try again at Sep 1st, 2026 1:00 AM", now()), None, "already in the past");
        assert_eq!(reset_time("try again in 900 days", now()), None, "beyond any real quota window");
        assert_eq!(reset_time("try again at Sep 22nd, 2031 1:35 AM", now()), None);
        assert!(reset_time("try again at Sep 22nd, 2026 1:35 AM", now()).unwrap().year() == 2026);
    }

    #[test]
    fn auth_failures_are_told_apart_from_rate_limits() {
        for m in ["Not logged in · Please run /login", "Error: authentication required", "401 Unauthorized", "No API key found. Set OPENAI_API_KEY", "Invalid API key provided", "please sign in to continue", "login required"] {
            assert!(looks_like_auth_failure(m), "{m:?}");
        }
        for m in ["Rate limit exceeded", "You've hit your usage limit", "error: file not found", ""] {
            assert!(!looks_like_auth_failure(m), "{m:?}");
        }
    }
}
