//! The unified plugin registry (spec section 29): one list of plugins,
//! stored at `~/.config/single/plugins.toml`, synced out to every agent
//! that has a real plugin-install command. Verified real commands per
//! agent (`--help` output on the reference machine, not assumed):
//! `claude plugin install <plugin[@marketplace]>`, `codex plugin add
//! <plugin[@marketplace]>`, `agy plugin install <target>` (same
//! `plugin@marketplace` convention as claude/codex), and `opencode plugin
//! <npm-module>` (a genuinely different addressing scheme — a plain npm
//! module name, no marketplace concept).

use anyhow::{Context, Result};
use divisi_protocol::PluginSpec;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct PluginRegistryFile {
    #[serde(default)]
    plugins: BTreeMap<String, PluginSpec>,
}

pub fn load(path: &Path) -> Result<Vec<PluginSpec>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: PluginRegistryFile = toml::from_str(&text).with_context(|| format!("parsing {} as TOML", path.display()))?;
    Ok(file.plugins.into_values().collect())
}

pub fn save(path: &Path, plugins: &[PluginSpec]) -> Result<()> {
    let mut map = BTreeMap::new();
    for plugin in plugins {
        map.insert(plugin.name.clone(), plugin.clone());
    }
    let file = PluginRegistryFile { plugins: map };
    let rendered = toml::to_string_pretty(&file).context("serializing plugin registry")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))
}

pub fn add(path: &Path, plugin: PluginSpec) -> Result<()> {
    let mut plugins = load(path)?;
    plugins.retain(|p| p.name != plugin.name);
    plugins.push(plugin);
    save(path, &plugins)
}

pub fn remove(path: &Path, name: &str) -> Result<bool> {
    let mut plugins = load(path)?;
    let before = plugins.len();
    plugins.retain(|p| p.name != name);
    let removed = plugins.len() != before;
    if removed {
        save(path, &plugins)?;
    }
    Ok(removed)
}

pub fn find(path: &Path, name: &str) -> Result<Option<PluginSpec>> {
    Ok(load(path)?.into_iter().find(|p| p.name == name))
}

/// Registers every built-in preset not already present in the registry.
/// Being registered doesn't install anything into any agent — `single
/// plugin sync` is the separate, explicit step that does that — so unlike
/// the MCP/LSP equivalents there's no `enabled` flag to force off here.
/// Idempotent: safe to call on every startup. Returns how many were newly
/// added.
pub fn sync_missing_presets(path: &Path) -> Result<usize> {
    let mut plugins = load(path)?;
    let existing: std::collections::HashSet<String> = plugins.iter().map(|p| p.name.clone()).collect();
    let mut added = 0;
    for preset in presets() {
        if !existing.contains(preset.name) {
            plugins.push(preset.to_spec());
            added += 1;
        }
    }
    if added > 0 {
        save(path, &plugins)?;
    }
    Ok(added)
}

/// A named starter config for a real plugin, not yet in the user's
/// registry — same growth mechanism as `mcp::McpPreset`/`lsp::LspPreset`
/// (`single plugin add-preset <name>`). Every entry below was pulled
/// verbatim from Anthropic's own official plugin marketplace manifest
/// (github.com/anthropics/claude-plugins-official,
/// `.claude-plugin/marketplace.json`, fetched live) — real plugin names
/// Anthropic itself lists as approved, not a guessed or scraped list.
///
/// **Verified sync target: `claude` only.** `target` resolves via
/// `claude plugin install <name>@claude-plugins-official`, which works
/// once that marketplace is added (`claude plugin marketplace add
/// anthropics/claude-plugins-official`) — the real, documented flow this
/// project's own `claude plugin install <plugin[@marketplace]>` command
/// already uses for any target string. Syncing the same preset to
/// codex/agy would require *that* agent to separately recognize the
/// `claude-plugins-official` marketplace, which isn't verified here (it's
/// an Anthropic-specific, Claude-Code-schema manifest) — same honesty
/// standard as `lsp.rs`'s claude-unsupported note, applied in reverse.
/// `opencode_module` is `None` for all of these: none has a confirmed
/// OpenCode npm-module equivalent.
pub struct PluginPreset {
    pub name: &'static str,
    pub target: &'static str,
    pub opencode_module: Option<&'static str>,
}

