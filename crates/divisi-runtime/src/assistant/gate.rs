//! The risk gate. Applied to the parsed [`Intent`], never to a prompt: a model can only
//! propose an intent, and nothing runs until the gate has classified it.

use super::{GoalAction, Intent};
use divisi_core::DivisiDirs;

/// `[chat]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatConfig {
    /// How long an unanswered confirmation stays valid.
    pub confirm_expiry_secs: u64,
    /// A plan with more nodes than this is paused for approval.
    pub fanout_cap: usize,
    /// A goal whose text contains one of these words needs confirmation.
    pub risky_verbs: Vec<String>,
    /// Goal mode used when a message does not say (`auto`, `plan`, `careful` or `dry`).
    pub default_mode: String,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            confirm_expiry_secs: 1800,
            fanout_cap: 12,
            risky_verbs: ["push", "publish", "deploy", "release", "delete", "drop", "force", "wipe", "destroy"].map(String::from).to_vec(),
            default_mode: "auto".into(),
        }
    }
}

impl ChatConfig {
    pub fn load(dirs: &DivisiDirs) -> Self {
        let mut cfg = Self::default();
        let Ok(text) = std::fs::read_to_string(dirs.config_file()) else { return cfg };
        let Ok(doc) = text.parse::<toml::Table>() else { return cfg };
        let Some(chat) = doc.get("chat").and_then(|c| c.as_table()) else { return cfg };
        if let Some(n) = chat.get("confirm_expiry_secs").and_then(|v| v.as_integer()).filter(|n| *n > 0) {
            cfg.confirm_expiry_secs = n as u64;
        }
        if let Some(n) = chat.get("fanout_cap").and_then(|v| v.as_integer()).filter(|n| *n > 0) {
            cfg.fanout_cap = n as usize;
        }
        if let Some(list) = chat.get("risky_verbs").and_then(|v| v.as_array()) {
            cfg.risky_verbs = list.iter().filter_map(|v| v.as_str()).map(|s| s.to_lowercase()).collect();
        }
        if let Some(m) = chat.get("default_mode").and_then(|v| v.as_str()).filter(|m| ["auto", "plan", "careful", "dry"].contains(m)) {
            cfg.default_mode = m.to_owned();
        }
        cfg
    }
}

/// What the gate decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Safe to run now.
    Run,
    /// Ask first. `resource` keys the approval store; `remember_ok` says whether "always allow" may be offered.
    Confirm { resource: String, summary: String, remember_ok: bool },
}

/// The first configured risky verb that appears as a whole word in `text`.
pub fn risky_verb<'a>(text: &str, verbs: &'a [String]) -> Option<&'a str> {
    let words: Vec<String> = text.split(|c: char| !c.is_alphanumeric()).map(|w| w.to_lowercase()).collect();
    verbs.iter().map(String::as_str).find(|v| words.iter().any(|w| w == v))
}

