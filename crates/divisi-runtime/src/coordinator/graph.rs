//! the `TaskGraph` value type (spec §3.3) and the pure operations the
//! scheduler and supervisor run over it: ready-set, critical-path depth,
//! and patch-op application. no db, no io — `goal.rs` persists it.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

macro_rules! str_enum {
    ($name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $s),+ }
            }
            pub fn parse(s: &str) -> Result<Self> {
                match s { $($s => Ok(Self::$variant),)+ other => bail!("unknown {}: {other}", stringify!($name)) }
            }
        }
    };
}

str_enum!(NodeKind {
    Code => "code", Test => "test", Research => "research", Review => "review",
    Docs => "docs", Infra => "infra", Plan => "plan", Supervise => "supervise",
    Integrate => "integrate",
});

str_enum!(Effort { Quick => "quick", Standard => "standard", Deep => "deep" });

str_enum!(NodeStatus {
    Pending => "pending", Ready => "ready", Running => "running", Done => "done",
    Failed => "failed", Skipped => "skipped", Blocked => "blocked",
});

str_enum!(GoalMode { Auto => "auto", Plan => "plan", Careful => "careful", Dry => "dry" });

str_enum!(GoalStatus {
    Planning => "planning", Running => "running", Queued => "queued", Blocked => "blocked",
    Done => "done", Failed => "failed", Cancelled => "cancelled",
    // E28 spec §8 (Part D): between `running` and `blocked` — every
    // routable candidate is exhausted/benched, holding for a stamped
    // retry time rather than failing outright.
    WaitingOnCapacity => "waiting_on_capacity",
    // E28 spec §10 (Part F): a clean `divisi daemon stop` marks its
    // non-terminal goals `Paused` instead of leaving them `Running` --
    // distinguishes a clean stop (caught here, by `resume_interrupted`)
    // from a crash (leaves rows `Running`, caught by the existing
    // PID-check `scheduler::reconcile`).
    Paused => "paused",
    // divisi has done what it can on its own and needs a person: the question is in
    // `blocked_reason`. `divisi goal amend <id> "<answer>"` (or `goal resume`) continues it.
    WaitingInput => "waiting_input",
});

impl NodeKind {
    /// spec §11.3: `code` / `infra` nodes run in their own worktree by
    /// default; `research` / `review` / `docs` and the brain kinds run in
    /// the goal cwd (read-only or additive).
    pub fn default_worktree(self) -> bool {
        matches!(self, NodeKind::Code | NodeKind::Infra)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub desc: String,
    pub kind: NodeKind,
    pub effort: Effort,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default = "pending")]
    pub status: NodeStatus,
    #[serde(default)]
    pub task_id: Option<i64>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub worktree: bool,
    #[serde(default)]
    pub output_ref: Option<String>,
    /// E28 spec §8: set when this node bounced back to `Pending` after a
    /// capacity exhaustion (`pool_agent::Exhausted` or a CLI fallback
    /// chain fully rate-limited) — `ready_set_at` excludes it until this
    /// time passes, driven by real cooldown state rather than a fixed
    /// sleep. `None` for an ordinary pending node.
    #[serde(default)]
    pub earliest_retry_at_ms: Option<i64>,
}

