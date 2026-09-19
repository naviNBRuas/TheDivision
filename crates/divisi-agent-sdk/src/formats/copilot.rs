//! Reads/writes `~/.copilot/mcp-config.json`'s `mcpServers` key. Format
//! confirmed by actually running `copilot mcp add` against a throwaway
//! `$HOME` on the reference machine and inspecting the file it wrote (the
//! file didn't exist there beforehand, so this couldn't be read off an
//! existing config the way claude/codex/opencode's formats were) —
//! `{ mcpServers: { <name>: { tools: ["*"], type: "local", command, args,
//! env } } }`.

use divisi_protocol::McpServerSpec;
use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::path::Path;

pub fn apply(path: &Path, servers: &[McpServerSpec]) -> Result<Value> {
    let mut root: Value = if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("parsing {} as JSON", path.display()))?
    } else {
        Value::Object(Map::new())
    };

    let root_obj = root.as_object_mut().context("~/.copilot/mcp-config.json root is not a JSON object")?;
    let mcp_obj = root_obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("mcpServers is not a JSON object")?;

    for server in servers {
        if !server.enabled {
            mcp_obj.remove(&server.name);
            continue;
        }
        let mut entry = Map::new();
        entry.insert("tools".into(), Value::Array(vec![Value::String("*".into())]));
        entry.insert("type".into(), Value::String("local".into()));
        entry.insert("command".into(), Value::String(server.command.clone()));
        entry.insert(
            "args".into(),
            Value::Array(server.args.iter().cloned().map(Value::String).collect()),
        );
        if !server.env.is_empty() {
            let env: Map<String, Value> = server
                .env
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            entry.insert("env".into(), Value::Object(env));
        }
        mcp_obj.insert(server.name.clone(), Value::Object(entry));
    }

    Ok(root)
}

/// Removes only the named servers from `mcpServers`, leaving anything else
/// (including MCP servers divisi doesn't manage) untouched.
pub fn remove(path: &Path, names: &[String]) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut root: Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {} as JSON", path.display()))?;
    let root_obj = root.as_object_mut().context("~/.copilot/mcp-config.json root is not a JSON object")?;
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

    #[test]
    fn adds_server_with_wildcard_tools_and_local_type() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-config.json");
        let servers = vec![McpServerSpec {
            name: "git".into(),
            command: "uvx".into(),
            args: vec!["mcp-server-git".into()],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: true,
        }];
        let result = apply(&path, &servers).unwrap();
        assert_eq!(result["mcpServers"]["git"]["command"], "uvx");
        assert_eq!(result["mcpServers"]["git"]["type"], "local");
        assert_eq!(result["mcpServers"]["git"]["tools"][0], "*");
    }

    #[test]
    fn preserves_unrelated_top_level_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-config.json");
        std::fs::write(&path, r#"{"mcpServers": {"existing": {"command": "x"}}}"#).unwrap();
        let servers = vec![McpServerSpec {
            name: "memory".into(),
            command: "npx".into(),
            args: vec![],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: true,
        }];
        let result = apply(&path, &servers).unwrap();
        assert_eq!(result["mcpServers"]["existing"]["command"], "x");
        assert_eq!(result["mcpServers"]["memory"]["command"], "npx");
    }

    #[test]
    fn disabled_server_is_removed_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-config.json");
        let servers = vec![McpServerSpec {
            name: "off".into(),
            command: "x".into(),
            args: vec![],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: false,
        }];
        let result = apply(&path, &servers).unwrap();
        assert!(result["mcpServers"].get("off").is_none());
    }
}
