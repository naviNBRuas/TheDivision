//! Per-agent SingleCLI-managed `$HOME` (see `paths.rs::homes_dir` doc
//! comment). SingleCLI used to run every agent CLI, sync its config, and
//! capture/switch its credentials directly against the real, ambient
//! `$HOME` (`~/.claude.json`, `~/.codex/`, ...). That meant SingleCLI's
//! behavior depended on — and could disturb — files it didn't own.
//!
//! Instead, each agent gets an isolated home under
//! `~/.config/single/homes/<agent>/`. The **first** time that directory is
//! used, it's bootstrapped with a one-time copy of that agent's real,
//! already-verified config/state paths (the same paths
//! `divisi-agent-sdk::formats` and `divisi-core::account` read/write) —
//! this is the only time the real home is touched. Every SingleCLI-driven
//! run after that (`task run`, `install-integrations`, `plugin sync`,
//! `account capture/use`, ...) reads and writes only inside the isolated
//! copy; the real `~/.claude`, `~/.codex`, etc. are left alone.
//!
//! **Credentials are never seeded.** The bootstrap copy carries over
//! non-auth config (MCP servers, settings) so a freshly-created isolated
//! home isn't empty, but `credential_paths_for` lists the specific files
//! that are stripped back out right after the copy (e.g. claude's
//! `.claude/.credentials.json`, codex's `.codex/auth.json`). A brand-new
//! isolated home always starts logged out, even on a machine where the
//! real home already has a live login — see `divisi-core::account`'s
//! module doc for why (`single account capture`/`is_authenticated` only
//! ever look inside the isolated home too).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn home_dir(homes_root: &Path, agent: &str) -> PathBuf {
    homes_root.join(agent)
}

/// Real, `$HOME`-relative paths that make up an agent's config/login
/// state — the same locations documented and verified elsewhere in this
/// project (`divisi-agent-sdk::formats::{claude,codex,opencode}`,
/// `divisi-core::account`'s per-agent doc comments), not a guessed list.
/// `agy` and any custom agent have no confirmed on-disk location, so
/// bootstrap for them is a no-op empty directory — SingleCLI simply has
/// nothing to seed the isolated home with.
fn real_paths_for(agent: &str) -> &'static [&'static str] {
    match agent {
        "claude" => &[".claude.json", ".claude"],
        "codex" => &[".codex"],
        "opencode" => &[".config/opencode"],
        "cursor" => &[".cursor"],
        "goose" => &[".config/goose"],
        "aider" => &[".aider.conf.yml"],
        "copilot" => &[".copilot"],
        _ => &[],
    }
}

/// Credential files (relative to a home dir) that must never survive the
/// bootstrap copy, even though they live inside a directory `real_paths_for`
/// otherwise copies wholesale for its non-auth config (MCP servers,
/// settings). Only claude and codex have a confirmed single-file credential
/// location inside those paths — see `divisi-core::account`'s per-agent
/// doc comments for the same paths used by `has_live_login`/`capture`.
/// Cursor's login lives as a *field* inside a shared prefs file instead of
/// its own file — see `strip_embedded_credential_fields` for that case.
fn credential_paths_for(agent: &str) -> &'static [&'static str] {
    match agent {
        "claude" => &[".claude/.credentials.json"],
        "codex" => &[".codex/auth.json"],
        _ => &[],
    }
}

