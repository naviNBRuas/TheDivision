//! Runs an approved [`Intent`] by calling the daemon's existing requests, so chat can do
//! exactly what the CLI can and nothing more.

use super::{gate::ChatConfig, reply, GoalAction, Intent};
use crate::context::Context;
use anyhow::{bail, Result};
use divisi_protocol::{Request, Response, ResponseData};

/// What to say back, and which goals it touched.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reply {
    pub text: String,
    pub goal_ids: Vec<String>,
}

impl Reply {
    fn say(text: impl Into<String>) -> Self {
        Self { text: text.into(), goal_ids: vec![] }
    }
    fn about(text: impl Into<String>, goal_id: &str) -> Self {
        Self { text: text.into(), goal_ids: vec![goal_id.to_owned()] }
    }
}

fn call(ctx: &Context, request: Request) -> Result<ResponseData> {
    match crate::handlers::handle(ctx, request) {
        Response::Ok { data } => Ok(data),
        Response::Error { message } => bail!(message),
    }
}

fn snapshot(ctx: &Context) -> Result<divisi_protocol::CoordinatorSnapshot> {
    match call(ctx, Request::CoordinatorStatus)? {
        ResponseData::CoordinatorSnapshot(s) => Ok(s),
        other => bail!("unexpected response to CoordinatorStatus: {other:?}"),
    }
}

/// `answer` produces a model answer for a `Question`; it returns `None` when no model is reachable.
pub fn execute(ctx: &Context, session_id: &str, intent: &Intent, cfg: &ChatConfig, agent: Option<&str>, answer: &dyn Fn(&str) -> Option<String>) -> Result<Reply> {
    Ok(match intent {
        Intent::Status => Reply::say(reply::status_text(&snapshot(ctx)?)),
        Intent::PoolQuery => {
            let ResponseData::PoolStatus(pool) = call(ctx, Request::PoolStatus)? else { bail!("unexpected response to PoolStatus") };
            Reply::say(reply::pool_text(&pool, &snapshot(ctx)?))
        }
        Intent::Usage => {
            let ResponseData::Usage(u) = call(ctx, Request::UsageShow { provider: None })? else { bail!("unexpected response to UsageShow") };
            Reply::say(reply::usage_text(&u))
        }
        Intent::GoalCreate { text, mode } => {
            let mode = mode.clone().unwrap_or_else(|| cfg.default_mode.clone());
            let ResponseData::GoalId(id) = call(
                ctx,
                Request::GoalSubmit { session_id: session_id.to_owned(), text: text.clone(), mode: Some(mode.clone()), max_dispatches: None, max_minutes: None, agent: agent.map(str::to_owned) },
            )? else {
                bail!("unexpected response to GoalSubmit")
            };
            Reply::about(format!("Started {id} in {mode} mode. Progress will show up here."), &id)
        }
        Intent::GoalControl { goal_id, action } => {
            let request = match action {
                GoalAction::Resume => Request::GoalResume { goal_id: goal_id.clone() },
                GoalAction::Cancel => Request::GoalCancel { goal_id: goal_id.clone() },
                GoalAction::Amend { text } => Request::GoalAmend { goal_id: goal_id.clone(), text: text.clone() },
            };
            call(ctx, request)?;
            let done = match action {
                GoalAction::Resume => "Resumed",
                GoalAction::Cancel => "Cancelled",
                GoalAction::Amend { .. } => "Amended",
            };
            Reply::about(format!("{done} {goal_id}."), goal_id)
        }
        Intent::MergeApply { goal_id } => {
            let ResponseData::PendingMerges(all) = call(ctx, Request::GoalMergeList)? else { bail!("unexpected response to GoalMergeList") };
            let pending: Vec<_> = all.into_iter().filter(|m| m.goal_id == *goal_id && m.status == "pending").collect();
            if pending.is_empty() {
                Reply::about(format!("There is no pending merge for {goal_id}."), goal_id)
            } else {
                for m in &pending {
                    call(ctx, Request::GoalMergeResolve { id: m.id, allow: true })?;
                }
                Reply::about(format!("Merged {} branch(es) for {goal_id}.", pending.len()), goal_id)
            }
        }
        // Approved, but changing configuration from chat is not wired up yet. Say so plainly
        // instead of pretending, so nothing is silently skipped.
        Intent::Config { what } => {
            // Simple static parser: split into words
            let mut words = what.split_whitespace();
            match words.next() {
                Some("enable") => match words.next() {
                    Some("mcp") => match words.next() {
                        Some(name) => {
                            // ignore rest
                            call(ctx, Request::McpEnable { name: name.to_owned() })?;
                            Reply::say(format!("Enabled MCP {name}.") )
                        }
                        None => Reply::say("Specify MCP name to enable."),
                    },
                    _ => Reply::say("Unsupported enable command."),
                },
                Some("disable") => match words.next() {
                    Some("mcp") => match words.next() {
                        Some(name) => {
                            call(ctx, Request::McpDisable { name: name.to_owned() })?;
                            Reply::say(format!("Disabled MCP {name}.") )
                        }
                        None => Reply::say("Specify MCP name to disable."),
                    },
                    _ => Reply::say("Unsupported disable command."),
                },
                Some("remove") => match words.next() {
                    Some("mcp") => match words.next() {
                        Some(name) => {
                            call(ctx, Request::McpRemove { name: name.to_owned() })?;
                            Reply::say(format!("Removed MCP {name}.") )
                        }
                        None => Reply::say("Specify MCP name to remove."),
                    },
                    _ => Reply::say("Unsupported remove command."),
                },
                Some("set") => match words.next() {
                    Some("provider") => {
                        let name = words.next().unwrap_or("");
                        match words.next() {
                            Some("key") => {
                                let value = words.collect::<Vec<_>>().join(" ") ;
                                if !value.is_empty() {
                                    call(ctx, Request::ProviderSetKey { name: name.to_owned(), value: value.clone() })?;
                                    Reply::say(format!("Set provider key for {name}.") )
                                } else {
                                    Reply::say("Specify key value.")
                                }
                            }
                            _ => Reply::say("Unsupported provider set command."),
                        }
                    }
                    _ => Reply::say("Unsupported set command."),
                },
                Some("add") => match words.next() {
                    Some("mcp") => {
                        // For brevity, ignore this
                        Reply::say("MCP add not implemented yet.")
                    }
                    _ => Reply::say("Unsupported add command."),
                },
                _ => Reply::say(format!("Unsupported config action: {what}")),
            }
        }
        Intent::Question { text } => match answer(text) {
            Some(a) => Reply::say(a),
            None => Reply::say("I can’t answer that without a model right now. I can still report status, usage and pool health."),
        },
        Intent::Clarify { what } => Reply::say(what.clone()),
    })
}
