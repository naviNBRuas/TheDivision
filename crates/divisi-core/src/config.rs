//! Configuration precedence: global -> profile -> project.
//!
//! Phase 1 scope is deliberately small (which agents/MCP servers are
//! enabled, which profile is active) — enough to prove the precedence model
//! works end to end. Later phases add more fields to `ConfigLayer` without
//! changing how merging works.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigLayer {
    pub profile: Option<String>,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub mcp: McpConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentsConfig {
    #[serde(default)]
    pub enabled: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpConfig {
    #[serde(default)]
    pub enabled: Option<Vec<String>>,
}

/// Fully resolved configuration after merging all layers. Unlike
/// `ConfigLayer`, every field has a concrete value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub active_profile: String,
    pub enabled_agents: Vec<String>,
    pub enabled_mcp: Vec<String>,
}

impl Default for ResolvedConfig {
    fn default() -> Self {
        Self {
            active_profile: "default".to_string(),
            enabled_agents: vec![
                "claude".into(),
                "codex".into(),
                "opencode".into(),
                "agy".into(),
                "perplexity".into(),
            ],
            enabled_mcp: Vec::new(),
        }
    }
}

/// Merges layers in precedence order: later layers win. A `None` field means
/// "layer doesn't specify this" and falls through to the next layer, or the
/// default if no layer specifies it. This mirrors spec section 7/28: project
/// config overrides profile config overrides global config.
pub fn merge_layers(layers: &[ConfigLayer]) -> ResolvedConfig {
    let mut resolved = ResolvedConfig::default();
    for layer in layers {
        if let Some(profile) = &layer.profile {
            resolved.active_profile = profile.clone();
        }
        if let Some(enabled) = &layer.agents.enabled {
            resolved.enabled_agents = enabled.clone();
        }
        if let Some(enabled) = &layer.mcp.enabled {
            resolved.enabled_mcp = enabled.clone();
        }
    }
    resolved
}

pub fn load_toml_layer(path: &Path) -> anyhow::Result<Option<ConfigLayer>> {
    load_layer(path, Format::Toml)
}

pub fn load_yaml_layer(path: &Path) -> anyhow::Result<Option<ConfigLayer>> {
    load_layer(path, Format::Yaml)
}

enum Format {
    Toml,
    Yaml,
}

fn load_layer(path: &Path, format: Format) -> anyhow::Result<Option<ConfigLayer>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    let layer = match format {
        Format::Toml => toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parsing {} as TOML: {e}", path.display()))?,
        Format::Yaml => serde_yaml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parsing {} as YAML: {e}", path.display()))?,
    };
    Ok(Some(layer))
}

/// Loads and merges the full global -> profile -> project chain for a given
/// project root (or just global+profile if `project_root` is `None`).
pub fn resolve(
    dirs: &crate::paths::DivisiDirs,
    project_root: Option<&Path>,
) -> anyhow::Result<ResolvedConfig> {
    let mut layers = Vec::new();

    if let Some(global) = load_toml_layer(&dirs.config_file())? {
        let profile_name = global.profile.clone();
        layers.push(global);

        if let Some(profile_name) = profile_name {
            let profile_path = dirs.profiles_dir().join(format!("{profile_name}.toml"));
            if let Some(profile_layer) = load_toml_layer(&profile_path)? {
                layers.push(profile_layer);
            }
        }
    }

    if let Some(root) = project_root {
        let project_path = crate::paths::project_config_path(root);
        if let Some(project_layer) = load_yaml_layer(&project_path)? {
            layers.push(project_layer);
        }
    }

    Ok(merge_layers(&layers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_layers_fall_back_to_defaults() {
        let resolved = merge_layers(&[]);
        assert_eq!(resolved, ResolvedConfig::default());
    }

    #[test]
    fn later_layer_overrides_earlier() {
        let global = ConfigLayer {
            profile: Some("personal".into()),
            agents: AgentsConfig { enabled: Some(vec!["claude".into()]) },
            mcp: McpConfig::default(),
        };
        let project = ConfigLayer {
            profile: None,
            agents: AgentsConfig { enabled: Some(vec!["claude".into(), "codex".into()]) },
            mcp: McpConfig { enabled: Some(vec!["git".into()]) },
        };
        let resolved = merge_layers(&[global, project]);
        assert_eq!(resolved.active_profile, "personal");
        assert_eq!(resolved.enabled_agents, vec!["claude", "codex"]);
        assert_eq!(resolved.enabled_mcp, vec!["git"]);
    }

    #[test]
    fn missing_field_falls_through_to_previous_layer() {
        let global = ConfigLayer {
            profile: Some("work".into()),
            agents: AgentsConfig { enabled: Some(vec!["codex".into()]) },
            mcp: McpConfig::default(),
        };
        let profile = ConfigLayer {
            profile: None, // doesn't override
            agents: AgentsConfig { enabled: None }, // doesn't override
            mcp: McpConfig { enabled: Some(vec!["memory".into()]) },
        };
        let resolved = merge_layers(&[global, profile]);
        assert_eq!(resolved.active_profile, "work");
        assert_eq!(resolved.enabled_agents, vec!["codex"]);
        assert_eq!(resolved.enabled_mcp, vec!["memory"]);
    }

    #[test]
    fn load_toml_layer_returns_none_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_toml_layer(&dir.path().join("nope.toml")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_toml_layer_parses_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "profile = \"security\"\n[agents]\nenabled = [\"claude\"]\n").unwrap();
        let layer = load_toml_layer(&path).unwrap().unwrap();
        assert_eq!(layer.profile.as_deref(), Some("security"));
        assert_eq!(layer.agents.enabled, Some(vec!["claude".to_string()]));
    }
}
