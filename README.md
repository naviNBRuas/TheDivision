# divisi

A unified control plane and coordinator for heterogeneous AI coding-agent
CLIs — 24 built-in agents (Claude Code, Codex, OpenCode, Antigravity,
Cursor CLI, GitHub Copilot CLI, Aider, Goose, Kiro, Cody, Grok, Crush,
Kilo Code, and more), plus any new agent CLI you describe in a TOML file
with no recompilation. Configure MCP servers, provider keys, and accounts
once; every supported agent gets the same configuration synced into its
own native format.

Beyond syncing config, divisi's **Coordinator** turns a single goal
("add tests for the parser and fix whatever they find") into a real
dependency graph of tasks, dispatches each node to whichever real agent
or model fits, retries and reroutes around rate limits and cooldowns
automatically, and reports back — through the CLI, the TUI's own **Goals**
tab, or Zed's agent panel via a native ACP bridge (`divisi acp`). When
none of your logged-in agent CLIs have capacity, the **free-provider
pool** (44 vendored providers, Thompson-sampling bandit routing, real
per-key cooldown/headroom tracking) dispatches straight over HTTP instead
of shelling a CLI at all — no agent login required to keep working.

> **Status: actively developed, well past early scaffolding.** Agent
> registry, MCP/LSP/tool/provider registries, multi-account concurrency,
> the Coordinator + goal graph, the free-provider pool, a native Zed ACP
> bridge, and a full TUI (Agents/Goals/Tasks/MCP/LSP/Plugins/Tools/
> Providers/Accounts/Usage/Pool/Backup/Memory) are all real and covered
> by the workspace's own test suite. See "What's implemented" below and
> `docs/architecture.md` for the full picture, including what's
> deliberately *not* here yet.

## Why

Every one of these CLIs maintains its own independent config for the same
underlying capabilities — MCP servers, provider API keys, login
credentials — each in a different file/format. divisi keeps one
unified registry for each and syncs it out to whichever CLIs are
installed, installs the CLIs themselves on a machine that has none of
them yet, and lets you add a brand-new agent CLI to the whole system by
writing one TOML file, no recompilation required.

divisi also doesn't touch your real, ambient `~/.claude`, `~/.codex`,
etc. on every run. Each agent gets its own divisi-managed home under
`~/.config/divisi/homes/<agent>/`, bootstrapped from the real one exactly
once; every `task run`, `install-integrations`, `plugin sync`, `provider
sync`, and `account capture`/`use` after that operates only inside that
isolated copy — see `docs/architecture.md`'s "Isolation" section.

## Install

**Linux and macOS (x86_64 and arm64):**

```bash
curl -fsSL https://raw.githubusercontent.com/naviNBRuas/SingleCLI/main/install.sh | sh
```

