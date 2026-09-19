//! Labeled per-agent provider keys (`~/.config/single/provider_keys.toml`).
//!
//! `divisi_core::providers` models one shared key per provider — fine for
//! "sync my Anthropic key into every agent," but useless for real per-agent
//! `$` attribution on the Usage page, since a shared key means a provider's
//! billing API can only report "this key spent $X," not which agent's
//! calls it was. This module adds a second, independent layer: any number
//! of *labeled* keys per provider, each optionally tagged with the one
//! agent it's for. `single provider set-key`/`sync` are untouched — they
//! keep working against the single-key registry exactly as before; this
//! is purely additive.
//!
//! `secret_name` follows `"provider-key:{provider}:{label}"`, kept
//! distinct from `providers.rs`'s `"provider:{name}"` convention so the
//! two registries can never collide in the OS keychain.

use anyhow::{Context, Result};
use divisi_protocol::ProviderKeySpec;
use std::collections::BTreeMap;
use std::path::Path;

pub const DEFAULT_LABEL: &str = "default";

pub fn secret_name(provider: &str, label: &str) -> String {
    format!("provider-key:{provider}:{label}")
}

pub fn load(path: &Path) -> Result<Vec<ProviderKeySpec>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: ProviderKeyRegistryFile =
        toml::from_str(&text).with_context(|| format!("parsing {} as TOML", path.display()))?;
    Ok(file.keys.into_values().collect())
}

pub fn save(path: &Path, keys: &[ProviderKeySpec]) -> Result<()> {
    let mut map = BTreeMap::new();
    for key in keys {
        map.insert(composite_key(&key.provider, &key.label), key.clone());
    }
    let file = ProviderKeyRegistryFile { keys: map };
    let rendered = toml::to_string_pretty(&file).context("serializing provider key registry")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))
}

/// Adds or replaces (by `provider`+`label`) one labeled key entry. Does
/// not touch the OS keychain — the caller stores the actual secret value
/// separately via `divisi_core::secrets`, keyed by `secret_name(provider,
/// label)`, same separation `providers.rs` already has between registry
/// metadata and the real key value.
pub fn add(path: &Path, key: ProviderKeySpec) -> Result<()> {
    let mut keys = load(path)?;
    keys.retain(|k| !(k.provider == key.provider && k.label == key.label));
    keys.push(key);
    save(path, &keys)
}

pub fn remove(path: &Path, provider: &str, label: &str) -> Result<bool> {
    let mut keys = load(path)?;
    let before = keys.len();
    keys.retain(|k| !(k.provider == provider && k.label == label));
    let removed = keys.len() != before;
    if removed {
        save(path, &keys)?;
    }
    Ok(removed)
}

pub fn find(path: &Path, provider: &str, label: &str) -> Result<Option<ProviderKeySpec>> {
    Ok(load(path)?.into_iter().find(|k| k.provider == provider && k.label == label))
}

pub fn list_for_provider(path: &Path, provider: &str) -> Result<Vec<ProviderKeySpec>> {
    Ok(load(path)?.into_iter().filter(|k| k.provider == provider).collect())
}

/// Resolves which provider API keys `agent` should see as environment
/// variables for a real run — the missing link between "a key is stored
/// in SingleCLI" and "the agent process that authenticates via a plain
/// env var, not OAuth, actually has it." Consumed by
/// `divisi-agent-sdk::backend::ExecBackend`'s `extra_env`.
///
/// Two sources, in this order:
/// 1. Labeled per-agent keys (`ProviderKeySpec::agent == Some(agent)`) —
///    the precise mechanism: `single provider add-key <provider> --label
///    <label> --agent <agent> <value>`.
/// 2. A shared single-key provider (`providers.toml`/`set-key`) whose
///    *name* exactly matches `agent` — covers the common case of a
///    standalone agent CLI that's also its own provider name (e.g. a
///    `droid` agent authenticating with the `droid` provider's key)
///    without injecting every configured provider's key into every
///    agent regardless of relevance. Skipped for a provider already
///    covered by a labeled key in (1), so an explicit per-agent
///    assignment always wins over the shared fallback.
///
/// Best-effort: a missing/unreachable keychain resolves to "nothing to
/// inject" rather than failing the caller — same discipline
/// `divisi_core::backup::export` already applies to the same failure
/// mode (secret-tool absent shouldn't block an otherwise-runnable task).
pub fn resolve_env_for_agent(dirs: &crate::paths::DivisiDirs, agent: &str) -> BTreeMap<String, String> {
    use crate::secrets::{SecretStore, SecretTool};

    let mut env = BTreeMap::new();
    let store = SecretTool;
    let mut covered_providers = std::collections::HashSet::new();

    if let Ok(keys) = load(&dirs.provider_keys_registry_file()) {
        for key in keys.into_iter().filter(|k| k.agent.as_deref() == Some(agent)) {
            let Ok(Some(provider_spec)) = crate::providers::find(&dirs.providers_registry_file(), &key.provider) else { continue };
            let Ok(Some(value)) = SecretStore::get(&store, &key.secret_name) else { continue };
            env.insert(provider_spec.env_var_name, value);
            covered_providers.insert(key.provider);
        }
    }

    if !covered_providers.contains(agent) {
        if let Ok(Some(provider_spec)) = crate::providers::find(&dirs.providers_registry_file(), agent) {
            if let Ok(Some(value)) = SecretStore::get(&store, &provider_spec.secret_name) {
                env.insert(provider_spec.env_var_name, value);
            }
        }
    }

    env
}

