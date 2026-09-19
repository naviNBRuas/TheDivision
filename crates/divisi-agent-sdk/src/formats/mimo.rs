//! Reads/writes `~/.config/mimocode/config.json`'s `mcpServers` key.
//!
//! **ASSUMED, not confirmed** — mimo (OpenCode-derived, per E28 spec §11)
//! wasn't installed and probed on the reference machine; shape follows
//! `opencode.rs`'s object-keyed convention (the closest confirmed
//! relative, since mimo is described as an OpenCode fork) rather than
//! guessing something unrelated. Spot-check against mimo's actual docs
//! before relying on this in production.

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use divisi_protocol::McpServerSpec;
use std::path::Path;

pub fn apply(path: &Path, servers: &[McpServerSpec]) -> Result<Value> {
    let mut root: Value = if path.exists() {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {} as JSON", path.display()))?
    } else {
        Value::Object(Map::new())
    };

    let root_obj = root.as_object_mut().context("mimo config.json root is not a JSON object")?;
    let mcp_obj = root_obj.entry("mcpServers").or_insert_with(|| Value::Object(Map::new())).as_object_mut().context("mcpServers is not a JSON object")?;

    for server in servers {
        if !server.enabled {
            mcp_obj.remove(&server.name);
            continue;
        }
        let mut entry = Map::new();
        entry.insert("command".into(), Value::String(server.command.clone()));
        entry.insert("args".into(), Value::Array(server.args.iter().cloned().map(Value::String).collect()));
        if !server.env.is_empty() {
            // mimo's `{env:…}` key-reference syntax (spec §11) is for
            // provider API keys, not MCP server env vars -- MCP env stays
            // plain key/value the way every other adapter writes it.
            let env: Map<String, Value> = server.env.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
            entry.insert("environment".into(), Value::Object(env));
        }
        mcp_obj.insert(server.name.clone(), Value::Object(entry));
    }

    Ok(root)
}

pub fn remove(path: &Path, names: &[String]) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut root: Value = serde_json::from_str(&text).with_context(|| format!("parsing {} as JSON", path.display()))?;
    let root_obj = root.as_object_mut().context("mimo config.json root is not a JSON object")?;
    if let Some(mcp_obj) = root_obj.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
        for name in names {
            mcp_obj.remove(name);
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
    fn adds_server_under_mcp_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let result = apply(&path, &[server("git", true)]).unwrap();
        assert_eq!(result["mcpServers"]["git"]["command"], "uvx");
    }

    #[test]
    fn preserves_unrelated_top_level_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"provider": "openai", "mcpServers": {"existing": {"command": "x"}}}"#).unwrap();
        let result = apply(&path, &[server("git", true)]).unwrap();
        assert_eq!(result["provider"], "openai");
        assert_eq!(result["mcpServers"]["existing"]["command"], "x");
    }

    #[test]
    fn disabled_server_is_removed_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let result = apply(&path, &[server("off", false)]).unwrap();
        assert!(result["mcpServers"].get("off").is_none());
    }
}
