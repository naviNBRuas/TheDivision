//! Multi-account credential switching: capture an agent's *current* login
//! state into a named profile, then swap between profiles later (e.g. two
//! separate Claude Code accounts). Real storage locations, verified by
//! direct inspection on the reference machine (not assumed):
//!
//! - **claude**: `~/.claude/.credentials.json` (OAuth tokens) plus the
//!   `oauthAccount`/`userID` fields inside `~/.claude.json` (which also
//!   holds unrelated config — MCP servers, settings — so only those two
//!   fields are captured/restored, the rest of the file is left alone,
//!   same merge approach as `divisi-agent-sdk::formats::claude`).
//! - **codex**: `~/.codex/auth.json` is a pure auth file (`auth_mode`,
//!   `OPENAI_API_KEY`, `tokens`, `last_refresh`) — safe to snapshot/
//!   restore wholesale.
//! - **cursor**: `~/.cursor/cli-config.json` mixes editor preferences with
//!   login state the same way claude's `.claude.json` does — only its
//!   `authInfo` field (`{email, displayName, userId, authId}`, confirmed
//!   by watching a real `cursor-agent login` populate it) is
//!   captured/restored; the rest of the file (model choice, permissions,
//!   UI prefs) is left alone.
//! - **grok**: `~/.grok/auth.json` is a pure auth file, same shape as
//!   codex's — safe to snapshot/restore wholesale. Its single top-level
//!   entry (keyed by a dynamic `"<oidc_issuer>::<client_id>"` string,
//!   confirmed live) holds the token alongside identity fields
//!   (`email`, `first_name`, ...); `derive_label` reads whichever entry
//!   is present rather than assuming the key's exact text.
//! - **codebuff**: `~/.config/manicode/credentials.json` ("manicode" is
//!   codebuff's old internal project name, confirmed live) is a pure auth
//!   file, keyed by profile name (`"default"`) — safe to snapshot/restore
//!   wholesale, same shape as codex/grok.
//! - **agy**: best-effort. Its real state directory (`~/.gemini/
//!   antigravity-cli/`) has no file literally named "auth"; the two files
//!   captured (`jetski_state.pbtxt`, `settings.json`) are the most likely
//!   candidates (both mode 600, i.e. treated as sensitive by the CLI
//!   itself) but this has **not** been confirmed to be the complete login
//!   state. Flagged as `unverified_complete: true` everywhere it's
//!   surfaced, rather than silently claiming full support.
//! - **opencode**: intentionally **unsupported**. Its login state lives as
//!   rows across several tables (`credential`, `account`,
//!   `account_state`, `control_account`) inside a single live SQLite
//!   database (`~/.local/share/opencode/opencode.db`) that also holds
//!   session/message/project history. Swapping the whole file would nuke
//!   unrelated app state; surgical row-level swap risks violating
//!   foreign-key relationships in a schema this project doesn't own.
//!   Rather than risk corrupting a real database, this is left
//!   unsupported until a safer mechanism is confirmed.
//! - **perplexity** (`pplx`): unsupported — not logged in on the
//!   reference machine and not a coding agent in the first place (see
//!   `docs/install-methods.md`).
//!
//! Every write here goes through the same backup-before-overwrite
//! discipline as `divisi-agent-sdk`'s config writers, and every snapshot
//! file is created with `0600` permissions since it can contain live
//! OAuth tokens. Nothing in this module ever prints or logs token
//! contents — only profile names and timestamps.
//!
//! **No real-home fallback.** `is_authenticated` and `capture` read only
//! SingleCLI's isolated `home` (`~/.config/divisi/homes/<agent>/`) — the
//! real, ambient `$HOME` is never consulted for auth detection or
//! snapshotting. A login done directly against the vendor CLI, outside
//! `single agent login`, is invisible to SingleCLI until you log in again
//! inside the isolated home. `agent_home::ensure_bootstrapped` still
//! seeds a brand-new isolated home from the real one once, but strips out
//! credential files while doing so — see that module's doc comment.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use divisi_protocol::{AccountProfileInfo, AccountStatus, AccountSwitchResult, AuthState};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProfileMeta {
    captured_at: String,
    unverified_complete: bool,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    status: AccountStatus,
}

/// Which agents support account switching at all, and how "complete" the
/// captured state is known to be.
fn support(agent: &str) -> Result<bool> {
    Ok(match agent {
        "claude" | "codex" | "cursor" | "grok" | "codebuff" => false, // fully verified
        "agy" | "copilot" => true,              // best-effort, flagged (copilot: keyring round-trip unverified — secret-tool reads are classifier-blocked from this tool session)
        "opencode" => bail!("opencode account switching is unsupported: its login state lives in a live multi-table SQLite database (~/.local/share/opencode/opencode.db) shared with session history — see account.rs module docs for why a safe snapshot mechanism doesn't exist yet"),
        "perplexity" => bail!("pplx has no coding-agent session to switch between; not supported"),
        // Deliberately *not* "unknown agent" — every other registry
        // entry (goose, kiro, cody, the v0.1.19
        // additions, ...) is perfectly real and may well have a working
        // `login()`; this module's named multi-account capture/switch
        // just hasn't had its credential-file location verified for it
        // yet (see this file's module docs for the ones that have).
        other => bail!("named multi-account capture/switching isn't implemented for {other} yet (only claude, codex, cursor, copilot, grok, codebuff, and agy are supported so far) — the login itself still worked fine, this only affects `single account capture/use`"),
    })
}

fn profile_dir(accounts_root: &Path, agent: &str, name: &str) -> PathBuf {
    accounts_root.join(agent).join(name)
}

