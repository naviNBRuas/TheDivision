//! the LLM brain roles (spec E27.02 §5): planner (goal → TaskGraph),
//! supervisor (patch the graph on failure), integrator (verify +
//! summarise). each is one `task::run` against a routed agent with a
//! strict "output ONLY JSON" prompt and a first-valid-JSON extractor
//! ported (and hardened — string/escape aware) from the Python `single-acp`
//! prototype.
//!
//! no agent is pinned — every role routes through `routing::select_agent`
//! with the same pool-health filter and dispatch-time fallback as work
//! nodes, so the coordinator never stalls on one agent's rate limit even
//! for its own thinking (spec §11.1).
//!
//! the pure parsers (`extract_first_json`, `parse_plan`, `parse_patch_ops`,
//! `parse_integration`) are unit-tested with fixtures. the live `plan` /
//! `supervise` / `integrate` calls are covered only by `#[ignore]`
//! integration tests, same as `orchestrate`'s real-agent tests.

use crate::coordinator::graph::{Effort, Node, NodeKind, NodeStatus, PatchOp, TaskGraph};
use crate::coordinator::routing::{self, PoolHealth, RoutingTable};
use crate::context::Context;
use anyhow::{bail, Context as _, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// scans `s` for the first balanced `{...}` or `[...]` span that parses as
/// JSON. unlike the Python prototype's version this respects string
/// literals and escapes, so a `}` inside a quoted value does not close the
/// span early. returns `None` if nothing balanced parses.
pub fn extract_first_json(s: &str) -> Option<Value> {
    let bytes = s.as_bytes();
    let mut start = 0usize;
    while start < bytes.len() {
        let open = bytes[start];
        if open != b'{' && open != b'[' {
            start += 1;
            continue;
        }
        let close = if open == b'{' { b'}' } else { b']' };
        let mut depth = 0i32;
        let mut in_str = false;
        let mut escaped = false;
        for (offset, &c) in bytes[start..].iter().enumerate() {
            if in_str {
                if escaped {
                    escaped = false;
                } else if c == b'\\' {
                    escaped = true;
                } else if c == b'"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                b'"' => in_str = true,
                x if x == open => depth += 1,
                x if x == close => {
                    depth -= 1;
                    if depth == 0 {
                        let span = &s[start..start + offset + 1];
                        if let Ok(v) = serde_json::from_str::<Value>(span) {
                            return Some(v);
                        }
                        break; // unbalanced-looking; advance start past this opener
                    }
                }
                _ => {}
            }
        }
        start += 1;
    }
    None
}

/// one element of the planner's output array.
#[derive(Debug, Clone, Deserialize)]
pub struct PlanNodeSpec {
    pub id: String,
    pub desc: String,
    pub kind: NodeKind,
    pub effort: Effort,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// integrator output (spec §5.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntegrationOutcome {
    pub summary: String,
    #[serde(default)]
    pub files_changed: Vec<String>,
    #[serde(default)]
    pub checks_pass: bool,
    #[serde(default)]
    pub residual_gaps: Vec<String>,
    /// integrator judged the result unrecoverable → goal fails.
    #[serde(default)]
    pub unrecoverable: bool,
}

pub fn parse_plan(v: &Value) -> Result<Vec<PlanNodeSpec>> {
    let arr = v.as_array().context("planner output is not a JSON array")?;
    let specs: Vec<PlanNodeSpec> =
        serde_json::from_value(Value::Array(arr.clone())).context("planner array has a malformed element")?;
    if specs.is_empty() || specs.len() > 8 {
        bail!("planner returned {} nodes; expected 2–8", specs.len());
    }
    Ok(specs)
}

pub fn parse_patch_ops(v: &Value) -> Result<Vec<PatchOp>> {
    // accept either a bare array of ops or `{ "ops": [ ... ] }`
    let arr = match v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.get("ops").and_then(|x| x.as_array()).cloned().context("no `ops` array")?,
        _ => bail!("supervisor output is neither an array nor an object with `ops`"),
    };
    serde_json::from_value(Value::Array(arr)).context("supervisor patch op is malformed / unknown")
}

pub fn parse_integration(v: &Value) -> Result<IntegrationOutcome> {
    serde_json::from_value(v.clone()).context("integrator output is malformed")
}

