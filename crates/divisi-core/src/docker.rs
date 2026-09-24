//! Settings for the opt-in Docker execution backend (spec: "docker for
//! them agents/cli accounts"). Host-process isolation (`$HOME` swapping,
//! `divisi_core::agent_home`/`account`) stays the default for every agent;
//! this is a per-(agent, optional account) flag a user turns on
//! explicitly. Stored at `~/.config/divisi/docker.toml`, same pattern as
//! `providers.rs`/`mcp.rs`.
//!
//! Doesn't itself talk to Docker — that's `divisi-runtime::docker`
//! (container lifecycle, daemon-invoked). This module is pure settings,
//! usable from any crate without a Docker dependency.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DockerSetting {
    pub agent: String,
    /// `None` = the agent-wide isolated home (`homes/<agent>/`); `Some` =
    /// a specific captured account's isolated home
    /// (`accounts/<agent>/<name>/home/`). Both are valid mount sources —
    /// see `divisi_core::agent_home`/`account`.
    #[serde(default)]
    pub account: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DockerSettingsFile {
    #[serde(default)]
    settings: Vec<DockerSetting>,
}

/// `divisi-agent-<agent>` or `divisi-agent-<agent>-<account>` — the
/// container name `divisi-runtime::docker`'s lifecycle functions use.
pub fn container_name(agent: &str, account: Option<&str>) -> String {
    match account {
        Some(name) => format!("divisi-agent-{agent}-{name}"),
        None => format!("divisi-agent-{agent}"),
    }
}

pub fn load(path: &Path) -> Result<Vec<DockerSetting>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: DockerSettingsFile = toml::from_str(&text).with_context(|| format!("parsing {} as TOML", path.display()))?;
    Ok(file.settings)
}

fn save(path: &Path, settings: &[DockerSetting]) -> Result<()> {
    let rendered = toml::to_string_pretty(&DockerSettingsFile { settings: settings.to_vec() }).context("serializing docker settings")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))
}

pub fn set_enabled(path: &Path, agent: &str, account: Option<&str>, enabled: bool) -> Result<()> {
    let mut settings = load(path)?;
    settings.retain(|s| !(s.agent == agent && s.account.as_deref() == account));
    settings.push(DockerSetting { agent: agent.to_string(), account: account.map(str::to_string), enabled });
    save(path, &settings)
}

/// Whether `agent`/`account` should run via Docker: an exact
/// `(agent, account)` match wins, falling back to the agent-wide
/// `(agent, None)` setting when a specific account has no override of its
/// own. Defaults to `false` (host) when nothing is configured.
pub fn is_enabled(path: &Path, agent: &str, account: Option<&str>) -> Result<bool> {
    let settings = load(path)?;
    if let Some(s) = settings.iter().find(|s| s.agent == agent && s.account.as_deref() == account) {
        return Ok(s.enabled);
    }
    if account.is_some() {
        if let Some(s) = settings.iter().find(|s| s.agent == agent && s.account.is_none()) {
            return Ok(s.enabled);
        }
    }
    Ok(false)
}

pub fn status(path: &Path, agent_filter: Option<&str>) -> Result<Vec<DockerSetting>> {
    let settings = load(path)?;
    Ok(match agent_filter {
        Some(a) => settings.into_iter().filter(|s| s.agent == a).collect(),
        None => settings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_name_includes_account_only_when_given() {
        assert_eq!(container_name("claude", None), "divisi-agent-claude");
        assert_eq!(container_name("claude", Some("work")), "divisi-agent-claude-work");
    }

    #[test]
    fn is_enabled_defaults_to_false_when_unconfigured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("docker.toml");
        assert!(!is_enabled(&path, "claude", None).unwrap());
    }

    #[test]
    fn set_enabled_round_trips_and_replaces_by_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("docker.toml");
        set_enabled(&path, "claude", None, true).unwrap();
        assert!(is_enabled(&path, "claude", None).unwrap());

        set_enabled(&path, "claude", None, false).unwrap();
        assert!(!is_enabled(&path, "claude", None).unwrap());
        assert_eq!(load(&path).unwrap().len(), 1, "re-setting the same key should replace, not duplicate");
    }

    #[test]
    fn account_specific_setting_overrides_agent_wide_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("docker.toml");
        set_enabled(&path, "claude", None, true).unwrap();
        set_enabled(&path, "claude", Some("work"), false).unwrap();

        assert!(is_enabled(&path, "claude", None).unwrap());
        assert!(!is_enabled(&path, "claude", Some("work")).unwrap(), "account-specific override must win");
        assert!(is_enabled(&path, "claude", Some("personal")).unwrap(), "falls back to agent-wide setting");
    }
}
