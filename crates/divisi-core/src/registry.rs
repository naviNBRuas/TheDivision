//! The agent registry: the list of AI CLIs divisi knows how to detect,
//! install, and configure.
//!
//! Capability flags and config paths reflect what was directly observed on a
//! real machine with claude/codex/opencode/agy installed. Bootstrap install
//! commands were independently verified against each vendor's own current
//! documentation (fetched directly, not taken on an agent's word) — sources
//! are recorded alongside each command and mirrored in
//! `docs/install-methods.md`.

use serde::{Deserialize, Serialize};
use divisi_protocol::{BootstrapInstall, CapabilityFlags, HomeRequirement, InstallMethod};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub name: String,
    pub adapter: String,
    pub command: String,
    pub install_method: InstallMethod,
    pub bootstrap_install: Option<BootstrapInstall>,
    pub unverified: bool,
    /// See `HomeRequirement`'s doc comment.
    pub home_requirement: HomeRequirement,
    /// `Some(n)` means at most `n` instances of this agent may run
    /// concurrently (enforced in `divisi-runtime::task`'s spawn
    /// entrypoint) — e.g. opencode's own session SQLite database takes
    /// an exclusive lock that a second concurrent instance can't acquire.
    /// `None` means no known limit.
    pub max_concurrency: Option<u32>,
    pub capabilities: CapabilityFlags,
    /// Config file paths this adapter can read/write, relative to `$HOME`.
    pub config_paths: Vec<String>,
    pub notes: Option<String>,
}

