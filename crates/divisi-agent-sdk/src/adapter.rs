use crate::backend::ExecBackend;
use crate::discover::{discover, Discovery};
use anyhow::Result;
use divisi_protocol::{IntegrationWrite, LspServerSpec, McpServerSpec, RunOutcome};
use std::path::Path;
use std::time::Duration;

/// What divisi asks of an agent adapter: real detection, writing
/// divisi's unified MCP registry into the agent's native config format,
/// and (Phase 4) a synchronous one-shot non-interactive invocation.
///
/// `run_prompt` is deliberately **not** the full spec section 39 lifecycle
/// (`start`/`stop`/`pause`/`resume`/`stream`/`cancel` against a live
/// session) — it shells out to each CLI's own non-interactive mode
/// (`claude -p`, `codex exec`, `opencode run`, `agy -p`), blocks until it
/// exits or a timeout fires, and returns the captured output. That's a
/// real, working capability (Phase 4's orchestrator uses it for real task
/// execution), just a narrower one than a fully streamed, cancellable
/// session — which needs the runtime to hold long-lived per-task process
/// state across multiple requests, not yet built. Extending this to true
/// streaming later is additive, not a rewrite of this trait.
pub trait AgentAdapter {
    fn command(&self) -> &str;

    fn discover(&self) -> Discovery {
        discover(self.command())
    }

    /// Applies `servers` to this agent's real config file(s) under `home`.
    /// When `dry_run` is true, computes what *would* change without writing
    /// anything (still backs up nothing, writes nothing).
    fn configure_mcp(&self, home: &Path, servers: &[McpServerSpec], dry_run: bool) -> Result<IntegrationWrite>;

    /// Inverse of `configure_mcp`: removes only the named servers, used by
    /// `divisi uninstall-integrations`.
    fn remove_mcp(&self, home: &Path, names: &[String], dry_run: bool) -> Result<IntegrationWrite>;

    /// Same shape as `configure_mcp`, for the LSP registry. Default:
    /// unsupported — only `opencode`'s `opencode.jsonc` has a directly
    /// observed native config surface for arbitrary LSP servers (see
    /// `divisi-core::lsp` module docs); every other agent's LSP story is
    /// either a different mechanism entirely (Claude's plugin-based
    /// `*-lsp` marketplace entries) or unconfirmed, so this returns an
    /// honest "not applied" result rather than guessing a file format.
    fn configure_lsp(&self, home: &Path, _servers: &[LspServerSpec], _dry_run: bool) -> Result<IntegrationWrite> {
        Ok(unsupported_lsp(self.command(), home))
    }

    /// Inverse of `configure_lsp`, used by `divisi uninstall-integrations`.
    fn remove_lsp(&self, home: &Path, _names: &[String], _dry_run: bool) -> Result<IntegrationWrite> {
        Ok(unsupported_lsp(self.command(), home))
    }

