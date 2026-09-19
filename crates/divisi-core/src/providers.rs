//! The provider registry (spec section 30): OpenAI, Anthropic, OpenCode
//! Zen, or any other LLM API, stored at `~/.config/divisi/providers.toml`.
//! The registry only holds *metadata* — which env var name an agent needs,
//! and which secret-store entry holds the actual key
//! (`divisi-core::secrets`). The key value itself never touches this file.

use anyhow::{Context, Result};
use divisi_protocol::ProviderSpec;
use std::collections::BTreeMap;
use std::path::Path;

pub fn load(path: &Path) -> Result<Vec<ProviderSpec>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: ProviderRegistryFile =
        toml::from_str(&text).with_context(|| format!("parsing {} as TOML", path.display()))?;
    Ok(file.providers.into_values().collect())
}

pub fn save(path: &Path, providers: &[ProviderSpec]) -> Result<()> {
    let mut map = BTreeMap::new();
    for provider in providers {
        map.insert(provider.name.clone(), provider.clone());
    }
    let file = ProviderRegistryFile { providers: map };
    let rendered = toml::to_string_pretty(&file).context("serializing provider registry")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))
}

/// Register a provider, or update one that already exists. There is no
/// separate `provider update` / `add-model` command, so `add` doubles as
/// both: a field the caller left empty means "keep what's already there",
/// not "clear it" — otherwise re-running `add` to tweak one flag silently
/// dropped `base_url` and `models`. Non-empty fields overwrite; `models`
/// merge additively (upsert by id).
pub fn add(path: &Path, provider: ProviderSpec) -> Result<()> {
    let mut providers = load(path)?;
    match providers.iter_mut().find(|p| p.name == provider.name) {
        Some(existing) => {
            if provider.base_url.is_some() {
                existing.base_url = provider.base_url;
            }
            if !provider.env_var_name.is_empty() {
                existing.env_var_name = provider.env_var_name;
            }
            for model in provider.models {
                match existing.models.iter_mut().find(|m| m.id == model.id) {
                    Some(slot) => *slot = model,
                    None => existing.models.push(model),
                }
            }
            // `secret_name` follows the `provider:<name>` convention and
            // never changes for a given name — leave the existing one.
        }
        None => providers.push(provider),
    }
    save(path, &providers)
}

pub fn remove(path: &Path, name: &str) -> Result<bool> {
    let mut providers = load(path)?;
    let before = providers.len();
    providers.retain(|p| p.name != name);
    let removed = providers.len() != before;
    if removed {
        save(path, &providers)?;
    }
    Ok(removed)
}

pub fn find(path: &Path, name: &str) -> Result<Option<ProviderSpec>> {
    Ok(load(path)?.into_iter().find(|p| p.name == name))
}

/// Providers that actually have a key stored — a shared key (`set-key`)
/// or any labeled per-agent key (`add-key`, see `provider_keys.rs`) —
/// distinct from `load()`'s full registry, which unconditionally carries
/// every built-in preset regardless of whether it's ever been configured
/// (`sync_missing_presets` below; `ProviderSpec` has no `enabled` field,
/// unlike MCP/LSP/Tools). This is what answers "which of these did I
/// actually set up," used by the TUI's Providers tab and `single
/// provider list --configured`.
pub fn configured(providers_path: &Path, provider_keys_path: &Path) -> Result<Vec<ProviderSpec>> {
    let all = load(providers_path)?;
    let store = crate::secrets::SecretTool;
    Ok(all
        .into_iter()
        .filter(|provider| {
            let has_shared_key = crate::secrets::SecretStore::get(&store, &provider.secret_name).ok().flatten().is_some();
            let has_labeled_key =
                crate::provider_keys::list_for_provider(provider_keys_path, &provider.name).map(|keys| !keys.is_empty()).unwrap_or(false);
            has_shared_key || has_labeled_key
        })
        .collect())
}

/// Registers every built-in preset not already present in the registry.
/// Safe to bulk-add: a `ProviderSpec` is just metadata (env var name, base
/// URL, and a pointer to a secret-store entry) — no key material moves and
/// nothing is used by any agent until its secret is actually set. Idempotent:
/// safe to call on every startup. Returns how many were newly added.
pub fn sync_missing_presets(path: &Path) -> Result<usize> {
    let mut providers = load(path)?;
    let existing: std::collections::HashSet<String> = providers.iter().map(|p| p.name.clone()).collect();
    let mut added = 0;
    for preset in presets() {
        if !existing.contains(preset.name) {
            providers.push(preset.to_spec());
            added += 1;
        }
    }
    if added > 0 {
        save(path, &providers)?;
    }
    Ok(added)
}