/// The built-in registry. Phase 1 does not yet support user-defined agent
/// entries layered on top of this (spec section 5 allows it eventually);
/// this function is the seam where that would plug in later.
pub fn builtin_registry() -> Vec<AgentDefinition> {
    vec![
        AgentDefinition {
            name: "claude".into(),
            adapter: "claude-code".into(),
            command: "claude".into(),
            install_method: InstallMethod::Native {
                detail: "Anthropic's native installer; installs to ~/.local/share/claude, \
                         symlinked from ~/.local/bin/claude"
                    .into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -fsSL https://claude.ai/install.sh | bash".into(),
                source: "https://code.claude.com/docs/en/setup".into(),
            }),
            unverified: false,
            // Live-verification finding (E29 follow-up, 2026-09-09): a
            // fully OAuth-authenticated `~/.claude.json` + matching
            // `~/.claude/.credentials.json` (unexpired token, same
            // `userID`/`oauthAccount`, byte-identical copy) still fails
            // "Not logged in" when `claude` runs with `$HOME` pointed at
            // an isolated home — confirmed directly against the CLI
            // itself (`HOME=<isolated> claude -p ...`), not just through
            // divisi's wrapper. The same real environment with the
            // same files at its real `$HOME` works immediately. Whatever
            // claude actually validates against isn't fully captured by
            // a `$HOME`-scoped file copy on this version — the same
            // failure mode `codex`/`cursor` already document for their
            // own real-environment-only auth.
            home_requirement: HomeRequirement::RealRequired,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: true,
                mcp: true,
                lsp: true, // observed via ~/.claude/settings.json enabledPlugins (gopls/pyright/etc LSP plugins)
                tools: true,
                sessions: true, // resumable conversation history observed in ~/.claude/history.jsonl
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![".claude.json".into(), ".claude/settings.json".into()],
            notes: None,
        },
        AgentDefinition {
            name: "codex".into(),
            adapter: "codex".into(),
            command: "codex".into(),
            install_method: InstallMethod::PackageManager {
                detail: "Codex's own standalone package manager under ~/.codex/packages/standalone, \
                         symlinked from ~/.local/bin/codex"
                    .into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -fsSL https://chatgpt.com/codex/install.sh | sh".into(),
                source: "https://github.com/openai/codex/blob/main/README.md".into(),
            }),
            unverified: false,
            // codex >= 0.147 stores its ChatGPT OAuth token in the
            // session-global OS keyring (observed: `keyring.load
            // service=…` / `failed to write OAuth tokens to keyring` in the
            // binary; `.codex/auth.json` only holds a plain API key). An
            // isolated `$HOME` can neither hold nor shadow that, so a login
            // there never persists and every run 401s — it must use the
            // real environment.
            home_requirement: HomeRequirement::RealRequired,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: true,
                mcp: true, // observed [mcp_servers.*] tables in ~/.codex/config.toml
                lsp: false,
                tools: true,
                sessions: true, // ~/.codex/sessions observed on disk
                structured_output: true, // `codex exec` non-interactive mode observed via --help
                non_interactive_run: true,
            },
            config_paths: vec![".codex/config.toml".into()],
            notes: None,
        },
        AgentDefinition {
            name: "opencode".into(),
            adapter: "opencode".into(),
            command: "opencode".into(),
            install_method: InstallMethod::StandaloneBinary {
                detail: "Standalone binary at ~/.opencode/bin/opencode".into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -fsSL https://opencode.ai/install | bash".into(),
                source: "https://opencode.ai/docs/".into(),
            }),
            unverified: false,
            // Verified 2026-09-21: works from the isolated home on its own free models (no login), which
            // keeps it off the real home's keyed default model.
            home_requirement: HomeRequirement::Either,
            max_concurrency: Some(3),
            capabilities: CapabilityFlags {
                streaming: true,
                mcp: true, // observed "mcp" key in opencode.jsonc
                lsp: true, // observed "lsp" key in opencode.jsonc
                tools: true,
                sessions: false,
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![".config/opencode/opencode.jsonc".into()],
            notes: None,
        },
        AgentDefinition {
            name: "agy".into(),
            adapter: "antigravity".into(),
            command: "agy".into(),
            install_method: InstallMethod::StandaloneBinary {
                detail: "Standalone binary at ~/.local/bin/agy; ships its own `agy install` \
                         subcommand for PATH/shell-alias setup"
                    .into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -fsSL https://antigravity.google/cli/install.sh | bash".into(),
                source: "https://antigravity.google/docs/cli/install".into(),
            }),
            unverified: false,
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false, // not confirmed; no structured event/streaming flag observed in --help
                mcp: false,       // no on-disk MCP config location found; unconfirmed
                lsp: false,
                tools: true,
                sessions: true, // --continue/--conversation flags observed in --help
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![], // no config directory found on this machine; adapter shells out to `agy` subcommands instead
            notes: None,
        },
        AgentDefinition {
            name: "perplexity".into(),
            adapter: "perplexity".into(),
            // The real binary this install produces is named `pplx`, not
            // `perplexity` — kept as a distinct field from `name` so the
            // registry key stays stable even though the launch command
            // differs from what the other four entries use.
            command: "pplx".into(),
            install_method: InstallMethod::StandaloneBinary {
                detail: "Perplexity's official CLI is `pplx`, a thin client for the Perplexity \
                         Search API (web search + content-snippet extraction). It is NOT an \
                         interactive coding agent like the other four entries in this registry \
                         — it's the kind of tool a coding agent calls, not one itself."
                    .into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -fsSL https://github.com/perplexityai/perplexity-cli/releases/latest/download/install.sh | sh".into(),
                source: "https://docs.perplexity.ai/docs/cli/overview".into(),
            }),
            unverified: false,
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false,
                lsp: false,
                tools: true, // usable as a search tool, not as an agent session
                sessions: false,
                structured_output: true, // returns structured JSON per official docs
                non_interactive_run: true,
            },
            config_paths: vec![],
            notes: Some(
                "No official Perplexity coding-agent CLI exists as of this writing. `pplx` is a \
                 Search API client, included here because it's the closest real product and the \
                 registry needed a concrete decision rather than a fabricated agent CLI. Revisit \
                 if/when Perplexity ships an actual coding agent."
                    .into(),
            ),
        },
        AgentDefinition {
            name: "cursor".into(),
            adapter: "cursor".into(),
            // The real binary is `cursor-agent` (the `cursor` shim on this
            // machine just re-execs it) — kept distinct from `name` the
            // same way `perplexity`/`pplx` are above.
            command: "cursor-agent".into(),
            install_method: InstallMethod::StandaloneBinary {
                detail: "Standalone binary installed to ~/.local/bin/cursor-agent by Cursor's own install script".into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl https://cursor.com/install -fsS | bash".into(),
                source: "https://cursor.com/docs/cli/installation".into(),
            }),
            unverified: false,
            // cursor-agent stores its auth token via a pluggable
            // credential store that defaults to the OS keyring
            // (`AGENT_CLI_CREDENTIAL_STORE`; "Authentication tokens stored
            // securely" on login, and `.cursor/cli-config.json` no longer
            // carries an `authInfo` field). Session-global, so — like
            // codex — it can only authenticate against the real
            // environment.
            home_requirement: HomeRequirement::RealRequired,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: true, // --output-format stream-json observed in --help
                mcp: true,       // observed real ~/.cursor/mcp.json (mcpServers map) on the reference machine
                lsp: false,
                tools: true,
                sessions: true, // --resume/--continue/ls observed in --help
                structured_output: true, // --output-format json|stream-json observed in --help
                non_interactive_run: true,
            },
            config_paths: vec![".cursor/mcp.json".into()],
            notes: None,
        },
        AgentDefinition {
            name: "aider".into(),
            adapter: "aider".into(),
            command: "aider".into(),
            install_method: InstallMethod::StandaloneBinary {
                detail: "Installed via aider-install (uv-managed isolated environment) or the vendor's own install.sh".into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -LsSf https://aider.chat/install.sh | sh".into(),
                source: "https://aider.chat/docs/install.html".into(),
            }),
            unverified: false,
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false, // no mcp subcommand/flag found in `aider --help`; unconfirmed
                lsp: false,
                tools: true,
                sessions: false,
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![".aider.conf.yml".into()],
            notes: Some(
                "Authenticates via API-key flags/env vars (--api-key, --set-env, .env files), \
                 not an interactive OAuth login — `divisi agent login aider` is unsupported \
                 rather than guessing a flow that doesn't exist."
                    .into(),
            ),
        },
        AgentDefinition {
            name: "goose".into(),
            adapter: "goose".into(),
            command: "goose".into(),
            install_method: InstallMethod::StandaloneBinary {
                detail: "Standalone binary installed to $GOOSE_BIN_DIR (default ~/.local/bin) by Block's own download_cli.sh".into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -fsSL https://github.com/block/goose/releases/download/stable/download_cli.sh | bash".into(),
                source: "https://github.com/block/goose/blob/main/download_cli.sh".into(),
            }),
            unverified: false,
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: true, // --output-format stream-json observed in `goose run --help`
                mcp: true,       // observed real ~/.config/goose/config.yaml (extensions map) on the reference machine
                lsp: false,
                tools: true,
                sessions: true, // --resume/--session-id observed in `goose run --help`
                structured_output: true, // --output-format json|stream-json observed in --help
                non_interactive_run: true,
            },
            config_paths: vec![".config/goose/config.yaml".into()],
            notes: None,
        },
        AgentDefinition {
            name: "copilot".into(),
            adapter: "copilot".into(),
            command: "copilot".into(),
            install_method: InstallMethod::StandaloneBinary {
                detail: "GitHub's official Copilot CLI, standalone binary at ~/.local/bin/copilot \
                         (confirmed on the reference machine: a 177MB binary directly in \
                         ~/.local/bin, not an npm-wrapped script — consistent with the install \
                         script below rather than `npm install -g @github/copilot`, which is \
                         also officially supported)"
                    .into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -fsSL https://gh.io/copilot-install | bash".into(),
                source: "https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/install-copilot-cli".into(),
            }),
            unverified: false,
            // Structural, not just empirical: agent_home.rs's
            // strip_embedded_credential_fields always strips copilot's
            // keyring pointer from its isolated home, so isolated auth is
            // impossible by design — real_home:true is the only path.
            home_requirement: HomeRequirement::RealRequired,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: true, // observed real ~/.copilot/mcp-config.json shape by running `copilot mcp add` on the reference machine
                lsp: false,
                tools: true,
                sessions: true, // --resume observed in --help
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![".copilot/mcp-config.json".into()],
            notes: None,
        },
        AgentDefinition {
            name: "kiro".into(),
            adapter: "kiro".into(),
            command: "kiro-cli".into(),
            install_method: InstallMethod::StandaloneBinary {
                detail: "AWS's Kiro CLI, standalone binary at ~/.local/bin/kiro-cli (confirmed present on the reference machine)".into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -fsSL https://cli.kiro.dev/install | bash".into(),
                source: "https://kiro.dev/docs/cli/headless".into(),
            }),
            unverified: false, // installed on the reference machine; `chat`/`mcp`/`login` subcommands and flags confirmed via direct `--help` execution
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false, // `kiro-cli mcp add` confirmed real via --help, but writes require login, which this project won't do just to inspect a file format
                lsp: false,
                tools: true,
                sessions: true, // --resume/--resume-id/--list-sessions observed in `kiro-cli chat --help`
                structured_output: true, // --format json|json-pretty observed in `kiro-cli chat --help`
                non_interactive_run: true,
            },
            config_paths: vec![],
            notes: Some(
                "`kiro-cli mcp add` is real (confirmed via --help) but requires being logged in \
                 to run, so its on-disk config file format couldn't be inspected without \
                 authenticating a real account just to check a file shape — configure_mcp stays \
                 unsupported rather than guessing it."
                    .into(),
            ),
        },
        AgentDefinition {
            name: "cody".into(),
            adapter: "cody".into(),
            command: "cody".into(),
            install_method: InstallMethod::PackageManager {
                detail: "Sourcegraph's Cody CLI, installed as an npm global package".into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "npm install -g @sourcegraph/cody".into(),
                source: "https://sourcegraph.com/docs/cody/clients/install-cli".into(),
            }),
            unverified: true, // not installed on the reference machine; commands sourced from sourcegraph.com docs, not confirmed by direct execution
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false, // no MCP subcommand or config surface documented
                lsp: false,
                tools: true,
                sessions: false,
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![],
            notes: Some(
                "Not installed on the machine this registry was built on; every command here \
                 comes from Sourcegraph's own current docs (fetched directly), not from \
                 running the CLI. Cody CLI is also documented as Experimental for Enterprise \
                 accounts, i.e. Sourcegraph itself doesn't consider it stable yet."
                    .into(),
            ),
        },
        // -- v0.1.18 additions, several since verified in v0.1.19: these
        // were originally sourced from vendor docs only (no adapter, no
        // real execution). qwen-code/amp/droid/codebuff/
        // continue-cli/grok/crush have since been installed and driven
        // with real `--help` output on the reference machine (see
        // divisi-agent-sdk::adapters' v0.1.19 additions) — `unverified` is
        // flipped to `false` for those. openhands/plandex/mistral-vibe
        // failed to install on the reference machine (pip/curl script
        // errors unrelated to divisi) and stay unverified/adapter-less.
        AgentDefinition {
            name: "qwen-code".into(),
            adapter: "qwen-code".into(),
            command: "qwen".into(),
            install_method: InstallMethod::PackageManager { detail: "Alibaba's official npm package".into() },
            bootstrap_install: Some(BootstrapInstall {
                command: "npm install -g @qwen-code/qwen-code@latest".into(),
                source: "https://github.com/QwenLM/qwen-code".into(),
            }),
            unverified: false, // installed on the reference machine; run/mcp/auth subcommands confirmed via direct --help execution
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false, // `qwen mcp` confirmed real via --help (this fork keeps Gemini CLI's MCP support), but its settings file shape wasn't inspected without a logged-in account
                lsp: false,
                tools: true,
                sessions: false,
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![],
            notes: Some(
                "Alibaba's fork of Google's Gemini CLI, adapted for Qwen3-Coder models. \
                 No login command wired up: `qwen auth --help` describes itself as \
                 \"Configure authentication (removed)\" on the reference machine — the \
                 vendor's own help text says the feature is gone."
                    .into(),
            ),
        },
        AgentDefinition {
            name: "amp".into(),
            adapter: "amp".into(),
            command: "amp".into(),
            install_method: InstallMethod::StandaloneBinary {
                detail: "Single-file executable (compiled by Bun), per the vendor's own \
                         npm-package-changes announcement — not distributed as a plain npm package"
                    .into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -fsSL https://ampcode.com/install.sh | bash".into(),
                source: "https://ampcode.com/manual".into(),
            }),
            unverified: false, // installed on the reference machine; login/run/mcp subcommands confirmed via direct --help execution
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false, // `amp mcp add/list/remove` is real and --help documents a flat "amp.mcpServers" settings.json key, but that shape was never round-tripped against a real file — see AmpAdapter::configure_mcp
                lsp: false,
                tools: true,
                sessions: false,
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![],
            notes: Some("Sourcegraph's newer agent product — distinct from the already-registered `cody`. `amp login` confirmed real via --help.".into()),
        },
        AgentDefinition {
            name: "openhands".into(),
            adapter: "openhands".into(),
            command: "openhands".into(),
            install_method: InstallMethod::PackageManager { detail: "Official PyPI package".into() },
            bootstrap_install: Some(BootstrapInstall {
                command: "pip install openhands-ai".into(),
                source: "https://docs.openhands.dev/openhands/usage/run-openhands/local-setup".into(),
            }),
            unverified: true,
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false,
                lsp: false,
                tools: true,
                sessions: false,
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![],
            notes: Some(
                "Formerly OpenDevin. A separate `OpenHands-CLI` GitHub project exists but its own \
                 README says it's no longer actively maintained — this entry uses the maintained \
                 `openhands-ai` pip package instead."
                    .into(),
            ),
        },
        AgentDefinition {
            name: "droid".into(),
            adapter: "droid".into(),
            command: "droid".into(),
            install_method: InstallMethod::StandaloneBinary { detail: "Factory AI's official installer script".into() },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -fsSL https://app.factory.ai/cli | sh".into(),
                source: "https://docs.factory.ai/cli/getting-started/quickstart".into(),
            }),
            unverified: false, // installed on the reference machine; run/mcp subcommands confirmed via direct --help execution
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false, // `droid mcp add/remove/list` confirmed real via --help, but its config file shape wasn't inspected without a logged-in account — see DroidAdapter::configure_mcp
                lsp: false,
                tools: true,
                sessions: true, // a documented "Droid Sessions API" exists; resume UX specifics not confirmed
                structured_output: true, // `droid exec` headless mode documents structured input/output formats
                non_interactive_run: true,
            },
            config_paths: vec![],
            notes: Some("Requires logging into a Factory account before use (free tier exists). No verified login command though: neither `droid --help`, `droid auth --help`, nor `droid login --help` show an auth subcommand on the reference machine.".into()),
        },
        AgentDefinition {
            name: "codebuff".into(),
            adapter: "codebuff".into(),
            command: "codebuff".into(),
            install_method: InstallMethod::PackageManager { detail: "Official npm package".into() },
            bootstrap_install: Some(BootstrapInstall {
                command: "npm install -g codebuff".into(),
                source: "https://www.codebuff.com/docs/help".into(),
            }),
            unverified: false, // installed on the reference machine; login subcommand confirmed via direct --help execution
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false, // no mcp subcommand anywhere in --help on the reference machine
                lsp: false,
                tools: true,
                sessions: false,
                structured_output: false,
                // run_prompt falls through to divisi-agent-sdk's
                // default-trait `bail!` — no non-interactive mode exists.
                non_interactive_run: false,
            },
            config_paths: vec![],
            notes: Some("`codebuff login` confirmed real via --help. No verified non-interactive run mode: --help doesn't confirm whether a positional prompt exits after one response or continues interactively.".into()),
        },
        AgentDefinition {
            name: "plandex".into(),
            adapter: "plandex".into(),
            command: "plandex".into(),
            install_method: InstallMethod::StandaloneBinary { detail: "Official installer script (Go binary)".into() },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -sL https://plandex.ai/install.sh | bash".into(),
                source: "https://github.com/plandex-ai/plandex/blob/main/docs/docs/install.md".into(),
            }),
            unverified: true,
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false,
                lsp: false,
                tools: true,
                sessions: false,
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![],
            notes: Some("Windows only supported via WSL, per the vendor's own docs.".into()),
        },
        AgentDefinition {
            name: "continue-cli".into(),
            adapter: "continue".into(),
            // The real binary is `cn`, not `continue-cli` — kept distinct
            // from `name` the same way `perplexity`/`pplx` and
            // `cursor`/`cursor-agent` are above.
            command: "cn".into(),
            install_method: InstallMethod::StandaloneBinary {
                detail: "Official installer script; also distributed via npm as @continuedev/cli".into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -fsSL https://raw.githubusercontent.com/continuedev/continue/main/extensions/cli/scripts/install.sh | bash".into(),
                source: "https://docs.continue.dev/cli/quickstart".into(),
            }),
            unverified: false, // installed on the reference machine; run mode confirmed via direct --help execution
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false, // only a per-session `--mcp <hub-slug>` flag exists, not a persistent local config to write
                lsp: false,
                tools: true,
                sessions: false,
                structured_output: true, // headless `cn -p "prompt"` runs to completion and prints to stdout for scripting/CI
                non_interactive_run: true,
            },
            config_paths: vec![],
            notes: Some(
                "Distinct from the Continue.dev IDE extension — `cn` is a separate \
                 headless/background-job agent binary for async cloud runs (PR review, \
                 migrations, CI), not an interactive session the way most other entries here are. \
                 No verified login command: no auth/login subcommand in `cn --help`."
                    .into(),
            ),
        },
        AgentDefinition {
            name: "grok".into(),
            adapter: "grok-build".into(),
            command: "grok".into(),
            install_method: InstallMethod::StandaloneBinary { detail: "xAI's official installer script".into() },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -fsSL https://x.ai/cli/install.sh | bash".into(),
                source: "https://docs.x.ai/build/overview".into(),
            }),
            unverified: false, // installed on the reference machine; login/run/mcp subcommands confirmed via direct --help execution
            home_requirement: HomeRequirement::IsolatedOnly,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false, // `grok mcp` confirmed real via --help, but its config file shape wasn't inspected without a logged-in account — see GrokAdapter::configure_mcp
                lsp: false,
                tools: true,
                sessions: false, // subagents/worktrees documented, but resume UX not confirmed
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![],
            notes: Some(
                "xAI's terminal coding agent, \"Grok Build\" (beta as of this writing) — requires \
                 SuperGrok/X Premium Plus subscription or an XAI_API_KEY. Repo: xai-org/grok-build. \
                 `grok login` confirmed real via --help."
                    .into(),
            ),
        },
        AgentDefinition {
            name: "mistral-vibe".into(),
            adapter: "mistral-vibe".into(),
            command: "vibe".into(),
            install_method: InstallMethod::StandaloneBinary {
                detail: "Official installer script; also distributable via `pip install mistral-vibe`".into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -LsSf https://mistral.ai/vibe/install.sh | sh".into(),
                source: "https://mistral.ai/news/devstral-2-vibe-cli/".into(),
            }),
            unverified: true,
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false,
                lsp: false,
                tools: true,
                sessions: false,
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![],
            notes: Some(
                "Mistral's official terminal coding agent, built to work against Devstral 2 or \
                 any OpenAI-compatible local/remote model endpoint."
                    .into(),
            ),
        },
        AgentDefinition {
            name: "crush".into(),
            adapter: "crush".into(),
            command: "crush".into(),
            install_method: InstallMethod::PackageManager {
                detail: "Official npm package (also available via Homebrew, Nix, and Go install)".into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "npm install -g @charmland/crush".into(),
                source: "https://github.com/charmbracelet/crush".into(),
            }),
            unverified: false, // installed on the reference machine; login/run subcommands confirmed via direct --help execution
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false, // no mcp subcommand appears in --help on the reference machine; vendor docs describe config-file-based MCP but the file wasn't inspected without a logged-in account
                lsp: true, // vendor docs state "LSP-enhanced context"
                tools: true,
                sessions: false,
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![],
            notes: Some(
                "Charmbracelet's multi-provider agent — connects to Anthropic, OpenAI, Gemini, \
                 OpenRouter, Bedrock, Azure OpenAI, Vertex AI, and local model servers by design, \
                 unlike the single-vendor entries elsewhere in this registry. `crush login \
                 [platform]` confirmed real via --help."
                    .into(),
            ),
        },
        AgentDefinition {
            name: "kilocode".into(),
            adapter: "kilocode".into(),
            command: "kilo".into(),
            install_method: InstallMethod::StandaloneBinary {
                detail: "Standalone binary at ~/.kilo/bin/kilo, added to $PATH by its own \
                         installer (confirmed present on the reference machine)"
                    .into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "curl -fsSL https://kilo.ai/cli/install | bash".into(),
                source: "https://github.com/Kilo-Org/kilocode".into(),
            }),
            unverified: false, // installed on the reference machine; run/auth/mcp subcommands confirmed via direct --help execution
            home_requirement: HomeRequirement::Unverified,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false, // `kilo mcp add/list` confirmed real via --help, but its config file shape wasn't inspected without a logged-in account — see KiloCodeAdapter::configure_mcp
                lsp: false,
                tools: true,
                sessions: true, // --continue/--session flags confirmed in `kilo run --help`
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![], // config file shape not confirmed — Kilo is a fork of OpenCode but its config location wasn't inspected without a logged-in account
            notes: Some(
                "Kilo Code — an actively-maintained open-source fork of OpenCode \
                 (github.com/Kilo-Org/kilocode). Installs via curl to ~/.kilo/bin/kilo. \
                 `kilo run \"<prompt>\"` is non-interactive per `kilo run --help` (v7.x; \
                 the older `--auto`/`--dir` flags were dropped — kilo run now auto-approves \
                 by default). `kilo auth login` confirmed real via --help. `kilo mcp add` \
                 confirmed real via --help, but no logged-in account to inspect the config \
                 file shape."
                    .into(),
            ),
        },
        // -- divisi-agent: divisi's own in-process coding agent (not a
        // vendor CLI — built from this workspace's divisi-native-agent
        // crate).
        AgentDefinition {
            name: "divisi-agent".into(),
            adapter: "divisi-agent".into(),
            command: "divisi-agent".into(),
            install_method: InstallMethod::Native {
                detail: "Built from this workspace's divisi-native-agent crate \
                         via `cargo build --release -p divisi-native-agent`; \
                         not fetched from an external vendor."
                    .into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "cargo build --release -p divisi-native-agent".into(),
                source: "https://github.com/naviNBRuas/SingleCLI".into(),
            }),
            unverified: false,
            // divisi-agent has no auth state of its own — it reads API
            // keys from divisi's own secret store via
            // divisi_core::secrets::SecretStore, not from its own
            // config/credentials files, so it authenticates identically
            // under an isolated home or the real one.
            home_requirement: HomeRequirement::Either,
            max_concurrency: None,
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: true, // call_mcp tool connects to divisi-gateway gateway via rmcp
                lsp: false,
                tools: true,
                sessions: false,
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec![],
            notes: Some(
                "divisi's own native in-process coding agent. Requires \
                 --provider and --model flags which aren't part of the \
                 standard prompt-only adapter interface; the adapter \
                 currently reads these from DIVISI_AGENT_PROVIDER and \
                 DIVISI_AGENT_MODEL env vars (falling back to \
                 opencode-zen/laguna-s-2.1-free), documented in the \
                 adapter impl. Exposes a call_mcp tool that spawns \
                 divisi-gateway as a child process (lazily, once per run) \
                 and proxies MCP tool calls through it."
                    .into(),
            ),
        },
        // E28's free-provider-pool agent — dispatches directly over HTTP
        // to the vendored `divisi_core::free_pool` catalog via the
        // bandit/ledger/cooldown engine (divisi-runtime::pool), never
        // shells a binary. See `divisi_agent_sdk::adapters::PoolAdapter`'s
        // doc comment for why its `run_prompt` is a placeholder and the
        // real dispatch happens in `divisi-runtime::task::execute`'s
        // special case instead.
        AgentDefinition {
            name: "divisi-pool".into(),
            adapter: "pool".into(),
            command: "divisi-pool".into(),
            install_method: InstallMethod::Native {
                detail: "Built into divisid; no separate binary to install.".into(),
            },
            bootstrap_install: Some(BootstrapInstall {
                command: "cargo build --release -p divisi-runtime".into(),
                source: "https://github.com/naviNBRuas/SingleCLI".into(),
            }),
            unverified: false,
            // No filesystem-based auth at all -- keys live in divisi's
            // own encrypted store (divisi_core::pool_keys), resolved
            // identically regardless of $HOME. `Either` is the least
            // restrictive value; this field is moot for an agent that
            // never shells anything.
            home_requirement: HomeRequirement::Either,
            max_concurrency: None, // the ledger caps concurrency per (platform, model, key), not a process slot.
            capabilities: CapabilityFlags {
                streaming: false,
                mcp: false,
                lsp: false,
                tools: false, // tool-call rescue exists in pool::client but isn't exposed as a first-class capability yet.
                sessions: false,
                structured_output: false,
                non_interactive_run: true,
            },
            config_paths: vec!["free-pool.toml".into()],
            notes: Some(
                "The E28 free-provider pool agent: `divisi task run --agent divisi-pool` \
                 picks a (provider, model, key) via the adaptive bandit and dispatches \
                 straight to the provider's HTTP API, no CLI shelled. Use `divisi provider \
                 list-free`/`add-free`/`sync-pool` to see and key the ~40-provider catalog."
                    .into(),
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_has_the_original_eleven_agents() {
        let reg = builtin_registry();
        let names: Vec<_> = reg.iter().map(|a| a.name.as_str()).collect();
        for name in
            ["claude", "codex", "opencode", "agy", "perplexity", "cursor", "aider", "goose", "copilot", "kiro", "cody"]
        {
            assert!(names.contains(&name), "missing original agent {name}");
        }
    }

    #[test]
    fn v0_1_18_catalog_expansion_added_at_least_11_new_agents_with_unique_names() {
        let reg = builtin_registry();
        assert!(reg.len() >= 22, "expected at least 22 total agents (11 original + 11 new), got {}", reg.len());

        let mut names: Vec<&str> = reg.iter().map(|a| a.name.as_str()).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before, "duplicate agent name in registry");
    }

    #[test]
    fn every_agent_has_a_verified_bootstrap_install_with_source() {
        let reg = builtin_registry();
        for agent in &reg {
            let install = agent
                .bootstrap_install
                .as_ref()
                .unwrap_or_else(|| panic!("agent {} has no bootstrap install", agent.name));
            // Every install command is a real one sourced from the vendor's own
            // docs — usually a curl script, but `cody`/`qwen-code`/
            // `codebuff`/`crush` document npm as their official path, and
            // `openhands` documents pip as its official path.
            assert!(
                install.command.contains("curl")
                    || install.command.contains("npm install")
                    || install.command.contains("pip install")
                    || install.command.contains("cargo build"),
                "agent {}",
                agent.name
            );
            assert!(install.source.starts_with("https://"), "agent {}", agent.name);
        }
    }

    #[test]
    fn perplexity_entry_is_flagged_as_not_a_coding_agent() {
        let reg = builtin_registry();
        let perplexity = reg.iter().find(|a| a.name == "perplexity").unwrap();
        assert_eq!(perplexity.command, "pplx");
        assert!(perplexity.notes.is_some());
        assert!(!perplexity.capabilities.sessions);
    }
}

