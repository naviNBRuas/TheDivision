//! Reads/writes `~/.claude/settings.json`'s `hooks.PreToolUse` array — a
//! different file from `claude.rs`'s `~/.claude.json` (confirmed by direct
//! inspection of a real settings.json on the reference machine, which
//! holds `permissions`/`statusLine`/`enabledPlugins`, not MCP servers).
//!
//! This is the real mechanism behind SingleCLI's per-agent mid-run
//! permission interception (`single agent hooks enable claude`): a
//! `PreToolUse` hook runs before every tool call and can deny it. The
//! exact request/response JSON contract here — stdin
//! `{tool_name, tool_input, hook_event_name, ...}`, stdout
//! `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny"}}`
//! to block or `{}` to allow — was verified against a real, shipped
//! plugin already installed on the reference machine (`hookify`'s
//! `hooks/pretooluse.py` and `core/rule_engine.py`), not guessed from
//! documentation.

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use std::path::Path;

/// Adds (or replaces, if a SingleCLI-managed entry already exists) a
/// `PreToolUse` hook entry running `hook_command`. Every other hook entry
/// — from plugins, or hand-written — is left exactly as-is; this only
/// ever touches entries whose command matches `hook_command`.
pub fn apply_hook(path: &Path, hook_command: &str, timeout_secs: u64) -> Result<Value> {
    let mut root = read_or_empty(path)?;
    let root_obj = root.as_object_mut().context("settings.json root is not a JSON object")?;
    let hooks_obj =
        root_obj.entry("hooks").or_insert_with(|| Value::Object(Map::new())).as_object_mut().context("hooks is not a JSON object")?;
    let pretooluse = hooks_obj
        .entry("PreToolUse")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .context("hooks.PreToolUse is not a JSON array")?;

    pretooluse.retain(|entry| !is_our_entry(entry, hook_command));
    pretooluse.push(json!({
        "hooks": [ { "type": "command", "command": hook_command, "timeout": timeout_secs } ]
    }));
    Ok(root)
}

/// Inverse of `apply_hook`: removes only the entry matching `hook_command`.
pub fn remove_hook(path: &Path, hook_command: &str) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut root = read_or_empty(path)?;
    if let Some(pretooluse) =
        root.as_object_mut().and_then(|o| o.get_mut("hooks")).and_then(|h| h.get_mut("PreToolUse")).and_then(|v| v.as_array_mut())
    {
        pretooluse.retain(|entry| !is_our_entry(entry, hook_command));
    }
    Ok(Some(root))
}

fn is_our_entry(entry: &Value, hook_command: &str) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hooks| hooks.iter().any(|h| h.get("command").and_then(|c| c.as_str()) == Some(hook_command)))
        .unwrap_or(false)
}

fn read_or_empty(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {} as JSON", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_hook_to_fresh_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let result = apply_hook(&path, "single internal claude-pretooluse-hook", 270).unwrap();
        let hooks = result["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0]["hooks"][0]["command"], "single internal claude-pretooluse-hook");
        assert_eq!(hooks[0]["hooks"][0]["timeout"], 270);
    }

    #[test]
    fn preserves_unrelated_settings_and_other_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"permissions": {"ask": ["Bash(sudo*)"]}, "hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "some-other-plugin-hook"}]}]}}"#,
        )
        .unwrap();
        let result = apply_hook(&path, "single internal claude-pretooluse-hook", 270).unwrap();
        assert_eq!(result["permissions"]["ask"][0], "Bash(sudo*)");
        let hooks = result["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(hooks.len(), 2, "must add alongside, not replace, an existing unrelated hook");
    }

    #[test]
    fn re_applying_replaces_rather_than_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let root1 = apply_hook(&path, "single internal claude-pretooluse-hook", 270).unwrap();
        std::fs::write(&path, serde_json::to_string(&root1).unwrap()).unwrap();
        let result = apply_hook(&path, "single internal claude-pretooluse-hook", 300).unwrap();
        let hooks = result["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0]["hooks"][0]["timeout"], 300);
    }

    #[test]
    fn remove_hook_leaves_other_hooks_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let root1 = apply_hook(&path, "single internal claude-pretooluse-hook", 270).unwrap();
        let mut root1 = root1;
        root1["hooks"]["PreToolUse"]
            .as_array_mut()
            .unwrap()
            .push(json!({"hooks": [{"type": "command", "command": "unrelated-plugin-hook"}]}));
        std::fs::write(&path, serde_json::to_string(&root1).unwrap()).unwrap();

        let result = remove_hook(&path, "single internal claude-pretooluse-hook").unwrap().unwrap();
        let hooks = result["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0]["hooks"][0]["command"], "unrelated-plugin-hook");
    }

    #[test]
    fn remove_hook_on_nonexistent_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(remove_hook(&path, "anything").unwrap().is_none());
    }
}