/// A well-known preset a user can pick from instead of typing an env var
/// name/base URL by hand (e.g. in the TUI's "add provider" flow).
/// `secret_name` is deliberately not part of the preset — it's always
/// derived as `provider:<name>` when the preset is turned into a real
/// `ProviderSpec`, keeping that convention in one place.
#[derive(Debug, Clone)]
pub struct ProviderPreset {
    pub name: &'static str,
    pub env_var_name: &'static str,
    pub base_url: &'static str,
}

/// Every base URL/env var pair here was verified against the vendor's own
/// current documentation, not guessed:
/// - OpenAI, Anthropic: their well-documented standard API endpoints.
/// - OpenCode Zen: `https://opencode.ai/zen/v1`, `OPENCODE_API_KEY` (opencode.ai/docs/providers).
/// - NVIDIA: `https://integrate.api.nvidia.com/v1`, `NVIDIA_API_KEY` (build.nvidia.com's own OpenAI-compatible endpoint docs).
///
/// v0.1.18 additions (each confirmed against the vendor's own current docs,
/// not guessed — all are OpenAI-compatible Chat Completions endpoints):
/// - Groq: `https://api.groq.com/openai/v1` (console.groq.com/docs/openai)
/// - DeepSeek: `https://api.deepseek.com` (api-docs.deepseek.com)
/// - Mistral: `https://api.mistral.ai/v1` (docs.mistral.ai)
/// - xAI (Grok): `https://api.x.ai/v1` (docs.x.ai)
/// - Google AI Studio: `https://generativelanguage.googleapis.com/v1beta/openai/` (ai.google.dev/gemini-api/docs/openai)
/// - OpenRouter: `https://openrouter.ai/api/v1` (openrouter.ai/docs)
/// - Together AI: `https://api.together.xyz/v1` (docs.together.ai)
/// - Fireworks AI: `https://api.fireworks.ai/inference/v1` (docs.fireworks.ai)
/// - Cerebras: `https://api.cerebras.ai/v1` (inference-docs.cerebras.ai)
/// - SambaNova: `https://api.sambanova.ai/v1` (docs.sambanova.ai)
/// - DeepInfra: `https://api.deepinfra.com/v1/openai` (docs.deepinfra.com)
/// - Perplexity: `https://api.perplexity.ai` (docs.perplexity.ai)
/// - Cohere: `https://api.cohere.ai/compatibility/v1` (docs.cohere.com)
pub fn presets() -> Vec<ProviderPreset> {
    vec![
        ProviderPreset { name: "openai", env_var_name: "OPENAI_API_KEY", base_url: "https://api.openai.com/v1" },
        ProviderPreset { name: "anthropic", env_var_name: "ANTHROPIC_API_KEY", base_url: "https://api.anthropic.com" },
        ProviderPreset { name: "opencode-zen", env_var_name: "OPENCODE_API_KEY", base_url: "https://opencode.ai/zen/v1" },
        ProviderPreset { name: "nvidia", env_var_name: "NVIDIA_API_KEY", base_url: "https://integrate.api.nvidia.com/v1" },
        ProviderPreset { name: "groq", env_var_name: "GROQ_API_KEY", base_url: "https://api.groq.com/openai/v1" },
        ProviderPreset { name: "deepseek", env_var_name: "DEEPSEEK_API_KEY", base_url: "https://api.deepseek.com" },
        ProviderPreset { name: "mistral", env_var_name: "MISTRAL_API_KEY", base_url: "https://api.mistral.ai/v1" },
        ProviderPreset { name: "xai", env_var_name: "XAI_API_KEY", base_url: "https://api.x.ai/v1" },
        ProviderPreset {
            name: "google",
            env_var_name: "GOOGLE_API_KEY",
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai/",
        },
        ProviderPreset { name: "openrouter", env_var_name: "OPENROUTER_API_KEY", base_url: "https://openrouter.ai/api/v1" },
        ProviderPreset { name: "together", env_var_name: "TOGETHER_API_KEY", base_url: "https://api.together.xyz/v1" },
        ProviderPreset { name: "fireworks", env_var_name: "FIREWORKS_API_KEY", base_url: "https://api.fireworks.ai/inference/v1" },
        ProviderPreset { name: "cerebras", env_var_name: "CEREBRAS_API_KEY", base_url: "https://api.cerebras.ai/v1" },
        ProviderPreset { name: "sambanova", env_var_name: "SAMBANOVA_API_KEY", base_url: "https://api.sambanova.ai/v1" },
        ProviderPreset { name: "deepinfra", env_var_name: "DEEPINFRA_API_KEY", base_url: "https://api.deepinfra.com/v1/openai" },
        ProviderPreset { name: "perplexity", env_var_name: "PERPLEXITY_API_KEY", base_url: "https://api.perplexity.ai" },
        ProviderPreset { name: "cohere", env_var_name: "COHERE_API_KEY", base_url: "https://api.cohere.ai/compatibility/v1" },
    ]
}

