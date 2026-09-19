//! Short, plain-text answers for chat. Goal texts here are often whole paragraphs, so every
//! summary is truncated: these lines also have to fit the notch.

use divisi_protocol::{CoordinatorSnapshot, GoalSummary, PoolStatusInfo, UsageSummary};

/// First line of `text`, trimmed and cut to `max` characters with an ellipsis.
pub fn short(text: &str, max: usize) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if line.chars().count() <= max {
        return line.to_owned();
    }
    let cut: String = line.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

fn goal_lines(label: &str, goals: &[GoalSummary], out: &mut String) {
    if goals.is_empty() {
        return;
    }
    out.push_str(&format!("{label} ({}):\n", goals.len()));
    for g in goals.iter().take(5) {
        out.push_str(&format!("  {}  {}\n", g.id, short(&g.text, 70)));
    }
    if goals.len() > 5 {
        out.push_str(&format!("  … and {} more\n", goals.len() - 5));
    }
}

pub fn status_text(s: &CoordinatorSnapshot) -> String {
    let (r, q, b, w) = (s.running_goals.len(), s.queued_goals.len(), s.blocked_goals.len(), s.waiting_goals.len());
    if r + q + b + w == 0 {
        return "Nothing is running, queued or blocked.".to_owned();
    }
    let mut out = format!("{r} running, {q} queued, {b} blocked, {w} waiting on capacity.\n");
    goal_lines("Running", &s.running_goals, &mut out);
    goal_lines("Queued", &s.queued_goals, &mut out);
    goal_lines("Blocked", &s.blocked_goals, &mut out);
    goal_lines("Waiting on capacity", &s.waiting_goals, &mut out);
    out.trim_end().to_owned()
}

pub fn pool_text(pool: &PoolStatusInfo, snap: &CoordinatorSnapshot) -> String {
    let mut out = format!("Pool {}% healthy{}.", (pool.healthy_ratio * 100.0).round() as i64, if pool.degraded { " (degraded)" } else { "" });
    if !pool.benched.is_empty() {
        out.push_str(&format!(" {} key(s) cooling down:", pool.benched.len()));
        for k in pool.benched.iter().take(4) {
            out.push_str(&format!(" {}/{} back in {}m;", k.platform, k.model, k.remaining_secs.div_ceil(60)));
        }
        out = out.trim_end_matches(';').to_owned();
        out.push('.');
    }
    let agents: Vec<String> = snap
        .pool
        .iter()
        .map(|a| {
            let cap = a.cap.map(|c| c.to_string()).unwrap_or_else(|| "-".into());
            format!("{} {}/{}{}", a.agent, a.running, cap, if a.rate_limited { " (rate limited)" } else { "" })
        })
        .collect();
    if !agents.is_empty() {
        out.push_str(&format!("\nAgents: {}.", agents.join(", ")));
    }
    out
}

pub fn usage_text(u: &UsageSummary) -> String {
    let mut out = format!("Estimated spend ${:.2} across {} provider(s)", u.total_usd, u.provider_usage.len());
    if !u.agent_local_stats.is_empty() {
        out.push_str(&format!(" and {} local agent(s)", u.agent_local_stats.len()));
    }
    match &u.last_refreshed {
        Some(t) => out.push_str(&format!("; last refreshed {t}.")),
        None => out.push_str("; never refreshed (run a usage refresh for live numbers)."),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn goal(id: &str, text: &str, status: &str) -> GoalSummary {
        serde_json::from_value(json!({
            "id": id, "session_id": "sess_1", "text": text, "mode": "auto", "status": status,
            "dispatches": 1, "max_dispatches": 25, "created_at": "2026-09-19T00:00:00Z"
        }))
        .unwrap()
    }

    fn snapshot(running: Vec<GoalSummary>, blocked: Vec<GoalSummary>) -> CoordinatorSnapshot {
        serde_json::from_value(json!({
            "running_goals": running, "queued_goals": [], "blocked_goals": blocked, "waiting_goals": [],
            "pool": [{"agent":"claude","running":0,"cap":null,"rate_limited":false},{"agent":"opencode","running":1,"cap":1,"rate_limited":true}],
            "max_parallel": 40
        }))
        .unwrap()
    }

    #[test]
    fn short_keeps_the_first_line_and_truncates() {
        assert_eq!(short("  first line\nsecond", 40), "first line");
        assert_eq!(short("abcdefghij", 5), "abcd…");
        assert_eq!(short("", 5), "");
        assert_eq!(short("\n\n  real one", 40), "real one");
    }

    #[test]
    fn an_idle_pool_says_so() {
        assert_eq!(status_text(&snapshot(vec![], vec![])), "Nothing is running, queued or blocked.");
    }

    #[test]
    fn status_lists_goals_briefly() {
        let long = "E05.00 (Sterling Provider Fleet audit). The sprint file is at the ABSOLUTE path /home/user/docs — read it with that exact path";
        let text = status_text(&snapshot(vec![goal("goal_a_1", "add tests", "running")], vec![goal("goal_b_2", long, "blocked")]));
        assert!(text.starts_with("1 running, 0 queued, 1 blocked, 0 waiting on capacity."), "{text}");
        assert!(text.contains("goal_a_1  add tests"));
        assert!(text.contains("goal_b_2  E05.00"));
        assert!(!text.contains("exact path"), "long goal text must be truncated: {text}");
    }

    #[test]
    fn status_caps_a_long_list() {
        let many: Vec<GoalSummary> = (0..8).map(|i| goal(&format!("goal_x_{i}"), "work", "running")).collect();
        let text = status_text(&snapshot(many, vec![]));
        assert!(text.contains("… and 3 more"), "{text}");
    }

    #[test]
    fn pool_text_reports_health_and_agents() {
        let pool: PoolStatusInfo = serde_json::from_value(json!({
            "degraded": true, "healthy_ratio": 0.75,
            "benched": [{"platform":"groq","model":"llama","key_id":"k1","remaining_secs":125,"provenance":"x"}]
        }))
        .unwrap();
        let text = pool_text(&pool, &snapshot(vec![], vec![]));
        assert!(text.starts_with("Pool 75% healthy (degraded)."), "{text}");
        assert!(text.contains("groq/llama back in 3m"), "{text}");
        assert!(text.contains("opencode 1/1 (rate limited)"), "{text}");
        assert!(text.contains("claude 0/-"), "{text}");
    }

    #[test]
    fn usage_text_handles_never_refreshed() {
        let u: UsageSummary = serde_json::from_value(json!({"provider_usage":[],"agent_local_stats":[],"total_usd":0.0,"last_refreshed":null})).unwrap();
        assert!(usage_text(&u).contains("never refreshed"));
        let u: UsageSummary = serde_json::from_value(json!({"provider_usage":[],"agent_local_stats":[],"total_usd":4.5,"last_refreshed":"2026-09-19T01:00:00Z"})).unwrap();
        assert!(usage_text(&u).starts_with("Estimated spend $4.50"));
    }
}