/// turns validated planner specs into a persisted-shape `TaskGraph`,
/// assigning each node an agent via routing and the worktree default for
/// its kind. `prefer_pool` only affects work-node routing here (spec §7:
/// `single-pool` slots in as "any coordinator node's agent") — it never
/// touches which agent runs the planner/supervisor/integrator role
/// itself (`plan`/`supervise`/`integrate` below keep their own
/// `select_agent` call unchanged; those need reliable structured JSON
/// output a free-tier pool model isn't confirmed to deliver).
pub fn specs_to_graph(specs: &[PlanNodeSpec], table: &RoutingTable, health: &PoolHealth, prefer_pool: bool) -> TaskGraph {
    let nodes = specs
        .iter()
        .map(|s| Node {
            id: s.id.clone(),
            desc: s.desc.clone(),
            kind: s.kind,
            effort: s.effort,
            agent: routing::select_agent_with_prefer_pool(table, s.kind, s.effort, health, prefer_pool).unwrap_or_default(),
            depends_on: s.depends_on.clone(),
            status: NodeStatus::Pending,
            task_id: None,
            attempts: 0,
            worktree: s.kind.default_worktree(),
            output_ref: None,
            earliest_retry_at_ms: None,
        })
        .collect();
    TaskGraph { nodes }
}

// ---------------------------------------------------------------- live roles

/// reads a finished task's captured output — the artifact file if present,
/// else the stored summary.
fn task_output(rec: &single_protocol::TaskRecord) -> String {
    if let Some(p) = &rec.artifact_path {
        if let Ok(s) = std::fs::read_to_string(p) {
            return s;
        }
    }
    rec.summary.clone().unwrap_or_default()
}

/// Extra attempts after the first when the agent's output doesn't contain
/// parseable JSON. Live-verification finding (2026-09-07): a brain role
/// (planner/supervisor/integrator) is a single stochastic LLM completion
/// with a strict "output ONLY JSON" instruction, and a free-pool model
/// occasionally ignores it (prose, truncation, a stray code fence it
/// forgets to close). Before this, that single bad sample permanently
/// blocked the goal (`run_integrator` maps any `Err` here straight to
/// `GoalStatus::Blocked`) even when every actual work node had already
/// succeeded — confirmed live on a goal whose two nodes both finished
/// fine days earlier but stayed `blocked` on "integrator failed: brain
/// role produced no parseable JSON" forever, since nothing ever retried
/// it. A malformed-JSON response is a content-quality problem a fresh
/// sample usually fixes, not a real infrastructure failure worth
/// escalating on the first miss.
const BRAIN_JSON_RETRIES: u32 = 2;

/// `agent` is the routing table's first pick; each retry re-selects via
/// `select_agent_excluding` (kind/effort/table/health), excluding every
/// agent already tried, so a stuck agent doesn't burn the whole retry
/// budget on itself — see `select_agent_excluding`'s doc comment for the
/// live incident this fixes. Falls back to resampling `agent` itself only
/// once every other candidate has already been excluded (still better
/// than erroring immediately: a transient blip on the same agent can
/// still clear on a second try).
#[allow(clippy::too_many_arguments)]
fn run_role(
    conn: &Connection,
    ctx: &Context,
    agent: &str,
    kind: NodeKind,
    effort: Effort,
    table: &RoutingTable,
    health: &PoolHealth,
    cwd: &std::path::Path,
    prompt: &str,
) -> Result<Value> {
    let mut tried: Vec<String> = Vec::new();
    let mut current = agent.to_string();
    for _ in 0..=BRAIN_JSON_RETRIES {
        tried.push(current.clone());
        let rec = crate::task::run(
            conn,
            ctx,
            crate::task::RunTaskOptions {
                description: prompt,
                agent: &current,
                cwd,
                use_worktree: false,
                account: None,
                real_home: false,
                no_memory_context: true, // brain prompts are self-contained
                timeout: Duration::from_secs(240),
                allow_fallback: true,
                usage_json: false,
                // Every brain role (plan/supervise/integrate) demands
                // strict single-shot JSON — exclude aihorde-class
                // free-pool providers whose wire contract doesn't
                // guarantee that (see `require_structured_output`'s doc).
                require_structured_output: true,
            },
        )?;
        let out = task_output(&rec);
        if let Some(v) = extract_first_json(&out) {
            return Ok(v);
        }
        current = routing::select_agent_excluding(table, kind, effort, health, &tried).unwrap_or(current);
    }
    bail!("brain role produced no parseable JSON after {} attempt(s) across {} agent(s): {}", BRAIN_JSON_RETRIES + 1, tried.len(), tried.join(", "))
}