pub fn preset(name: &str) -> Option<ProviderPreset> {
    presets().into_iter().find(|p| p.name == name)
}

impl ProviderPreset {
    pub fn to_spec(&self) -> ProviderSpec {
        ProviderSpec {
            name: self.name.to_string(),
            env_var_name: self.env_var_name.to_string(),
            secret_name: format!("provider:{}", self.name),
            base_url: Some(self.base_url.to_string()),
            models: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct ProviderRegistryFile {
    #[serde(default)]
    providers: BTreeMap<String, ProviderSpec>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ProviderSpec {
        ProviderSpec { name: "anthropic".into(), env_var_name: "ANTHROPIC_API_KEY".into(), secret_name: "provider:anthropic".into(), base_url: None, models: Vec::new() }
    }

    #[test]
    fn add_then_find_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        add(&path, sample()).unwrap();
        let found = find(&path, "anthropic").unwrap().unwrap();
        assert_eq!(found.env_var_name, "ANTHROPIC_API_KEY");
    }

    #[test]
    fn add_updates_an_existing_provider_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        add(&path, sample()).unwrap();
        let mut updated = sample();
        updated.base_url = Some("https://custom.example.com".into());
        add(&path, updated).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].base_url.as_deref(), Some("https://custom.example.com"));
    }

    #[test]
    fn add_preserves_fields_the_caller_left_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        add(&path, ProviderSpec {
            name: "zen".into(),
            env_var_name: "OPENCODE_API_KEY".into(),
            secret_name: "provider:zen".into(),
            base_url: Some("https://opencode.ai/zen/v1".into()),
            models: vec![divisi_protocol::ModelSpec { id: "a".into(), name: "A".into() }],
        })
        .unwrap();

        // Re-add naming only the new model — base_url and the existing
        // model must survive, and the new model is appended.
        add(&path, ProviderSpec {
            name: "zen".into(),
            env_var_name: "OPENCODE_API_KEY".into(),
            secret_name: "provider:zen".into(),
            base_url: None,
            models: vec![divisi_protocol::ModelSpec { id: "b".into(), name: "B".into() }],
        })
        .unwrap();

        let loaded = find(&path, "zen").unwrap().unwrap();
        assert_eq!(loaded.base_url.as_deref(), Some("https://opencode.ai/zen/v1"));
        let ids: Vec<&str> = loaded.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn remove_reports_whether_anything_was_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        add(&path, sample()).unwrap();
        assert!(remove(&path, "anthropic").unwrap());
        assert!(!remove(&path, "anthropic").unwrap());
    }

    #[test]
    fn presets_include_the_four_original_providers() {
        let names: Vec<_> = presets().iter().map(|p| p.name).collect();
        for name in ["openai", "anthropic", "opencode-zen", "nvidia"] {
            assert!(names.contains(&name), "missing original preset {name}");
        }
    }

    #[test]
    fn v0_1_18_catalog_expansion_added_at_least_13_new_providers_with_unique_names_and_https_urls() {
        let presets = presets();
        assert!(presets.len() >= 17, "expected at least 17 total providers (4 original + 13 new), got {}", presets.len());

        let mut names: Vec<&str> = presets.iter().map(|p| p.name).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before, "duplicate provider preset name");

        for preset in &presets {
            assert!(preset.base_url.starts_with("https://"), "{} base_url should be https", preset.name);
            assert!(preset.env_var_name.ends_with("_API_KEY"), "{} env var should end in _API_KEY", preset.name);
        }
    }

    #[test]
    fn sync_missing_presets_registers_every_preset_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.toml");

        let added_first = sync_missing_presets(&path).unwrap();
        assert_eq!(added_first, presets().len());

        let providers = load(&path).unwrap();
        for preset in presets() {
            assert!(providers.iter().any(|p| p.name == preset.name), "missing {}", preset.name);
        }

        let added_second = sync_missing_presets(&path).unwrap();
        assert_eq!(added_second, 0, "second run should be a no-op");
    }

    #[test]
    fn sync_missing_presets_never_overwrites_an_already_registered_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        let first_preset = presets().first().unwrap().name.to_string();
        add(&path, ProviderSpec {
            name: first_preset.clone(),
            env_var_name: "CUSTOM_KEY".into(),
            secret_name: "provider:custom".into(),
            base_url: Some("https://custom.example.com".into()),
            models: Vec::new(),
        })
        .unwrap();

        sync_missing_presets(&path).unwrap();

        let provider = find(&path, &first_preset).unwrap().unwrap();
        assert_eq!(provider.env_var_name, "CUSTOM_KEY");
    }

    #[test]
    fn preset_to_spec_derives_secret_name_convention() {
        let spec = preset("nvidia").unwrap().to_spec();
        assert_eq!(spec.secret_name, "provider:nvidia");
        assert_eq!(spec.env_var_name, "NVIDIA_API_KEY");
        assert_eq!(spec.base_url.as_deref(), Some("https://integrate.api.nvidia.com/v1"));
    }

    #[test]
    fn preset_returns_none_for_unknown_name() {
        assert!(preset("does-not-exist").is_none());
    }

    /// Real OS keychain round trips (not mocked), same discipline
    /// `provider_keys.rs`'s own `resolve_env_for_agent` tests use — clearly
    /// test-scoped provider names so nothing here can collide with a real
    /// stored key, with cleanup at the end of each test.
    mod configured_tests {
        use super::*;
        use crate::secrets::{SecretStore, SecretTool};

        #[test]
        fn a_preset_with_no_key_at_all_is_not_configured() {
            let dir = tempfile::tempdir().unwrap();
            let providers_path = dir.path().join("providers.toml");
            let keys_path = dir.path().join("provider_keys.toml");
            add(&providers_path, ProviderSpec {
                name: "divisi-test-unconfigured".into(),
                env_var_name: "SINGLECLI_TEST_UNCONFIGURED_KEY".into(),
                secret_name: "provider:divisi-test-unconfigured".into(),
                base_url: None,
                models: Vec::new(),
            })
            .unwrap();

            let result = configured(&providers_path, &keys_path).unwrap();
            assert!(result.is_empty(), "a preset with no stored key anywhere must not appear as configured");
        }

        #[test]
        fn a_shared_key_marks_the_provider_configured() {
            let dir = tempfile::tempdir().unwrap();
            let providers_path = dir.path().join("providers.toml");
            let keys_path = dir.path().join("provider_keys.toml");
            let secret_name = "provider:divisi-test-shared-configured".to_string();
            add(&providers_path, ProviderSpec {
                name: "divisi-test-shared-configured".into(),
                env_var_name: "SINGLECLI_TEST_SHARED_CONFIGURED_KEY".into(),
                secret_name: secret_name.clone(),
                base_url: None,
                models: Vec::new(),
            })
            .unwrap();
            let store = SecretTool;
            SecretStore::set(&store, &secret_name, "some-value").unwrap();

            let result = configured(&providers_path, &keys_path).unwrap();
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].name, "divisi-test-shared-configured");

            SecretStore::delete(&store, &secret_name).unwrap();
        }

        #[test]
        fn a_labeled_key_with_no_shared_key_also_marks_the_provider_configured() {
            let dir = tempfile::tempdir().unwrap();
            let providers_path = dir.path().join("providers.toml");
            let keys_path = dir.path().join("provider_keys.toml");
            let provider = "divisi-test-labeled-configured";
            add(&providers_path, ProviderSpec {
                name: provider.into(),
                env_var_name: "SINGLECLI_TEST_LABELED_CONFIGURED_KEY".into(),
                secret_name: format!("provider:{provider}"),
                base_url: None,
                models: Vec::new(),
            })
            .unwrap();
            let key_secret_name = crate::provider_keys::secret_name(provider, "mylabel");
            let store = SecretTool;
            SecretStore::set(&store, &key_secret_name, "labeled-value").unwrap();
            crate::provider_keys::add(&keys_path, divisi_protocol::ProviderKeySpec {
                provider: provider.into(),
                label: "mylabel".into(),
                agent: Some("some-agent".into()),
                secret_name: key_secret_name.clone(),
            })
            .unwrap();

            let result = configured(&providers_path, &keys_path).unwrap();
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].name, provider);

            SecretStore::delete(&store, &key_secret_name).unwrap();
        }
    }

    #[test]
    fn add_and_load_preserves_declared_models() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        add(&path, ProviderSpec {
            name: "omniroute".into(),
            env_var_name: "OMNIROUTE_API_KEY".into(),
            secret_name: "provider:omniroute".into(),
            base_url: Some("http://localhost:20128/v1".into()),
            models: vec![divisi_protocol::ModelSpec { id: "auto".into(), name: "Auto (best available)".into() }],
        })
        .unwrap();

        let loaded = find(&path, "omniroute").unwrap().unwrap();
        assert_eq!(loaded.models.len(), 1);
        assert_eq!(loaded.models[0].id, "auto");
        assert_eq!(loaded.models[0].name, "Auto (best available)");
    }
}