/// Ask an agent CLI whether it is currently authenticated, by running its
/// own status subcommand with `HOME` set to `home`. For agents (codex,
/// cursor) whose token lives in the OS keyring rather than a file, this is
/// the only reliable check. Bounded by a short timeout — a status
/// subcommand that hangs reads as "not logged in" rather than blocking
/// `has_live_login`'s infallible `bool` return. Exit 0 with output that
/// does not say "not logged in" / "no credentials" counts as logged in.
fn cli_reports_logged_in(home: &Path, command: &str, args: &[&str]) -> bool {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let Ok(mut child) = Command::new(command)
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return false;
    };

    let deadline = Instant::now() + Duration::from_secs(6);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return false,
        }
    };

    let mut out = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut out);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut out);
    }
    let lower = out.to_ascii_lowercase();
    status.success()
        && !lower.contains("not logged in")
        && !lower.contains("no credentials")
        && !lower.contains("please run")
        && !lower.contains("please log in")
}

/// Reads a secret from the desktop OS keyring (`secret-tool`), for agents
/// (e.g. copilot) whose real login token lives there instead of a file
/// under `$HOME` — the secret-service D-Bus daemon is system-wide, not
/// namespaced per isolated home, so shelling out here is the only way to
/// read it. Best-effort by design: no secret-service running (headless
/// session), a missing entry, or any other failure all read as "no value"
/// rather than a hard error, matching `has_live_login`'s infallible
/// `bool` return for every other agent.
fn keyring_lookup(service: &str, username: &str) -> Option<String> {
    let output = std::process::Command::new("secret-tool").args(["lookup", "service", service, "username", username]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Writes a secret into the desktop OS keyring — the write-side
/// counterpart to `keyring_lookup`, used by `capture`/`switch` to restore
/// a captured copilot token. Unlike the file-based agents, this can't
/// "back up what was live" first: keyring entries here are keyed by
/// `service` + `username` (which encodes the GitHub login), so restoring
/// a *different* account's token writes to a different entry entirely
/// rather than clobbering the current one.
fn keyring_store(service: &str, username: &str, label: &str, value: &str) -> Result<()> {
    use std::io::Write;
    let mut child = std::process::Command::new("secret-tool")
        .args(["store", "--label", label, "service", service, "username", username])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("spawning secret-tool store")?;
    child.stdin.take().context("secret-tool store subprocess has no stdin")?.write_all(value.as_bytes())?;
    let status = child.wait().context("waiting for secret-tool store")?;
    if !status.success() {
        bail!("secret-tool store failed for service={service}");
    }
    Ok(())
}

/// copilot's `~/.copilot/config.json` `lastLoggedInUser` field
/// (`{host, login}`), confirmed live by watching `copilot login` populate
/// it. `None` when never logged in or the file/field is missing.
fn copilot_identity(home: &Path) -> Option<(String, String)> {
    let fields = extract_json_fields(&home.join(".copilot/config.json"), &["lastLoggedInUser"]).ok()?;
    let user = fields.get("lastLoggedInUser")?;
    let host = user.get("host")?.as_str()?.to_string();
    let login = user.get("login")?.as_str()?.to_string();
    Some((host, login))
}

/// The `username` attribute copilot's own keyring entry is stored under
/// (confirmed live: `service=copilot-cli, username=https://github.com:<login>`).
fn copilot_keyring_username(host: &str, login: &str) -> String {
    format!("{host}:{login}")
}

/// Shared restore logic for copilot's split file+keyring credential,
/// used by both `write_credentials_into` (switch) and `ensure_isolated_home`
/// (concurrent isolated accounts) so the two pieces — which login identity
/// `config.json` points at, and which token the keyring holds for it —
/// never drift apart.
fn restore_copilot_credentials(config_path: &Path, profile: &Path) -> Result<()> {
    let identity: Value = serde_json::from_slice(&fs::read(profile.join("identity.json"))?)?;
    merge_json_fields(config_path, &identity)?;

    let user = identity.get("lastLoggedInUser").context("captured copilot profile has no lastLoggedInUser")?;
    let host = user.get("host").and_then(|v| v.as_str()).context("captured copilot identity missing host")?;
    let login = user.get("login").and_then(|v| v.as_str()).context("captured copilot identity missing login")?;
    let secret = fs::read_to_string(profile.join("keyring_secret")).context("reading captured copilot keyring secret")?;
    keyring_store("copilot-cli", &copilot_keyring_username(host, login), &format!("GitHub Copilot CLI token for {login}"), secret.trim_end())
}

/// Whether `agent`'s credential file(s) are present under `home` — the
/// same presence check `capture` already used, factored out so it can also
/// answer "is *anything* logged in" (see `is_authenticated`) without the
/// side effects of an actual capture.
pub fn has_live_login(home: &Path, agent: &str) -> bool {
    match agent {
        "claude" => home.join(".claude/.credentials.json").exists(),
        // codex >= 0.147 keeps its OAuth token in the OS keyring, not
        // `.codex/auth.json` (that file is API-key only). Ask codex itself,
        // in the given HOME, whether it is logged in. `.codex/auth.json`
        // still counts (plain API-key auth).
        "codex" => {
            home.join(".codex/auth.json").exists()
                || cli_reports_logged_in(home, "codex", &["login", "status"])
        }
        "grok" => home.join(".grok/auth.json").exists(),
        "codebuff" => home.join(".config/manicode/credentials.json").exists(),
        // cursor-agent moved its token to the OS keyring too; the old
        // `.cursor/cli-config.json` `authInfo` field is gone. Ask the CLI.
        "cursor" => {
            extract_json_fields(&home.join(".cursor/cli-config.json"), &["authInfo"])
                .ok()
                .and_then(|f| f.get("authInfo").cloned())
                .is_some_and(|v| !v.is_null())
                || cli_reports_logged_in(home, "cursor-agent", &["status"])
        }
        "copilot" => copilot_identity(home)
            .map(|(host, login)| keyring_lookup("copilot-cli", &copilot_keyring_username(&host, &login)).is_some())
            .unwrap_or(false),
        "agy" => {
            let dir = home.join(".gemini/antigravity-cli");
            ["jetski_state.pbtxt", "settings.json"].iter().any(|f| dir.join(f).exists())
        }
        _ => false,
    }
}

/// Auto-detected "is anything logged in right now" for `agent`, checked
/// **only** against SingleCLI's isolated `home` — the real, ambient
/// `$HOME` is never consulted here. A login that only exists in the real
/// home (e.g. the user ran the vendor CLI directly instead of `single
/// agent login`) is invisible to SingleCLI by design; see `capture`'s doc
/// comment for the same rule applied to snapshotting. See `AuthState`
/// docs for how this differs from the manually-set `AccountStatus` on a
/// captured profile.
pub fn is_authenticated(home: &Path, agent: &str) -> AuthState {
    if support(agent).is_err() {
        return AuthState::Unsupported;
    }
    if has_live_login(home, agent) {
        AuthState::Authenticated
    } else {
        AuthState::NotAuthenticated
    }
}

/// Best-effort human-readable identity for the currently-live login under
/// `home`, used to auto-label an account right after `single agent login`.
/// `None` when the agent's credential format has no identity field
/// (e.g. codex's `auth.json`) or nothing is logged in yet — callers should
/// fall back to a generated name in that case.
pub fn derive_label(home: &Path, agent: &str) -> Option<String> {
    match agent {
        "claude" => {
            let fields = extract_json_fields(&home.join(".claude.json"), &["oauthAccount", "userID"]).ok()?;
            fields
                .get("oauthAccount")
                .and_then(|o| o.get("email"))
                .and_then(|v| v.as_str())
                .or_else(|| fields.get("userID").and_then(|v| v.as_str()))
                .map(str::to_string)
        }
        "cursor" => {
            let fields = extract_json_fields(&home.join(".cursor/cli-config.json"), &["authInfo"]).ok()?;
            fields.get("authInfo").and_then(|o| o.get("email")).and_then(|v| v.as_str()).map(str::to_string)
        }
        "copilot" => copilot_identity(home).map(|(_, login)| login),
        "grok" => {
            let text = fs::read_to_string(home.join(".grok/auth.json")).ok()?;
            let full: Value = serde_json::from_str(&text).ok()?;
            full.as_object()?.values().next()?.get("email")?.as_str().map(str::to_string)
        }
        "codebuff" => {
            let text = fs::read_to_string(home.join(".config/manicode/credentials.json")).ok()?;
            let full: Value = serde_json::from_str(&text).ok()?;
            full.get("default")?.get("email")?.as_str().map(str::to_string)
        }
        _ => None,
    }
}

fn write_secure(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn backup(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup_path = path.with_extension(format!(
        "{}.bak-{timestamp}",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    fs::copy(path, &backup_path)?;
    Ok(Some(backup_path))
}

/// Captures the agent's currently-live login state into a new named
/// profile. Only ever reads from SingleCLI's isolated `home` — the real,
/// ambient `$HOME` is never consulted, so a login done outside `single
/// agent login` (directly against the vendor CLI) is invisible here.
/// Fails if the isolated home has no live login.
pub fn capture(accounts_root: &Path, home: &Path, agent: &str, name: &str, label: Option<String>) -> Result<AccountProfileInfo> {
    let unverified_complete = support(agent)?;
    let dir = profile_dir(accounts_root, agent, name);
    fs::create_dir_all(&dir)?;

    if !has_live_login(home, agent) {
        bail!(
            "{agent} is not currently logged in inside SingleCLI's isolated home ({}) — run `single agent login {agent}` first",
            home.display()
        );
    }
    let source = home;

    match agent {
        "claude" => {
            let creds = fs::read(source.join(".claude/.credentials.json"))?;
            write_secure(&dir.join("credentials.json"), &creds)?;

            let config_path = source.join(".claude.json");
            let oauth_fields = extract_json_fields(&config_path, &["oauthAccount", "userID"])?;
            write_secure(&dir.join("oauth_account.json"), serde_json::to_string_pretty(&oauth_fields)?.as_bytes())?;
        }
        "codex" => {
            let auth = fs::read(source.join(".codex/auth.json"))?;
            write_secure(&dir.join("auth.json"), &auth)?;
        }
        "cursor" => {
            let config_path = source.join(".cursor/cli-config.json");
            let auth_fields = extract_json_fields(&config_path, &["authInfo"])?;
            write_secure(&dir.join("auth_info.json"), serde_json::to_string_pretty(&auth_fields)?.as_bytes())?;
        }
        "copilot" => {
            let (host, login) = copilot_identity(source).context("copilot: no lastLoggedInUser in config.json — log in first")?;
            let identity = extract_json_fields(&source.join(".copilot/config.json"), &["lastLoggedInUser", "loggedInUsers"])?;
            write_secure(&dir.join("identity.json"), serde_json::to_string_pretty(&identity)?.as_bytes())?;

            let username = copilot_keyring_username(&host, &login);
            let secret = keyring_lookup("copilot-cli", &username).with_context(|| {
                format!("copilot: no OS-keyring entry found for {username} (service=copilot-cli) — is the desktop secret service running?")
            })?;
            write_secure(&dir.join("keyring_secret"), secret.as_bytes())?;
        }
        "grok" => {
            let auth = fs::read(source.join(".grok/auth.json"))?;
            write_secure(&dir.join("auth.json"), &auth)?;
        }
        "codebuff" => {
            let creds = fs::read(source.join(".config/manicode/credentials.json"))?;
            write_secure(&dir.join("credentials.json"), &creds)?;
        }
        "agy" => {
            let state_dir = source.join(".gemini/antigravity-cli");
            for filename in ["jetski_state.pbtxt", "settings.json"] {
                let src = state_dir.join(filename);
                if src.exists() {
                    write_secure(&dir.join(filename), &fs::read(&src)?)?;
                }
            }
        }
        _ => unreachable!("support() already rejected unsupported agents"),
    }

    let meta = ProfileMeta { captured_at: chrono::Utc::now().to_rfc3339(), unverified_complete, label, status: AccountStatus::Unknown };
    fs::write(dir.join("meta.json"), serde_json::to_string_pretty(&meta)?)?;

    Ok(AccountProfileInfo {
        agent: agent.to_string(),
        name: name.to_string(),
        label: meta.label,
        captured_at: meta.captured_at,
        unverified_complete,
        status: meta.status,
    })
}

/// Sets the manually-tracked usability status of a captured account (see
/// `AccountStatus` docs — SingleCLI never auto-detects this).
pub fn set_status(accounts_root: &Path, agent: &str, name: &str, status: AccountStatus) -> Result<()> {
    let dir = profile_dir(accounts_root, agent, name);
    let meta_path = dir.join("meta.json");
    if !meta_path.exists() {
        bail!("no profile named '{name}' for agent '{agent}'");
    }
    let mut meta: ProfileMeta = serde_json::from_str(&fs::read_to_string(&meta_path)?)?;
    meta.status = status;
    fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)?;
    Ok(())
}

/// Swaps a captured profile into place as the agent's live login state,
/// backing up whatever was live first.
pub fn switch(accounts_root: &Path, home: &Path, agent: &str, name: &str) -> Result<AccountSwitchResult> {
    support(agent)?;
    let dir = profile_dir(accounts_root, agent, name);
    if !dir.exists() {
        bail!("no profile named '{name}' for agent '{agent}'");
    }
    let backed_up = write_credentials_into(home, &dir, agent)?;
    Ok(AccountSwitchResult { agent: agent.to_string(), name: name.to_string(), backed_up })
}

/// Writes a captured profile's credential files into `target_home` as its
/// live login state, backing up whatever was already there first. Used by
/// `switch` (explicit account swap) so the per-agent file layout only
/// lives in one place.
fn write_credentials_into(target_home: &Path, profile: &Path, agent: &str) -> Result<Vec<String>> {
    let mut backed_up = Vec::new();

    match agent {
        "claude" => {
            let creds_path = target_home.join(".claude/.credentials.json");
            if let Some(b) = backup(&creds_path)? {
                backed_up.push(b.display().to_string());
            }
            write_secure(&creds_path, &fs::read(profile.join("credentials.json"))?)?;

            let config_path = target_home.join(".claude.json");
            if let Some(b) = backup(&config_path)? {
                backed_up.push(b.display().to_string());
            }
            let oauth_fields: Value = serde_json::from_slice(&fs::read(profile.join("oauth_account.json"))?)?;
            merge_json_fields(&config_path, &oauth_fields)?;
        }
        "codex" => {
            let auth_path = target_home.join(".codex/auth.json");
            if let Some(b) = backup(&auth_path)? {
                backed_up.push(b.display().to_string());
            }
            write_secure(&auth_path, &fs::read(profile.join("auth.json"))?)?;
        }
        "cursor" => {
            let config_path = target_home.join(".cursor/cli-config.json");
            if let Some(b) = backup(&config_path)? {
                backed_up.push(b.display().to_string());
            }
            let auth_fields: Value = serde_json::from_slice(&fs::read(profile.join("auth_info.json"))?)?;
            merge_json_fields(&config_path, &auth_fields)?;
        }
        "copilot" => {
            let config_path = target_home.join(".copilot/config.json");
            if let Some(b) = backup(&config_path)? {
                backed_up.push(b.display().to_string());
            }
            restore_copilot_credentials(&config_path, profile)?;
        }
        "grok" => {
            let auth_path = target_home.join(".grok/auth.json");
            if let Some(b) = backup(&auth_path)? {
                backed_up.push(b.display().to_string());
            }
            write_secure(&auth_path, &fs::read(profile.join("auth.json"))?)?;
        }
        "codebuff" => {
            let creds_path = target_home.join(".config/manicode/credentials.json");
            if let Some(b) = backup(&creds_path)? {
                backed_up.push(b.display().to_string());
            }
            write_secure(&creds_path, &fs::read(profile.join("credentials.json"))?)?;
        }
        "agy" => {
            let state_dir = target_home.join(".gemini/antigravity-cli");
            for filename in ["jetski_state.pbtxt", "settings.json"] {
                let snapshot = profile.join(filename);
                if !snapshot.exists() {
                    continue;
                }
                let live = state_dir.join(filename);
                if let Some(b) = backup(&live)? {
                    backed_up.push(b.display().to_string());
                }
                write_secure(&live, &fs::read(&snapshot)?)?;
            }
        }
        _ => unreachable!("support() already rejected unsupported agents"),
    }

    Ok(backed_up)
}

/// Materializes an isolated `$HOME` for `agent`/`name` at
/// `<accounts_root>/<agent>/<name>/home`, so this captured account can run
/// **concurrently** with other accounts of the same agent (spec: "2 claude
/// code, 3 codex... as i need") without any of them overwriting each
/// other's live credentials the way `switch` does. On every call, copies
/// `base_home`'s non-credential config the first time (so MCP servers /
/// settings carry over from the real install) and always re-overlays this
/// profile's captured credentials on top, so re-running after `capture`
/// picks up refreshed tokens. Returns the isolated home directory —
/// callers pass it as `$HOME` to the agent's subprocess (see
/// `divisi-agent-sdk::run::run_command_with_home`).
pub fn ensure_isolated_home(accounts_root: &Path, base_home: &Path, agent: &str, name: &str) -> Result<PathBuf> {
    support(agent)?;
    let profile = profile_dir(accounts_root, agent, name);
    if !profile.exists() {
        bail!("no profile named '{name}' for agent '{agent}'");
    }
    let isolated = profile.join("home");
    fs::create_dir_all(&isolated)?;

    match agent {
        "claude" => {
            let config_path = isolated.join(".claude.json");
            if !config_path.exists() {
                let base_config = base_home.join(".claude.json");
                if base_config.exists() {
                    fs::copy(&base_config, &config_path)?;
                }
            }
            fs::create_dir_all(isolated.join(".claude"))?;
            write_secure(&isolated.join(".claude/.credentials.json"), &fs::read(profile.join("credentials.json"))?)?;
            let oauth_fields: Value = serde_json::from_slice(&fs::read(profile.join("oauth_account.json"))?)?;
            merge_json_fields(&config_path, &oauth_fields)?;
        }
        "codex" => {
            let isolated_codex_dir = isolated.join(".codex");
            fs::create_dir_all(&isolated_codex_dir)?;
            let config_path = isolated_codex_dir.join("config.toml");
            if !config_path.exists() {
                let base_config = base_home.join(".codex").join("config.toml");
                if base_config.exists() {
                    fs::copy(&base_config, &config_path)?;
                }
            }
            write_secure(&isolated_codex_dir.join("auth.json"), &fs::read(profile.join("auth.json"))?)?;
        }
        "cursor" => {
            let isolated_cursor_dir = isolated.join(".cursor");
            fs::create_dir_all(&isolated_cursor_dir)?;
            let config_path = isolated_cursor_dir.join("cli-config.json");
            if !config_path.exists() {
                let base_config = base_home.join(".cursor").join("cli-config.json");
                if base_config.exists() {
                    fs::copy(&base_config, &config_path)?;
                }
            }
            let auth_fields: Value = serde_json::from_slice(&fs::read(profile.join("auth_info.json"))?)?;
            merge_json_fields(&config_path, &auth_fields)?;
        }
        "copilot" => {
            let isolated_copilot_dir = isolated.join(".copilot");
            fs::create_dir_all(&isolated_copilot_dir)?;
            let config_path = isolated_copilot_dir.join("config.json");
            if !config_path.exists() {
                let base_config = base_home.join(".copilot").join("config.json");
                if base_config.exists() {
                    fs::copy(&base_config, &config_path)?;
                }
            }
            restore_copilot_credentials(&config_path, &profile)?;
        }
        "grok" => {
            let isolated_grok_dir = isolated.join(".grok");
            fs::create_dir_all(&isolated_grok_dir)?;
            let config_path = isolated_grok_dir.join("config.toml");
            if !config_path.exists() {
                let base_config = base_home.join(".grok").join("config.toml");
                if base_config.exists() {
                    fs::copy(&base_config, &config_path)?;
                }
            }
            write_secure(&isolated_grok_dir.join("auth.json"), &fs::read(profile.join("auth.json"))?)?;
        }
        "codebuff" => {
            let isolated_manicode_dir = isolated.join(".config/manicode");
            fs::create_dir_all(&isolated_manicode_dir)?;
            let settings_path = isolated_manicode_dir.join("settings.json");
            if !settings_path.exists() {
                let base_settings = base_home.join(".config/manicode/settings.json");
                if base_settings.exists() {
                    fs::copy(&base_settings, &settings_path)?;
                }
            }
            write_secure(&isolated_manicode_dir.join("credentials.json"), &fs::read(profile.join("credentials.json"))?)?;
        }
        "agy" => {
            let base_state_dir = base_home.join(".gemini/antigravity-cli");
            let isolated_state_dir = isolated.join(".gemini/antigravity-cli");
            fs::create_dir_all(&isolated_state_dir)?;
            for filename in ["jetski_state.pbtxt", "settings.json"] {
                let snapshot = profile.join(filename);
                if snapshot.exists() {
                    write_secure(&isolated_state_dir.join(filename), &fs::read(&snapshot)?)?;
                } else {
                    let base_file = base_state_dir.join(filename);
                    let dest = isolated_state_dir.join(filename);
                    if base_file.exists() && !dest.exists() {
                        fs::copy(&base_file, &dest)?;
                    }
                }
            }
        }
        _ => unreachable!("support() already rejected unsupported agents"),
    }

    Ok(isolated)
}

pub fn list(accounts_root: &Path, agent_filter: Option<&str>) -> Result<Vec<AccountProfileInfo>> {
    let mut results = Vec::new();
    if !accounts_root.exists() {
        return Ok(results);
    }
    for agent_entry in fs::read_dir(accounts_root)? {
        let agent_entry = agent_entry?;
        if !agent_entry.file_type()?.is_dir() {
            continue;
        }
        let agent = agent_entry.file_name().to_string_lossy().to_string();
        if let Some(filter) = agent_filter {
            if agent != filter {
                continue;
            }
        }
        for profile_entry in fs::read_dir(agent_entry.path())? {
            let profile_entry = profile_entry?;
            if !profile_entry.file_type()?.is_dir() {
                continue;
            }
            let name = profile_entry.file_name().to_string_lossy().to_string();
            let meta_path = profile_entry.path().join("meta.json");
            if let Ok(text) = fs::read_to_string(&meta_path) {
                if let Ok(meta) = serde_json::from_str::<ProfileMeta>(&text) {
                    results.push(AccountProfileInfo {
                        agent: agent.clone(),
                        name,
                        label: meta.label,
                        captured_at: meta.captured_at,
                        unverified_complete: meta.unverified_complete,
                        status: meta.status,
                    });
                }
            }
        }
    }
    results.sort_by(|a, b| (a.agent.as_str(), a.name.as_str()).cmp(&(b.agent.as_str(), b.name.as_str())));
    Ok(results)
}

pub fn remove(accounts_root: &Path, agent: &str, name: &str) -> Result<bool> {
    let dir = profile_dir(accounts_root, agent, name);
    if !dir.exists() {
        return Ok(false);
    }
    fs::remove_dir_all(&dir)?;
    Ok(true)
}

fn extract_json_fields(path: &Path, fields: &[&str]) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Default::default()));
    }
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    // copilot's ~/.copilot/config.json starts with `//`-prefixed comment
    // lines (confirmed live) — strict JSON parsing chokes on those, so
    // strip any line that's a comment once trimmed. No-op for every other
    // agent's plain-JSON config.
    let text: String = text.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    let full: Value = serde_json::from_str(&text)?;
    let mut extracted = serde_json::Map::new();
    if let Some(obj) = full.as_object() {
        for field in fields {
            if let Some(v) = obj.get(*field) {
                extracted.insert(field.to_string(), v.clone());
            }
        }
    }
    Ok(Value::Object(extracted))
}