const PLAN_INSTRUCTION: &str = "\
You are a build planner. Break the GOAL into 2 to 8 concrete subtasks.\n\
Output ONLY a JSON array, no prose. Each element:\n\
{\"id\":\"s1\",\"desc\":\"...\",\"kind\":\"code|test|research|review|docs|infra\",\
\"effort\":\"quick|standard|deep\",\"depends_on\":[]}\n\
depends_on lists the ids that must finish first ([] = independent).\n\
Give code subtasks non-overlapping file scopes so they can run in parallel.\n";

/// planner: goal text (+ cwd context) → validated `TaskGraph`.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    conn: &Connection,
    ctx: &Context,
    goal_text: &str,
    cwd: &std::path::Path,
    table: &RoutingTable,
    health: &PoolHealth,
    prefer_pool: bool,
) -> Result<TaskGraph> {
    let agent = routing::select_agent(table, NodeKind::Plan, Effort::Standard, health)
        .context("no agent available for planning")?;
    let pc = single_core::project_context::resolve(cwd);
    let ctx_blurb = format!(
        "\n\nPROJECT CONTEXT:\ncwd: {}\nrepo root: {}\nbranch: {}\nchanged files: {}\ndocs: {}",
        pc.cwd,
        pc.repo_root.as_deref().unwrap_or("(not a git repo)"),
        pc.branch.as_deref().unwrap_or("(none)"),
        pc.changed_files.len(),
        pc.project_docs.join(", "),
    );
    let prompt = format!("{PLAN_INSTRUCTION}{ctx_blurb}\n\nGOAL:\n{goal_text}\n");
    let v = run_role(conn, ctx, &agent, NodeKind::Plan, Effort::Standard, table, health, cwd, &prompt)?;
    let specs = parse_plan(&v)?;
    Ok(specs_to_graph(&specs, table, health, prefer_pool))
}

/// supervisor: current graph + a failing node's output → a list of patch
/// ops. the caller (scheduler) enforces the per-goal patch cap and turns
/// an over-cap trigger into a `Block`.
#[allow(clippy::too_many_arguments)]
pub fn supervise(
    conn: &Connection,
    ctx: &Context,
    cwd: &std::path::Path,
    graph: &TaskGraph,
    failing_node_id: &str,
    failing_output: &str,
    table: &RoutingTable,
    health: &PoolHealth,
) -> Result<Vec<PatchOp>> {
    let agent = routing::select_agent(table, NodeKind::Supervise, Effort::Standard, health)
        .context("no agent available for supervising")?;
    let graph_json = serde_json::to_string_pretty(graph)?;
    let prompt = format!(
        "You are a build supervisor. A subtask failed. Patch the task graph.\n\
        Output ONLY a JSON array of ops, no prose. Allowed ops:\n\
        {{\"op\":\"retarget\",\"node\":\"s2\",\"kind\":\"research\"|null,\"agent\":\"grok\"|null}}\n\
        {{\"op\":\"split\",\"node\":\"s2\",\"into\":[<node objects>]}}\n\
        {{\"op\":\"mark_optional\",\"node\":\"s2\"}}\n\
        {{\"op\":\"add_dependency\",\"node\":\"s2\",\"on\":\"s1\"}}\n\
        {{\"op\":\"block\",\"node\":\"s2\",\"question\":\"...\"}}\n\
        {{\"op\":\"abort\",\"reason\":\"...\"}}\n\n\
        GRAPH:\n{graph_json}\n\nFAILING NODE: {failing_node_id}\nFAILURE OUTPUT:\n{}\n",
        crate::orchestrate::truncate(failing_output, 4000)
    );
    let v = run_role(conn, ctx, &agent, NodeKind::Supervise, Effort::Standard, table, health, cwd, &prompt)?;
    parse_patch_ops(&v)
}