fn pending() -> NodeStatus {
    NodeStatus::Pending
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TaskGraph {
    pub nodes: Vec<Node>,
}

/// one supervisor edit to the graph (spec §5.2). the scheduler applies a
/// list of these and re-ticks; `Block` is the only op that reaches the
/// human, `Abort` fails the goal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PatchOp {
    Retarget { node: String, #[serde(default)] kind: Option<NodeKind>, #[serde(default)] agent: Option<String> },
    Split { node: String, into: Vec<Node> },
    MarkOptional { node: String },
    AddDependency { node: String, on: String },
    Block { node: String, question: String },
    Abort { reason: String },
}

impl TaskGraph {
    pub fn find(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
    pub fn find_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// nodes eligible to run now: `pending`/`ready` and every dependency is
    /// `done` or `skipped`. a dependency id with no matching node counts as
    /// unsatisfied (defensive — a malformed plan can't unblock a node).
    pub fn ready_set(&self) -> Vec<&Node> {
        self.ready_set_at(i64::MAX)
    }

    /// Same as `ready_set`, but also excludes a node whose
    /// `earliest_retry_at_ms` stamp (E28 spec §8) is still in the future
    /// relative to `now_ms` — the scheduler tick's admit pass calls this
    /// with the real clock so a capacity-exhausted node can't spin, and
    /// re-admits itself automatically once the stamp passes.
    pub fn ready_set_at(&self, now_ms: i64) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|n| matches!(n.status, NodeStatus::Pending | NodeStatus::Ready))
            .filter(|n| n.earliest_retry_at_ms.is_none_or(|t| t <= now_ms))
            .filter(|n| {
                // A dependency on a node that isn't in the graph (or on itself) can never be met.
                // Live finding (2026-09-24): planners copied a sprint's own step numbers into
                // `depends_on` ("s3".."s8" in a graph of s9-1..s9-6), so goals sat `running` with
                // 0 dispatches for hours. Such a dependency is ignored rather than waited on.
                n.depends_on.iter().filter(|dep| **dep != n.id).all(|dep| {
                    matches!(
                        self.find(dep).map(|d| d.status),
                        None | Some(NodeStatus::Done) | Some(NodeStatus::Skipped)
                    )
                })
            })
            .collect()
    }

    /// longest chain of dependents rooted at `id` (0 = nothing depends on
    /// it). the scheduler prefers higher values at admit time so the
    /// critical path is never the thing left waiting.
    pub fn critical_path_depth(&self, id: &str) -> usize {
        let mut memo: HashMap<&str, usize> = HashMap::new();
        self.depth_of(id, &mut memo)
    }

    fn depth_of<'a>(&'a self, id: &'a str, memo: &mut HashMap<&'a str, usize>) -> usize {
        if let Some(&d) = memo.get(id) {
            return d;
        }
        let dependents: Vec<&str> = self
            .nodes
            .iter()
            .filter(|n| n.depends_on.iter().any(|d| d == id))
            .map(|n| n.id.as_str())
            .collect();
        let d = dependents.iter().map(|dep| 1 + self.depth_of(dep, memo)).max().unwrap_or(0);
        memo.insert(id, d);
        d
    }

    /// read-only view of every other node's current status, keyed by id —
    /// lets a dispatched node see what its siblings are doing without any
    /// ability to affect them. Coordinator state (this graph) stays the
    /// single source of truth; this is display-only, never consulted for
    /// scheduling decisions.
    pub fn sibling_status(&self, id: &str) -> Vec<(&str, NodeStatus)> {
        self.nodes.iter().filter(|n| n.id != id).map(|n| (n.id.as_str(), n.status)).collect()
    }

    pub fn is_all_terminal(&self) -> bool {
        self.nodes.iter().all(|n| {
            matches!(
                n.status,
                NodeStatus::Done | NodeStatus::Failed | NodeStatus::Skipped | NodeStatus::Blocked
            )
        })
    }

    /// Why the graph can never progress on its own, or `None` if it still can: work remains, nothing is
    /// running, and no node is ready even ignoring retry stamps, because the remaining nodes wait
    /// (directly or not) on a failed or blocked node. Names those nodes.
    pub fn stall_reason(&self) -> Option<String> {
        if self.is_all_terminal() || !self.ready_set().is_empty() || self.nodes.iter().any(|n| n.status == NodeStatus::Running) {
            return None;
        }
        let stuck: Vec<String> = self
            .nodes
            .iter()
            .filter(|n| matches!(n.status, NodeStatus::Failed | NodeStatus::Blocked))
            .map(|n| format!("{} ({})", n.id, n.status.as_str()))
            .collect();
        let waiting = self.nodes.iter().filter(|n| matches!(n.status, NodeStatus::Pending | NodeStatus::Ready)).count();
        Some(format!(
            "stalled: {waiting} node(s) wait on {}; needs your decision: `divisi goal retry-node` one of them, amend the goal, or cancel it",
            if stuck.is_empty() { "dependencies that can never finish".to_string() } else { stuck.join(", ") }
        ))
    }

    pub fn has_failure(&self) -> bool {
        self.nodes.iter().any(|n| n.status == NodeStatus::Failed)
    }

    /// applies supervisor patch ops in order. an `Abort` short-circuits
    /// with an error tagged so the scheduler can fail the goal.
    pub fn apply_patch(&mut self, ops: &[PatchOp]) -> Result<()> {
        for op in ops {
            match op {
                PatchOp::Retarget { node, kind, agent } => {
                    let n = self.find_mut(node).ok_or_else(|| anyhow::anyhow!("retarget: no node {node}"))?;
                    if let Some(k) = kind {
                        n.kind = *k;
                    }
                    if let Some(a) = agent {
                        n.agent = a.clone();
                    }
                    // give the retargeted node a fresh shot
                    n.status = NodeStatus::Pending;
                    n.task_id = None;
                }
                PatchOp::Split { node, into } => {
                    if self.find(node).is_none() {
                        bail!("split: no node {node}");
                    }
                    let new_ids: Vec<String> = into.iter().map(|n| n.id.clone()).collect();
                    // rewire anything depending on the split node onto all
                    // of its replacements
                    for n in &mut self.nodes {
                        if let Some(pos) = n.depends_on.iter().position(|d| d == node) {
                            n.depends_on.remove(pos);
                            n.depends_on.extend(new_ids.iter().cloned());
                        }
                    }
                    self.nodes.retain(|n| &n.id != node);
                    self.nodes.extend(into.iter().cloned());
                }
                PatchOp::MarkOptional { node } => {
                    let n = self.find_mut(node).ok_or_else(|| anyhow::anyhow!("mark_optional: no node {node}"))?;
                    // only skip if it hasn't already produced a result
                    if !matches!(n.status, NodeStatus::Done | NodeStatus::Running) {
                        n.status = NodeStatus::Skipped;
                    }
                }
                PatchOp::AddDependency { node, on } => {
                    if self.find(on).is_none() {
                        bail!("add_dependency: no node {on}");
                    }
                    let n = self.find_mut(node).ok_or_else(|| anyhow::anyhow!("add_dependency: no node {node}"))?;
                    if !n.depends_on.iter().any(|d| d == on) {
                        n.depends_on.push(on.clone());
                    }
                }
                PatchOp::Block { node, .. } => {
                    let n = self.find_mut(node).ok_or_else(|| anyhow::anyhow!("block: no node {node}"))?;
                    n.status = NodeStatus::Blocked;
                }
                PatchOp::Abort { reason } => {
                    bail!("coordinator abort: {reason}");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, deps: &[&str]) -> Node {
        Node {
            id: id.into(),
            desc: format!("do {id}"),
            kind: NodeKind::Code,
            effort: Effort::Standard,
            agent: String::new(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            status: NodeStatus::Pending,
            task_id: None,
            attempts: 0,
            worktree: false,
            output_ref: None,
            earliest_retry_at_ms: None,
        }
    }

    #[test]
    fn dangling_and_self_dependencies_do_not_block_a_node() {
        let g = TaskGraph { nodes: vec![node("s9-1", &["s3", "s8"]), node("s9-2", &["s9-1"]), node("s9-3", &["s9-3"])] };
        let ready: Vec<&str> = g.ready_set_at(0).iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ready, ["s9-1", "s9-3"], "s9-2 still waits on its real dependency");
    }

    #[test]
    fn ready_set_returns_only_dependency_satisfied_pending_nodes() {
        let mut g = TaskGraph { nodes: vec![node("s1", &[]), node("s2", &["s1"])] };
        let ready: Vec<_> = g.ready_set().iter().map(|n| n.id.clone()).collect();
        assert_eq!(ready, vec!["s1"]);

        g.find_mut("s1").unwrap().status = NodeStatus::Done;
        let ready: Vec<_> = g.ready_set().iter().map(|n| n.id.clone()).collect();
        assert_eq!(ready, vec!["s2"]);
    }

    #[test]
    fn critical_path_prefers_longest_chain() {
        let g = TaskGraph {
            nodes: vec![node("s1", &[]), node("s2", &["s1"]), node("s3", &["s2"]), node("s4", &[])],
        };
        assert_eq!(g.critical_path_depth("s1"), 2);
        assert_eq!(g.critical_path_depth("s4"), 0);
    }

    #[test]
    fn sibling_status_excludes_self_and_reports_every_other_node() {
        let mut g = TaskGraph { nodes: vec![node("s1", &[]), node("s2", &["s1"]), node("s3", &[])] };
        g.find_mut("s1").unwrap().status = NodeStatus::Done;
        g.find_mut("s3").unwrap().status = NodeStatus::Failed;

        let siblings = g.sibling_status("s2");
        assert_eq!(siblings.len(), 2);
        assert!(!siblings.iter().any(|(id, _)| *id == "s2"));
        assert!(siblings.contains(&("s1", NodeStatus::Done)));
        assert!(siblings.contains(&("s3", NodeStatus::Failed)));
    }

    #[test]
    fn sibling_status_is_empty_for_a_lone_node() {
        let g = TaskGraph { nodes: vec![node("s1", &[])] };
        assert!(g.sibling_status("s1").is_empty());
    }

    #[test]
    fn apply_patch_split_rewires_dependents() {
        let mut g = TaskGraph { nodes: vec![node("s1", &[]), node("s2", &["s1"])] };
        g.apply_patch(&[PatchOp::Split {
            node: "s1".into(),
            into: vec![node("s1a", &[]), node("s1b", &[])],
        }])
        .unwrap();
        assert!(g.find("s1").is_none());
        let s2 = g.find("s2").unwrap();
        assert_eq!(s2.depends_on, vec!["s1a", "s1b"]);
    }

    #[test]
    fn apply_patch_mark_optional_skips_unrun_node() {
        let mut g = TaskGraph { nodes: vec![node("s1", &[])] };
        g.apply_patch(&[PatchOp::MarkOptional { node: "s1".into() }]).unwrap();
        assert_eq!(g.find("s1").unwrap().status, NodeStatus::Skipped);
    }

    #[test]
    fn apply_patch_abort_is_an_error() {
        let mut g = TaskGraph { nodes: vec![node("s1", &[])] };
        let err = g.apply_patch(&[PatchOp::Abort { reason: "no path".into() }]).unwrap_err();
        assert!(err.to_string().contains("abort"));
    }

    #[test]
    fn patch_ops_deserialize_from_json() {
        let v: Vec<PatchOp> = serde_json::from_str(
            r#"[{"op":"retarget","node":"s1","agent":"grok"},
                {"op":"add_dependency","node":"s2","on":"s1"}]"#,
        )
        .unwrap();
        assert_eq!(v.len(), 2);
    }
}
