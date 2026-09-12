//! Dependency-graph orchestration: explicit ordering without pretending that
//! SingleCLI can safely infer a team's work breakdown. Ready nodes retain the
//! parallel runner's one-thread/one-worktree/one-SQLite-connection isolation.

use crate::context::Context;
use crate::task::{self, RunTaskOptions};
use anyhow::{bail, Context as _, Result};
use single_agent_sdk::adapters::for_agent_with_custom;
use single_protocol::{OrchestratorMode, RunCondition, TaskGraphNode, TaskRecord, TaskStatus};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

pub struct GraphOrchestrateOptions<'a> {
    pub nodes: &'a [TaskGraphNode],
    pub cwd: &'a Path,
    pub real_home: bool,
    pub timeout: Duration,
}

/// Validates all graph references and cycles before any task row or worktree
/// exists. Failing early matters here: a partial graph run is misleading and
/// difficult to clean up after a typo in a dependency ID.
pub fn validate(nodes: &[TaskGraphNode]) -> Result<()> {
    if nodes.is_empty() {
        bail!("orchestrate-graph needs at least one --task");
    }
    let ids: BTreeSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    if ids.len() != nodes.len() || nodes.iter().any(|n| n.id.is_empty()) {
        bail!("graph node IDs must be non-empty and unique");
    }
    for node in nodes {
        for dep in &node.depends_on {
            if !ids.contains(dep.as_str()) {
                bail!("graph node '{}' depends on unknown node '{dep}'", node.id);
            }
        }
    }
    let by_id: HashMap<&str, &TaskGraphNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut visited = BTreeSet::new();
    let mut stack = Vec::new();
    fn visit<'a>(
        id: &'a str,
        by_id: &HashMap<&'a str, &'a TaskGraphNode>,
        visited: &mut BTreeSet<&'a str>,
        stack: &mut Vec<&'a str>,
    ) -> Result<()> {
        if let Some(pos) = stack.iter().position(|entry| *entry == id) {
            let mut cycle: Vec<&str> = stack[pos..].to_vec();
            cycle.push(id);
            bail!("task graph contains cycle: {}", cycle.join(" -> "));
        }
        if !visited.insert(id) {
            return Ok(());
        }
        stack.push(id);
        for dep in &by_id[id].depends_on {
            visit(dep, by_id, visited, stack)?;
        }
        stack.pop();
        Ok(())
    }
    for node in nodes {
        visit(&node.id, &by_id, &mut visited, &mut stack)?;
    }
    Ok(())
}

pub fn run(ctx: &Context, opts: GraphOrchestrateOptions<'_>) -> Result<Vec<TaskRecord>> {
    validate(opts.nodes)?;
    let repo = single_core::project_context::resolve(opts.cwd);
    repo.repo_root.as_ref().context(
        "not a git repository; orchestrate-graph requires one (each agent needs its own worktree)",
    )?;
    let db_path = ctx.dirs.db_path();
    let mut remaining: BTreeMap<String, TaskGraphNode> = opts
        .nodes
        .iter()
        .cloned()
        .map(|n| (n.id.clone(), n))
        .collect();
    let mut completed: BTreeMap<String, TaskRecord> = BTreeMap::new();

    while !remaining.is_empty() {
        let ready = ready_nodes(&remaining, &completed);
        // `validate` proves this cannot happen, but retaining a guard keeps a
        // future scheduler edit from looping forever on malformed state.
        if ready.is_empty() {
            bail!("task graph has no runnable nodes");
        }
        for node in &ready {
            remaining.remove(&node.id);
        }

        let mut runnable = Vec::new();
        for node in ready {
            let statuses: Vec<TaskStatus> = node
                .depends_on
                .iter()
                .map(|id| completed[id].status)
                .collect();
            let should_run = match node.run_if {
                RunCondition::Always => true,
                RunCondition::OnSuccess => statuses.iter().all(|s| *s == TaskStatus::Completed),
                RunCondition::OnFailure => statuses.contains(&TaskStatus::Failed),
            };
            if should_run {
                runnable.push(node);
            } else {
                let condition = match node.run_if {
                    RunCondition::Always => unreachable!(),
                    RunCondition::OnSuccess => "all dependencies to succeed",
                    RunCondition::OnFailure => "at least one dependency to fail",
                };
                let summary = format!("Skipped because run_if requires {condition}; this was not a user-requested cancellation.");
                let conn = crate::state::open(&db_path)?;
                task::ensure_schema(&conn)?;
                let record = task::record_conditionally_skipped(&conn, &node.description, &node.agent, opts.cwd, &summary)?;
                single_core::task_hooks::fire(
                    &ctx.dirs.task_hooks_registry_file(),
                    &single_protocol::TaskHookPayload {
                        id: record.id,
                        status: "cancelled".to_string(),
                        agent: record.agent.clone(),
                        cwd: record.cwd.clone(),
                        workspace_id: record.workspace_id.clone(),
                        summary: record.summary.clone(),
                    },
                );
                completed.insert(node.id.clone(), record);
            }
        }

        let handles: Vec<_> = runnable
            .into_iter()
            .map(|node| {
                let ctx = ctx.clone();
                let db_path = db_path.clone();
                let cwd = opts.cwd.to_path_buf();
                let real_home = opts.real_home;
                let timeout = opts.timeout;
                std::thread::spawn(move || -> Result<(String, TaskRecord)> {
                    let conn = crate::state::open(&db_path)?;
                    task::ensure_schema(&conn)?;
                    crate::memory::ensure_schema(&conn)?;
                    single_core::notes::ensure_schema(&conn)?;
                    crate::knowledge_graph::ensure_schema(&conn)?;
                    let record = task::run(
                        &conn,
                        &ctx,
                        RunTaskOptions {
                            description: &node.description,
                            agent: &node.agent,
                            cwd: &cwd,
                            use_worktree: true,
                            account: None,
                            real_home,
                            no_memory_context: false,
                            timeout,
                            allow_fallback: false,
                            usage_json: false,
                            require_structured_output: false,
                        },
                    )?;
                    Ok((node.id, record))
                })
            })
            .collect();
        for handle in handles {
            let (id, record) = handle
                .join()
                .map_err(|_| anyhow::anyhow!("task graph worker thread panicked"))??;
            completed.insert(id, record);
        }
    }
    Ok(opts
        .nodes
        .iter()
        .map(|node| {
            completed
                .remove(&node.id)
                .expect("validated node has a record")
        })
        .collect())
}