/// integrator: every node's output + the goal → verify + summarise.
pub fn integrate(
    conn: &Connection,
    ctx: &Context,
    cwd: &std::path::Path,
    goal_text: &str,
    node_outputs: &[(String, String)],
    table: &RoutingTable,
    health: &PoolHealth,
) -> Result<IntegrationOutcome> {
    let agent = routing::select_agent(table, NodeKind::Integrate, Effort::Standard, health)
        .context("no agent available for integrating")?;
    let mut body = String::new();
    for (id, out) in node_outputs {
        body.push_str(&format!("\n--- node {id} ---\n{}\n", crate::orchestrate::truncate(out, 2000)));
    }
    let prompt = format!(
        "You are a build integrator. Verify the subtask results against the GOAL.\n\
        Output ONLY JSON: {{\"summary\":\"...\",\"files_changed\":[],\"checks_pass\":true,\
        \"residual_gaps\":[],\"unrecoverable\":false}}\n\
        Fix only trivial glue (imports, a rename mismatch).\n\n\
        GOAL:\n{goal_text}\n\nSUBTASK RESULTS:{body}\n"
    );
    let v = run_role(conn, ctx, &agent, NodeKind::Integrate, Effort::Standard, table, health, cwd, &prompt)?;
    parse_integration(&v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_json_object_from_surrounding_prose() {
        let s = "sure, here is the plan:\n{\"a\": 1, \"b\": [2, 3]}\nhope that helps";
        let v = extract_first_json(s).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn handles_braces_inside_strings() {
        let s = r#"prefix {"desc": "use a } brace and a ] bracket", "ok": true} suffix"#;
        let v = extract_first_json(s).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["desc"], "use a } brace and a ] bracket");
    }

    #[test]
    fn returns_none_when_no_balanced_json() {
        assert!(extract_first_json("just prose, no json here").is_none());
        assert!(extract_first_json("{ unterminated").is_none());
    }

    #[test]
    fn prefers_first_of_two_json_blocks() {
        let s = "{\"first\": 1}\n\n{\"second\": 2}";
        let v = extract_first_json(s).unwrap();
        assert_eq!(v["first"], 1);
        assert!(v.get("second").is_none());
    }

    #[test]
    fn skips_an_unbalanced_opener_then_finds_the_real_object() {
        let s = "noise { not json at all \n then the real one: {\"real\": 42}";
        let v = extract_first_json(s).unwrap();
        assert_eq!(v["real"], 42);
    }

    #[test]
    fn parse_plan_reads_specs_and_rejects_empty() {
        let v = serde_json::json!([
            {"id":"s1","desc":"write lib","kind":"code","effort":"standard","depends_on":[]},
            {"id":"s2","desc":"test it","kind":"test","effort":"quick","depends_on":["s1"]}
        ]);
        let specs = parse_plan(&v).unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[1].depends_on, vec!["s1"]);

        assert!(parse_plan(&serde_json::json!([])).is_err());
    }

    #[test]
    fn parse_patch_ops_reads_retarget_and_split_and_rejects_unknown() {
        let v = serde_json::json!([
            {"op":"retarget","node":"s1","agent":"grok"},
            {"op":"add_dependency","node":"s2","on":"s1"}
        ]);
        assert_eq!(parse_patch_ops(&v).unwrap().len(), 2);

        // wrapped form
        let wrapped = serde_json::json!({"ops":[{"op":"mark_optional","node":"s3"}]});
        assert_eq!(parse_patch_ops(&wrapped).unwrap().len(), 1);

        let bad = serde_json::json!([{"op":"frobnicate","node":"s1"}]);
        assert!(parse_patch_ops(&bad).is_err());
    }

    #[test]
    fn parse_integration_reads_summary_and_flags() {
        let v = serde_json::json!({
            "summary":"all three landed",
            "files_changed":["a.rs","b.rs"],
            "checks_pass":true,
            "residual_gaps":[],
            "unrecoverable":false
        });
        let o = parse_integration(&v).unwrap();
        assert_eq!(o.files_changed.len(), 2);
        assert!(o.checks_pass);
        assert!(!o.unrecoverable);
    }

    #[test]
    fn specs_to_graph_assigns_agent_and_worktree_default() {
        let specs = vec![
            PlanNodeSpec { id: "s1".into(), desc: "x".into(), kind: NodeKind::Code, effort: Effort::Standard, depends_on: vec![] },
            PlanNodeSpec { id: "s2".into(), desc: "y".into(), kind: NodeKind::Review, effort: Effort::Standard, depends_on: vec!["s1".into()] },
        ];
        let health = PoolHealth::default();
        let g = specs_to_graph(&specs, &RoutingTable::default(), &health, false);
        assert!(g.find("s1").unwrap().worktree); // code -> worktree
        assert!(!g.find("s2").unwrap().worktree); // review -> no worktree
        assert!(!g.find("s1").unwrap().agent.is_empty());
    }
}
