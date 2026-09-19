//! User-defined agent adapters (spec section 39: "adding a new agent
//! becomes implementing one adapter, not rebuilding the ecosystem"),
//! loaded from `~/.config/divisi/agents/*.toml` — no recompilation needed
//! to add a new CLI agent to the registry.
//!
//! This is deliberately narrower than a real Rust `AgentAdapter` impl: it
//! only supports the two MCP config shapes already proven correct
//! elsewhere in this codebase (`divisi-agent-sdk::formats::claude`'s flat
//! JSON object, and `formats::codex`'s flat TOML table), generalized to a
//! configurable file path and key path, plus a simple flag/subcommand
//! prompt-invocation mode. If a new CLI's actual config format doesn't
//! match either shape, this honestly can't sync MCP into it (`mcp.format
//! = "unsupported"`) rather than guessing at a shape that might corrupt a
//! real config file.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use divisi_protocol::{AgentInfo, BootstrapInstall, CapabilityFlags, HomeRequirement, InstallMethod};
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CustomAgentFile {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub install: Option<InstallSpec>,
    #[serde(default)]
    pub run: Option<RunSpec>,
    #[serde(default)]
    pub mcp: Option<McpSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InstallSpec {
    pub command: String,
    pub source: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RunSpec {
    /// "flag" (e.g. `agent -p "<prompt>"`) or "subcommand" (e.g. `agent exec "<prompt>"`).
    pub mode: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpSpec {
    /// "json_flat" (claude-shaped) or "toml_flat" (codex-shaped).
    pub format: String,
    /// Relative to `$HOME`.
    pub config_path: String,
    pub key_path: String,
}

/// Loads every `*.toml` in `dir`. A malformed file is reported alongside
/// its path rather than aborting the whole load — one bad custom agent
/// definition shouldn't take down the registry for the built-in agents.
pub fn load_all(dir: &Path) -> Result<(Vec<CustomAgentFile>, Vec<(String, String)>)> {
    let mut agents = Vec::new();
    let mut errors = Vec::new();
    if !dir.exists() {
        return Ok((agents, errors));
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let label = path.display().to_string();
        match std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|text| {
            toml::from_str::<CustomAgentFile>(&text).map_err(anyhow::Error::from)
        }) {
            Ok(def) => agents.push(def),
            Err(e) => errors.push((label, e.to_string())),
        }
    }
    agents.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((agents, errors))
}

pub fn to_agent_definition(def: &CustomAgentFile) -> crate::registry::AgentDefinition {
    let mcp_supported = def.mcp.as_ref().map(|m| m.format == "json_flat" || m.format == "toml_flat").unwrap_or(false);

    crate::registry::AgentDefinition {
        name: def.name.clone(),
        adapter: "custom".into(),
        command: def.command.clone(),
        install_method: match &def.install {
            Some(_) => InstallMethod::StandaloneBinary { detail: "user-defined custom agent".into() },
            None => InstallMethod::Unsupported { reason: "no install command defined in the custom agent's TOML file".into() },
        },
        bootstrap_install: def.install.as_ref().map(|i| BootstrapInstall { command: i.command.clone(), source: i.source.clone() }),
        unverified: true, // user-supplied, not independently verified by this project
        home_requirement: HomeRequirement::Unverified,
        max_concurrency: None,
        capabilities: CapabilityFlags {
            streaming: false,
            mcp: mcp_supported,
            lsp: false,
            // Live-verification finding: this used to be `run_supported`
            // -- "has a `[run]` block, so it can be invoked
            // non-interactively" -- which has nothing to do with tool
            // execution and was silently true for every custom agent
            // with a `[run]` section, including the `single-agent run
            // --provider X` wrappers (single-openrouter, single-typhoon,
            // etc.), none of which actually parse or execute tool-call
            // syntax; one leaked a literal `<tool_call>` token straight
            // into its output. `[run]`-based custom agents are one-shot
            // prompt→completion wrappers with no real tool-call protocol
            // here, so this is always false for them.
            tools: false,
            sessions: false,
            structured_output: false,
            non_interactive_run: true,
        },
        config_paths: def.mcp.as_ref().map(|m| vec![m.config_path.clone()]).unwrap_or_default(),
        notes: Some("user-defined custom agent (~/.config/divisi/agents/*.toml) — not verified by divisi itself".into()),
    }
}

/// Convenience used by `doctor`/`agent inspect` to fold a custom def's
/// static info together with a live `AgentInfo` the same way built-in
/// agents are presented (detection is still real, done by the generic
/// adapter's `discover()`, not fabricated here).
pub fn placeholder_agent_info(def: &CustomAgentFile) -> AgentInfo {
    let agent_def = to_agent_definition(def);
    AgentInfo {
        name: agent_def.name,
        adapter: agent_def.adapter,
        command: agent_def.command,
        detected: false,
        version: None,
        install_method: agent_def.install_method,
        bootstrap_install: agent_def.bootstrap_install,
        unverified: agent_def.unverified,
        home_requirement: agent_def.home_requirement,
        max_concurrency: agent_def.max_concurrency,
        capabilities: agent_def.capabilities,
        config_paths: agent_def.config_paths,
        notes: agent_def.notes,
        authenticated: divisi_protocol::AuthState::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_all_returns_empty_when_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let (agents, errors) = load_all(&dir.path().join("nope")).unwrap();
        assert!(agents.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn load_all_parses_a_real_definition() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("myagent.toml"),
            r#"
name = "myagent"
command = "myagent"

[install]
command = "curl -fsSL https://example.com/install.sh | sh"
source = "https://example.com/docs/install"

[run]
mode = "flag"
value = "-p"

[mcp]
format = "json_flat"
config_path = ".myagent/config.json"
key_path = "mcpServers"
"#,
        )
        .unwrap();
        let (agents, errors) = load_all(dir.path()).unwrap();
        assert!(errors.is_empty());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "myagent");
        assert_eq!(agents[0].mcp.as_ref().unwrap().format, "json_flat");
    }

    #[test]
    fn load_all_reports_malformed_file_without_aborting() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("broken.toml"), "not = [valid").unwrap();
        std::fs::write(
            dir.path().join("good.toml"),
            "name = \"good\"\ncommand = \"good\"\n",
        )
        .unwrap();
        let (agents, errors) = load_all(dir.path()).unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "good");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn to_agent_definition_reflects_declared_capabilities() {
        let def = CustomAgentFile {
            name: "x".into(),
            command: "x".into(),
            install: None,
            run: Some(RunSpec { mode: "subcommand".into(), value: "exec".into() }),
            mcp: Some(McpSpec { format: "toml_flat".into(), config_path: ".x/config.toml".into(), key_path: "mcp_servers".into() }),
        };
        let agent_def = to_agent_definition(&def);
        assert!(agent_def.capabilities.mcp);
        assert!(matches!(agent_def.install_method, InstallMethod::Unsupported { .. }));
        assert!(agent_def.unverified);
    }

    /// Live-verification finding: `tools` used to be true for any custom
    /// agent with a `[run]` block, conflating "can be invoked
    /// non-interactively" with "actually executes tool calls" — every
    /// `single-agent run --provider X` wrapper (single-openrouter,
    /// single-typhoon, etc.) claimed full tool capability despite being
    /// a one-shot prompt→completion wrapper with no tool-call protocol.
    /// One observed leaking a literal `<tool_call>` token into its
    /// output as a direct result.
    #[test]
    fn to_agent_definition_never_claims_tools_for_a_run_based_custom_agent() {
        let def = CustomAgentFile {
            name: "single-example".into(),
            command: "single-agent-example".into(),
            install: None,
            run: Some(RunSpec { mode: "flag".into(), value: "--prompt".into() }),
            mcp: None,
        };
        assert!(!to_agent_definition(&def).capabilities.tools);
    }
}