/// Same purpose as `credential_paths_for`, for agents whose login state is
/// a field inside a config file `real_paths_for` otherwise copies
/// wholesale, rather than a dedicated credential file. Removing the whole
/// file would also drop legitimate non-auth prefs (editor/display
/// settings), so this strips just the named field(s) in place instead.
///
/// copilot is here for a sharper reason than cursor: its actual token
/// lives in the system OS keyring, not under `$HOME` at all, so a bare
/// file copy can't leak the secret itself — but `config.json`'s
/// `lastLoggedInUser`/`loggedInUsers` fields are the *pointer* copilot
/// uses to look that keyring entry up. Since the keyring is shared
/// system-wide (not namespaced per isolated home), leaving that pointer
/// in place would let a freshly-bootstrapped isolated home silently
/// authenticate as whoever is logged in on the real machine — see
/// `divisi-core::account`'s `copilot_identity`/`keyring_lookup` docs.
fn strip_embedded_credential_fields(dest: &Path, agent: &str) -> Result<()> {
    let (rel, fields): (&str, &[&str]) = match agent {
        "cursor" => (".cursor/cli-config.json", &["authInfo"]),
        "copilot" => (".copilot/config.json", &["lastLoggedInUser", "loggedInUsers"]),
        _ => return Ok(()),
    };
    let path = dest.join(rel);
    if !path.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    // copilot's config.json starts with `//`-prefixed comment lines,
    // which strict JSON parsing rejects — see the matching strip in
    // `divisi-core::account::extract_json_fields`. No-op for cursor's
    // plain-JSON file.
    let text: String = text.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    let mut value: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {} as JSON", path.display()))?;
    if let Some(obj) = value.as_object_mut() {
        let mut changed = false;
        for field in fields {
            changed |= obj.remove(*field).is_some();
        }
        if changed {
            std::fs::write(&path, serde_json::to_string_pretty(&value)?).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(())
}

/// Ensures `homes_root/<agent>` exists, bootstrapping it from `real_home`
/// only if it doesn't exist yet. An already-bootstrapped isolated home is
/// never re-synced from the real one on subsequent calls — once it
/// exists, it's independent, which is the whole point.
pub fn ensure_bootstrapped(homes_root: &Path, real_home: &Path, agent: &str) -> Result<PathBuf> {
    let dest = home_dir(homes_root, agent);
    if dest.exists() {
        // The directory itself is never re-synced from `real_home` here,
        // but credential stripping always re-runs: isolated homes created
        // by an older SingleCLI build (before a given agent had a
        // `credential_paths_for`/`strip_embedded_credential_fields` entry)
        // can still be carrying real, possibly-stale copied credentials
        // that were never stripped. Re-stripping on every call is cheap
        // and idempotent, and closes that leak without disturbing an
        // isolated home's independence from the real one otherwise.
        strip_credentials(&dest, agent)?;
        return Ok(dest);
    }
    std::fs::create_dir_all(&dest).with_context(|| format!("creating {}", dest.display()))?;
    for rel in real_paths_for(agent) {
        let src = real_home.join(rel);
        if !src.exists() {
            continue;
        }
        let dst = dest.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if src.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    strip_credentials(&dest, agent)?;
    Ok(dest)
}

fn strip_credentials(dest: &Path, agent: &str) -> Result<()> {
    for rel in credential_paths_for(agent) {
        let path = dest.join(rel);
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("removing seeded credential {}", path.display()))?;
        }
    }
    strip_embedded_credential_fields(dest, agent)
}

fn copy_dir_recursive(source: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        // Symlinks (e.g. a `lib64 -> lib` venv link under a plugin's own
        // state dir) are skipped rather than followed: `fs::copy` can't
        // handle a symlink pointing at a directory (errors loudly), and
        // resolving+copying its target risks pulling in something outside
        // this agent's real config/state paths entirely — not what
        // `real_paths_for` promises to bootstrap. Bootstrap is best-effort
        // config seeding, not a full mirror.
        if file_type.is_symlink() {
            continue;
        }
        let dest_path = dest.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else if file_type.is_file() {
            // Best-effort per file: a real agent config dir can hold a
            // FIFO/socket (an IDE-integration socket, a `daemon.lock`
            // that's really a pipe) or a file the owning app deletes out
            // from under us mid-copy. `fs::copy` errors hard on those
            // (`copy_file_range` returns ENXIO for a non-regular source,
            // ENOENT for a vanished one) — a single odd entry shouldn't
            // abort bootstrapping the whole isolated home, so skip it and
            // keep going. `is_file()` above already filters out the
            // obvious special types before we even try.
            if let Err(e) = std::fs::copy(entry.path(), &dest_path) {
                tracing::debug!(path = %entry.path().display(), error = %e, "skipping unreadable entry during isolated-home bootstrap");
            }
        }
        // Anything else (FIFO, socket, char/block device) is skipped.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstraps_known_paths_on_first_use_only() {
        let real_home = tempfile::tempdir().unwrap();
        std::fs::write(real_home.path().join(".claude.json"), r#"{"numStartups":1}"#).unwrap();
        std::fs::create_dir_all(real_home.path().join(".claude")).unwrap();
        std::fs::write(real_home.path().join(".claude/settings.json"), r#"{"theme":"dark"}"#).unwrap();

        let homes_root = tempfile::tempdir().unwrap();
        let isolated = ensure_bootstrapped(homes_root.path(), real_home.path(), "claude").unwrap();
        assert_eq!(std::fs::read_to_string(isolated.join(".claude.json")).unwrap(), r#"{"numStartups":1}"#);
        assert_eq!(std::fs::read_to_string(isolated.join(".claude/settings.json")).unwrap(), r#"{"theme":"dark"}"#);

        // Real home changes after bootstrap; the isolated copy must NOT pick it up.
        std::fs::write(real_home.path().join(".claude.json"), r#"{"numStartups":99}"#).unwrap();
        let isolated_again = ensure_bootstrapped(homes_root.path(), real_home.path(), "claude").unwrap();
        assert_eq!(std::fs::read_to_string(isolated_again.join(".claude.json")).unwrap(), r#"{"numStartups":1}"#);
    }

    #[test]
    fn bootstrap_seeds_non_auth_config_but_never_credentials() {
        let real_home = tempfile::tempdir().unwrap();
        std::fs::write(real_home.path().join(".claude.json"), r#"{"numStartups":1}"#).unwrap();
        std::fs::create_dir_all(real_home.path().join(".claude")).unwrap();
        std::fs::write(real_home.path().join(".claude/.credentials.json"), r#"{"token":"a"}"#).unwrap();
        std::fs::write(real_home.path().join(".claude/settings.json"), r#"{"theme":"dark"}"#).unwrap();
        std::fs::create_dir_all(real_home.path().join(".codex")).unwrap();
        std::fs::write(real_home.path().join(".codex/auth.json"), r#"{"tokens":"b"}"#).unwrap();
        std::fs::write(real_home.path().join(".codex/config.toml"), "profile = \"default\"").unwrap();

        let homes_root = tempfile::tempdir().unwrap();
        let claude_isolated = ensure_bootstrapped(homes_root.path(), real_home.path(), "claude").unwrap();
        assert!(!claude_isolated.join(".claude/.credentials.json").exists());
        assert_eq!(std::fs::read_to_string(claude_isolated.join(".claude/settings.json")).unwrap(), r#"{"theme":"dark"}"#);

        let codex_isolated = ensure_bootstrapped(homes_root.path(), real_home.path(), "codex").unwrap();
        assert!(!codex_isolated.join(".codex/auth.json").exists());
        assert_eq!(std::fs::read_to_string(codex_isolated.join(".codex/config.toml")).unwrap(), "profile = \"default\"");
    }

    #[test]
    fn bootstrap_strips_cursor_auth_info_field_but_keeps_other_prefs() {
        let real_home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(real_home.path().join(".cursor")).unwrap();
        std::fs::write(
            real_home.path().join(".cursor/cli-config.json"),
            r#"{"editor":{"vimMode":true},"authInfo":{"email":"a@example.com","authId":"secretA"}}"#,
        )
        .unwrap();

        let homes_root = tempfile::tempdir().unwrap();
        let isolated = ensure_bootstrapped(homes_root.path(), real_home.path(), "cursor").unwrap();

        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(isolated.join(".cursor/cli-config.json")).unwrap()).unwrap();
        assert!(config.get("authInfo").is_none(), "authInfo must be stripped from a freshly bootstrapped isolated home");
        assert_eq!(config["editor"]["vimMode"], true, "non-auth prefs must survive the strip");
    }

    #[test]
    fn bootstrap_strips_copilot_login_pointer_despite_comment_header() {
        let real_home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(real_home.path().join(".copilot")).unwrap();
        std::fs::write(
            real_home.path().join(".copilot/config.json"),
            "// User settings belong in settings.json.\n// This file is managed automatically.\n{\"lastLoggedInUser\":{\"host\":\"https://github.com\",\"login\":\"navinbruas\"},\"loggedInUsers\":[{\"host\":\"https://github.com\",\"login\":\"navinbruas\"}]}\n",
        )
        .unwrap();

        let homes_root = tempfile::tempdir().unwrap();
        let isolated = ensure_bootstrapped(homes_root.path(), real_home.path(), "copilot").unwrap();

        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(isolated.join(".copilot/config.json")).unwrap()).unwrap();
        assert!(config.get("lastLoggedInUser").is_none(), "a freshly bootstrapped isolated home must not inherit the real login pointer");
        assert!(config.get("loggedInUsers").is_none());
    }

    #[test]
    fn agents_with_no_confirmed_real_path_get_an_empty_isolated_home() {
        let real_home = tempfile::tempdir().unwrap();
        let homes_root = tempfile::tempdir().unwrap();
        let isolated = ensure_bootstrapped(homes_root.path(), real_home.path(), "agy").unwrap();
        assert!(isolated.exists());
        assert_eq!(std::fs::read_dir(&isolated).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn bootstrap_skips_symlinks_instead_of_erroring_on_a_dir_symlink() {
        // Reproduces a real case: Claude Code's own security-plugin venv
        // leaves a `lib64 -> lib` symlink under `~/.claude/`, which used to
        // make `fs::copy` fail outright ("neither a regular file nor a
        // symlink to a regular file") and abort the whole bootstrap.
        use std::os::unix::fs::symlink;

        let real_home = tempfile::tempdir().unwrap();
        std::fs::write(real_home.path().join(".claude.json"), r#"{}"#).unwrap();
        std::fs::create_dir_all(real_home.path().join(".claude/venv/lib")).unwrap();
        std::fs::write(real_home.path().join(".claude/venv/lib/marker.txt"), "present").unwrap();
        std::fs::write(real_home.path().join(".claude/.credentials.json"), r#"{"token":"a"}"#).unwrap();
        symlink(real_home.path().join(".claude/venv/lib"), real_home.path().join(".claude/venv/lib64")).unwrap();

        let homes_root = tempfile::tempdir().unwrap();
        let isolated = ensure_bootstrapped(homes_root.path(), real_home.path(), "claude").unwrap();
        assert_eq!(std::fs::read_to_string(isolated.join(".claude/venv/lib/marker.txt")).unwrap(), "present");
        assert!(!isolated.join(".claude/venv/lib64").exists());
        // Credentials still get stripped even inside a directory copied wholesale.
        assert!(!isolated.join(".claude/.credentials.json").exists());
    }
}