pub fn presets() -> Vec<PluginPreset> {
    vec![
        PluginPreset { name: "stripe", target: "stripe@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "github", target: "github@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "sentry", target: "sentry@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "sentry-cli", target: "sentry-cli@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "datadog", target: "datadog@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "grafana-assistant", target: "grafana-assistant@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "playwright", target: "playwright@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "figma", target: "figma@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "notion", target: "notion@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "slack", target: "slack@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "atlassian", target: "atlassian@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "asana", target: "asana@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "cloudflare", target: "cloudflare@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "aws-core", target: "aws-core@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "aws-serverless", target: "aws-serverless@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "aws-dev-toolkit", target: "aws-dev-toolkit@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "azure", target: "azure@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "google-cloud-storage", target: "google-cloud-storage@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "mongodb", target: "mongodb@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "mongodb-atlas", target: "mongodb-atlas@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "auth0", target: "auth0@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "twilio-developer-kit", target: "twilio-developer-kit@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "vercel", target: "vercel@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "netlify-skills", target: "netlify-skills@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "linear", target: "linear@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "shopify-ai-toolkit", target: "shopify-ai-toolkit@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "salesforce-development", target: "salesforce-development@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "pagerduty", target: "pagerduty@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "snowflake-cortex-code", target: "snowflake-cortex-code@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "redis-development", target: "redis-development@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "supabase", target: "supabase@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "sonarqube", target: "sonarqube@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "terraform", target: "terraform@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "zapier", target: "zapier@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "discord", target: "discord@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "airtable", target: "airtable@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "monday-crm", target: "monday-crm@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "claude-security", target: "claude-security@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "crowdstrike-falcon-foundry", target: "crowdstrike-falcon-foundry@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "hostinger", target: "hostinger@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "railway", target: "railway@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "canva", target: "canva@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "miro", target: "miro@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "fullstory", target: "fullstory@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "amplitude", target: "amplitude@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "gitlab", target: "gitlab@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "cloud-sql-postgresql", target: "cloud-sql-postgresql@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "cloud-sql-mysql", target: "cloud-sql-mysql@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "build-with-wordpress", target: "build-with-wordpress@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "newrelic", target: "newrelic@claude-plugins-official", opencode_module: None },
        PluginPreset { name: "honeycomb", target: "honeycomb@claude-plugins-official", opencode_module: None },
    ]
}

pub fn preset(name: &str) -> Option<PluginPreset> {
    presets().into_iter().find(|p| p.name == name)
}

impl PluginPreset {
    pub fn to_spec(&self) -> PluginSpec {
        PluginSpec { name: self.name.to_string(), target: self.target.to_string(), opencode_module: self.opencode_module.map(str::to_string) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PluginSpec {
        PluginSpec { name: "example".into(), target: "example@official".into(), opencode_module: Some("@example/opencode-plugin".into()) }
    }

    #[test]
    fn add_then_find_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        add(&path, sample()).unwrap();
        let found = find(&path, "example").unwrap().unwrap();
        assert_eq!(found.target, "example@official");
        assert_eq!(found.opencode_module.as_deref(), Some("@example/opencode-plugin"));
    }

    #[test]
    fn remove_reports_whether_anything_was_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        add(&path, sample()).unwrap();
        assert!(remove(&path, "example").unwrap());
        assert!(!remove(&path, "example").unwrap());
    }

    #[test]
    fn v0_1_16_catalog_has_50_plus_unique_real_marketplace_presets() {
        let presets = presets();
        assert!(presets.len() >= 50, "expected 50+ plugin presets, got {}", presets.len());

        let mut seen = std::collections::HashSet::new();
        for p in &presets {
            assert!(seen.insert(p.name), "duplicate preset name: {}", p.name);
            assert_eq!(p.target, format!("{}@claude-plugins-official", p.name), "preset {} target doesn't match its marketplace convention", p.name);
            assert!(p.opencode_module.is_none(), "preset {} should have no verified opencode module", p.name);
        }
    }

    #[test]
    fn preset_resolves_by_name_and_to_spec_round_trips() {
        assert!(preset("nonexistent").is_none());
        let spec = preset("github").unwrap().to_spec();
        assert_eq!(spec.name, "github");
        assert_eq!(spec.target, "github@claude-plugins-official");
        assert!(spec.opencode_module.is_none());
    }

    #[test]
    fn sync_missing_presets_registers_every_preset_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");

        let added_first = sync_missing_presets(&path).unwrap();
        assert_eq!(added_first, presets().len());

        let plugins = load(&path).unwrap();
        for preset in presets() {
            assert!(plugins.iter().any(|p| p.name == preset.name), "missing {}", preset.name);
        }

        let added_second = sync_missing_presets(&path).unwrap();
        assert_eq!(added_second, 0, "second run should be a no-op");
    }

    #[test]
    fn sync_missing_presets_never_overwrites_an_already_registered_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        let first_preset = presets().first().unwrap().name.to_string();
        add(&path, PluginSpec { name: first_preset.clone(), target: "custom-target".into(), opencode_module: None }).unwrap();

        sync_missing_presets(&path).unwrap();

        let plugin = find(&path, &first_preset).unwrap().unwrap();
        assert_eq!(plugin.target, "custom-target");
    }
}
