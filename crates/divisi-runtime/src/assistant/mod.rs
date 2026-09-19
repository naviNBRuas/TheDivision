//! The conversational assistant: plain language in, goals and answers out.
//!
//! One message becomes one [`Intent`]: [`rules`] resolve the obvious ones for free, a routed
//! pool model resolves the rest, and [`gate`] decides whether an intent runs at once or waits
//! for your confirmation. Nothing a model proposes runs without passing the gate.

pub mod gate;
pub mod rules;

use serde::{Deserialize, Serialize};

/// What to do with a goal that already exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum GoalAction {
    /// Pick it back up (also what "retry" means).
    Resume,
    /// Change the goal's text or budget.
    Amend { text: String },
    /// Stop it and discard in-flight work.
    Cancel,
}

/// The single structured meaning of a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum Intent {
    Status,
    Usage,
    PoolQuery,
    GoalCreate { text: String, #[serde(default)] mode: Option<String> },
    GoalControl { goal_id: String, #[serde(flatten)] action: GoalAction },
    MergeApply { goal_id: String },
    /// Provider, key, account, MCP, plugin or daemon changes.
    Config { what: String },
    /// A question answered from daemon state or by a model, creating no goal.
    Question { text: String },
    /// Divisi needs one more detail before it can act.
    Clarify { what: String },
}

impl Intent {
    /// Short name for logs and the reply's `intent` field.
    pub fn name(&self) -> &'static str {
        match self {
            Intent::Status => "status",
            Intent::Usage => "usage",
            Intent::PoolQuery => "pool_query",
            Intent::GoalCreate { .. } => "goal_create",
            Intent::GoalControl { .. } => "goal_control",
            Intent::MergeApply { .. } => "merge_apply",
            Intent::Config { .. } => "config",
            Intent::Question { .. } => "question",
            Intent::Clarify { .. } => "clarify",
        }
    }
}
