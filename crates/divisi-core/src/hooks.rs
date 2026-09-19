//! Per-agent opt-in mid-run permission interception — an agent's own
//! process pausing mid-task to ask divisi (and, when undecided, the
//! user) before it uses a tool, as opposed to `divisi-gateway`'s gateway,
//! which only gates tool calls divisi itself routes. Stored at
//! `~/.config/divisi/hooks.toml`, same shape as `docker.rs`'s settings.
//!
//! Only `claude` is actually wired up (Claude Code's `PreToolUse` hook —
//! see `divisi_agent_sdk::formats::claude_settings`, whose real contract
//! was verified against an installed plugin, not guessed). Enabling any
//! other agent errors clearly instead of silently doing nothing — see
//! this module's own doc-comment precedent elsewhere in this project for
//! why a fake capability is worse than an honest "not yet".

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Total time Claude Code allows the hook process before killing it —
/// shared by the settings.json writer (`apply_hook`'s `timeout_secs`) and
/// the hook binary's own internal poll deadline, so they can't drift
/// apart into two different numbers.
pub const CLAUDE_HOOK_TIMEOUT_SECS: u64 = 300;

pub const SUPPORTED_AGENTS: &[&str] = &["claude"];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HookSetting {
    agent: String,
    enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct HooksFile {
    #[serde(default)]
    settings: Vec<HookSetting>,
}

fn load(path: &Path) -> Result<Vec<HookSetting>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: HooksFile = toml::from_str(&text).with_context(|| format!("parsing {} as TOML", path.display()))?;
    Ok(file.settings)
}

fn save(path: &Path, settings: &[HookSetting]) -> Result<()> {
    let rendered = toml::to_string_pretty(&HooksFile { settings: settings.to_vec() }).context("serializing hook settings")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))
}

pub fn is_enabled(path: &Path, agent: &str) -> Result<bool> {
    Ok(load(path)?.into_iter().find(|s| s.agent == agent).map(|s| s.enabled).unwrap_or(false))
}

pub fn status(path: &Path) -> Result<Vec<(String, bool)>> {
    Ok(load(path)?.into_iter().map(|s| (s.agent, s.enabled)).collect())
}

/// Errors for any agent not in `SUPPORTED_AGENTS` — an honest "not built
/// yet" rather than silently recording a setting nothing acts on.
pub fn set_enabled(path: &Path, agent: &str, enabled: bool) -> Result<()> {
    if !SUPPORTED_AGENTS.contains(&agent) {
        bail!(
            "mid-run permission interception isn't implemented for '{agent}' yet (only: {}) — its own hook/approval mechanism hasn't been verified",
            SUPPORTED_AGENTS.join(", ")
        );
    }
    let mut settings = load(path)?;
    settings.retain(|s| s.agent != agent);
    settings.push(HookSetting { agent: agent.to_string(), enabled });
    save(path, &settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_enabled_defaults_to_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.toml");
        assert!(!is_enabled(&path, "claude").unwrap());
    }

    #[test]
    fn set_enabled_round_trips_and_replaces_by_agent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.toml");
        set_enabled(&path, "claude", true).unwrap();
        assert!(is_enabled(&path, "claude").unwrap());
        set_enabled(&path, "claude", false).unwrap();
        assert!(!is_enabled(&path, "claude").unwrap());
        assert_eq!(status(&path).unwrap().len(), 1, "re-setting must replace, not duplicate");
    }

    #[test]
    fn unsupported_agent_errors_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.toml");
        assert!(set_enabled(&path, "codex", true).is_err());
    }
}
