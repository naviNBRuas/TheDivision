//! Interprets a user-defined `CustomAgentFile` (see
//! `divisi-core::custom_agents`) as a real `AgentAdapter` — detection,
//! MCP sync (for the two supported shapes), and prompt invocation, all
//! without a compiled Rust implementation for the new agent.

use crate::adapter::AgentAdapter;
use crate::backend::ExecBackend;
use crate::backup::backup_before_write;
use crate::discover::{discover, discover_detect_only, Discovery};
use anyhow::Result;
use serde_json::{Map, Value};
use divisi_core::custom_agents::CustomAgentFile;
use divisi_protocol::{IntegrationWrite, McpServerSpec, RunOutcome};
use std::path::Path;
use std::time::Duration;
use toml::value::Table;

pub struct GenericAdapter {
    def: CustomAgentFile,
}

impl GenericAdapter {
    pub fn new(def: CustomAgentFile) -> Self {
        Self { def }
    }
}

impl AgentAdapter for GenericAdapter {
    fn command(&self) -> &str {
        &self.def.command
    }

    fn discover(&self) -> Discovery {
        // `[run]`-mode custom agents are one-shot `single-agent run
        // --provider X --prompt "$2"` wrapper scripts (see
        // `custom_agents::to_agent_definition`'s `tools: false` for the
        // same root cause): a real `--version` probe fires a billed LLM
        // call and reports the model's chat reply as the version, so skip
        // it and only check the command exists.
        if self.def.run.is_some() {
            discover_detect_only(&self.def.command)
        } else {
            discover(&self.def.command)
        }
    }

    fn configure_mcp(&self, home: &Path, servers: &[McpServerSpec], dry_run: bool) -> Result<IntegrationWrite> {
        let Some(mcp) = &self.def.mcp else {
            return Ok(unsupported(&self.def.name, home, "no [mcp] section defined for this custom agent"));
        };
        let path = home.join(&mcp.config_path);
        match mcp.format.as_str() {
            "json_flat" => {
                let updated = apply_json_flat(&path, &mcp.key_path, servers)?;
                let rendered = serde_json::to_string_pretty(&updated)?;
                write_with_backup(&self.def.name, &path, &rendered, dry_run)
            }
            "toml_flat" => {
                let updated = apply_toml_flat(&path, &mcp.key_path, servers)?;
                let rendered = toml::to_string_pretty(&updated)?;
                write_with_backup(&self.def.name, &path, &rendered, dry_run)
            }
            other => Ok(unsupported(&self.def.name, home, &format!("unsupported mcp.format '{other}' (only json_flat, toml_flat are supported)"))),
        }
    }

