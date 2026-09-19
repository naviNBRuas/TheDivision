//! Settings for the notch UI's background poller — how often it checks
//! task/agent state to refresh what it shows. Stored at
//! `~/.config/divisi/notch.toml`, same load/save shape as
//! `docker.rs`/`task_hooks.rs`.
//!
//! `DIVISI_NOTCH_POLL_MS` overrides the on-disk `poll_ms` at load time
//! (tests, and anyone who wants a one-off poll interval without editing
//! the config file) — same override precedence as `DIVISI_CONFIG_DIR` in
//! `paths.rs`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Default poll interval: frequent enough that the notch feels live,
/// infrequent enough not to burn CPU polling task/agent state at rest.
pub const DEFAULT_POLL_MS: u64 = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotchConfig {
    pub poll_ms: u64,
}

impl Default for NotchConfig {
    fn default() -> Self {
        Self { poll_ms: DEFAULT_POLL_MS }
    }
}

/// Loads `path`, falling back to defaults if it doesn't exist. Whatever
/// `poll_ms` results (file or default) is then overridden by
/// `DIVISI_NOTCH_POLL_MS`, if set and parseable, so a caller never needs
/// to special-case "no config file yet" vs. "config file with an env
/// override" — both land on the same value.
pub fn load(path: &Path) -> Result<NotchConfig> {
    let mut config = if path.exists() {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {} as TOML", path.display()))?
    } else {
        NotchConfig::default()
    };
    if let Ok(raw) = std::env::var("DIVISI_NOTCH_POLL_MS") {
        config.poll_ms = raw.parse().with_context(|| format!("DIVISI_NOTCH_POLL_MS={raw:?} is not a valid u64"))?;
    }
    Ok(config)
}

pub fn save(path: &Path, config: &NotchConfig) -> Result<()> {
    let rendered = toml::to_string_pretty(config).context("serializing notch config")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `DIVISI_NOTCH_POLL_MS` is process-global env state — serialize the
    // tests that touch it so they can't interleave and clobber each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn load_missing_file_returns_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("DIVISI_NOTCH_POLL_MS");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notch.toml");
        assert_eq!(load(&path).unwrap(), NotchConfig::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("DIVISI_NOTCH_POLL_MS");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notch.toml");
        let config = NotchConfig { poll_ms: 2500 };
        save(&path, &config).unwrap();
        assert_eq!(load(&path).unwrap(), config);
    }

    #[test]
    fn env_var_overrides_file_value() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notch.toml");
        save(&path, &NotchConfig { poll_ms: 2500 }).unwrap();
        std::env::set_var("DIVISI_NOTCH_POLL_MS", "42");
        let result = load(&path);
        std::env::remove_var("DIVISI_NOTCH_POLL_MS");
        assert_eq!(result.unwrap().poll_ms, 42);
    }

    #[test]
    fn env_var_overrides_defaults_when_no_file_exists() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notch.toml");
        std::env::set_var("DIVISI_NOTCH_POLL_MS", "77");
        let result = load(&path);
        std::env::remove_var("DIVISI_NOTCH_POLL_MS");
        assert_eq!(result.unwrap().poll_ms, 77);
    }

    #[test]
    fn invalid_env_var_errors_instead_of_silently_ignoring() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notch.toml");
        std::env::set_var("DIVISI_NOTCH_POLL_MS", "not-a-number");
        let result = load(&path);
        std::env::remove_var("DIVISI_NOTCH_POLL_MS");
        assert!(result.is_err());
    }
}