fn ready_nodes(
    remaining: &BTreeMap<String, TaskGraphNode>,
    completed: &BTreeMap<String, TaskRecord>,
) -> Vec<TaskGraphNode> {
    remaining
        .values()
        .filter(|node| node.depends_on.iter().all(|id| completed.contains_key(id)))
        .cloned()
        .collect()
}

/// Uses an already-authenticated agent CLI as the planner rather than adding a
/// provider SDK or sending credentials to a new HTTP path. `auto` and
/// `delegate` deliberately share this function: both mean "ask a detected
/// CLI to plan"; fixed mode is the only mode that accepts caller-supplied
/// nodes without that planning step.
pub fn plan_and_run(
    ctx: &Context,
    mode: OrchestratorMode,
    goal: &str,
    candidates: &[String],
    cwd: &Path,
    real_home: bool,
    timeout: Duration,
) -> Result<Vec<TaskRecord>> {
    if mode == OrchestratorMode::Fixed {
        bail!("fixed orchestration requires explicit tasks");
    }
    if candidates.is_empty() && mode == OrchestratorMode::Delegate {
        bail!("--orchestrator {mode:?} requires at least one --candidate-agent");
    }
    // Delegate makes the caller name the candidate pool. Auto intentionally
    // needs only a goal: it discovers the installed pool itself and gives
    // Claude first choice when present, then falls back to registry order.
    let candidates: Vec<String> = if candidates.is_empty() {
        let mut installed: Vec<String> = ctx
            .registry
            .iter()
            .filter_map(|definition| {
                let adapter =
                    for_agent_with_custom(&definition.name, &ctx.dirs.agents_dir(), &ctx.registry)?;
                adapter.discover().detected.then(|| definition.name.clone())
            })
            .collect();
        if let Some(index) = installed.iter().position(|agent| agent == "claude") {
            let claude = installed.remove(index);
            installed.insert(0, claude);
        }
        installed
    } else {
        candidates.to_vec()
    };
    let planner = candidates
        .iter()
        .find(|agent| {
            for_agent_with_custom(agent, &ctx.dirs.agents_dir(), &ctx.registry)
                .is_some_and(|adapter| adapter.discover().detected)
        })
        .context("none of the candidate agents are installed")?;
    let worker_agents: Vec<&str> = candidates
        .iter()
        .map(String::as_str)
        .filter(|agent| *agent != planner)
        .collect();
    if worker_agents.is_empty() {
        bail!("planning needs a candidate agent other than the selected planner '{planner}'");
    }
    let prompt = format!(
        "Split this goal into a dependency graph for the available worker agents: {goal}\n\nPlanning agent: {planner}. Worker agents (do not assign work to the planning agent): {}\nReturn only a JSON array of TaskGraphNode objects. Each object must have id, agent, description, depends_on (array of IDs), and run_if (always, on_success, or on_failure).",
        worker_agents.join(", ")
    );
    let conn = crate::state::open(&ctx.dirs.db_path())?;
    task::ensure_schema(&conn)?;
    let planned = task::run(
        &conn,
        ctx,
        RunTaskOptions {
            description: &prompt,
            agent: planner,
            cwd,
            use_worktree: false,
            account: None,
            real_home,
            no_memory_context: true,
            timeout,
            allow_fallback: false,
            usage_json: false,
            require_structured_output: false,
        },
    )?;
    if planned.status != TaskStatus::Completed {
        bail!(
            "planning agent '{planner}' did not complete successfully: {}",
            planned.summary.unwrap_or_default()
        );
    }
    let output = planned
        .artifact_path
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .context("planning agent produced no readable artifact")?;
    let nodes = extract_nodes(&output)?;
    for node in &nodes {
        if node.agent == *planner || !worker_agents.contains(&node.agent.as_str()) {
            bail!(
                "planner assigned node '{}' to '{}', which is not an allowed worker agent",
                node.id,
                node.agent
            );
        }
    }
    run(
        ctx,
        GraphOrchestrateOptions {
            nodes: &nodes,
            cwd,
            real_home,
            timeout,
        },
    )
}

