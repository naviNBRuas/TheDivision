//! Secret abstraction (spec section 15): OS keychain first, never
//! plaintext config. Phase 2 implements the Linux backend only
//! (`secret-tool`, part of libsecret — already the mechanism this
//! machine's own MCP configs use to keep API keys out of config files, so
//! it's a real, observed-working backend rather than an invented one).
//! macOS Keychain / Windows Credential Manager backends are future work —
//! `SecretStore` is the trait seam for them.
//!
//! Values are never returned in a form that gets logged: callers get an
//! opaque `Result<String>` for `get`, and `list` returns names only, never
//! values. `divisi-runtime`'s event log (`state.rs`) must not record
//! secret values — see `handlers.rs`, which never passes a value into
//! `record_event`.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

const SERVICE: &str = "divisi-cli";

/// Service names earlier builds stored keys under. The rebrand renamed `SERVICE`, which left every key
/// already in the keyring invisible (provider agents failed with "no API key stored"). Reads and clears
/// also look here, `list` merges them, and a key found under a legacy name is copied to `SERVICE`.
const LEGACY_SERVICES: &[&str] = &["single-cli"];

fn lookup(service: &str, name: &str) -> Result<Option<String>> {
    let output = Command::new("secret-tool")
        .args(["lookup", "service", service, "name", name])
        .output()
        .context("spawning secret-tool lookup")?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8(output.stdout).context("secret-tool returned non-UTF8 output")?;
    Ok(Some(value))
}

fn clear(service: &str, name: &str) -> Result<bool> {
    let status = Command::new("secret-tool")
        .args(["clear", "service", service, "name", name])
        .status()
        .context("spawning secret-tool clear")?;
    Ok(status.success())
}

fn search_names(service: &str) -> Result<Vec<String>> {
    let output = Command::new("secret-tool")
        .args(["search", "--all", "--unlock", "service", service])
        .output()
        .context("spawning secret-tool search")?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    // secret-tool (libsecret 0.21.x, observed on this machine) writes the "attribute.name = " lines
    // this parser needs to stderr, not stdout, for `search --all` specifically (label/secret/created/
    // modified/schema go to stdout as expected) — a real quirk of this version, not assumed.
    // Concatenating both streams is the robust fix rather than depending on which stream a given
    // secret-tool build happens to use.
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(parse_names(&text))
}

fn parse_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = text.lines().filter_map(|l| l.trim().strip_prefix("attribute.name = ")).map(str::to_string).collect();
    names.sort();
    names.dedup();
    names
}

pub trait SecretStore {
    fn set(&self, name: &str, value: &str) -> Result<()>;
    fn get(&self, name: &str) -> Result<Option<String>>;
    fn delete(&self, name: &str) -> Result<bool>;
    fn list(&self) -> Result<Vec<String>>;
}

/// Linux backend via `secret-tool` (libsecret). Requires a running
/// keyring daemon (gnome-keyring/kwallet-equivalent) — the same
/// requirement the user's existing MCP configs already depend on.
pub struct SecretTool;

impl SecretStore for SecretTool {
    fn set(&self, name: &str, value: &str) -> Result<()> {
        let mut child = Command::new("secret-tool")
            .args(["store", "--label", &format!("divisi secret: {name}"), "service", SERVICE, "name", name])
            .stdin(Stdio::piped())
            .spawn()
            .context("spawning secret-tool store (is libsecret-tools installed?)")?;
        child
            .stdin
            .take()
            .context("secret-tool stdin unavailable")?
            .write_all(value.as_bytes())
            .context("writing secret value to secret-tool")?;
        let status = child.wait().context("waiting for secret-tool store")?;
        if !status.success() {
            bail!("secret-tool store exited with {status}");
        }
        Ok(())
    }

    fn get(&self, name: &str) -> Result<Option<String>> {
        if let Some(value) = lookup(SERVICE, name)? {
            return Ok(Some(value));
        }
        for legacy in LEGACY_SERVICES {
            if let Some(value) = lookup(legacy, name)? {
                // Best effort: move it to the current service so later lookups hit directly.
                let _ = self.set(name, &value);
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    fn delete(&self, name: &str) -> Result<bool> {
        let mut cleared = clear(SERVICE, name)?;
        for legacy in LEGACY_SERVICES {
            cleared |= clear(legacy, name)?;
        }
        Ok(cleared)
    }

    fn list(&self) -> Result<Vec<String>> {
        let mut names = search_names(SERVICE)?;
        for legacy in LEGACY_SERVICES {
            names.extend(search_names(legacy)?);
        }
        names.sort();
        names.dedup();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `secret-tool` needs a real keyring daemon, unavailable in most CI/
    /// sandboxed environments — this only checks that a missing binary
    /// produces a clear error rather than a panic, without asserting on
    /// keyring behavior itself.
    #[test]
    fn get_on_missing_name_or_missing_backend_does_not_panic() {
        let store = SecretTool;
        let _ = store.get("single-cli-test-key-that-should-not-exist-xyz");
    }

    #[test]
    fn names_are_parsed_from_search_output_and_deduplicated() {
        let text = "[/org/x/1]\nlabel = a\nattribute.name = provider:nvidia\nattribute.service = single-cli\n[/org/x/2]\nattribute.name = provider:groq\nattribute.name = provider:nvidia\n";
        assert_eq!(parse_names(text), vec!["provider:groq".to_string(), "provider:nvidia".to_string()]);
    }

    #[test]
    fn the_pre_rename_service_is_still_consulted() {
        // Guards the rebrand regression: keys stored under the old service must stay reachable.
        assert!(LEGACY_SERVICES.contains(&"single-cli"));
        assert!(!LEGACY_SERVICES.contains(&SERVICE));
    }
}
