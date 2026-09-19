//! Permission policy model (spec section 16): `deny`/`ask`/`allow` rules
//! scoped by resource pattern, stored at `~/.config/divisi/permissions.toml`.
//!
//! `evaluate`'s first real caller is the `divisi-gateway` gateway
//! (`crates/divisi-gateway/src/gateway.rs`), via
//! `divisi_core::preferences::evaluate_and_learn` — every `invoke_mcp`
//! call is checked against the `tools` rule set below before being
//! proxied. `Ask` (the default for anything unlisted) escalates through
//! `preferences.rs`'s learned-decision + pending-approval flow rather
//! than blocking silently.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Deny,
    Ask,
    Allow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// A resource pattern, e.g. a filesystem path prefix or a hostname.
    /// Phase 2 matching is a plain prefix match — glob/regex matching is
    /// future work once real callers exist to drive requirements.
    pub pattern: String,
    pub decision: Decision,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionSet {
    #[serde(default)]
    pub filesystem: Vec<Rule>,
    #[serde(default)]
    pub network: Vec<Rule>,
    /// Arbitrary tool/resource patterns — e.g. `mcp:<server>:<tool>` for
    /// the divisi-gateway gateway's `invoke_mcp` calls. Distinct from
    /// `filesystem`/`network` since a tool call isn't cleanly either one.
    #[serde(default)]
    pub tools: Vec<Rule>,
}

pub fn load(path: &Path) -> Result<PermissionSet> {
    if !path.exists() {
        return Ok(PermissionSet::default());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {} as TOML", path.display()))
}

pub fn save(path: &Path, set: &PermissionSet) -> Result<()> {
    let rendered = toml::to_string_pretty(set).context("serializing permission set")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))
}

/// Evaluates `resource` against `rules`, longest-matching-prefix wins;
/// defaults to `Ask` (never silently `Allow` an unlisted resource, per
/// spec section 16: "never assume an agent should automatically receive
/// unrestricted access").
pub fn evaluate(rules: &[Rule], resource: &str) -> Decision {
    rules
        .iter()
        .filter(|r| resource.starts_with(&r.pattern))
        .max_by_key(|r| r.pattern.len())
        .map(|r| r.decision)
        .unwrap_or(Decision::Ask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlisted_resource_defaults_to_ask() {
        assert_eq!(evaluate(&[], "/home/user/project"), Decision::Ask);
    }

    #[test]
    fn most_specific_rule_wins() {
        let rules = vec![
            Rule { pattern: "/home/user".into(), decision: Decision::Allow },
            Rule { pattern: "/home/user/.ssh".into(), decision: Decision::Deny },
        ];
        assert_eq!(evaluate(&rules, "/home/user/.ssh/id_rsa"), Decision::Deny);
        assert_eq!(evaluate(&rules, "/home/user/project"), Decision::Allow);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.toml");
        let set = PermissionSet {
            filesystem: vec![Rule { pattern: "~/.ssh".into(), decision: Decision::Deny }],
            network: vec![Rule { pattern: "github.com".into(), decision: Decision::Allow }],
            tools: vec![Rule { pattern: "mcp:danger".into(), decision: Decision::Deny }],
        };
        save(&path, &set).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.filesystem[0].pattern, "~/.ssh");
        assert_eq!(loaded.network[0].decision, Decision::Allow);
        assert_eq!(loaded.tools[0].pattern, "mcp:danger");
    }
}
