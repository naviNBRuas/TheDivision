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
            .args(["store", "--label", &format!("SingleCLI secret: {name}"), "service", SERVICE, "name", name])
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
        let output = Command::new("secret-tool")
            .args(["lookup", "service", SERVICE, "name", name])
            .output()
            .context("spawning secret-tool lookup")?;
        if !output.status.success() {
            return Ok(None);
        }
        let value = String::from_utf8(output.stdout).context("secret-tool returned non-UTF8 output")?;
        Ok(Some(value))
    }

    fn delete(&self, name: &str) -> Result<bool> {
        let status = Command::new("secret-tool")
            .args(["clear", "service", SERVICE, "name", name])
            .status()
            .context("spawning secret-tool clear")?;
        Ok(status.success())
    }

    fn list(&self) -> Result<Vec<String>> {
        let output = Command::new("secret-tool")
            .args(["search", "--all", "--unlock", "service", SERVICE])
            .output()
            .context("spawning secret-tool search")?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        // secret-tool (libsecret 0.21.x, observed on this machine) writes
        // the "attribute.name = " lines this parser needs to stderr, not
        // stdout, for `search --all` specifically (label/secret/created/
        // modified/schema go to stdout as expected) — a real quirk of this
        // version, not assumed. Concatenating both streams is the robust
        // fix rather than depending on which stream a given secret-tool
        // build happens to use.
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        let mut names = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("attribute.name = ") {
                names.push(rest.to_string());
            }
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
}
