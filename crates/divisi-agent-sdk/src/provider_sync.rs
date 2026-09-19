//! Syncs a provider's API key into an agent's real config, using only
//! verified mechanisms:
//!
//! - **claude**: `~/.claude/settings.json` has a real, documented `env`
//!   object the CLI applies to its own process environment (confirmed via
//!   `code.claude.com/docs/en/setup`'s `USE_BUILTIN_RIPGREP` example, and
//!   already observed with `CLAUDE_CODE_GIT_BASH_PATH` in this project's
//!   Phase 1 investigation notes). Any `env_var_name` can be merged in —
//!   Claude Code itself supports `ANTHROPIC_API_KEY` and third-party
//!   provider vars (Bedrock/Vertex) this way.
//! - **codex**: `~/.codex/auth.json` has a real, observed `OPENAI_API_KEY`
//!   top-level field (see this project's own account.rs investigation
//!   notes) — but *only* that field; codex's auth file has no generic
//!   "arbitrary provider key" slot. Syncing anything other than
//!   `OPENAI_API_KEY` here would be fabricating a field, so it's refused.
//! - Every other agent: unsupported. `opencode`'s provider/auth storage
//!   lives in a live multi-table SQLite database (see `account.rs`) that
//!   this project already decided not to touch surgically; `agy` and
//!   `perplexity` have no verified provider-key config surface.

use crate::backup::backup_before_write;
use crate::formats;
use anyhow::{Context, Result};
use serde_json::{Map, Value};
use divisi_protocol::{ProviderSpec, ProviderSyncResult};
use std::path::Path;

pub fn sync(agent: &str, home: &Path, provider: &ProviderSpec, value: &str, dry_run: bool) -> Result<ProviderSyncResult> {
    match agent {
        "claude" => sync_claude(home, &provider.env_var_name, value, dry_run),
        "codex" if provider.env_var_name == "OPENAI_API_KEY" => sync_codex(home, value, dry_run),
        "codex" => Ok(unsupported(
            "codex",
            home,
            &format!("codex's auth.json only has a slot for OPENAI_API_KEY, not '{}'", provider.env_var_name),
        )),
        "opencode" => sync_opencode(home, provider, dry_run),
        other => Ok(unsupported(other, home, "no verified provider-key config location for this agent")),
    }
}

fn sync_opencode(home: &Path, provider: &ProviderSpec, dry_run: bool) -> Result<ProviderSyncResult> {
    if provider.models.is_empty() {
        return Ok(unsupported(
            "opencode",
            home,
            "no models declared for this provider (add it with `--model id:name`); nothing to add to opencode's model picker",
        ));
    }
    let path = home.join(".config").join("opencode").join("opencode.jsonc");
    let updated = formats::opencode::apply_provider(&path, provider)?;
    let rendered = serde_json::to_string_pretty(&updated)?;
    let mut result = write_result(&provider.name, "opencode", &path, &rendered, dry_run)?;
    let key = formats::opencode::opencode_provider_key(&path, &provider.name);
    if key != provider.name {
        result.detail = format!(
            "{} — written as `{key}` (not `{}`) so opencode doesn't merge that provider's \
             full models.dev catalog; use `opencode -m {key}/<id>`",
            result.detail, provider.name
        );
    }
    Ok(result)
}

fn sync_claude(home: &Path, env_var_name: &str, value: &str, dry_run: bool) -> Result<ProviderSyncResult> {
    let path = home.join(".claude/settings.json");
    let mut root: Value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&path)?)?
    } else {
        Value::Object(Map::new())
    };
    let root_obj = root.as_object_mut().context("~/.claude/settings.json root is not a JSON object")?;
    let env_obj = root_obj.entry("env").or_insert_with(|| Value::Object(Map::new())).as_object_mut().context("settings.json's env is not a JSON object")?;
    env_obj.insert(env_var_name.to_string(), Value::String(value.to_string()));

    let rendered = serde_json::to_string_pretty(&root)?;
    write_result("anthropic-compatible", "claude", &path, &rendered, dry_run)
}

fn sync_codex(home: &Path, value: &str, dry_run: bool) -> Result<ProviderSyncResult> {
    let path = home.join(".codex/auth.json");
    let mut root: Value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&path)?)?
    } else {
        Value::Object(Map::new())
    };
    let root_obj = root.as_object_mut().context("~/.codex/auth.json root is not a JSON object")?;
    root_obj.insert("OPENAI_API_KEY".to_string(), Value::String(value.to_string()));

    let rendered = serde_json::to_string_pretty(&root)?;
    write_result("openai", "codex", &path, &rendered, dry_run)
}

fn unsupported(agent: &str, home: &Path, detail: &str) -> ProviderSyncResult {
    ProviderSyncResult {
        provider: String::new(),
        agent: agent.to_string(),
        config_path: home.display().to_string(),
        backup_path: None,
        applied: false,
        detail: detail.to_string(),
    }
}

