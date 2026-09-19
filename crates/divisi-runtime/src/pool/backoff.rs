//! Retry-After / error-body / prose back-off parser — spec §6.2.
//!
//! Privacy note (spec, explicit): only the numeric duration is ever kept.
//! `resolve` never returns the raw header/body/prose it parsed, so a
//! caller can't accidentally log a provider's error text.

use chrono::{DateTime, Utc};
use serde_json::Value;
use std::time::Duration;

const MAX_CLAMP: Duration = Duration::from_secs(24 * 60 * 60);
const BODY_DEPTH_CAP: usize = 6;

/// `Retry-After` header: delta-seconds ("120") or an HTTP-date.
pub fn parse_retry_after(header: Option<&str>) -> Option<Duration> {
    let header = header?.trim();
    if let Ok(secs) = header.parse::<u64>() {
        return Some(clamp(Duration::from_secs(secs)));
    }
    if let Ok(date) = DateTime::parse_from_rfc2822(header) {
        let now = Utc::now();
        let delta = date.with_timezone(&Utc).signed_duration_since(now);
        let secs = delta.num_seconds().max(0) as u64;
        return Some(clamp(Duration::from_secs(secs)));
    }
    None
}

/// Depth-capped walk for `retryDelay` / `retry_after` / `retryAfterSeconds`
/// keys anywhere in the body, including Gemini's
/// `error.details[].RetryInfo.retryDelay = "17s"` shape.
pub fn parse_body_shape(body: &Value) -> Option<Duration> {
    walk(body, 0)
}

fn walk(value: &Value, depth: usize) -> Option<Duration> {
    if depth > BODY_DEPTH_CAP {
        return None;
    }
    match value {
        Value::Object(map) => {
            for key in ["retryDelay", "retry_after", "retryAfterSeconds", "RetryInfo"] {
                if let Some(v) = map.get(key) {
                    if let Some(d) = value_to_duration(v) {
                        return Some(d);
                    }
                    if let Some(d) = walk(v, depth + 1) {
                        return Some(d);
                    }
                }
            }
            for v in map.values() {
                if let Some(d) = walk(v, depth + 1) {
                    return Some(d);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(|v| walk(v, depth + 1)),
        _ => None,
    }
}

fn value_to_duration(v: &Value) -> Option<Duration> {
    match v {
        Value::Number(n) => n.as_u64().map(Duration::from_secs).map(clamp),
        Value::String(s) => parse_duration_token(s),
        _ => None,
    }
}

/// Parses a bare duration token like "17s", "2m", "1h".
fn parse_duration_token(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (num_part, unit) = s.split_at(s.len().saturating_sub(1));
    let n: u64 = num_part.parse().ok()?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return s.parse::<u64>().ok().map(Duration::from_secs).map(clamp),
    };
    Some(clamp(Duration::from_secs(secs)))
}

/// Anchored prose patterns: "try again in N seconds/minutes", "retry
/// after Nm/Nh". Hand-rolled (no `regex` dep in the workspace) — a small
/// scanner keeps "no new dependency" honest.
pub fn parse_prose(text: &str) -> Option<Duration> {
    let lower = text.to_lowercase();
    if let Some(pos) = lower.find("try again in ") {
        let rest = &lower[pos + "try again in ".len()..];
        return scan_number_unit(rest);
    }
    if let Some(pos) = lower.find("retry after ") {
        let rest = &lower[pos + "retry after ".len()..];
        return scan_number_unit(rest);
    }
    None
}

fn scan_number_unit(rest: &str) -> Option<Duration> {
    let rest = rest.trim_start();
    let digits_end = rest.find(|c: char| !c.is_ascii_digit())?;
    if digits_end == 0 {
        return None;
    }
    let n: u64 = rest[..digits_end].parse().ok()?;
    let tail = rest[digits_end..].trim_start();
    let secs = if tail.starts_with("second") || tail.starts_with('s') {
        n
    } else if tail.starts_with("minute") || tail.starts_with('m') {
        n * 60
    } else if tail.starts_with("hour") || tail.starts_with('h') {
        n * 3600
    } else {
        return None;
    };
    Some(clamp(Duration::from_secs(secs)))
}

fn clamp(d: Duration) -> Duration {
    d.min(MAX_CLAMP)
}

/// Header wins, else body, else prose fallback. Result always clamped to
/// 24h. Never returns the source string — only the parsed number.
pub fn resolve(header: Option<&str>, body: Option<&Value>, prose_fallback: Option<&str>) -> Option<Duration> {
    if let Some(d) = parse_retry_after(header) {
        return Some(d);
    }
    if let Some(body) = body {
        if let Some(d) = parse_body_shape(body) {
            return Some(d);
        }
    }
    if let Some(prose) = prose_fallback {
        if let Some(d) = parse_prose(prose) {
            return Some(d);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn header_delta_seconds() {
        assert_eq!(parse_retry_after(Some("120")), Some(Duration::from_secs(120)));
    }

    #[test]
    fn header_http_date() {
        let future = Utc::now() + chrono::Duration::seconds(90);
        let header = future.to_rfc2822();
        let d = parse_retry_after(Some(&header)).unwrap();
        assert!(d.as_secs() > 80 && d.as_secs() <= 90);
    }

    #[test]
    fn gemini_retryinfo_shape() {
        let body = json!({
            "error": {
                "details": [
                    { "RetryInfo": { "retryDelay": "17s" } }
                ]
            }
        });
        assert_eq!(parse_body_shape(&body), Some(Duration::from_secs(17)));
    }

    #[test]
    fn snake_case_retry_after_shape() {
        let body = json!({ "retry_after": 42 });
        assert_eq!(parse_body_shape(&body), Some(Duration::from_secs(42)));
    }

    #[test]
    fn prose_try_again_in_30_seconds() {
        assert_eq!(parse_prose("Rate limited. Try again in 30 seconds."), Some(Duration::from_secs(30)));
    }

    #[test]
    fn prose_retry_after_2m() {
        assert_eq!(parse_prose("Please retry after 2m"), Some(Duration::from_secs(120)));
    }

    #[test]
    fn clamps_to_24h() {
        assert_eq!(parse_retry_after(Some("100000000")), Some(MAX_CLAMP));
    }

    #[test]
    fn header_wins_over_body_wins_over_prose() {
        let body = json!({ "retry_after": 5 });
        let d = resolve(Some("10"), Some(&body), Some("try again in 999 seconds"));
        assert_eq!(d, Some(Duration::from_secs(10)));

        let d = resolve(None, Some(&body), Some("try again in 999 seconds"));
        assert_eq!(d, Some(Duration::from_secs(5)));

        let d = resolve(None, None, Some("try again in 999 seconds"));
        assert_eq!(d, Some(Duration::from_secs(999)));
    }

    #[test]
    fn returns_none_when_nothing_parses() {
        assert_eq!(resolve(None, None, None), None);
        assert_eq!(parse_retry_after(Some("not-a-number-or-date")), None);
        assert_eq!(parse_prose("nothing useful here"), None);
    }
}
