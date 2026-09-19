//! Reads/writes VS Code's `settings.json`, merging only the
//! `"cline.mcpServers"` key — everything else in the file (the user's
//! actual VS Code settings: theme, font, hundreds of other extension
//! keys) is a flat dotted-key JSON object and must be left byte-for-byte
//! untouched apart from this one key.
//!
//! **ASSUMED, not confirmed** — no live Cline install was available to
//! probe on the reference machine; `"cline.mcpServers"` follows VS Code
//! extensions' standard `"<extension>.<setting>"` naming convention
//! (confirmed pattern for the platform generally) with an object-keyed
//! MCP shape matching every other adapter here. Spot-check against
//! Cline's actual settings schema before relying on this in production.

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use divisi_protocol::McpServerSpec;
use std::path::Path;

const KEY: &str = "cline.mcpServers";

pub fn apply(path: &Path, servers: &[McpServerSpec]) -> Result<Value> {
    let mut root: Value = if path.exists() {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {} as JSON", path.display()))?
    } else {
        Value::Object(Map::new())
    };

    let root_obj = root.as_object_mut().context("VS Code settings.json root is not a JSON object")?;
    let mcp_obj = root_obj.entry(KEY).or_insert_with(|| Value::Object(Map::new())).as_object_mut().context("cline.mcpServers is not a JSON object")?;

    for server in servers {
        if !server.enabled {
            mcp_obj.remove(&server.name);
            continue;
        }
        let mut entry = Map::new();
        entry.insert("command".into(), Value::String(server.command.clone()));
        entry.insert("args".into(), Value::Array(server.args.iter().cloned().map(Value::String).collect()));
        if !server.env.is_empty() {
            let env: Map<String, Value> = server.env.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
            entry.insert("env".into(), Value::Object(env));
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
    let root_obj = root.as_object_mut().context("VS Code settings.json root is not a JSON object")?;
    if let Some(mcp_obj) = root_obj.get_mut(KEY).and_then(|v| v.as_object_mut()) {
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
    fn adds_server_under_cline_mcp_servers_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let result = apply(&path, &[server("git", true)]).unwrap();
        assert_eq!(result["cline.mcpServers"]["git"]["command"], "uvx");
    }

    #[test]
    fn preserves_every_unrelated_vscode_setting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"editor.fontSize": 14, "workbench.colorTheme": "Dark+", "cline.mcpServers": {"existing": {"command": "x"}}}"#).unwrap();
        let result = apply(&path, &[server("git", true)]).unwrap();
        assert_eq!(result["editor.fontSize"], 14);
        assert_eq!(result["workbench.colorTheme"], "Dark+");
        assert_eq!(result["cline.mcpServers"]["existing"]["command"], "x");
        assert_eq!(result["cline.mcpServers"]["git"]["command"], "uvx");
    }

    #[test]
    fn disabled_server_is_removed_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let result = apply(&path, &[server("off", false)]).unwrap();
        assert!(result["cline.mcpServers"].get("off").is_none());
    }
}
