//! The free, instant layer: deterministic phrase rules for the obvious requests. Anything
//! these do not recognise returns `None` and goes to the model.

use super::{GoalAction, Intent};

/// The first `goal_…` id mentioned in the text, if any.
pub fn find_goal_id(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .find(|w| w.starts_with("goal_") && w.len() > 5)
        .map(str::to_owned)
}

fn has_any(t: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| t.contains(n))
}

fn starts_with_any(t: &str, verbs: &[&str]) -> bool {
    verbs.iter().any(|v| t == *v || t.starts_with(&format!("{v} ")))
}

pub fn parse(text: &str) -> Option<Intent> {
    let original = text.trim();
    let t = original.to_lowercase();
    let t = t.trim_end_matches(['?', '!', '.', ' ']);
    if t.is_empty() {
        return None;
    }

    if let Some(id) = find_goal_id(t) {
        // Everything after the id, in the user's own casing, is the payload of an amend.
        let rest = original.split_once(id.as_str()).map(|(_, r)| r.trim_start_matches([':', ',', ' ', '-']).trim()).unwrap_or("");
        if starts_with_any(t, &["cancel", "stop", "abort", "kill"]) {
            return Some(Intent::GoalControl { goal_id: id, action: GoalAction::Cancel });
        }
        if starts_with_any(t, &["retry", "resume", "continue", "restart", "unblock"]) {
            return Some(Intent::GoalControl { goal_id: id, action: GoalAction::Resume });
        }
        if starts_with_any(t, &["merge", "land"]) || t.starts_with("apply merge") {
            return Some(Intent::MergeApply { goal_id: id });
        }
        if starts_with_any(t, &["amend", "change", "update", "edit"]) && !rest.is_empty() {
            return Some(Intent::GoalControl { goal_id: id, action: GoalAction::Amend { text: rest.to_owned() } });
        }
        if has_any(t, &["status", "how is", "how's", "hows", "what happened", "progress", "state of"]) {
            return Some(Intent::Status);
        }
        return None;
    }

    if has_any(t, &["restart the daemon", "restart daemon", "restart divisi", "restart the runtime"]) {
        return Some(Intent::Config { what: "daemon restart".into() });
    }
    if starts_with_any(t, &["add", "remove", "set", "enable", "disable", "delete", "rotate"])
        && has_any(t, &["provider", "api key", " key", "account", "plugin", "mcp server", "mcp "])
    {
        return Some(Intent::Config { what: original.to_owned() });
    }
    if has_any(t, &["how much", "usage", "tokens", "spent", "burned", "quota"])
        && has_any(t, &["used", "usage", "spent", "burned", "tokens", "quota", "left", "remaining"])
    {
        return Some(Intent::Usage);
    }
    if has_any(t, &["pool", "providers", "which agents", "what agents", "who is available", "who's available", "capacity", "rate limit"]) {
        return Some(Intent::PoolQuery);
    }
    if t == "status"
        || has_any(t, &["what's going on", "whats going on", "what is going on", "how are things", "what's running", "whats running", "what is running", "anything running", "any goals", "active goals"])
    {
        return Some(Intent::Status);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cancel(id: &str) -> Intent {
        Intent::GoalControl { goal_id: id.into(), action: GoalAction::Cancel }
    }

    #[test]
    fn status_phrasings() {
        for t in ["status", "Status?", "what's going on", "how are things?", "is anything running", "what is running right now", "any goals active?"] {
            assert_eq!(parse(t), Some(Intent::Status), "{t:?}");
        }
    }

    #[test]
    fn usage_phrasings() {
        for t in ["how much have I used", "what's my usage", "how many tokens did I burn", "how much quota is left?", "usage today"] {
            assert_eq!(parse(t), Some(Intent::Usage), "{t:?}");
        }
    }

    #[test]
    fn pool_phrasings() {
        for t in ["how is the pool", "which agents are available", "who's available?", "any provider rate limited", "what's the pool capacity"] {
            assert_eq!(parse(t), Some(Intent::PoolQuery), "{t:?}");
        }
    }

    #[test]
    fn goal_control_needs_a_goal_id() {
        assert_eq!(parse("cancel goal_dli1afoi2biy_0009"), Some(cancel("goal_dli1afoi2biy_0009")));
        assert_eq!(parse("Stop goal_abc_1!"), Some(cancel("goal_abc_1")));
        assert_eq!(parse("retry goal_abc_1"), Some(Intent::GoalControl { goal_id: "goal_abc_1".into(), action: GoalAction::Resume }));
        assert_eq!(parse("resume goal_abc_1 please"), Some(Intent::GoalControl { goal_id: "goal_abc_1".into(), action: GoalAction::Resume }));
        assert_eq!(parse("cancel everything"), None, "no id, so it is not a rule: the model or a clarification handles it");
    }

    #[test]
    fn amend_keeps_the_users_own_words() {
        assert_eq!(
            parse("amend goal_abc_1: Only touch the Parser module"),
            Some(Intent::GoalControl { goal_id: "goal_abc_1".into(), action: GoalAction::Amend { text: "Only touch the Parser module".into() } })
        );
        assert_eq!(parse("amend goal_abc_1"), None, "an amend with no new text is not actionable");
    }

    #[test]
    fn merge_and_status_of_a_specific_goal() {
        assert_eq!(parse("merge goal_abc_1"), Some(Intent::MergeApply { goal_id: "goal_abc_1".into() }));
        assert_eq!(parse("apply merge goal_abc_1"), Some(Intent::MergeApply { goal_id: "goal_abc_1".into() }));
        assert_eq!(parse("how is goal_abc_1 doing"), Some(Intent::Status));
    }

    #[test]
    fn config_changes_are_recognised_so_the_gate_can_catch_them() {
        assert_eq!(parse("restart the daemon"), Some(Intent::Config { what: "daemon restart".into() }));
        assert!(matches!(parse("add a provider called groq"), Some(Intent::Config { .. })));
        assert!(matches!(parse("rotate the openai api key"), Some(Intent::Config { .. })));
        assert!(matches!(parse("disable the slack mcp server"), Some(Intent::Config { .. })));
    }

    #[test]
    fn everything_else_goes_to_the_model() {
        for t in ["write a parser for the config format", "add tests for the tokenizer", "refactor the scheduler", "hello", "  ", ""] {
            assert_eq!(parse(t), None, "{t:?}");
        }
    }

    #[test]
    fn goal_id_extraction() {
        assert_eq!(find_goal_id("cancel goal_x1_0002 now"), Some("goal_x1_0002".into()));
        assert_eq!(find_goal_id("the goal is to ship"), None);
        assert_eq!(find_goal_id("goal_"), None);
    }
}