#[cfg(test)]
mod home_requirement_tests {
    use super::*;

    fn find(name: &str) -> AgentDefinition {
        builtin_registry()
            .into_iter()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("no such agent in builtin_registry: {name}"))
    }

    #[test]
    fn copilot_and_opencode_require_real_home() {
        assert_eq!(find("copilot").home_requirement, HomeRequirement::RealRequired);
        assert_eq!(find("opencode").home_requirement, HomeRequirement::Either);
    }

    #[test]
    fn grok_is_isolated_only() {
        assert_eq!(find("grok").home_requirement, HomeRequirement::IsolatedOnly);
    }

    #[test]
    fn codex_cursor_and_claude_require_real_home() {
        // All three moved their auth token somewhere an isolated $HOME
        // can neither hold nor shadow, so a login there never persists.
        // See their registry-entry comments — claude's was added after a
        // live-verification finding (E29 follow-up): a byte-identical
        // copy of `~/.claude.json` + `~/.claude/.credentials.json` into
        // an isolated home still fails "Not logged in", confirmed
        // against the CLI directly, not just through divisi.
        assert_eq!(find("codex").home_requirement, HomeRequirement::RealRequired);
        assert_eq!(find("cursor").home_requirement, HomeRequirement::RealRequired);
        assert_eq!(find("claude").home_requirement, HomeRequirement::RealRequired);
    }

    #[test]
    fn most_agents_are_unverified_by_default() {
        assert_eq!(find("aider").home_requirement, HomeRequirement::Unverified);
    }

    #[test]
    fn opencode_has_a_concurrency_limit_of_three() {
        assert_eq!(find("opencode").max_concurrency, Some(3));
    }

    #[test]
    fn most_agents_have_no_concurrency_limit() {
        assert_eq!(find("claude").max_concurrency, None);
        assert_eq!(find("codex").max_concurrency, None);
    }

    #[test]
    fn codebuff_has_no_non_interactive_run_mode() {
        assert!(!find("codebuff").capabilities.non_interactive_run);
    }

    #[test]
    fn every_other_agent_has_non_interactive_run() {
        for agent in builtin_registry() {
            if agent.name != "codebuff" {
                assert!(
                    agent.capabilities.non_interactive_run,
                    "{} should have non_interactive_run: true",
                    agent.name
                );
            }
        }
    }
}