fn composite_key(provider: &str, label: &str) -> String {
    format!("{provider}:{label}")
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct ProviderKeyRegistryFile {
    #[serde(default)]
    keys: BTreeMap<String, ProviderKeySpec>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(provider: &str, label: &str, agent: Option<&str>) -> ProviderKeySpec {
        ProviderKeySpec {
            provider: provider.into(),
            label: label.into(),
            agent: agent.map(String::from),
            secret_name: secret_name(provider, label),
        }
    }

    #[test]
    fn add_then_find_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider_keys.toml");
        add(&path, sample("anthropic", "claude", Some("claude"))).unwrap();
        let found = find(&path, "anthropic", "claude").unwrap().unwrap();
        assert_eq!(found.agent.as_deref(), Some("claude"));
        assert_eq!(found.secret_name, "provider-key:anthropic:claude");
    }

    #[test]
    fn multiple_labels_coexist_for_the_same_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider_keys.toml");
        add(&path, sample("anthropic", "claude", Some("claude"))).unwrap();
        add(&path, sample("anthropic", "aider", Some("aider"))).unwrap();
        let all = list_for_provider(&path, "anthropic").unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn add_replaces_by_provider_and_label() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider_keys.toml");
        add(&path, sample("anthropic", "claude", Some("claude"))).unwrap();
        add(&path, sample("anthropic", "claude", Some("claude-2"))).unwrap();
        let all = list_for_provider(&path, "anthropic").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].agent.as_deref(), Some("claude-2"));
    }

    #[test]
    fn remove_reports_whether_anything_was_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider_keys.toml");
        add(&path, sample("anthropic", "claude", None)).unwrap();
        assert!(remove(&path, "anthropic", "claude").unwrap());
        assert!(!remove(&path, "anthropic", "claude").unwrap());
    }

    #[test]
    fn different_providers_with_the_same_label_dont_collide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider_keys.toml");
        add(&path, sample("anthropic", "default", None)).unwrap();
        add(&path, sample("openai", "default", None)).unwrap();
        assert_eq!(load(&path).unwrap().len(), 2);
    }

    /// Real OS keychain round trips (not mocked) — `resolve_env_for_agent`
    /// only means anything as an end-to-end test against the real
    /// `secret-tool` backend, same as `secrets.rs`'s own test. Uses
    /// clearly test-scoped provider/agent/secret names so nothing here
    /// can collide with a real stored key, and cleans up what it wrote.
    mod resolve_env_for_agent_tests {
        use super::*;
        use crate::paths::DivisiDirs;
        use crate::secrets::{SecretStore, SecretTool};

        fn test_dirs() -> (tempfile::TempDir, DivisiDirs) {
            let dir = tempfile::tempdir().unwrap();
            let dirs = DivisiDirs::from_root(dir.path().to_path_buf());
            (dir, dirs)
        }

        #[test]
        fn labeled_per_agent_key_resolves_to_the_providers_env_var_name() {
            let (_tmp, dirs) = test_dirs();
            let provider = "singlecli-test-provider-labeled";
            let agent = "singlecli-test-agent-labeled";
            crate::providers::add(&dirs.providers_registry_file(), divisi_protocol::ProviderSpec {
                name: provider.into(),
                env_var_name: "SINGLECLI_TEST_LABELED_KEY".into(),
                secret_name: format!("provider:{provider}"),
                base_url: None,
                models: Vec::new(),
            })
            .unwrap();
            let key_secret_name = secret_name(provider, "mylabel");
            let store = SecretTool;
            SecretStore::set(&store, &key_secret_name, "labeled-value").unwrap();
            add(&dirs.provider_keys_registry_file(), ProviderKeySpec {
                provider: provider.into(),
                label: "mylabel".into(),
                agent: Some(agent.into()),
                secret_name: key_secret_name.clone(),
            })
            .unwrap();

            let env = resolve_env_for_agent(&dirs, agent);
            assert_eq!(env.get("SINGLECLI_TEST_LABELED_KEY").map(String::as_str), Some("labeled-value"));

            SecretStore::delete(&store, &key_secret_name).unwrap();
        }

        #[test]
        fn shared_key_falls_back_when_provider_name_matches_agent_name() {
            let (_tmp, dirs) = test_dirs();
            let provider_and_agent = "singlecli-test-agent-shared";
            let secret_name = format!("provider:{provider_and_agent}");
            crate::providers::add(&dirs.providers_registry_file(), divisi_protocol::ProviderSpec {
                name: provider_and_agent.into(),
                env_var_name: "SINGLECLI_TEST_SHARED_KEY".into(),
                secret_name: secret_name.clone(),
                base_url: None,
                models: Vec::new(),
            })
            .unwrap();
            let store = SecretTool;
            SecretStore::set(&store, &secret_name, "shared-value").unwrap();

            let env = resolve_env_for_agent(&dirs, provider_and_agent);
            assert_eq!(env.get("SINGLECLI_TEST_SHARED_KEY").map(String::as_str), Some("shared-value"));

            SecretStore::delete(&store, &secret_name).unwrap();
        }

        #[test]
        fn unrelated_agent_gets_nothing_injected() {
            let (_tmp, dirs) = test_dirs();
            let provider = "singlecli-test-provider-unrelated";
            crate::providers::add(&dirs.providers_registry_file(), divisi_protocol::ProviderSpec {
                name: provider.into(),
                env_var_name: "SINGLECLI_TEST_UNRELATED_KEY".into(),
                secret_name: format!("provider:{provider}"),
                base_url: None,
                models: Vec::new(),
            })
            .unwrap();
            let store = SecretTool;
            SecretStore::set(&store, &format!("provider:{provider}"), "unrelated-value").unwrap();

            let env = resolve_env_for_agent(&dirs, "some-other-agent-entirely");
            assert!(env.is_empty(), "a provider whose name doesn't match the agent, with no labeled key, must not be injected");

            SecretStore::delete(&store, &format!("provider:{provider}")).unwrap();
        }
    }
}