fn write_result(provider: &str, agent: &str, path: &Path, rendered: &str, dry_run: bool) -> Result<ProviderSyncResult> {
    if dry_run {
        return Ok(ProviderSyncResult {
            provider: provider.to_string(),
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
    Ok(ProviderSyncResult {
        provider: provider.to_string(),
        agent: agent.to_string(),
        config_path: path.display().to_string(),
        backup_path: backup_path.map(|p| p.display().to_string()),
        applied: true,
        detail: format!("wrote {} bytes", rendered.len()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_sync_preserves_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(dir.path().join(".claude/settings.json"), r#"{"theme":"dark","env":{"OTHER_VAR":"x"}}"#).unwrap();

        let provider = ProviderSpec { name: "anthropic".into(), env_var_name: "ANTHROPIC_API_KEY".into(), secret_name: "provider:anthropic".into(), base_url: None, models: vec![] };
        let result = sync("claude", dir.path(), &provider, "sk-test-123", false).unwrap();
        assert!(result.applied);

        let written: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(written["theme"], "dark");
        assert_eq!(written["env"]["OTHER_VAR"], "x");
        assert_eq!(written["env"]["ANTHROPIC_API_KEY"], "sk-test-123");
    }

    #[test]
    fn codex_sync_only_accepts_openai_api_key() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".codex")).unwrap();
        std::fs::write(dir.path().join(".codex/auth.json"), r#"{"auth_mode":"apikey","tokens":{}}"#).unwrap();

        let openai_provider = ProviderSpec { name: "openai".into(), env_var_name: "OPENAI_API_KEY".into(), secret_name: "provider:openai".into(), base_url: None, models: vec![] };
        let ok = sync("codex", dir.path(), &openai_provider, "sk-openai-1", false).unwrap();
        assert!(ok.applied);
        let written: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join(".codex/auth.json")).unwrap()).unwrap();
        assert_eq!(written["auth_mode"], "apikey");
        assert_eq!(written["OPENAI_API_KEY"], "sk-openai-1");

        let other_provider = ProviderSpec { name: "other".into(), env_var_name: "SOME_OTHER_KEY".into(), secret_name: "provider:other".into(), base_url: None, models: vec![] };
        let rejected = sync("codex", dir.path(), &other_provider, "value", false).unwrap();
        assert!(!rejected.applied);
    }

    #[test]
    fn unverified_agents_are_refused_not_faked() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ProviderSpec { name: "test".into(), env_var_name: "OPENAI_API_KEY".into(), secret_name: "provider:test".into(), base_url: None, models: vec![] };
        let result = sync("perplexity", dir.path(), &provider, "x", false).unwrap();
        assert!(!result.applied);
    }

    #[test]
    fn dry_run_never_writes() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ProviderSpec { name: "anthropic".into(), env_var_name: "ANTHROPIC_API_KEY".into(), secret_name: "provider:anthropic".into(), base_url: None, models: vec![] };
        let result = sync("claude", dir.path(), &provider, "sk-test", true).unwrap();
        assert!(!result.applied);
        assert!(!dir.path().join(".claude/settings.json").exists());
    }

    #[test]
    fn opencode_sync_is_unsupported_without_declared_models() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ProviderSpec { name: "omniroute".into(), env_var_name: "OMNIROUTE_API_KEY".into(), secret_name: "provider:omniroute".into(), base_url: Some("http://localhost:20128/v1".into()), models: vec![] };
        let result = sync("opencode", dir.path(), &provider, "placeholder", false).unwrap();
        assert!(!result.applied);
        assert!(result.detail.contains("no models declared"));
    }

    #[test]
    fn opencode_sync_writes_provider_config_when_models_are_declared() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ProviderSpec {
            name: "omniroute".into(),
            env_var_name: "OMNIROUTE_API_KEY".into(),
            secret_name: "provider:omniroute".into(),
            base_url: Some("http://localhost:20128/v1".into()),
            models: vec![divisi_protocol::ModelSpec { id: "auto".into(), name: "Auto (best available)".into() }],
        };
        let result = sync("opencode", dir.path(), &provider, "placeholder", false).unwrap();
        assert!(result.applied);
        let path = dir.path().join(".config/opencode/opencode.jsonc");
        let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["provider"]["omniroute"]["options"]["apiKey"], "{env:OMNIROUTE_API_KEY}");
    }

    #[test]
    fn opencode_sync_backs_up_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".config/opencode")).unwrap();
        std::fs::write(dir.path().join(".config/opencode/opencode.jsonc"), "{\"mcp\": {}}").unwrap();
        let provider = ProviderSpec {
            name: "omniroute".into(),
            env_var_name: "OMNIROUTE_API_KEY".into(),
            secret_name: "provider:omniroute".into(),
            base_url: Some("http://localhost:20128/v1".into()),
            models: vec![divisi_protocol::ModelSpec { id: "auto".into(), name: "Auto".into() }],
        };
        let result = sync("opencode", dir.path(), &provider, "placeholder", false).unwrap();
        assert!(result.backup_path.is_some());
    }
}