fn merge_json_fields(path: &Path, fields: &Value) -> Result<()> {
    let mut root: Value = if path.exists() {
        serde_json::from_str(&fs::read_to_string(path)?)?
    } else {
        Value::Object(Default::default())
    };
    let root_obj = root.as_object_mut().context("target config root is not a JSON object")?;
    if let Some(fields_obj) = fields.as_object() {
        for (k, v) in fields_obj {
            root_obj.insert(k.clone(), v.clone());
        }
    }
    fs::write(path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_fake_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::write(home.path().join(".claude/.credentials.json"), r#"{"claudeAiOauth":{"token":"secretA"}}"#).unwrap();
        fs::write(home.path().join(".claude.json"), r#"{"numStartups": 5, "oauthAccount": {"email":"a@example.com"}, "userID": "user-a"}"#).unwrap();

        fs::create_dir_all(home.path().join(".codex")).unwrap();
        fs::write(home.path().join(".codex/auth.json"), r#"{"auth_mode":"chatgpt","tokens":{"access":"secretB"}}"#).unwrap();

        fs::create_dir_all(home.path().join(".cursor")).unwrap();
        fs::write(
            home.path().join(".cursor/cli-config.json"),
            r#"{"editor":{"vimMode":false},"authInfo":{"email":"a@example.com","displayName":"A","userId":1,"authId":"secretC"}}"#,
        )
        .unwrap();

        fs::create_dir_all(home.path().join(".grok")).unwrap();
        fs::write(
            home.path().join(".grok/auth.json"),
            r#"{"https://auth.x.ai::client-id":{"key":"secretE","email":"a@example.com","refresh_token":"secretF"}}"#,
        )
        .unwrap();

        fs::create_dir_all(home.path().join(".config/manicode")).unwrap();
        fs::write(
            home.path().join(".config/manicode/credentials.json"),
            r#"{"default":{"id":"u1","name":"A","email":"a@example.com","authToken":"secretI"}}"#,
        )
        .unwrap();
        home
    }

    #[test]
    fn capture_then_switch_round_trips_claude_credentials_and_preserves_other_config() {
        let home = setup_fake_home();
        let accounts_root = tempfile::tempdir().unwrap();

        capture(accounts_root.path(), home.path(), "claude", "work", None).unwrap();

        // Simulate logging into a second account: different creds, different oauthAccount,
        // but numStartups (unrelated config) also changes to prove it's untouched by switch.
        fs::write(home.path().join(".claude/.credentials.json"), r#"{"claudeAiOauth":{"token":"secretC"}}"#).unwrap();
        fs::write(home.path().join(".claude.json"), r#"{"numStartups": 99, "oauthAccount": {"email":"b@example.com"}, "userID": "user-b"}"#).unwrap();
        capture(accounts_root.path(), home.path(), "claude", "personal", None).unwrap();

        let result = switch(accounts_root.path(), home.path(), "claude", "work").unwrap();
        assert_eq!(result.backed_up.len(), 2);

        let creds: Value = serde_json::from_str(&fs::read_to_string(home.path().join(".claude/.credentials.json")).unwrap()).unwrap();
        assert_eq!(creds["claudeAiOauth"]["token"], "secretA");

        let config: Value = serde_json::from_str(&fs::read_to_string(home.path().join(".claude.json")).unwrap()).unwrap();
        assert_eq!(config["oauthAccount"]["email"], "a@example.com");
        assert_eq!(config["userID"], "user-a");
        // numStartups was 99 (from the "personal" session) and must survive the switch
        // untouched, proving this only merges the two account-identity fields.
        assert_eq!(config["numStartups"], 99);
    }

    #[test]
    fn cursor_capture_then_switch_round_trips_auth_info_and_preserves_other_config() {
        let home = setup_fake_home();
        let accounts_root = tempfile::tempdir().unwrap();

        capture(accounts_root.path(), home.path(), "cursor", "work", None).unwrap();

        // Simulate a second account: different authInfo, different editor
        // pref too, to prove the pref survives switch untouched.
        fs::write(
            home.path().join(".cursor/cli-config.json"),
            r#"{"editor":{"vimMode":true},"authInfo":{"email":"b@example.com","displayName":"B","userId":2,"authId":"secretD"}}"#,
        )
        .unwrap();
        capture(accounts_root.path(), home.path(), "cursor", "personal", None).unwrap();

        let result = switch(accounts_root.path(), home.path(), "cursor", "work").unwrap();
        assert_eq!(result.backed_up.len(), 1);

        let config: Value = serde_json::from_str(&fs::read_to_string(home.path().join(".cursor/cli-config.json")).unwrap()).unwrap();
        assert_eq!(config["authInfo"]["email"], "a@example.com");
        assert_eq!(config["authInfo"]["authId"], "secretC");
        // vimMode was flipped to true during the "personal" session and must
        // survive the switch untouched, proving this only merges authInfo.
        assert_eq!(config["editor"]["vimMode"], true);
    }

    #[test]
    fn codex_full_file_swap_round_trips() {
        let home = setup_fake_home();
        let accounts_root = tempfile::tempdir().unwrap();
        capture(accounts_root.path(), home.path(), "codex", "work", None).unwrap();

        fs::write(home.path().join(".codex/auth.json"), r#"{"auth_mode":"apikey","tokens":{"access":"secretD"}}"#).unwrap();
        switch(accounts_root.path(), home.path(), "codex", "work").unwrap();

        let auth: Value = serde_json::from_str(&fs::read_to_string(home.path().join(".codex/auth.json")).unwrap()).unwrap();
        assert_eq!(auth["tokens"]["access"], "secretB");
    }

    #[test]
    fn capture_fails_when_not_logged_in() {
        let home = tempfile::tempdir().unwrap();
        let accounts_root = tempfile::tempdir().unwrap();
        assert!(capture(accounts_root.path(), home.path(), "codex", "x", None).is_err());
    }

    #[test]
    fn opencode_and_perplexity_are_explicitly_unsupported() {
        let home = setup_fake_home();
        let accounts_root = tempfile::tempdir().unwrap();
        assert!(capture(accounts_root.path(), home.path(), "opencode", "x", None).is_err());
        assert!(capture(accounts_root.path(), home.path(), "perplexity", "x", None).is_err());
    }

    #[test]
    fn snapshot_files_are_written_with_owner_only_permissions() {
        let home = setup_fake_home();
        let accounts_root = tempfile::tempdir().unwrap();
        capture(accounts_root.path(), home.path(), "codex", "work", None).unwrap();
        let perms = fs::metadata(accounts_root.path().join("codex/work/auth.json")).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
    }

    #[test]
    fn isolated_homes_let_two_accounts_of_the_same_agent_coexist() {
        let home = setup_fake_home();
        let accounts_root = tempfile::tempdir().unwrap();
        capture(accounts_root.path(), home.path(), "codex", "work", Some("work@example.com".into())).unwrap();

        fs::write(home.path().join(".codex/auth.json"), r#"{"auth_mode":"apikey","tokens":{"access":"secretPersonal"}}"#).unwrap();
        capture(accounts_root.path(), home.path(), "codex", "personal", Some("me@example.com".into())).unwrap();

        let work_home = ensure_isolated_home(accounts_root.path(), home.path(), "codex", "work").unwrap();
        let personal_home = ensure_isolated_home(accounts_root.path(), home.path(), "codex", "personal").unwrap();
        assert_ne!(work_home, personal_home);

        let work_auth: Value = serde_json::from_str(&fs::read_to_string(work_home.join(".codex/auth.json")).unwrap()).unwrap();
        assert_eq!(work_auth["tokens"]["access"], "secretB");
        let personal_auth: Value = serde_json::from_str(&fs::read_to_string(personal_home.join(".codex/auth.json")).unwrap()).unwrap();
        assert_eq!(personal_auth["tokens"]["access"], "secretPersonal");

        // The real, live $HOME's auth.json must be untouched by either call.
        let live_auth: Value = serde_json::from_str(&fs::read_to_string(home.path().join(".codex/auth.json")).unwrap()).unwrap();
        assert_eq!(live_auth["tokens"]["access"], "secretPersonal");
    }

    #[test]
    fn list_and_remove_work() {
        let home = setup_fake_home();
        let accounts_root = tempfile::tempdir().unwrap();
        capture(accounts_root.path(), home.path(), "claude", "work", None).unwrap();
        capture(accounts_root.path(), home.path(), "codex", "work", None).unwrap();

        let all = list(accounts_root.path(), None).unwrap();
        assert_eq!(all.len(), 2);
        let claude_only = list(accounts_root.path(), Some("claude")).unwrap();
        assert_eq!(claude_only.len(), 1);

        assert!(remove(accounts_root.path(), "claude", "work").unwrap());
        assert!(!remove(accounts_root.path(), "claude", "work").unwrap());
        assert_eq!(list(accounts_root.path(), None).unwrap().len(), 1);
    }

    #[test]
    fn capture_ignores_real_home_and_fails_when_isolated_home_has_no_login() {
        // Isolated home exists but has never had claude credentials written
        // into it (the scenario when a user ran `claude` directly instead
        // of `single agent login claude`). Real home has a live login, but
        // that must not matter — SingleCLI never reads the real home here.
        let isolated_home = tempfile::tempdir().unwrap();
        let real_home = setup_fake_home();
        let accounts_root = tempfile::tempdir().unwrap();

        assert!(!has_live_login(isolated_home.path(), "claude"));
        assert!(has_live_login(real_home.path(), "claude"));

        let err = capture(accounts_root.path(), isolated_home.path(), "claude", "work", None).unwrap_err();
        assert!(err.to_string().contains("single agent login claude"));

        // The isolated home must remain untouched — no sync from real_home.
        assert!(!has_live_login(isolated_home.path(), "claude"));
    }

    #[test]
    fn is_authenticated_only_checks_the_isolated_home_and_flags_unsupported_agents() {
        let isolated_home = tempfile::tempdir().unwrap();
        let real_home = setup_fake_home();

        // Live login only in the real home: still NotAuthenticated, since
        // is_authenticated never looks at real_home.
        assert_eq!(is_authenticated(isolated_home.path(), "codex"), AuthState::NotAuthenticated);
        // Live login in the isolated home itself: Authenticated.
        assert_eq!(is_authenticated(real_home.path(), "codex"), AuthState::Authenticated);
        assert_eq!(is_authenticated(isolated_home.path(), "opencode"), AuthState::Unsupported);
    }

    #[test]
    fn derive_label_reads_claude_and_cursor_email_but_not_codex() {
        let home = setup_fake_home();
        assert_eq!(derive_label(home.path(), "claude").as_deref(), Some("a@example.com"));
        assert_eq!(derive_label(home.path(), "cursor").as_deref(), Some("a@example.com"));
        assert_eq!(derive_label(home.path(), "codex"), None);
    }

    #[test]
    fn grok_full_file_swap_round_trips_and_derives_label_from_dynamic_key() {
        let home = setup_fake_home();
        let accounts_root = tempfile::tempdir().unwrap();
        assert_eq!(derive_label(home.path(), "grok").as_deref(), Some("a@example.com"));

        capture(accounts_root.path(), home.path(), "grok", "work", None).unwrap();

        fs::write(
            home.path().join(".grok/auth.json"),
            r#"{"https://auth.x.ai::client-id":{"key":"secretG","email":"b@example.com","refresh_token":"secretH"}}"#,
        )
        .unwrap();
        switch(accounts_root.path(), home.path(), "grok", "work").unwrap();

        let auth: Value = serde_json::from_str(&fs::read_to_string(home.path().join(".grok/auth.json")).unwrap()).unwrap();
        assert_eq!(auth["https://auth.x.ai::client-id"]["key"], "secretE");
    }

    #[test]
    fn codebuff_full_file_swap_round_trips_and_derives_label() {
        let home = setup_fake_home();
        let accounts_root = tempfile::tempdir().unwrap();
        assert_eq!(derive_label(home.path(), "codebuff").as_deref(), Some("a@example.com"));

        capture(accounts_root.path(), home.path(), "codebuff", "work", None).unwrap();

        fs::write(
            home.path().join(".config/manicode/credentials.json"),
            r#"{"default":{"id":"u2","name":"B","email":"b@example.com","authToken":"secretJ"}}"#,
        )
        .unwrap();
        switch(accounts_root.path(), home.path(), "codebuff", "work").unwrap();

        let creds: Value = serde_json::from_str(&fs::read_to_string(home.path().join(".config/manicode/credentials.json")).unwrap()).unwrap();
        assert_eq!(creds["default"]["email"], "a@example.com");
        assert_eq!(creds["default"]["authToken"], "secretI");
    }

    #[test]
    fn copilot_identity_parses_config_json_despite_leading_comment_lines() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join(".copilot")).unwrap();
        fs::write(
            home.path().join(".copilot/config.json"),
            "// User settings belong in settings.json.\n// This file is managed automatically.\n{\"lastLoggedInUser\":{\"host\":\"https://github.com\",\"login\":\"navinbruas\"},\"loggedInUsers\":[{\"host\":\"https://github.com\",\"login\":\"navinbruas\"}]}\n",
        )
        .unwrap();

        assert_eq!(copilot_identity(home.path()), Some(("https://github.com".to_string(), "navinbruas".to_string())));
        assert_eq!(copilot_keyring_username("https://github.com", "navinbruas"), "https://github.com:navinbruas");
        assert_eq!(derive_label(home.path(), "copilot").as_deref(), Some("navinbruas"));
    }

    #[test]
    fn copilot_identity_is_none_when_never_logged_in() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join(".copilot")).unwrap();
        fs::write(home.path().join(".copilot/config.json"), "// managed automatically\n{}\n").unwrap();
        assert_eq!(copilot_identity(home.path()), None);
        assert!(!has_live_login(home.path(), "copilot"));
    }

    #[test]
    fn cursor_has_live_login_only_when_auth_info_is_present() {
        let home = setup_fake_home();
        assert!(has_live_login(home.path(), "cursor"));
        assert_eq!(is_authenticated(home.path(), "cursor"), AuthState::Authenticated);

        // A cursor config with no authInfo field (e.g. never logged in) must
        // read as not authenticated, not error out.
        let logged_out_home = tempfile::tempdir().unwrap();
        fs::create_dir_all(logged_out_home.path().join(".cursor")).unwrap();
        fs::write(logged_out_home.path().join(".cursor/cli-config.json"), r#"{"editor":{"vimMode":false}}"#).unwrap();
        assert!(!has_live_login(logged_out_home.path(), "cursor"));
        assert_eq!(is_authenticated(logged_out_home.path(), "cursor"), AuthState::NotAuthenticated);
    }
}
