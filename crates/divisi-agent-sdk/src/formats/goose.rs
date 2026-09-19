//! Reads/writes `~/.config/goose/config.yaml`'s `extensions` key. Format
//! confirmed by direct inspection of a real file on the reference machine:
//! a top-level YAML object with an `extensions` map of
//! `{ name, cmd, args, enabled, envs, type: stdio, timeout }`. `name` is a
//! human-readable display label (Goose shows its own title-cased version
//! in some places) — this writer just reuses the registry key so it stays
//! deterministic, not fabricating a separate display-name convention.

use divisi_protocol::McpServerSpec;
use anyhow::{Context, Result};
use serde_yaml::{Mapping, Value};

fn to_value(s: &str) -> Value {
    Value::String(s.to_string())
}

pub fn apply(path: &std::path::Path, servers: &[McpServerSpec]) -> Result<Value> {
    let mut root: Value = if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_yaml::from_str(&text)
            .with_context(|| format!("parsing {} as YAML", path.display()))?
    } else {
        Value::Mapping(Mapping::new())
    };

    let root_map = root.as_mapping_mut().context("goose config.yaml root is not a YAML mapping")?;
    let extensions_key = to_value("extensions");
    let extensions_map = root_map
        .entry(extensions_key)
        .or_insert_with(|| Value::Mapping(Mapping::new()))
        .as_mapping_mut()
        .context("extensions is not a YAML mapping")?;

    for server in servers {
        if !server.enabled {
            extensions_map.remove(to_value(&server.name));
            continue;
        }
        let mut entry = Mapping::new();
        entry.insert(to_value("name"), to_value(&server.name));
        entry.insert(to_value("cmd"), to_value(&server.command));
        entry.insert(to_value("args"), Value::Sequence(server.args.iter().map(|a| to_value(a)).collect()));
        entry.insert(to_value("enabled"), Value::Bool(true));
        let mut envs = Mapping::new();
        for (k, v) in &server.env {
            envs.insert(to_value(k), to_value(v));
        }
        entry.insert(to_value("envs"), Value::Mapping(envs));
        entry.insert(to_value("type"), to_value("stdio"));
        entry.insert(to_value("timeout"), Value::Number(300.into()));
        extensions_map.insert(to_value(&server.name), Value::Mapping(entry));
    }

    Ok(root)
}

/// Removes only the named servers from `extensions`, leaving anything else
/// (goose's own settings, other extensions) untouched.
pub fn remove(path: &std::path::Path, names: &[String]) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut root: Value = serde_yaml::from_str(&text).with_context(|| format!("parsing {} as YAML", path.display()))?;
    let root_map = root.as_mapping_mut().context("goose config.yaml root is not a YAML mapping")?;
    if let Some(extensions_map) = root_map.get_mut(to_value("extensions")).and_then(|v| v.as_mapping_mut()) {
        for name in names {
            extensions_map.remove(to_value(name));
        }
    }
    Ok(Some(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn adds_extension_to_fresh_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let servers = vec![McpServerSpec {
            name: "git".into(),
            command: "uvx".into(),
            args: vec!["mcp-server-git".into()],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: true,
        }];
        let result = apply(&path, &servers).unwrap();
        assert_eq!(result["extensions"]["git"]["cmd"], "uvx");
        assert_eq!(result["extensions"]["git"]["type"], "stdio");
    }

    #[test]
    fn preserves_unrelated_top_level_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "GOOSE_PROVIDER: anthropic\nextensions:\n  existing:\n    cmd: x\n").unwrap();
        let servers = vec![McpServerSpec {
            name: "memory".into(),
            command: "npx".into(),
            args: vec![],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: true,
        }];
        let result = apply(&path, &servers).unwrap();
        assert_eq!(result["GOOSE_PROVIDER"], "anthropic");
        assert_eq!(result["extensions"]["existing"]["cmd"], "x");
        assert_eq!(result["extensions"]["memory"]["cmd"], "npx");
    }

    #[test]
    fn disabled_extension_is_removed_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let servers = vec![McpServerSpec {
            name: "off".into(),
            command: "x".into(),
            args: vec![],
            env: BTreeMap::new(), secret_env: BTreeMap::new(),
            enabled: false,
        }];
        let result = apply(&path, &servers).unwrap();
        assert!(result["extensions"].get("off").is_none());
    }
}
