//! Reads/writes `~/.continue/config.json`'s `mcpServers` key.
//!
//! **ASSUMED, not confirmed** — no live Continue install was available to
//! probe on the reference machine. Continue's config schema is
//! documented (publicly) as an **array** of server objects under
//! `mcpServers`, unlike the object-keyed shape most other adapters in
//! this file use — kept that way here rather than flattening to an
//! object, since guessing the wrong shape would silently produce a file
//! Continue can't parse. Spot-check against Continue's actual schema
//! before relying on this in production. Module named `continue_cfg`
//! (not `continue`) since `continue` is a Rust keyword.

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use divisi_protocol::McpServerSpec;
use std::path::Path;

pub fn apply(path: &Path, servers: &[McpServerSpec]) -> Result<Value> {
    let mut root: Value = if path.exists() {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {} as JSON", path.display()))?
    } else {
        Value::Object(Map::new())
    };

    let root_obj = root.as_object_mut().context("~/.continue/config.json root is not a JSON object")?;
    let mut existing: Vec<Value> = root_obj.get("mcpServers").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    for server in servers {
        existing.retain(|e| e.get("name").and_then(|n| n.as_str()) != Some(server.name.as_str()));
        if !server.enabled {
            continue;
        }
        let mut entry = json!({
            "name": server.name,
            "command": server.command,
            "args": server.args,
        });
        if !server.env.is_empty() {
            entry["env"] = Value::Object(server.env.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect());
        }
        existing.push(entry);
    }

    root_obj.insert("mcpServers".to_string(), Value::Array(existing));
    Ok(root)
}

pub fn remove(path: &Path, names: &[String]) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut root: Value = serde_json::from_str(&text).with_context(|| format!("parsing {} as JSON", path.display()))?;
    let root_obj = root.as_object_mut().context("~/.continue/config.json root is not a JSON object")?;
    if let Some(existing) = root_obj.get_mut("mcpServers").and_then(|v| v.as_array_mut()) {
        existing.retain(|e| !names.iter().any(|n| e.get("name").and_then(|v| v.as_str()) == Some(n.as_str())));
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
    fn adds_server_as_array_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let result = apply(&path, &[server("git", true)]).unwrap();
        let arr = result["mcpServers"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["command"], "uvx");
    }

    #[test]
    fn preserves_unrelated_top_level_keys_and_other_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"models": ["gpt-4"], "mcpServers": [{"name": "existing", "command": "x", "args": []}]}"#).unwrap();
        let result = apply(&path, &[server("git", true)]).unwrap();
        assert_eq!(result["models"][0], "gpt-4");
        let arr = result["mcpServers"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr.iter().any(|e| e["name"] == "existing"));
        assert!(arr.iter().any(|e| e["name"] == "git"));
    }

    #[test]
    fn disabled_server_is_removed_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"mcpServers": [{"name": "off", "command": "x", "args": []}]}"#).unwrap();
        let result = apply(&path, &[server("off", false)]).unwrap();
        assert!(result["mcpServers"].as_array().unwrap().is_empty());
    }
}