Downloads the prebuilt `divisi` and `divisid` binaries for your
platform from the latest [release](https://github.com/naviNBRuas/SingleCLI/releases)
to `~/.local/bin` (override with `DIVISI_INSTALL_DIR`). See
[`install.sh`](install.sh) — it's a plain shell script, read it before
piping it into `sh` if you want to know exactly what it does.

**From source** (any platform with a Rust toolchain):

```bash
cargo build --release --workspace
```

Binaries land in `target/release/`: `divisi` (the CLI/TUI) and
`divisid` (the headless runtime daemon). Put both on `$PATH`.

## Quickstart

```bash
single doctor          # what's installed, what divisi can manage
single agent list      # the agent registry, live detection status
single agent login claude   # log in to claude's divisi-managed home (real terminal, real OAuth)
single agent login codex    # same for codex, opencode, or perplexity
single mcp list         # the unified MCP registry
single setup --yes       # install missing agent CLIs + sync config
single install-integrations --yes   # sync MCP config into every agent, with backups
single task run "add a .gitignore" --agent claude --cwd ~/code/some-project   # delegate a prompt in any project directory
single orchestrate "add tests for the parser" --agents claude,codex --worktree   # relay across multiple agents

# use an agent, through single, to actually set up a fresh machine (real $HOME, not the sandbox):
single task run "install my usual dev tools, set up my dotfiles, configure the desktop" \
  --agent claude --real-home --timeout-secs 3600

single account capture claude work      # snapshot the currently logged-in Claude account
single account use claude personal      # switch to a different captured account

single provider presets                             # OpenAI, Anthropic, OpenCode Zen, NVIDIA
single provider add-preset nvidia
single provider set-key nvidia nvapi-...
single provider sync nvidia --agents claude --yes

single mcp presets                                  # brave-search, slack, puppeteer, postgres
single mcp add-preset brave-search
single lsp presets                                  # rust-analyzer, pyright, typescript, gopls, dockerfile, clangd, bash, yaml, terraform, json
single lsp add-preset clangd

single plugin add my-plugin my-plugin@official       # target used verbatim by claude/codex/agy
single plugin sync my-plugin --agents claude --yes   # runs `claude plugin install my-plugin@official`

single account capture claude work --label work@example.com   # snapshot the currently logged-in Claude account
single account use claude personal                             # switch to a different captured account
single account set-status claude work rate_limited             # track usability (manual — no agent exposes a quota API)
single task run "..." --agent claude --account work             # run against an isolated $HOME so multiple
                                                                  # accounts of the same agent run concurrently

single skill install my-skill ./my-skill-dir
single skill sync-claude my-skill       # copies it into ~/.claude/skills/my-skill/

single memory graph create-entity divisi project
single memory graph show                # dump the shared knowledge graph

single                  # launch the TUI: Agents/Goals/Tasks/MCP/LSP/Plugins/Tools/Providers/Accounts/
                        # Usage/Pool/Backup/Memory tabs. [i] installs an agent interactively, [n]
                        # creates a task, [enter] on a task shows its live output (auto-refreshing
                        # while running); orchestrate runs create one row per agent per step, so each
                        # agent's own output is one [enter] away. [a] quick-adds into MCP/LSP/Plugins/
                        # Tools, [d]/[e]/[s] remove/toggle/sync the selection

single goal submit "add tests for the parser and fix whatever they find" --mode auto
single coordinator status                    # running/queued/blocked/waiting-on-capacity goals + pool
single goal status <goal-id>                 # that goal's task graph, node-by-node status
single acp                                   # stdio ACP server — point Zed's agent panel at this

single task run --agent single-pool "explain this diff"   # dispatch straight to the free-provider
                                                            # pool over HTTP, no agent CLI, no login
single provider list-free                    # the vendored ~44-provider free-LLM catalog
single provider add-free groq                # register + best-effort validate a key
single provider validate                     # re-probe every already-keyed free-pool key on demand
single provider key-status                   # keyed?/valid?/cooldown/headroom, per provider

single update --check                       # is a newer stable build available?
single update --yes                         # replace the running binaries in place
single update --channel nightly --yes       # track main instead of tagged releases
```

Every list/inspect command supports `--json` for scripting.

## What's implemented

- **Phase 1** — a real agent registry for `claude`, `codex`, `opencode`,
  `agy`, and `perplexity` (`pplx`) with detection/versions/capabilities
  observed from real config files and `--version` output; a unified MCP
  registry synced into each agent's native config format with backups;
  vendor-verified bootstrap installers (`divisi setup`); a headless
  runtime daemon over a Unix socket with a CLI and TUI client; profiles
  and config precedence.
- **Phase 2** — CRUD for the MCP/LSP/tool registries, an OS-keychain
  secrets abstraction, a deny/ask/allow permission model (data only, not
  yet enforced), and a local skills directory.
- **Phase 3** — a SQLite-backed, scoped, provenance-tagged memory store,
  and a git/project context resolver.
- **Phase 4** — real single-agent task execution: `divisi task run`
  invokes an agent CLI's actual non-interactive mode (`claude -p`, `codex
  exec`, `opencode run`, `agy -p`), optionally isolated in a real git
  worktree, captures the output as an artifact, and records the result.
- **Phase 5** — declarative custom agents: describe a brand-new CLI agent
  in `~/.config/divisi/agents/<name>.toml` (command, install command,
  prompt mode, MCP format) and it gets real detection, MCP sync, and task
  execution identically to the five built-in agents — no Rust required.
- **Phase 6 (partial)** — a provider registry (OpenAI, Anthropic, OpenCode
  Zen, ...) with keys held in the OS keychain, synced into the agents with
  a verified config slot for them (Claude Code's `env` settings, Codex's
  `OPENAI_API_KEY`).
- **Auth** — `divisi account capture/use/list/remove`: snapshot an agent's
  current login state as a named profile and switch between them later —
  e.g. two separate Claude Code accounts — with automatic backups and
  without ever printing token contents.
- **Memory upgrades** — a shared SQLite knowledge graph (entities,
  observations, typed relations — `divisi memory graph ...`), plus
  optional Redis working memory (`divisi memory cache ...`,
  `DIVISI_REDIS_URL`) and Qdrant vector storage/search for RAG
  (`divisi memory vector ...`, `DIVISI_QDRANT_URL`) — both built and
  tested against real local instances. Task failures are automatically
  recorded as searchable memory ("learn from errors").
- **Multi-agent orchestration** — `divisi orchestrate "<goal>" --agents
  a,b,c [--worktree]` runs several agents in sequence on one goal, sharing
  one git worktree and handing each agent the previous one's real
  captured output. A sequential relay, not live parallel chat — see
  `docs/architecture.md` for the honest scope.
- **Provider presets** — OpenAI, Anthropic, OpenCode Zen, and NVIDIA,
  configurable from the TUI's Providers tab (`[a]`, masked key entry
  straight to the OS keychain) or `divisi provider add-preset <name>`.
- **Richer starter registries** — the default MCP/LSP/tool catalogs ship
  with real, commonly-used entries (fetch, sequential-thinking,
  rust-analyzer, pyright, docker, gh, ...) instead of a near-empty list,
  seeded from this project's own verified working configuration, plus
  preset catalogs (`divisi mcp/lsp presets`, `add-preset <name>`) to grow
  either registry without a code change.
- **Self-update** — `divisi update` checks GitHub Releases and replaces
  its own binaries in place, on a `stable` (tagged releases) or `nightly`
  (rebuilt on every push to `main`) channel.
- **Plugin management** — `divisi plugin add/remove/list/inspect/sync`:
  installs a named plugin via each agent's own real command (`claude
  plugin install`, `codex plugin add`, `opencode plugin <module>`, `agy
  plugin install`). Installation only — no marketplace browsing/discovery.
- **LSP sync into OpenCode** — `divisi install-integrations` now writes
  the LSP registry into `opencode.jsonc`'s real `lsp` key alongside MCP
  (the only agent with a confirmed native LSP config surface).
- **Skills synced into Claude Code** — `divisi skill sync-claude <name>`
  copies a locally-installed skill into Claude's real skill directory
  (`~/.claude/skills/<name>/`), backing up any existing same-named
  directory first.
- **Account labels, status, and concurrent multi-account execution** —
  captured account profiles can carry a human label and a manually-set
  status (`available`/`rate_limited`/`needs_topup`/`unknown` — never
  auto-detected, since no agent exposes a verified quota API). `single
  task run --account <name>` runs against a materialized, isolated
  `$HOME` for that account instead of swapping the live one in place, so
  multiple accounts of the same agent (two `claude`, three `codex`, ...)
  can run **concurrently** without clobbering each other.
- **TUI: full config surface + task creation** — LSP/Plugins/Tools tabs
  alongside the original Agents/Tasks/MCP/Providers/Accounts/Memory/Help;
  a quick-add flow for MCP/LSP/Plugins/Tools, remove/toggle/sync
  keybindings, and an in-TUI task-creation flow (description → workspace
  path → pick one or more agents).
- **`--real-home` for system-configuration tasks** — `divisi task run
  --agent claude --real-home "set up my dotfiles, install my usual
  tools, make this look nice"` runs against your actual `$HOME`, not the
  isolated sandbox every other task uses. Off by default (prints a
  warning when used) since it gives the agent real credentials/file
  access — for the one legitimate case where that's the point: using an
  agent through `divisi` to actually configure your machine. Also a
  `[g]` toggle in the TUI's task-creation flow.
- **Live task output in the TUI** — press `Enter` on any row in the Tasks
  tab to see that task's real output, tailed as it's produced while the
  task is still running (auto-refreshing) and switching to the full
  final output once it finishes. Since an `orchestrate` run creates a
  separate task row per agent per step, this is how you watch each agent
  in a multi-agent run individually.
- **Isolated agent homes + `divisi agent login`** — every agent runs
  against a divisi-managed home under `~/.config/divisi/homes/<agent>/`
  (bootstrapped from the real one exactly once), never the real, ambient
  `~/.claude`/`~/.codex`/etc. after that. `divisi agent login <name>` runs
  that agent's own real interactive login command (`claude auth login`,
  `codex login`, `opencode auth login`, `pplx auth login`,
  `cursor-agent login`, `goose configure`) attached to your terminal so
  credentials land in the isolated home directly.