    /// Runs `prompt` non-interactively with `cwd` as the working directory,
    /// killing the process and setting `timed_out: true` if it runs past
    /// `timeout`. Default: unsupported (used by agents with no verified
    /// non-interactive mode, e.g. `pplx`, which isn't a coding agent at all).
    ///
    /// `backend` picks where the process actually runs — the host (with an
    /// optional `$HOME` override, same meaning as the old plain
    /// `Option<&Path>` this replaced; this is how multiple isolated
    /// accounts of the same agent run concurrently, see
    /// `divisi-core::account::ensure_isolated_home`) or an opt-in
    /// persistent Docker container (see `backend::ExecBackend`).
    ///
    /// `live_output_path`, when set, receives the agent's stdout/stderr
    /// tee'd there as they're produced (see
    /// `divisi-agent-sdk::run::run_command_live`), so a concurrent reader
    /// — the TUI's task-detail view — can watch the run in progress
    /// instead of only seeing output once it finishes.
    ///
    /// `cancel`, when set, is polled by the same loop that already watches
    /// `timeout` — flipping it kills the child early and marks the
    /// outcome `cancelled` instead of `timed_out`. Only meaningful for a
    /// task started with `background: true` (see
    /// `divisi-runtime::task::run_background`); a synchronous foreground
    /// run has nothing to flip it, so `None` there is correct, not a
    /// missing feature.
    #[allow(clippy::too_many_arguments)]
    fn run_prompt(
        &self,
        _cwd: &Path,
        _prompt: &str,
        _backend: &ExecBackend,
        _live_output_path: Option<&Path>,
        _timeout: Duration,
        _cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<RunOutcome> {
        anyhow::bail!("{} has no non-interactive run mode wired up", self.command())
    }

    /// Same as `run_prompt`, but asks the agent for a structured-output
    /// mode that reports real token usage (`RunOutcome::usage`). Opt-in
    /// per agent — the coordinator sets it for agents listed in
    /// `coordinator.toml`'s `usage_json_agents`, and `divisi task run
    /// --usage-json` requests it directly. Default: fall through to
    /// `run_prompt` (no usage; the caller then parse-or-estimates). Only
    /// override where the agent has a verified JSON envelope carrying
    /// usage — currently just `claude --output-format json`.
    #[allow(clippy::too_many_arguments)]
    fn run_prompt_json(
        &self,
        cwd: &Path,
        prompt: &str,
        backend: &ExecBackend,
        live_output_path: Option<&Path>,
        timeout: Duration,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<RunOutcome> {
        self.run_prompt(cwd, prompt, backend, live_output_path, timeout, cancel)
    }

    /// Installs a plugin via this agent's own real plugin-install command
    /// (`claude plugin install`, `codex plugin add`, `opencode plugin`,
    /// `agy plugin install` — verified per-agent, see each impl). `home`
    /// is used as both the subprocess's working directory and its `$HOME`
    /// override, so the install lands in the agent's divisi-managed
    /// home (`divisi_core::agent_home`), not the real ambient one.
    /// Default: unsupported (agents with no verified plugin CLI, e.g.
    /// `pplx`).
    fn install_plugin(&self, _target: &str, _home: &Path, _timeout: Duration) -> Result<RunOutcome> {
        anyhow::bail!("{} has no verified plugin-install command wired up", self.command())
    }

    /// Runs this agent's own real interactive login command attached to
    /// the user's terminal, with `$HOME` overridden to `home` (its
    /// divisi-managed isolated home — `divisi_core::agent_home`) so
    /// the resulting credentials land there, not in the real ambient
    /// `$HOME`. Interactive by design: OAuth logins need a browser
    /// round-trip or a device code the user reads and confirms, so this
    /// can't be captured/timeout-bounded like `run_prompt`. Default:
    /// unsupported (no verified login command for this agent).
    fn login(&self, _home: &Path) -> Result<()> {
        anyhow::bail!("{} has no verified interactive login command wired up", self.command())
    }

    /// Whether `login()` above is a real, verified command rather than the
    /// default unsupported stub — lets callers (namely `doctor`) give an
    /// honest answer instead of pointing at `divisi agent login <name>`
    /// for an agent that will just error. Override to `true` alongside any
    /// real `login()` implementation.
    fn login_supported(&self) -> bool {
        false
    }
}

/// `<command> -p -- <prompt>` — for agents where `-p` is a bare mode-switch
/// flag and the prompt is a separate positional argument (confirmed for
/// claude by direct execution). The `--` is load-bearing, not decorative:
/// `divisi task run`'s memory/notes preamble (`task::context_preamble`)
/// starts with a literal `"---"`, and without a `--` separator these
/// agents' clap-style parsers reject that value as an unrecognized option
/// rather than binding it as the positional prompt. **Not every agent
/// works this way** — some (goose, grok, qwen-code, agy) define
/// their prompt as a named flag's *value* instead, where `--` breaks
/// parsing differently and `--flag=value` is the fix instead; those get
/// their own `run_prompt` rather than using this helper. Confirmed by
/// direct execution on the reference machine for every current caller —
/// don't assume it's safe for a new one without checking the same way.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_with_prompt_flag(
    command: &str,
    cwd: &Path,
    prompt: &str,
    backend: &ExecBackend,
    live_output_path: Option<&Path>,
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<RunOutcome> {
    crate::run::run_command_live(command, &["-p".to_string(), "--".to_string(), prompt.to_string()], cwd, backend, live_output_path, timeout, cancel)
}

fn unsupported_lsp(agent: &str, home: &Path) -> IntegrationWrite {
    IntegrationWrite {
        agent: agent.to_string(),
        config_path: home.display().to_string(),
        backup_path: None,
        applied: false,
        detail: format!("{agent} has no verified LSP config sync (only opencode's opencode.jsonc lsp key is confirmed)"),
    }
}
