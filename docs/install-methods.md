# Verified install methods for agent CLIs

This records exactly how `single setup` installs each agent CLI, and where
each command was verified. Per the project's own rule ("do not fabricate
APIs, flags, configuration formats, or capabilities"), every command below
was fetched directly from the vendor's own current documentation or GitHub
README — not taken on trust from a research summary — before being wired
into `crates/divisi-core/src/registry.rs`.

| Agent | Command | Install command | Source |
|---|---|---|---|
| Claude Code | `claude` | `curl -fsSL https://claude.ai/install.sh \| bash` | https://code.claude.com/docs/en/setup |
| Codex CLI | `codex` | `curl -fsSL https://chatgpt.com/codex/install.sh \| sh` | https://github.com/openai/codex/blob/main/README.md |
| OpenCode | `opencode` | `curl -fsSL https://opencode.ai/install \| bash` | https://opencode.ai/docs/ |
| Antigravity CLI | `agy` | `curl -fsSL https://antigravity.google/cli/install.sh \| bash` | https://antigravity.google/docs/cli/install |
| Perplexity | `pplx` | `curl -fsSL https://github.com/perplexityai/perplexity-cli/releases/latest/download/install.sh \| sh` | https://docs.perplexity.ai/docs/cli/overview |
| Cursor CLI | `cursor-agent` | `curl https://cursor.com/install -fsS \| bash` | https://cursor.com/docs/cli/installation |
| Aider | `aider` | `curl -LsSf https://aider.chat/install.sh \| sh` | https://aider.chat/docs/install.html |
| Goose | `goose` | `curl -fsSL https://github.com/block/goose/releases/download/stable/download_cli.sh \| bash` | https://github.com/block/goose/blob/main/download_cli.sh |
| GitHub Copilot CLI | `copilot` | `curl -fsSL https://gh.io/copilot-install \| bash` | https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/install-copilot-cli |
| Kiro CLI | `kiro-cli` | `curl -fsSL https://cli.kiro.dev/install \| bash` | https://kiro.dev/docs/cli/headless |
| Cody CLI | `cody` | `npm install -g @sourcegraph/cody` | https://sourcegraph.com/docs/cody/clients/install-cli |

## Important caveat: Perplexity does not have a coding-agent CLI

The task that seeded this project assumed a "Perplexity CLI" comparable to
Claude Code, Codex, OpenCode, and Antigravity — an interactive coding agent.
As of this writing, no such product exists. Perplexity's only official CLI
is `pplx`, a thin client for the **Perplexity Search API**: it does web
search and content-snippet extraction and returns structured JSON. It has
no chat/session model, no tool-use loop, and nothing resembling an
interactive coding agent — it's the kind of tool a coding agent *calls*,
not one itself.

The registry still installs it (the user explicitly asked for every agent
to be installable through SingleCLI, and `pplx` is the closest real
product), but `AgentDefinition.notes` on the `perplexity` entry flags this
plainly, `capabilities.sessions` is `false`, and `single doctor` /
`single agent inspect perplexity` surface the caveat rather than presenting
it as equivalent to the other four. If Perplexity ships an actual coding
agent in the future, this entry should be revisited.

## Confirmed-installed reference points (used to build the registry)

On the machine this was built on, `claude`, `codex`, `opencode`, `agy`,
`cursor-agent`, `aider`, `goose`, `copilot`, and `kiro-cli` were already
installed (`cody` was not — see "Not installed" below). Their real,
observed installation methods (as opposed to the fresh-install bootstrap
commands above) are recorded in `registry.rs`'s `InstallMethod` field for
each agent:

- **Claude Code**: native installer, binary under `~/.local/share/claude/versions/<ver>`, symlinked from `~/.local/bin/claude`. `installMethod: "native"` is also recorded directly in `~/.claude.json`.
- **Codex**: its own standalone package manager under `~/.codex/packages/standalone/`, symlinked from `~/.local/bin/codex`.
- **OpenCode**: a standalone binary at `~/.opencode/bin/opencode`.
- **Antigravity (`agy`)**: a standalone binary at `~/.local/bin/agy`, with its own `agy install` subcommand for PATH/shell-alias setup (confirmed via `agy install --help`, which shows the same `--skip-aliases`/`--skip-path` flags documented on antigravity.google's install page — good corroborating evidence the fetched page is genuine).
- **Cursor CLI (`cursor-agent`)**: a standalone binary at `~/.local/bin/cursor-agent`; the separate `cursor` shim on this machine just re-execs it.
- **Aider**: installed via `aider-install` (a `uv`-managed isolated environment).
- **Goose**: a standalone binary at `~/.local/bin/goose`, installed by Block's own `download_cli.sh`.
- **GitHub Copilot CLI**: a standalone 177MB binary at `~/.local/bin/copilot` — confirmed not an npm-wrapped script (also officially supported: `npm install -g @github/copilot`), consistent with the `curl | bash` install script rather than the npm path.
- **Kiro CLI**: a standalone binary at `~/.local/bin/kiro-cli`.

None of these nine were installed via a shared/generic package manager
(no npm-global entries for any of them) — this is why `single setup` runs
each vendor's own install script rather than assuming `npm install -g`
works universally.

## Not installed: Cody (verified from docs only) and Windsurf (not added)

`cody` was not installed on the reference machine, so unlike every other
entry in this file, its command syntax (`cody auth login --web`, `cody
chat -m "..."`) comes from Sourcegraph's own current docs (fetched
directly), not from running the CLI — `divisi-core::registry` marks it
`unverified: true` for this reason, and it has no MCP support wired up
since none is documented. Sourcegraph itself also documents Cody CLI as
"Experimental" for Enterprise accounts.

**Windsurf was investigated and deliberately not added.** As of this
writing there is no standalone Windsurf agent CLI comparable to the other
entries in this registry: Windsurf's own install script
(`https://cli.devin.ai/install.sh`) now installs **Devin's** CLI, since
Windsurf was acquired and rebranded into "Devin Desktop"; the other
`windsurf-cli` projects found are unofficial community tools that just
open files/directories in the Windsurf editor, not AI coding agents. This
is the same honesty standard applied to Perplexity above — a real gap
gets documented, not papered over with a guessed or unofficial tool.

## Version-check caveat

Codex CLI's and Antigravity's official docs don't explicitly document a
`--version` flag the way Claude Code and OpenCode do. `codex --version` and
`agy --version` were confirmed to work directly on the reference machine
(see the Phase 1 investigation in this repo's git history), so
`divisi-agent-sdk::discover` uses `<command> --version` uniformly — but
treat this as machine-observed behavior, not vendor-documented contract,
if either CLI changes its `--version` output format in the future.
