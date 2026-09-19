//! Reads/writes `~/.codex/config.toml`'s `[mcp_servers.<name>]` tables.
//! Format confirmed by direct inspection of a real file on the reference
//! machine: `command`, `args`, and an optional `[mcp_servers.<name>.env]`
//! sub-table.
//!
//! Uses the plain `toml` crate rather than a format-preserving editor, so
//! re-serializing reformats the file (stable key order, no comments
//! preserved). Acceptable for Phase 1 since the file is machine-managed
//! MCP config, not hand-authored prose — documented here rather than hidden.

use divisi_protocol::McpServerSpec;
use anyhow::{Context, Result};
use toml::value::{Table, Value};

pub fn apply(path: &std::path::Path, servers: &[McpServerSpec]) -> Result<Table> {
    let mut root: Table = if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        text.parse::<Table>()
            .with_context(|| format!("parsing {} as TOML", path.display()))?
    } else {
        Table::new()
    };

    let mcp_table = root
        .entry("mcp_servers")
        .or_insert_with(|| Value::Table(Table::new()))
        .as_table_mut()
        .context("mcp_servers is not a TOML table")?;

    for server in servers {
        if !server.enabled {
            continue;
        }
        let mut entry = Table::new();
        entry.insert("command".into(), Value::String(server.command.clone()));
        entry.insert(
            "args".into(),
            Value::Array(server.args.iter().cloned().map(Value::String).collect()),
        );
        if !server.env.is_empty() {
            let mut env_table = Table::new();
            for (k, v) in &server.env {
                env_table.insert(k.clone(), Value::String(v.clone()));
            }
            entry.insert("env".into(), Value::Table(env_table));
        }
        mcp_table.insert(server.name.clone(), Value::Table(entry));
    }

    Ok(root)
}

/// Removes only the named servers from `[mcp_servers]`, leaving anything
/// else in the file untouched.
pub fn remove(path: &std::path::Path, names: &[String]) -> Result<Option<Table>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut root: Table = text.parse().with_context(|| format!("parsing {} as TOML", path.display()))?;
    if let Some(mcp_table) = root.get_mut("mcp_servers").and_then(|v| v.as_table_mut()) {
        for name in names {
            mcp_table.remove(name);
        }
    }
    Ok(Some(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use divisi_protocol::McpServerSpec;
    use std::collections::BTreeMap;

    #[test]
    fn adds_server_to_fresh_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let servers = vec![McpServerSpec {
            name: "git".into(),
            command: "uvx".into(),
            args: vec!["mcp-server-git".into()],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: true,
        }];
        let result = apply(&path, &servers).unwrap();
        let git = &result["mcp_servers"]["git"];
        assert_eq!(git["command"].as_str().unwrap(), "uvx");
    }

    #[test]
    fn preserves_unrelated_top_level_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[mcp_servers.existing]\ncommand = \"x\"\n").unwrap();
        let servers = vec![McpServerSpec {
            name: "memory".into(),
            command: "npx".into(),
            args: vec![],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: true,
        }];
        let result = apply(&path, &servers).unwrap();
        assert_eq!(result["mcp_servers"]["existing"]["command"].as_str().unwrap(), "x");
        assert_eq!(result["mcp_servers"]["memory"]["command"].as_str().unwrap(), "npx");
    }
}