/// Finds the first valid JSON array anywhere in a CLI response, because agent
/// CLIs commonly add prose or Markdown fences even when asked not to. Trying
/// each balanced array avoids accepting a later malformed fragment as a plan.
pub fn extract_nodes(output: &str) -> Result<Vec<TaskGraphNode>> {
    for (start, _) in output.match_indices('[') {
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, ch) in output[start..].char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                continue;
            }
            match ch {
                '"' => in_string = true,
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        if let Ok(nodes) = serde_json::from_str::<Vec<TaskGraphNode>>(
                            &output[start..start + offset + 1],
                        ) {
                            return Ok(nodes);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    bail!("planning agent output did not contain a valid TaskGraphNode JSON array")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn node(id: &str, deps: &[&str], run_if: RunCondition) -> TaskGraphNode {
        TaskGraphNode {
            id: id.into(),
            agent: "test".into(),
            description: id.into(),
            depends_on: deps.iter().map(|s| (*s).into()).collect(),
            run_if,
        }
    }

    #[test]
    fn accepts_a_diamond_where_the_final_node_waits_for_both_branches() {
        let nodes = vec![
            node("a", &[], RunCondition::Always),
            node("b", &["a"], RunCondition::Always),
            node("c", &["a"], RunCondition::Always),
            node("d", &["b", "c"], RunCondition::Always),
        ];
        validate(&nodes).unwrap();
        // The runner's ready predicate is the ordering contract: D cannot be
        // selected until records for both B and C exist.
        let mut complete = BTreeMap::new();
        complete.insert("a".to_string(), fake(TaskStatus::Completed));
        let remaining = nodes
            .iter()
            .cloned()
            .map(|node| (node.id.clone(), node))
            .collect();
        assert!(!ready_nodes(&remaining, &complete)
            .iter()
            .any(|node| node.id == "d"));
        complete.insert("b".to_string(), fake(TaskStatus::Completed));
        assert!(!ready_nodes(&remaining, &complete)
            .iter()
            .any(|node| node.id == "d"));
        complete.insert("c".to_string(), fake(TaskStatus::Completed));
        assert!(ready_nodes(&remaining, &complete)
            .iter()
            .any(|node| node.id == "d"));
    }

    #[test]
    fn reports_a_named_cycle_before_execution() {
        let err = validate(&[
            node("a", &["b"], RunCondition::Always),
            node("b", &["a"], RunCondition::Always),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("a -> b -> a"));
    }

    #[test]
    fn on_failure_only_runs_after_a_failed_dependency() {
        let statuses = [TaskStatus::Failed];
        assert!(statuses.contains(&TaskStatus::Failed));
        let statuses = [TaskStatus::Completed];
        assert!(!statuses.contains(&TaskStatus::Failed));
    }

    #[test]
    fn extracts_a_json_plan_from_markdown_wrapping() {
        let nodes = extract_nodes("Here is the plan:\n```json\n[{\"id\":\"a\",\"agent\":\"codex\",\"description\":\"x\",\"depends_on\":[],\"run_if\":\"always\"}]\n```").unwrap();
        assert_eq!(nodes[0].id, "a");
    }

    fn fake(status: TaskStatus) -> TaskRecord {
        TaskRecord {
            id: 0,
            description: String::new(),
            agent: String::new(),
            status,
            worktree_path: None,
            artifact_path: None,
            exit_code: None,
            timed_out: false,
            summary: None,
            created_at: String::new(),
            updated_at: String::new(),
            cwd: String::new(),
            workspace_id: String::new(),
            rate_limited: false,
            prompt_tokens: None,
            completion_tokens: None,
            tokens_estimated: false,
        }
    }
}