pub fn classify(intent: &Intent, cfg: &ChatConfig) -> Verdict {
    match intent {
        Intent::GoalControl { goal_id, action: GoalAction::Cancel } => Verdict::Confirm {
            resource: "chat:goal.cancel".into(),
            summary: format!("cancel {goal_id} and discard its in-flight work"),
            remember_ok: true,
        },
        Intent::MergeApply { goal_id } => Verdict::Confirm {
            resource: "chat:merge.apply".into(),
            summary: format!("merge {goal_id}'s worktree changes onto a real branch"),
            remember_ok: false,
        },
        Intent::Config { what } => Verdict::Confirm {
            resource: "chat:config".into(),
            summary: format!("change configuration: {what}"),
            remember_ok: false,
        },
        Intent::GoalCreate { text, .. } => match risky_verb(text, &cfg.risky_verbs) {
            Some(verb) => Verdict::Confirm {
                resource: "chat:goal.create.risky".into(),
                summary: format!("start a goal that mentions \"{verb}\": {text}"),
                remember_ok: true,
            },
            None => Verdict::Run,
        },
        Intent::Status
        | Intent::Usage
        | Intent::PoolQuery
        | Intent::Question { .. }
        | Intent::Clarify { .. }
        | Intent::GoalControl { action: GoalAction::Resume | GoalAction::Amend { .. }, .. } => Verdict::Run,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn confirm(v: &Verdict) -> (&str, bool) {
        match v {
            Verdict::Confirm { resource, remember_ok, .. } => (resource.as_str(), *remember_ok),
            Verdict::Run => panic!("expected a confirmation, got Run"),
        }
    }

    #[test]
    fn harmless_intents_run_immediately() {
        let cfg = ChatConfig::default();
        for i in [
            Intent::Status,
            Intent::Usage,
            Intent::PoolQuery,
            Intent::Question { text: "what does the scheduler do".into() },
            Intent::Clarify { what: "which goal".into() },
            Intent::GoalCreate { text: "add tests for the parser".into(), mode: None },
            Intent::GoalControl { goal_id: "goal_a_1".into(), action: GoalAction::Resume },
            Intent::GoalControl { goal_id: "goal_a_1".into(), action: GoalAction::Amend { text: "narrower".into() } },
        ] {
            assert_eq!(classify(&i, &cfg), Verdict::Run, "{i:?}");
        }
    }

    #[test]
    fn cancel_needs_confirmation_and_may_be_remembered() {
        let v = classify(&Intent::GoalControl { goal_id: "goal_a_1".into(), action: GoalAction::Cancel }, &ChatConfig::default());
        assert_eq!(confirm(&v), ("chat:goal.cancel", true));
    }

    #[test]
    fn merge_and_config_need_confirmation_and_are_never_remembered() {
        let cfg = ChatConfig::default();
        assert_eq!(confirm(&classify(&Intent::MergeApply { goal_id: "goal_a_1".into() }, &cfg)), ("chat:merge.apply", false));
        assert_eq!(confirm(&classify(&Intent::Config { what: "rotate a key".into() }, &cfg)), ("chat:config", false));
    }

    #[test]
    fn a_risky_verb_in_a_goal_needs_confirmation() {
        let cfg = ChatConfig::default();
        for text in ["push the branch to origin", "Deploy the site", "delete the old logs", "please FORCE the migration", "publish v2"] {
            let v = classify(&Intent::GoalCreate { text: text.into(), mode: None }, &cfg);
            assert_eq!(confirm(&v), ("chat:goal.create.risky", true), "{text:?}");
        }
    }

    #[test]
    fn risky_verbs_match_whole_words_only() {
        let cfg = ChatConfig::default();
        // "pushback" and "released" contain a risky verb only as a substring or another form.
        for text in ["handle pushback in the parser", "the released notes are done", "reproduce the dropdown bug"] {
            assert_eq!(classify(&Intent::GoalCreate { text: text.into(), mode: None }, &cfg), Verdict::Run, "{text:?}");
        }
    }

    #[test]
    fn config_defaults() {
        let cfg = ChatConfig::default();
        assert_eq!((cfg.confirm_expiry_secs, cfg.fanout_cap), (1800, 12));
        assert!(cfg.risky_verbs.iter().any(|v| v == "deploy"));
    }

    #[test]
    fn config_reads_the_chat_section_and_ignores_nonsense() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = DivisiDirs::from_root(dir.path().to_path_buf());
        std::fs::write(dirs.config_file(), "[chat]\nconfirm_expiry_secs = 60\nfanout_cap = 3\nrisky_verbs = [\"Ship\", \"nuke\"]\ndefault_mode = \"plan\"\n").unwrap();
        let cfg = ChatConfig::load(&dirs);
        assert_eq!((cfg.confirm_expiry_secs, cfg.fanout_cap), (60, 3));
        assert_eq!(cfg.risky_verbs, ["ship", "nuke"], "lower-cased");
        assert_eq!(cfg.default_mode, "plan");

        std::fs::write(dirs.config_file(), "[chat]\nconfirm_expiry_secs = -5\nfanout_cap = \"lots\"\n").unwrap();
        assert_eq!(ChatConfig::load(&dirs), ChatConfig::default(), "bad values fall back to defaults");

        std::fs::write(dirs.config_file(), "this is = not [valid toml").unwrap();
        assert_eq!(ChatConfig::load(&dirs), ChatConfig::default(), "a broken file never breaks chat");
    }
}