- **Cursor CLI, Aider, and Goose** — three more built-in agents.
  Cursor gets full parity with claude/codex/opencode (MCP sync into
  `~/.cursor/mcp.json`, non-interactive runs, login); Goose gets MCP sync
  into its YAML config (`~/.config/goose/config.yaml`) plus non-interactive
  runs and its `configure` wizard wired as login; Aider gets
  non-interactive runs only — it has no MCP support and authenticates via
  API-key flags/env vars, not an interactive login.
- **GitHub Copilot CLI, Kiro CLI, and Cody** — 11 of the current 24 built-in agents; the registry has since grown to also include qwen-code, amp, openhands, droid, codebuff, plandex, continue-cli, grok, mistral-vibe, crush, kilocode, and `single-pool`/`single-agent` (divisi's own native, MCP-only agents — see "Coordinator, goals, and the free-provider pool" below).
  Copilot gets full parity too (MCP sync into `~/.copilot/mcp-config.json`,
  non-interactive runs, login, plugin install). Kiro gets non-interactive
  runs and login (both confirmed by running it directly), but MCP stays
  unsupported since its real `mcp add` command requires being logged in
  to run, and this project won't authenticate a real account just to
  check a file format. Cody was the one agent verified from vendor docs
  alone (not installed on the reference machine) — non-interactive runs
  and login only. **Windsurf was investigated and left out**: there's no
  standalone Windsurf agent CLI anymore (it was folded into Devin
  Desktop) — see `docs/install-methods.md` for the full reasoning.

## Coordinator, goals, and the free-provider pool

Everything above is the config/registry layer. On top of it, the
**Coordinator** (`divisi goal`/`divisi coordinator`) is a second, higher
level of the tool: submit one goal in plain text, and it plans a real
dependency graph (`code`/`test`/`research`/`review`/`docs`/`infra`
nodes), dispatches each ready node to whichever agent fits, runs
independent nodes in parallel (each in its own git worktree when the node
kind calls for isolation), retries around failures and rate limits, and
supervises the result — auto-continuing on success, queuing a
human-confirmed merge when it isn't sure, per `--mode auto/plan/careful/
dry`. `divisi goal status <id>` shows the graph node-by-node; the TUI's
**Goals** tab and `divisi coordinator status` show everything running/
queued/blocked at once.

Two more pieces plug into this:

- **The free-provider pool (E28)** — `single-pool` is a built-in agent
  that never shells a CLI at all: it picks a `(provider, model, key)` via
  a Thompson-sampling bandit over a vendored ~44-provider free-LLM
  catalog (`divisi provider list-free`) and dispatches straight to that
  provider's HTTP API, benching whatever's rate-limited/failing and
  retrying the next candidate before you ever see a failure. `single
  provider add-free <id>` keys a provider; `divisi provider validate`
  re-probes existing keys on demand; `divisi provider key-status` shows
  keyed/valid/cooldown/headroom per provider — real, live state, not a
  placeholder.
- **A native Zed ACP bridge (`divisi acp`)** — a stdio [Agent Client
  Protocol](https://agentclientprotocol.com) server: every prompt from
  Zed's agent panel becomes a coordinator goal, with progress streamed
  back as it runs. `/goals` in the panel shows active goals plus recent
  failures by default (`/goals all` for the full history); `/status`
  folds in provider auth/exhaustion state since Zed has no native
  status-bar API for that.

## What's not (yet)

A real text-to-vector embeddings pipeline (Qdrant integration
stores/searches vectors you already have), LSP syncing into agents other
than OpenCode, a plugin marketplace/discovery layer (installing a named
plugin is real; browsing what's available is not), and full generic
model/provider abstraction beyond the free-pool's own dispatch (live
model-catalog discovery per provider — each free-pool provider is
currently treated as offering one nominal model) are later work. The
Coordinator's parallel task graph and `divisi-mcp`'s permission
enforcement, both listed as future work in earlier revisions of this
README, are real and shipped — see "Coordinator, goals, and the
free-provider pool" above and `divisi-core::preferences`/`permissions`.
See `docs/architecture.md`'s "Not in Phase 1-6" section for
the full, honest list.

## Development

```bash
cargo build --workspace
cargo test --workspace
```

No API keys are required to build, test, or run `doctor`/`agent list`.
`divisi setup --yes`, `divisi install-integrations --yes`, `divisi account
use`, and `divisi provider sync --yes` touch real files/run real
installers — everything else is read-only or operates in a temp directory
during tests. The Redis and Qdrant backends are optional (unset
`DIVISI_REDIS_URL`/`DIVISI_QDRANT_URL` and their commands just report "not
configured"); their tests skip cleanly, rather than fail, when no such
service is reachable — to actually exercise them locally:

```bash
docker run -d -p 6379:6379 redis:7-alpine
docker run -d -p 6333:6333 qdrant/qdrant
DIVISI_REDIS_URL=redis://127.0.0.1:6379 DIVISI_QDRANT_URL=http://127.0.0.1:6333 cargo test --workspace
```

## Documentation

- [`docs/architecture.md`](docs/architecture.md) — crate layout, data flow, and every phase's honest scope/limitations.
- [`docs/adr/0001-tech-stack.md`](docs/adr/0001-tech-stack.md) — why Rust/Unix-socket/SQLite, and what was considered instead.
- [`docs/install-methods.md`](docs/install-methods.md) — verified install commands and sources for every agent CLI.

## License

MIT — see [`LICENSE`](LICENSE).
