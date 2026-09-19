//! Ordered agent/account fallback chains, stored at
//! `~/.config/single/fallback.toml`: what `task run --allow-fallback`
//! (`divisi-runtime::task::execute`) tries next when a run hits a detected
//! rate limit (`divisi_core::ratelimit`). Mirrors `providers.rs`'s
//! load/save shape.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use divisi_protocol::AgentAccountRef;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FallbackFile {
    #[serde(default)]
    chains: Vec<Vec<AgentAccountRef>>,
}

pub fn load(path: &Path) -> Result<Vec<Vec<AgentAccountRef>>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: FallbackFile = toml::from_str(&text).with_context(|| format!("parsing {} as TOML", path.display()))?;
    Ok(file.chains)
}

fn save(path: &Path, chains: &[Vec<AgentAccountRef>]) -> Result<()> {
    let file = FallbackFile { chains: chains.to_vec() };
    let rendered = toml::to_string_pretty(&file).context("serializing fallback registry")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))
}

/// Replaces any existing chain whose first entry matches `chain`'s first
/// entry, rather than accumulating duplicates — re-running `fallback set`
/// with a different tail is how you edit a chain.
pub fn set(path: &Path, chain: Vec<AgentAccountRef>) -> Result<()> {
    anyhow::ensure!(chain.len() >= 2, "a fallback chain needs at least two entries (what to run, and what to try next)");
    let mut chains = load(path)?;
    let Some(first) = chain.first().cloned() else { unreachable!("checked above") };
    chains.retain(|c| c.first() != Some(&first));
    chains.push(chain);
    save(path, &chains)
}

pub fn remove(path: &Path, first: &AgentAccountRef) -> Result<bool> {
    let mut chains = load(path)?;
    let before = chains.len();
    chains.retain(|c| c.first() != Some(first));
    let removed = chains.len() != before;
    if removed {
        save(path, &chains)?;
    }
    Ok(removed)
}

/// The next entry after `current` in whichever saved chain contains it, if
/// any — `divisi-runtime::task::execute`'s one hop per detected rate
/// limit. Searching by position (not just "any entry after a match")
/// means a chain that happens to repeat an entry can't loop: each call
/// only ever looks at the single chain containing `current`, and advances
/// exactly one step along it.
pub fn next_after(path: &Path, current: &AgentAccountRef) -> Result<Option<AgentAccountRef>> {
    let chains = load(path)?;
    for chain in chains {
        if let Some(pos) = chain.iter().position(|entry| entry == current) {
            return Ok(chain.get(pos + 1).cloned());
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(agent: &str, account: Option<&str>) -> AgentAccountRef {
        AgentAccountRef { agent: agent.into(), account: account.map(String::from) }
    }

    #[test]
    fn set_then_next_after_walks_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fallback.toml");
        set(&path, vec![entry("claude", Some("work")), entry("claude", Some("personal")), entry("codex", None)]).unwrap();

        assert_eq!(next_after(&path, &entry("claude", Some("work"))).unwrap(), Some(entry("claude", Some("personal"))));
        assert_eq!(next_after(&path, &entry("claude", Some("personal"))).unwrap(), Some(entry("codex", None)));
        assert_eq!(next_after(&path, &entry("codex", None)).unwrap(), None, "last entry has nothing after it");
    }

    #[test]
    fn unrelated_entry_has_no_next() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fallback.toml");
        set(&path, vec![entry("claude", None), entry("codex", None)]).unwrap();
        assert_eq!(next_after(&path, &entry("opencode", None)).unwrap(), None);
    }

    #[test]
    fn set_replaces_the_chain_with_the_same_first_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fallback.toml");
        set(&path, vec![entry("claude", None), entry("codex", None)]).unwrap();
        set(&path, vec![entry("claude", None), entry("opencode", None)]).unwrap();

        let chains = load(&path).unwrap();
        assert_eq!(chains.len(), 1, "re-setting a chain must replace, not accumulate");
        assert_eq!(next_after(&path, &entry("claude", None)).unwrap(), Some(entry("opencode", None)));
    }

    #[test]
    fn remove_reports_whether_anything_was_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fallback.toml");
        set(&path, vec![entry("claude", None), entry("codex", None)]).unwrap();

        assert!(remove(&path, &entry("claude", None)).unwrap());
        assert!(!remove(&path, &entry("claude", None)).unwrap(), "already gone");
    }
}
