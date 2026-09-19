//! Reads/writes `~/.atomcode/config.toml`'s `[mcp_servers.<name>]` tables.
//!
//! **ASSUMED, not confirmed** — no live atomcode install was available to
//! probe on the reference machine. E28 spec §11 confirms atomcode is a
//! Rust CLI using `[providers.freellmapi]`-style TOML for provider
//! config; `[mcp_servers.<name>]` follows that same section-per-entry
//! TOML convention for the MCP side, the most natural extrapolation
//! without a real file to inspect. Spot-check against atomcode's actual
//! config schema before relying on this in production.

use anyhow::{Context, Result};
use divisi_protocol::McpServerSpec;
use std::path::Path;
use toml::Value;

pub fn apply(path: &Path, servers: &[McpServerSpec]) -> Result<Value> {
    let mut root: Value = if path.exists() {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        text.parse().with_context(|| format!("parsing {} as TOML", path.display()))?
    } else {
        Value::Table(toml::map::Map::new())
    };

    let root_table = root.as_table_mut().context("atomcode config.toml root is not a table")?;
    let mcp_table = root_table.entry("mcp_servers").or_insert_with(|| Value::Table(toml::map::Map::new())).as_table_mut().context("mcp_servers is not a table")?;

    for server in servers {
        if !server.enabled {
            mcp_table.remove(&server.name);
            continue;
        }
        let mut entry = toml::map::Map::new();
        entry.insert("command".to_string(), Value::String(server.command.clone()));
        entry.insert("args".to_string(), Value::Array(server.args.iter().cloned().map(Value::String).collect()));
        if !server.env.is_empty() {
            let mut env = toml::map::Map::new();
            for (k, v) in &server.env {
                env.insert(k.clone(), Value::String(v.clone()));
            }
            entry.insert("env".to_string(), Value::Table(env));
        }
        mcp_table.insert(server.name.clone(), Value::Table(entry));
    }

    Ok(root)
}

pub fn remove(path: &Path, names: &[String]) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut root: Value = text.parse().with_context(|| format!("parsing {} as TOML", path.display()))?;
    let root_table = root.as_table_mut().context("atomcode config.toml root is not a table")?;
    if let Some(mcp_table) = root_table.get_mut("mcp_servers").and_then(|v| v.as_table_mut()) {
        for name in names {
            mcp_table.remove(name);
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
    fn adds_server_as_mcp_servers_subtable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let result = apply(&path, &[server("git", true)]).unwrap();
        assert_eq!(result["mcp_servers"]["git"]["command"].as_str(), Some("uvx"));
    }

    #[test]
    fn preserves_unrelated_top_level_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "default_provider = \"freellmapi\"\n\n[providers.freellmapi]\nkey = \"abc\"\n").unwrap();
        let result = apply(&path, &[server("git", true)]).unwrap();
        assert_eq!(result["default_provider"].as_str(), Some("freellmapi"));
        assert_eq!(result["providers"]["freellmapi"]["key"].as_str(), Some("abc"));
        assert_eq!(result["mcp_servers"]["git"]["command"].as_str(), Some("uvx"));
    }

    #[test]
    fn disabled_server_is_removed_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let result = apply(&path, &[server("off", false)]).unwrap();
        assert!(result["mcp_servers"].get("off").is_none());
    }
}