    fn remove_mcp(&self, home: &Path, names: &[String], dry_run: bool) -> Result<IntegrationWrite> {
        let Some(mcp) = &self.def.mcp else {
            return Ok(unsupported(&self.def.name, home, "no [mcp] section defined for this custom agent"));
        };
        let path = home.join(&mcp.config_path);
        if !path.exists() {
            return Ok(unsupported(&self.def.name, home, "no config file present; nothing to remove"));
        }
        match mcp.format.as_str() {
            "json_flat" => {
                let updated = remove_json_flat(&path, &mcp.key_path, names)?;
                let rendered = serde_json::to_string_pretty(&updated)?;
                write_with_backup(&self.def.name, &path, &rendered, dry_run)
            }
            "toml_flat" => {
                let updated = remove_toml_flat(&path, &mcp.key_path, names)?;
                let rendered = toml::to_string_pretty(&updated)?;
                write_with_backup(&self.def.name, &path, &rendered, dry_run)
            }
            other => Ok(unsupported(&self.def.name, home, &format!("unsupported mcp.format '{other}'"))),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_prompt(
        &self,
        cwd: &Path,
        prompt: &str,
        backend: &ExecBackend,
        live_output_path: Option<&Path>,
        timeout: Duration,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<RunOutcome> {
        match &self.def.run {
            Some(run) if run.mode == "flag" => {
                crate::run::run_command_live(&self.def.command, &[run.value.clone(), prompt.to_string()], cwd, backend, live_output_path, timeout, cancel)
            }
            Some(run) if run.mode == "subcommand" => {
                crate::run::run_command_live(&self.def.command, &[run.value.clone(), prompt.to_string()], cwd, backend, live_output_path, timeout, cancel)
            }
            _ => anyhow::bail!("{} has no [run] mode defined", self.def.name),
        }
    }
}

fn unsupported(agent: &str, home: &Path, detail: &str) -> IntegrationWrite {
    IntegrationWrite { agent: agent.to_string(), config_path: home.display().to_string(), backup_path: None, applied: false, detail: detail.to_string() }
}

fn write_with_backup(agent: &str, path: &Path, rendered: &str, dry_run: bool) -> Result<IntegrationWrite> {
    if dry_run {
        return Ok(IntegrationWrite {
            agent: agent.to_string(),
            config_path: path.display().to_string(),
            backup_path: None,
            applied: false,
            detail: format!("dry run: would write {} bytes to {}", rendered.len(), path.display()),
        });
    }
    let backup_path = backup_before_write(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered)?;
    Ok(IntegrationWrite {
        agent: agent.to_string(),
        config_path: path.display().to_string(),
        backup_path: backup_path.map(|p| p.display().to_string()),
        applied: true,
        detail: format!("wrote {} bytes", rendered.len()),
    })
}

/// Generalized version of `formats::claude::apply`: a flat JSON object at
/// `key_path` mapping server name -> `{command, args, env}`.
fn apply_json_flat(path: &Path, key_path: &str, servers: &[McpServerSpec]) -> Result<Value> {
    let mut root: Value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(path)?)?
    } else {
        Value::Object(Map::new())
    };
    let root_obj = root.as_object_mut().ok_or_else(|| anyhow::anyhow!("{} root is not a JSON object", path.display()))?;
    let key_obj = root_obj.entry(key_path).or_insert_with(|| Value::Object(Map::new())).as_object_mut().ok_or_else(|| anyhow::anyhow!("{key_path} is not a JSON object"))?;

    for server in servers {
        if !server.enabled {
            continue;
        }
        let mut entry = Map::new();
        entry.insert("command".into(), Value::String(server.command.clone()));
        entry.insert("args".into(), Value::Array(server.args.iter().cloned().map(Value::String).collect()));
        let env: Map<String, Value> = server.env.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
        entry.insert("env".into(), Value::Object(env));
        key_obj.insert(server.name.clone(), Value::Object(entry));
    }
    Ok(root)
}

fn remove_json_flat(path: &Path, key_path: &str, names: &[String]) -> Result<Value> {
    let mut root: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let root_obj = root.as_object_mut().ok_or_else(|| anyhow::anyhow!("{} root is not a JSON object", path.display()))?;
    if let Some(key_obj) = root_obj.get_mut(key_path).and_then(|v| v.as_object_mut()) {
        for name in names {
            key_obj.remove(name);
        }
    }
    Ok(root)
}

/// Generalized version of `formats::codex::apply`: a flat TOML table at
/// `key_path` mapping server name -> `{command, args, env}`.
fn apply_toml_flat(path: &Path, key_path: &str, servers: &[McpServerSpec]) -> Result<Table> {
    let mut root: Table = if path.exists() {
        std::fs::read_to_string(path)?.parse()?
    } else {
        Table::new()
    };
    let key_table = root.entry(key_path).or_insert_with(|| toml::Value::Table(Table::new())).as_table_mut().ok_or_else(|| anyhow::anyhow!("{key_path} is not a TOML table"))?;

    for server in servers {
        if !server.enabled {
            continue;
        }
        let mut entry = Table::new();
        entry.insert("command".into(), toml::Value::String(server.command.clone()));
        entry.insert("args".into(), toml::Value::Array(server.args.iter().cloned().map(toml::Value::String).collect()));
        if !server.env.is_empty() {
            let mut env_table = Table::new();
            for (k, v) in &server.env {
                env_table.insert(k.clone(), toml::Value::String(v.clone()));
            }
            entry.insert("env".into(), toml::Value::Table(env_table));
        }
        key_table.insert(server.name.clone(), toml::Value::Table(entry));
    }
    Ok(root)
}

fn remove_toml_flat(path: &Path, key_path: &str, names: &[String]) -> Result<Table> {
    let mut root: Table = std::fs::read_to_string(path)?.parse()?;
    if let Some(key_table) = root.get_mut(key_path).and_then(|v| v.as_table_mut()) {
        for name in names {
            key_table.remove(name);
        }
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use divisi_core::custom_agents::{McpSpec, RunSpec};
    use std::collections::BTreeMap;

    fn sample_def() -> CustomAgentFile {
        CustomAgentFile {
            name: "myagent".into(),
            command: "myagent".into(),
            install: None,
            run: Some(RunSpec { mode: "flag".into(), value: "-p".into() }),
            mcp: Some(McpSpec { format: "json_flat".into(), config_path: ".myagent/config.json".into(), key_path: "mcpServers".into() }),
        }
    }

    fn sample_servers() -> Vec<McpServerSpec> {
        vec![McpServerSpec { name: "git".into(), command: "uvx".into(), args: vec!["mcp-server-git".into()], env: BTreeMap::new(), secret_env: BTreeMap::new(), enabled: true }]
    }

    #[test]
    fn json_flat_writes_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = GenericAdapter::new(sample_def());
        let result = adapter.configure_mcp(dir.path(), &sample_servers(), false).unwrap();
        assert!(result.applied);
        let written = std::fs::read_to_string(dir.path().join(".myagent/config.json")).unwrap();
        let value: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(value["mcpServers"]["git"]["command"], "uvx");
    }

    #[test]
    fn toml_flat_writes_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut def = sample_def();
        def.mcp = Some(McpSpec { format: "toml_flat".into(), config_path: ".myagent/config.toml".into(), key_path: "mcp_servers".into() });
        let adapter = GenericAdapter::new(def);
        let result = adapter.configure_mcp(dir.path(), &sample_servers(), false).unwrap();
        assert!(result.applied);
        let written = std::fs::read_to_string(dir.path().join(".myagent/config.toml")).unwrap();
        assert!(written.contains("[mcp_servers.git]"));
    }

