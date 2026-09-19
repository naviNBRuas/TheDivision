//! Profiles (spec section 8). Phase 1 scope: list profiles that exist as
//! `<name>.toml` files under `profiles/`, and switch the active one by
//! writing `profile = "<name>"` into the global config file. Profile bodies
//! reuse `ConfigLayer`, so a profile can already override `agents.enabled`
//! and `mcp.enabled` — richer profile-scoped policy (permissions, budgets,
//! sandbox) is later-phase work per spec section 8.

use crate::config::ConfigLayer;
use crate::paths::DivisiDirs;
use anyhow::{Context, Result};

pub fn list_profiles(dirs: &DivisiDirs) -> Result<Vec<String>> {
    let dir = dirs.profiles_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Switches the active profile by rewriting the `profile` key in the global
/// config file. Preserves the rest of the file's content (parses, mutates,
/// re-serializes) rather than clobbering it.
pub fn use_profile(dirs: &DivisiDirs, name: &str) -> Result<()> {
    let profile_path = dirs.profiles_dir().join(format!("{name}.toml"));
    if !profile_path.exists() {
        anyhow::bail!(
            "no profile named '{name}' at {} — create it first",
            profile_path.display()
        );
    }

    let config_path = dirs.config_file();
    let mut layer = if config_path.exists() {
        let text = std::fs::read_to_string(&config_path)
            .with_context(|| format!("reading {}", config_path.display()))?;
        toml::from_str::<ConfigLayer>(&text)
            .with_context(|| format!("parsing {}", config_path.display()))?
    } else {
        ConfigLayer::default()
    };

    layer.profile = Some(name.to_string());

    let serialized = toml::to_string_pretty(&layer).context("serializing config")?;
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&config_path, serialized)
        .with_context(|| format!("writing {}", config_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_profiles_returns_empty_when_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = DivisiDirs::from_root(dir.path().to_path_buf());
        assert_eq!(list_profiles(&dirs).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn list_profiles_finds_toml_files() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = DivisiDirs::from_root(dir.path().to_path_buf());
        dirs.ensure_created().unwrap();
        std::fs::write(dirs.profiles_dir().join("work.toml"), "").unwrap();
        std::fs::write(dirs.profiles_dir().join("personal.toml"), "").unwrap();
        std::fs::write(dirs.profiles_dir().join("notes.txt"), "").unwrap();
        assert_eq!(list_profiles(&dirs).unwrap(), vec!["personal", "work"]);
    }

    #[test]
    fn use_profile_fails_when_profile_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = DivisiDirs::from_root(dir.path().to_path_buf());
        dirs.ensure_created().unwrap();
        assert!(use_profile(&dirs, "ghost").is_err());
    }

    #[test]
    fn use_profile_writes_profile_key() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = DivisiDirs::from_root(dir.path().to_path_buf());
        dirs.ensure_created().unwrap();
        std::fs::write(dirs.profiles_dir().join("security.toml"), "").unwrap();
        use_profile(&dirs, "security").unwrap();
        let resolved = crate::config::resolve(&dirs, None).unwrap();
        assert_eq!(resolved.active_profile, "security");
    }
}
