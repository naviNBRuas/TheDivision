//! Reads/writes `~/.dsh/settings.yaml`'s `mcp_servers` map.
//!
//! **ASSUMED, not confirmed** — no live deepseek-harness (`dsh`) install
//! was available to probe on the reference machine. E28 spec §11 says
//! provider config "routes under `llm-pi-ai.providers`" and the API key
//! lives in a sibling `~/.dsh/.env` (mode 0600) — that's provider-key
//! wiring, a separate concern from this module's job (MCP server
//! config), so `.env` is out of scope here; `write_with_backup`'s 0600
//! handling in `adapters.rs` covers `settings.yaml` itself the same way
//! as the other five new adapters. Spot-check against dsh's actual
//! config schema before relying on this in production.

use anyhow::{Context, Result};
use serde_yaml::{Mapping, Value};
use divisi_protocol::McpServerSpec;
use std::path::Path;

fn key(s: &str) -> Value {
    Value::String(s.to_string())
}

pub fn apply(path: &Path, servers: &[McpServerSpec]) -> Result<Value> {
    let mut root: Value = if path.exists() {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_yaml::from_str(&text).with_context(|| format!("parsing {} as YAML", path.display()))?
    } else {
        Value::Mapping(Mapping::new())
    };

    let root_map = root.as_mapping_mut().context("~/.dsh/settings.yaml root is not a mapping")?;
    let mcp_key = key("mcp_servers");
    if !matches!(root_map.get(&mcp_key), Some(Value::Mapping(_))) {
        root_map.insert(mcp_key.clone(), Value::Mapping(Mapping::new()));
    }
    let mcp_map = root_map.get_mut(&mcp_key).unwrap().as_mapping_mut().context("mcp_servers is not a mapping")?;

    for server in servers {
        let server_key = key(&server.name);
        if !server.enabled {
            mcp_map.remove(&server_key);
            continue;
        }
        let mut entry = Mapping::new();
        entry.insert(key("command"), Value::String(server.command.clone()));
        entry.insert(key("args"), Value::Sequence(server.args.iter().cloned().map(Value::String).collect()));
        if !server.env.is_empty() {
            let mut env = Mapping::new();
            for (k, v) in &server.env {
                env.insert(key(k), Value::String(v.clone()));
            }
            entry.insert(key("env"), Value::Mapping(env));
        }
        mcp_map.insert(server_key, Value::Mapping(entry));
    }

    Ok(root)
}

pub fn remove(path: &Path, names: &[String]) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut root: Value = serde_yaml::from_str(&text).with_context(|| format!("parsing {} as YAML", path.display()))?;
    let root_map = root.as_mapping_mut().context("~/.dsh/settings.yaml root is not a mapping")?;
    if let Some(mcp_map) = root_map.get_mut(key("mcp_servers")).and_then(|v| v.as_mapping_mut()) {
        for name in names {
            mcp_map.remove(key(name));
        }
    }
    Ok(Some(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn server(name: &str, enabled: bool) -> McpServerSpec {
        McpServerSpec { name: name.into(), command: "uvx".into(), args: vec!["mcp-server-git".into()], env: BTreeMap::new(), secret_env: BTreeMap::new(), enabled }
    }

    #[test]
    fn adds_server_under_mcp_servers_map() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.yaml");
        let result = apply(&path, &[server("git", true)]).unwrap();
        assert_eq!(result["mcp_servers"]["git"]["command"].as_str(), Some("uvx"));
    }

    #[test]
    fn preserves_unrelated_top_level_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.yaml");
        std::fs::write(&path, "llm-pi-ai:\n  providers:\n    - freellmapi\nmcp_servers:\n  existing:\n    command: x\n    args: []\n").unwrap();
        let result = apply(&path, &[server("git", true)]).unwrap();
        assert_eq!(result["llm-pi-ai"]["providers"][0].as_str(), Some("freellmapi"));
        assert_eq!(result["mcp_servers"]["existing"]["command"].as_str(), Some("x"));
        assert_eq!(result["mcp_servers"]["git"]["command"].as_str(), Some("uvx"));
    }

    #[test]
    fn disabled_server_is_removed_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.yaml");
        let result = apply(&path, &[server("off", false)]).unwrap();
        assert!(result["mcp_servers"].as_mapping().unwrap().get(key("off")).is_none());
    }
}