    #[test]
    fn unsupported_format_does_not_write_anything() {
        let dir = tempfile::tempdir().unwrap();
        let mut def = sample_def();
        def.mcp = Some(McpSpec { format: "yaml_nested".into(), config_path: ".myagent/config.yaml".into(), key_path: "mcp".into() });
        let adapter = GenericAdapter::new(def);
        let result = adapter.configure_mcp(dir.path(), &sample_servers(), false).unwrap();
        assert!(!result.applied);
        assert!(!dir.path().join(".myagent/config.yaml").exists());
    }

    #[test]
    fn remove_only_deletes_named_servers() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = GenericAdapter::new(sample_def());
        adapter.configure_mcp(dir.path(), &sample_servers(), false).unwrap();
        let mut two_servers = sample_servers();
        two_servers.push(McpServerSpec { name: "memory".into(), command: "npx".into(), args: vec![], env: BTreeMap::new(), secret_env: BTreeMap::new(), enabled: true });
        adapter.configure_mcp(dir.path(), &two_servers, false).unwrap();

        adapter.remove_mcp(dir.path(), &["git".to_string()], false).unwrap();
        let written = std::fs::read_to_string(dir.path().join(".myagent/config.json")).unwrap();
        let value: Value = serde_json::from_str(&written).unwrap();
        assert!(value["mcpServers"].get("git").is_none());
        assert!(value["mcpServers"].get("memory").is_some());
    }

    #[test]
    fn run_prompt_without_run_section_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut def = sample_def();
        def.run = None;
        let adapter = GenericAdapter::new(def);
        assert!(adapter.run_prompt(dir.path(), "hi", &ExecBackend::host(None), None, Duration::from_secs(1), None).is_err());
    }
}
