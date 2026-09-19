# SingleCLI architecture — Phase 1 through 6 (partial) + auth + memory upgrades

This describes what's actually built, not the full long-term vision (see
the project's original request for that).

- **Phase 1** — foundation: config, registry, adapters, a runtime daemon, a CLI, a minimal TUI dashboard.
- **Phase 2** — shared-capability registries: MCP CRUD, an LSP registry, a tool metadata registry, an OS-keychain secrets abstraction, a local skills directory.
- **Phase 3** — a SQLite-backed scoped memory subsystem and a git/project context resolver.
- **Phase 4** — real single-agent task execution: a task record, git worktree isolation, and actually invoking each agent CLI's non-interactive mode.
- **Phase 5** — declarative custom agent adapters (`~/.config/single/agents/*.toml`): a new CLI agent gets real detection, MCP sync, and task execution without recompiling SingleCLI.
- **Phase 6 (partial)** — a provider registry (OpenAI, Anthropic, ...) syncing API keys into the two agents with a verified config slot for them.
- **Auth** — multi-account credential switching (`single account ...`) so one agent CLI (e.g. Claude Code) can have several logged-in accounts, swapped safely.
- **Memory upgrades** — a SQLite knowledge graph (entities/observations/relations), plus optional Redis (working memory) and Qdrant (vector store) backends.
- **Distribution** — a cross-platform release workflow (Linux/macOS × x86_64/arm64) and a `curl | sh` installer (`install.sh`), plus a tabbed TUI with in-app interactive agent-install and provider-add flows.
- **Growth** — richer default MCP/LSP/tool registries seeded from this project's own verified real configuration, provider presets (OpenAI, Anthropic, OpenCode Zen, NVIDIA), automatic "learn from errors" memory on task failure, and a sequential multi-agent orchestration relay (`single orchestrate`).
- **Self-update** — `single update` checks GitHub Releases and replaces its own binaries in place; a `stable` channel (tagged `vX.Y.Z` releases) and a rolling `nightly` channel that tracks every push to `main`.
- **Growth Phase 2** — a plugin registry synced into every agent with a real plugin-install command (`claude`/`codex`/`opencode`/`agy`); LSP sync into OpenCode's real `opencode.jsonc` `lsp` key; skills synced into Claude Code's real skill directory; account profiles gained a human label and a manually-tracked usability status, plus isolated-`$HOME` materialization so multiple accounts of the same agent can run **concurrently**; MCP/LSP preset catalogs for growing the registries without a code change per entry; an expanded tool registry; and a TUI that covers the full config surface (MCP/LSP/Plugins/Tools tabs, in-app task creation).
- **Isolation** — SingleCLI stopped reading/writing agents' real, ambient config (`~/.claude.json`, `~/.codex/`, `~/.config/opencode/`) on every run. Every agent now gets a SingleCLI-managed home under `~/.config/single/homes/<agent>/`, bootstrapped from the real one **once**; every subsequent `task run`, `install-integrations`, `plugin sync`, `provider sync`, and `account capture`/`use` operates only inside that isolated copy.

A parallel/live multi-agent task-graph (as opposed to the sequential relay
that exists), full provider abstraction (model discovery, streaming, usage
accounting), and permission *enforcement* are still out of scope — see
"Not in Phase 1-6" below.

```text
single (CLI, no subcommand)          single <command>
        │                                    │
        ▼                                    │
  ensure daemon running                      │
        │                                    │
        ▼                                    ▼
   divisi-tui  ────────────────►  Unix socket (runtime.sock)
                                             │
                                    divisid (daemon)
                                             │
                    ┌────────────────────────┼────────────────────────┐
                    ▼                        ▼                        ▼
              divisi-core              divisi-agent-sdk          state.rs
        (config, profiles,           (discover, configure_mcp,     (SQLite
         agent registry,              per-agent format writers)    event log)
         MCP registry)                        │
                                    ┌──────────┼──────────┬─────────────┐
                                    ▼          ▼          ▼             ▼
                                 claude      codex     opencode    agy / pplx
                              (~/.claude.json) (config.toml) (opencode.jsonc) (no verified
                                                                              config location)
```

## Crates

- **`divisi-protocol`** — wire types shared by every other crate:
  `Request`/`Response`, `AgentInfo`, `McpServerSpec`, `CapabilityFlags`,
  `InstallMethod`/`BootstrapInstall`. No I/O, no logic — just the shapes
  both sides of the IPC boundary agree on.
- **`divisi-core`** — configuration precedence (global → profile →
  project, see `config.rs`), the built-in agent registry (`registry.rs`),
  the unified MCP registry (`mcp.rs`), profile switching (`profile.rs`),
  and the canonical `~/.config/single/` directory layout (`paths.rs`).
  Pure data/logic, no process spawning.
- **`divisi-agent-sdk`** — real detection (`discover.rs`, shells out to
  `which`/`<cmd> --version`) and per-agent MCP config writers
  (`formats/{claude,codex,opencode}.rs`), each format independently
  verified against a real config file from the reference machine. Backups
  before every write (`backup.rs`). `AgentAdapter` (`adapter.rs`) is the
  seam Phase 4 extends with process lifecycle (start/stop/stream) without
  reshaping what's here.
- **`divisi-runtime`** — the headless daemon: `context.rs` loads config +
  registry per request, `handlers.rs` dispatches `Request` → `Response`,
  `doctor.rs`/`bootstrap.rs`/`integrations.rs` implement the corresponding
  CLI commands, `server.rs` is the actual Unix-socket accept loop
  (`bin/divisid.rs` is the binary entry point), `state.rs` is the
  SQLite event log.
- **`divisi-cli`** (binary name `single`) — argument parsing (`clap`),
  talks to the runtime via `client.rs` (socket first, in-process fallback
  if no daemon is running — see ADR 0001), spawns the daemon for the TUI
  path (`daemon.rs`), and renders responses as text or `--json`
  (`render.rs`).
- **`divisi-tui`** — the dashboard (`dashboard.rs`, `ratatui`/`crossterm`),
  which insists on real socket IPC (`client.rs`) rather than calling into
  the runtime in-process, so it's a genuine second client of the same
  daemon the CLI talks to.

## Investigation notes (why the config formats above are what they are)

Before writing any adapter code, the real config files already present on
the reference machine were read directly, not assumed:

- `~/.claude.json` — top-level JSON object; `mcpServers` is a map of
  `{ type: "stdio", command, args, env }`. `~/.claude/settings.json`
  separately holds model/plugin/theme settings.
- `~/.codex/config.toml` — `[mcp_servers.<name>]` TOML tables with
  `command`, `args`, and an optional `[mcp_servers.<name>.env]` sub-table.
- `~/.config/opencode/opencode.jsonc` — JSON-with-comments; `mcp` is a map
  of `{ type: "local", command: [...] (array, not a string), environment,
  enabled }`; `lsp` is a sibling top-level key with the same shape idea.
- `agy` (Antigravity) has no on-disk config directory that could be found
  on this machine — its adapter shells out to `agy` subcommands
  (`agy agents`, `agy install`) for anything it needs, rather than assuming
  a config file location that was never actually observed.

See `docs/install-methods.md` for the install-command side of this same
investigation.

## Known Phase 1 simplifications (adapter emulation, not native capability)

- **OpenCode's JSONC writer strips comments on write.** `formats/opencode.rs`
  parses JSONC by stripping `//`/`/* */` comments (outside string literals)
  before calling `serde_json`, then re-serializes as plain JSON. Any
  comments a user had in `opencode.jsonc` are lost on the first
  `single install-integrations` run. A backup is always taken first
  (`backup_before_write`), so nothing is unrecoverable, but this is
  explicitly a lossy emulation, not a native capability — a
  format-preserving JSONC editor is the fix if this becomes a problem in
  practice.
- **Codex's TOML writer reformats the whole file.** Same tradeoff as
  above: `toml::Table` round-trips values but not comments/formatting.
- **No persistent multi-connection daemon lifecycle commands.** There's no
  `single daemon start/stop/status` yet. `divisid` is started
  on-demand by the TUI (`daemon.rs::ensure_running`) and otherwise callers
  either connect to an already-running one or fall back to an in-process
  call. This is a deliberate Phase 1 scope cut, not an oversight — see ADR
  0001's "Consequences" section.
- **`agy` and `perplexity` (`pplx`) have no MCP integration.** Neither has
  a verified on-disk MCP config location (`agy`) or an MCP-capable surface
  at all (`pplx` is a Search API client, not a coding agent — see
  `docs/install-methods.md`). Their `configure_mcp`/`remove_mcp` are
  documented no-ops, not silently-skipped failures.

## Phase 2 additions

- **`divisi-core::mcp`** gained CRUD (`add`/`remove`/`set_enabled`/`find`)
  on top of Phase 1's `load`/`save`, exposed as `single mcp
  add/remove/enable/disable/inspect`.
- **`divisi-core::lsp`** — a registry mirroring `mcp.rs`'s shape
  (`~/.config/single/lsp.toml`), exposed as `single lsp
  list/add/remove/inspect`. **Agent sync exists for OpenCode**
  (`AgentAdapter::configure_lsp`/`remove_lsp`, wired into `single
  install-integrations`/`uninstall-integrations` alongside MCP): it writes
  into `opencode.jsonc`'s real `lsp` key (`{"command": [...]}`, keyed by
  name), skipping disabled entries since no per-entry enable field is
  confirmed there. Claude Code's LSP support is still a marketplace
  *plugin* mechanism (`enabledPlugins` in `~/.claude/settings.json`), a
  fundamentally different shape from "register an arbitrary command," so
  `configure_lsp` stays the trait's default "unsupported" for claude/
  codex/agy/perplexity rather than guessing a translation. See "Growth
  Phase 2 additions" below for the preset catalog on top of this registry.
- **`divisi-core::tools`** — a metadata catalog
  (`~/.config/single/tools.toml`: name, description, risk level, enabled),
  exposed as `single tool list/add/inspect/enable/disable`. Deliberately
  metadata-only: there is no execution engine yet to actually invoke a tool
  on an agent's behalf (that's the Phase 4 orchestrator), so this doesn't
  pretend tools are wired into any agent today.
- **`divisi-core::secrets`** — an OS-keychain-backed secret store
  (`SecretStore` trait, `SecretTool` impl using `secret-tool`/libsecret on
  Linux — the same mechanism this machine's own existing MCP configs
  already rely on), exposed as `single secret list/set/get/delete`. Values
  never pass through `divisi-runtime`'s SQLite event log. macOS Keychain /
  Windows Credential Manager backends are unimplemented; `SecretStore` is
  the seam for them.
- **`divisi-core::permissions`** — a `deny`/`ask`/`allow` rule model with
  longest-prefix-match evaluation (`~/.config/single/permissions.toml`).
  **Not exposed via CLI or IPC** — nothing in SingleCLI executes a tool or
  agent action on the user's behalf yet, so a `single permission allow ...`
  command would have no enforcement behind it. This stays a library-only
  seam (with its own unit tests) until the Phase 4 orchestrator is a real
  caller; shipping the CLI surface first would be exactly the "fake
  integration" spec section 52 rules out.
- **`divisi-core::skills`** — local directory-based skills under
  `~/.config/single/skills/<name>/`, exposed as `single skill
  list/install/remove/inspect`. `install` copies a local source directory
  in; there's no network/marketplace fetch and SingleCLI doesn't interpret
  a skill's contents. **`single skill sync-claude <name>`** (added in
  Growth Phase 2, `divisi_core::skills::sync_to_claude`) copies a skill
  into Claude Code's real skill directory (`~/.claude/skills/<name>/`,
  confirmed via `claude plugin init --help`'s own scaffold-path output),
  backing up any pre-existing same-named directory first. Other agents'
  skill mechanisms remain untranslated.

## Phase 3 additions

- **`divisi-runtime::memory`** — SQLite-backed structured memory
  (`~/.config/single/state/divisi.db`, `memories` table), scoped
  (working/project/user/agent/task/long_term/knowledge) and provenance-
  tagged (`MemorySource`: user_instruction/agent_output/tool_output/
  project_content/external_content — spec sections 46-47's trust
  classification). Exposed as `single memory store/search/get/delete/list`.
  `search` is SQLite `LIKE` substring matching, **not semantic/embedding
  search** — that needs a configurable embedding provider, which doesn't
  exist until Phase 6's provider abstraction. Nothing auto-promotes agent
  output into this store; every write is an explicit, caller-tagged call,
  since there's no orchestrator yet that could do such promotion
  responsibly (or irresponsibly).
- **`divisi-core::project_context`** — resolves git state (repo root,
  branch, changed files via real `git` subprocess calls) and finds project
  documentation files (README/CLAUDE.md/AGENTS.md/CONTRIBUTING.md) for a
  given directory. Exposed as `single context [cwd]`. This is an *ambient
  snapshot*, not the full spec section 10 picture: "relevant source
  files," "relevant memory," and "previous agent actions" require
  relevance-ranking against an actual task, which needs the Phase 4
  orchestrator to have a task to resolve context for — that selection
  logic doesn't exist yet.

## Phase 4 additions

- **`divisi-agent-sdk::adapter::run_prompt`** — a real, blocking,
  non-interactive invocation of each agent's own CLI: `claude -p`, `codex
  exec`, `opencode run --dir`, `agy -p` (each flag confirmed against that
  CLI's own `--help` on the reference machine — see the doc comments on
  each `impl AgentAdapter`). No built-in `std::process` timeout exists, so
  `divisi-agent-sdk::run::run_command` polls `try_wait` and kills the
  child past a deadline. `perplexity` keeps the trait's default
  "unsupported" response — `pplx` isn't a coding agent (see
  `docs/install-methods.md`).
- **`divisi-core::worktree`** — real `git worktree add`/`remove`/`list`
  subprocess calls, tested against real temporary git repos (not mocked).
  One worktree per task, branch named `single/task-<id>`.
- **`divisi-runtime::task`** — a `tasks` SQLite table and a synchronous
  `run()` that ties the above together: creates the task record,
  optionally isolates it in a fresh worktree, invokes the agent, captures
  stdout+stderr as an artifact file under
  `~/.config/single/state/artifacts/`, and records the final status.
  Exposed as `single task run/list/inspect`. Every stage also writes to
  the existing generic `events` table (`task.created`/`task.started`/
  `task.completed`/`task.failed`) so a run is auditable per spec section 32,
  even without a live event *stream*.
- **Client-side fix**: `divisi-cli`'s socket client used to apply a flat
  5-second read timeout to every request, including `task run`, which can
  legitimately take minutes. On a timeout it fell back to the in-process
  path — which, for a request *already sent to a live daemon*, would have
  silently re-run the same task a second time. Fixed by only allowing
  fallback before a request is written to the socket; once sent, the
  client waits it out rather than risking a duplicate side-effecting run.
  (`crates/divisi-cli/src/client.rs`)

**What Phase 4 is honestly not**: there is no task *graph* (DAG), no
automatic agent selection, no parallel multi-agent coordination, and no
background/cancellable execution — `single task run` blocks the calling
request until the agent finishes or its timeout fires. Those need the
runtime to hold live process state across multiple requests (deferred
since ADR 0001) or a reasoning step to pick agents (spec section 20) that
SingleCLI itself doesn't perform. What's built is real for the one-task,
one-agent case: a real subprocess, real git isolation, a real captured
artifact, a real persisted record.

## Phase 5 additions: declarative custom agents

- **`divisi-core::custom_agents`** — a TOML schema
  (`~/.config/single/agents/<name>.toml`: `command`, `[install]`,
  `[run]` mode/value, `[mcp]` format/config_path/key_path) describing a
  new agent CLI without writing Rust.
- **`divisi-agent-sdk::GenericAdapter`** — interprets that TOML as a real
  `AgentAdapter`: detection via `discover()`, MCP sync for the two proven
  shapes already used by the built-in adapters (`json_flat` = claude's
  flat `mcpServers` object, `toml_flat` = codex's flat `[mcp_servers.x]`
  tables, generalized to a configurable path/key), and prompt invocation
  via a flag or subcommand. A format the two options don't cover is
  refused (`mcp.format = "unsupported"`), not guessed at.
- `for_agent_with_custom` is the single lookup every call site (doctor,
  bootstrap, integrations, task, handlers) now goes through, so a custom
  agent gets identical treatment to the five built-in ones everywhere.
  Verified end-to-end with a real fake CLI script: appeared in `agent
  list`/`inspect` with live detection, received real MCP config sync, and
  completed a real `task run`.

## Phase 6 additions (partial): provider registry

- **`divisi-core::providers`** — metadata only (`providers.toml`: name,
  env var name, base URL); the actual key lives in the OS keychain via
  `divisi-core::secrets`, referenced by name.
- **`divisi-agent-sdk::provider_sync`** — writes a provider's key into
  only the two *verified* real config locations: Claude Code's
  `~/.claude/settings.json` `env` object (a documented mechanism), and
  Codex's `~/.codex/auth.json` `OPENAI_API_KEY` field (the only
  provider-key slot that file actually has — any other env var name for
  codex is refused, not guessed). Every other agent is refused with a
  reason. This is the metadata/key-distribution layer of spec section 30,
  not the full provider abstraction (no model discovery, streaming, or
  usage accounting across providers).

## Auth: multi-account credential switching

- **`divisi-core::account`** — capture the *current* live login state of
  an agent into a named profile, then switch between profiles later (the
  "two Claude Code accounts" use case). Real, verified storage per agent:
  - **claude**: full `~/.claude/.credentials.json` swap, plus a surgical
    merge of only `oauthAccount`/`userID` in `~/.claude.json` — the rest
    of that file (MCP servers, settings, everything else) is provably
    untouched by the switch (see the test asserting an unrelated field
    survives).
  - **codex**: full `~/.codex/auth.json` swap (a pure auth file).
  - **agy**: best-effort — captures `~/.gemini/antigravity-cli/
    {jetski_state.pbtxt,settings.json}` (both mode 600, i.e. treated as
    sensitive by the CLI itself), flagged `unverified_complete` since the
    full login-state surface wasn't confirmed.
  - **opencode, perplexity**: explicitly unsupported with a stated reason
    (opencode's login state lives in a live multi-table SQLite database
    shared with session history — too risky to snapshot at the file
    level; perplexity isn't a coding agent) rather than a fabricated or
    risky implementation.
  - Every snapshot file is `0600`; every live-state write is backed up
    first; no token contents are ever printed, logged, or recorded in the
    event log — only agent/profile names and timestamps.
  - **Growth Phase 2**: profiles gained an optional human `label` (`single
    account capture ... --label you@email.com`) and a manually-set
    `AccountStatus` (`available`/`rate_limited`/`needs_topup`/`unknown`
    via `single account set-status`) — never auto-detected, since no
    agent exposes a verified quota/rate-limit API; SingleCLI just
    remembers what it's told. `divisi_core::account::ensure_isolated_home`
    materializes a per-account `$HOME` (copies the real home's
    non-credential config once, then overlays that account's captured
    credentials), so `single task run --account <name>` can run **multiple
    accounts of the same agent concurrently** (e.g. two `claude`, three
    `codex`) without any of them clobbering another's live login state —
    the `AgentAdapter::run_prompt`/`divisi-agent-sdk::run::run_command`
    plumbing takes an optional `$HOME` override for exactly this.
  - **No real-home fallback.** `is_authenticated`/`capture` used to also
    check the real, ambient `$HOME` and treat a login found only there as
    "authenticated" (falling back and syncing it into the isolated home on
    capture) — a login done via the vendor CLI directly, outside `single
    agent login`, would silently count. That fallback is gone:
    `is_authenticated` and `capture` now read only SingleCLI's isolated
    home; a real-home-only login is invisible until you `single agent
    login <agent>` again inside the isolated home. `agent_home`'s
    one-time bootstrap copy (below) was narrowed to match — it no longer
    seeds credential files.

## Memory upgrades: knowledge graph, Redis, Qdrant

- **`divisi-runtime::knowledge_graph`** — entities with accumulated
  observations plus typed relations, in the same SQLite database as
  everything else. Mirrors the entity/observation/relation shape of
  `@modelcontextprotocol/server-memory`, a proven convention already
  configured in this project's own real MCP setup, rather than inventing
  a new graph schema. Cascading deletes, idempotent entity creation,
  substring query, full-graph dump. `single memory graph ...`. Writing
  stays manual (no auto-promotion of task output into entities, to avoid
  guessing at entity-naming heuristics), but as of v0.1.17 reading is
  automatic: `task::build_context_preamble` queries it for entities
  relevant to a task's description and injects them alongside memory and
  notes, the same shared-blackboard treatment those two already got.
- **`divisi-runtime::redis_backend`** — optional (`SINGLE_REDIS_URL`)
  TTL-capable key/value working memory: genuinely useful as fast shared
  state *while several agent processes are concurrently running*, a role
  SQLite's request-scoped connections don't fill well. Built and tested
  against a real local Redis container; unit tests skip (not fail) when
  no Redis is reachable. `single memory cache ...`.
- **`divisi-runtime::qdrant_backend`** — optional (`SINGLE_QDRANT_URL`)
  vector store: upsert/search/delete over Qdrant's real REST API, whose
  shape was captured directly from a running local Qdrant instance during
  development (not assumed from documentation). This module itself stores
  and searches *pre-computed* vectors — `single memory vector
  upsert/search` still take one directly. `divisi-runtime::embeddings`
  closes the text→vector gap for the memory-entry path specifically: a
  real call to OpenAI's `/v1/embeddings` (API key via `single secret set
  embeddings:api_key <key>`), wired into `MemoryStore` (best-effort
  auto-embed on write into a `single_memory` collection) and
  `MemorySearchSemantic` (`single memory search --semantic`, embeds the
  query and searches Qdrant, falling back to substring search if either
  the key or `SINGLE_QDRANT_URL` isn't configured) — see `handlers.rs`.

## Distribution: release workflow, installer, and the TUI rewrite

- **`.github/workflows/release.yml`** — on a `v*` tag push, builds
  `single`+`divisid` for linux-x86_64, linux-arm64 (native
  `ubuntu-24.04-arm` runner, no cross-compilation toolchain needed),
  macos-arm64, and macos-x86_64, packages each as a tar.gz, and publishes
  them to a GitHub Release. Verified for real: tagged `v0.1.0`, pushed,
  and watched the run — all four matrix jobs succeeded.
- **`install.sh`** — a POSIX shell script (`curl -fsSL .../install.sh |
  sh`) that detects OS/arch, downloads the matching release tarball,
  installs both binaries to `~/.local/bin` (override with
  `SINGLE_INSTALL_DIR`), and prints PATH guidance for bash/zsh/fish.
  Unsupported platforms are told to build from source rather than
  silently failing.
- **`divisi-tui`** was rewritten from a single read-only table into a
  tabbed control center: **Agents / Tasks / MCP / Providers / Accounts /
  Memory / Help**, each rendering live data fetched over the same runtime
  socket the CLI uses (`app.rs` owns all fetched state; `ui.rs` is pure
  rendering). Keyboard nav: `Tab`/`Shift+Tab` between tabs, `↑↓`/`j`/`k`
  within a tab, `r` to refresh, `q`/`Esc` to quit.
- **In-TUI agent install** (the flagship ask: "for agent install let's
  make them inside the tui, like claude code"): pressing `i` on a
  not-yet-installed agent in the Agents tab opens a confirmation modal
  showing the *exact* real bootstrap command and its source — never
  installs silently. Confirming (`y`) runs the real install
  (`divisi-runtime::bootstrap::run_one`, extracted from the existing
  `single setup` logic so both paths share one implementation) on a
  background OS thread via an `mpsc` channel, so the UI keeps redrawing
  a live elapsed-time spinner instead of freezing for however long the
  network install script takes, then shows a real success/failure result
  pulled from the actual command's exit status. Verified end-to-end in a
  real terminal session (tmux + a fake agent CLI with no real binary):
  confirm → running spinner → real completion, then the newly "installed"
  agent's live status updated on the next refresh.

## Growth: richer defaults, providers, error-learning, orchestration

- **Richer registry defaults.** `mcp.rs::default_servers()` grew from
  git+memory to 8 entries (fetch, sequential-thinking auto-enabled;
  filesystem/github/playwright/chrome-devtools present but disabled until
  scoped/secreted), `lsp.rs::default_servers()` and
  `tools.rs::default_tools()` went from empty to real starter catalogs.
  Every entry is a package this project has directly observed running —
  either in its own real MCP config or as one of the four LSP plugins
  behind `~/.claude/settings.json`'s `enabledPlugins` — not an arbitrary
  or fabricated list.
- **Provider presets.** `divisi-core::providers::presets()` — OpenAI,
  Anthropic, OpenCode Zen, NVIDIA — each base URL/env var pair
  independently verified against the vendor's own current docs (NVIDIA:
  `https://integrate.api.nvidia.com/v1` / `NVIDIA_API_KEY`, confirmed via
  build.nvidia.com's own OpenAI-compatible endpoint documentation;
  OpenCode Zen: `https://opencode.ai/zen/v1` / `OPENCODE_API_KEY`, per
  opencode.ai/docs/providers). Configurable from the TUI (Providers tab,
  `[a]`) or `single provider add-preset <name>`.
- **Learn from errors.** Every task failure path in `task.rs` (worktree
  setup failure, agent run failure, non-zero exit/timeout) now also
  writes a project-scoped, `tool_output`-sourced memory entry via
  `remember_failure`, so `single memory search`/`list` surface past
  failures to whoever looks next — human or a future agent run. It's
  best-effort: a memory-write failure never masks the real task failure.
- **`divisi-runtime::orchestrate`** — multi-agent task execution: run
  several agents in sequence on one goal via `single orchestrate "<goal>"
  --agents a,b,c [--worktree]`. Every step is a real `task::run` call
  (identical worktree isolation, artifact capture, and error-learning to
  a standalone task), so this is additive, not a parallel implementation.
  Two real mechanisms make later agents actually build on earlier ones,
  not just watch text scroll by:
  1. **Shared worktree** — when `--worktree` is set, one worktree is
     created for the *whole relay* (not one per step), so agent 2 sees
     agent 1's actual file changes on disk.
  2. **Output hand-off** — each step's prompt is the original goal plus
     the previous step's real captured artifact content (capped at 4000
     chars to avoid unbounded prompt growth over a long relay).

  The relay stops at the first failed step rather than handing broken/
  missing output forward. Verified end-to-end with real `claude`→`codex`
  runs: a plain-text goal relayed and both agents produced the expected
  output, and a file-creation goal correctly demonstrated a real, honest
  limit — both agents refused the write under their own default
  non-interactive safety/approval policies, since neither `task::run` nor
  `orchestrate` auto-injects an agent's own permission-bypass flags (e.g.
  Claude's `--dangerously-skip-permissions`, Codex's approval-policy
  flags). SingleCLI treats that as the agent's decision to make, not one
  to silently override — a user who wants that needs to configure it
  through the agent's own trust mechanism.

  **Honest scope**: this is a sequential relay, not the full spec's
  parallel task-graph/DAG with live bidirectional agent messaging — see
  the module doc comment on `orchestrate.rs` for why (no long-lived
  cross-request process state yet, and none of the five CLIs speak a
  shared live inter-agent protocol to begin with).

## Self-update

`crates/divisi-cli/src/update.rs` — `single update [--channel
stable|nightly] [--check] [--yes]`, mirroring the real `claude update`/
`codex update` commands this project already investigated:

- **stable**: GitHub's `/releases/latest` endpoint (`vX.Y.Z` tags, e.g.
  this project's own `v0.1.1`), with real `major.minor.patch` comparison
  against the running binary's compiled-in version
  (`env!("CARGO_PKG_VERSION")`).
- **nightly**: a single rolling pre-release tagged `nightly`, rebuilt and
  republished by `.github/workflows/nightly.yml` on every push to `main`
  (delete-then-recreate the tag so each push cleanly replaces the last
  rather than colliding on asset names). There's no meaningful semver to
  diff for a rolling tag, so this channel is always reported as "an
  update is available" rather than silently claiming it's current.
- Applying an update downloads the same `singlecli-<target>.tar.gz` asset
  shape `install.sh`/`release.yml` already use, extracts it, and
  atomically replaces `single`/`divisid` next to whichever binary
  is currently running (`std::env::current_exe()`'s directory) — not a
  fixed path, so it works whether the CLI was installed via `install.sh`,
  built from source, or copied somewhere custom.
- Verified for real, twice: `single update --check` against the actual
  published `v0.1.1` release correctly reported "already up to date";
  and, with a deliberately older test build, `single update --yes`
  downloaded the real release asset and replaced the binary in place —
  confirmed by the file's hash changing and the updated binary still
  running correctly afterward.

## Growth Phase 2 additions

- **`divisi-core::plugins`** — a plugin registry (`plugins.toml`: `name`,
  `target` — the `plugin[@marketplace]` selector used verbatim by
  claude/codex/agy — and an optional `opencode_module`, since OpenCode's
  real `opencode plugin <module>` command addresses plugins by plain npm
  module name, a genuinely different scheme). `single plugin
  add/remove/list/inspect` manage the registry; `single plugin sync <name>
  --agents ... [--yes]` actually installs it via each agent's own real
  command (`AgentAdapter::install_plugin`: `claude plugin install`, `codex
  plugin add`, `opencode plugin <module>`, `agy plugin install` — each
  confirmed against that CLI's own `--help`). This is plugin
  *installation*, not a marketplace browser — SingleCLI doesn't discover
  or search available plugins, it installs a target you already know
  by name.
- **MCP/LSP preset catalogs** — `divisi-core::mcp::presets()` and
  `divisi-core::lsp::presets()` mirror the provider-presets pattern: a
  named starter config not yet in the user's registry, opted into one at a
  time (`single mcp/lsp add-preset <name>`) instead of needing a code
  change per entry. Every LSP preset's command/flags were confirmed via
  that binary's own `--help` on the reference machine (`clangd`,
  `bash-language-server`, `yaml-language-server`, `terraform-ls`,
  `vscode-json-language-server`, on top of the five defaults); every MCP
  preset's npm package was confirmed to resolve a real published version
  via `npm view <package> version` (`brave-search`, `slack`, `puppeteer`,
  `postgres`) — all four ship disabled since each needs a secret or drives
  something invasive, same posture as `github`/`playwright` in the
  original defaults.
- **Expanded tool registry** — `tools.rs::default_tools()` grew from 7 to
  26 entries (ripgrep, fd, jq, make, compilers/build tools per language,
  package managers, `ssh`, `kubectl`/`helm`/`ansible`, the three major
  cloud CLIs, `tmux`, `vim`), each confirmed present on the reference
  machine (`command -v`) before being added, with risk levels reflecting
  what the tool can actually reach (cloud/cluster/remote-shell tools are
  `High`).
- **TUI: full config surface.** Three new tabs (**LSP / Plugins / Tools**,
  alongside the existing Agents/Tasks/MCP/Providers/Accounts/Memory/Help)
  render their registries live with selection highlighting. A generic
  "quick add" flow (`[a]`, one pipe-separated line parsed per registry
  type — deliberately a power-user shortcut, not a five-screen wizard, for
  cases needing finer control like MCP env vars the full `single ...
  add` CLI still exists) covers MCP/LSP/Plugins/Tools; `[d]` removes,
  `[e]` toggles enabled/disabled where the registry supports it, `[s]`
  syncs the selected plugin into every registered agent.
- **TUI: task creation.** `[n]` on the Tasks tab opens a three-step flow —
  description, workspace path, then toggle-select one or more agents
  (space to toggle) — that calls `TaskRun` for a single agent or
  `Orchestrate` for more than one, mirroring the same background-thread-
  plus-`mpsc`-channel shape as the existing install/provider-add flows so
  the UI keeps redrawing while the request is in flight.

## Isolation: SingleCLI-managed homes

- **`divisi-core::agent_home`** — every agent gets an isolated `$HOME`
  under `~/.config/single/homes/<agent>/`. The first time it's needed,
  `ensure_bootstrapped` copies that agent's known real config/state paths
  (`~/.claude.json` + `~/.claude/` for claude, `~/.codex/` for codex,
  `~/.config/opencode/` for opencode — the same locations
  `divisi-agent-sdk::formats` already reads/writes) into the isolated
  tree, then immediately strips out `credential_paths_for(agent)` (claude's
  `.claude/.credentials.json`, codex's `.codex/auth.json`) so a freshly
  bootstrapped home starts logged out even if the real home has a live
  session — see the Auth section's "no real-home fallback" note. That's
  the **only** time the real home is touched. Every call after that — even
  on a different day, a different process — finds the isolated home
  already there and leaves it alone; changes never flow back from the real
  home once bootstrapped.
- **What changed.** `single task run`, `single install-integrations`/
  `uninstall-integrations`, `single plugin sync`, `single provider sync`,
  and `single account capture`/`use` used to operate directly against the
  real, ambient `$HOME`. They now all resolve an isolated home first
  (`integrations::home_dir()` — the real one — is only ever passed as the
  *bootstrap source*, never written to). Verified for real: ran `single
  install-integrations --yes` against a fake real home containing a
  `.claude.json` with `numStartups: 42`; afterward that file was
  byte-for-byte unchanged, while `~/.config/single/homes/claude/.claude.json`
  held the newly-synced MCP config, with no credentials copied in.
- **Relationship to account isolation.** `divisi-core::account`'s
  per-account isolated homes (`accounts_dir()/<agent>/<name>/home/`, for
  running several accounts of one agent concurrently — see the Auth
  section above) and `agent_home`'s per-agent default isolated home
  (`homes_dir()/<agent>/`, for "don't touch anything outside SingleCLI")
  are two instances of the same idea at different granularity: `single
  task run --agent codex` with no `--account` uses the default per-agent
  home; adding `--account work` swaps in that named account's own
  isolated home instead. Neither ever reads from or writes to the real
  `$HOME` after its own first bootstrap.
- **Custom agents and `agy`** have no confirmed real config
  path (`agent_home::real_paths_for` returns an empty list for them), so
  their isolated home simply starts empty — SingleCLI has nothing to seed
  it with, and says so rather than guessing a location.
- **`single agent login <name>`** — since agents now run against isolated
  homes rather than the real one, there needs to be a way to actually log
  in *to* that isolated home. This runs the agent's own real interactive
  login command (`claude auth login`, `codex login`, `opencode auth
  login`, `pplx auth login` — each confirmed via that CLI's own `--help`)
  attached directly to the user's terminal (inherited stdin/stdout/stderr,
  no timeout — `divisi-agent-sdk::run::run_interactive_with_home`, not the
  captured/bounded `run_command` every other adapter method uses), with
  `$HOME` overridden to the agent's isolated home so the resulting
  credentials land there. Runs entirely in `divisi-cli`, bypassing the
  daemon socket (same reasoning as `single update`): the daemon may have
  no TTY at all, and login needs the real one. `agy` has no confirmed
  login subcommand, so `AgentAdapter::login` stays the trait's default
  "unsupported" for it rather than guessing one. Verified for real:
  `single agent login claude` correctly bootstrapped the isolated home
  from the reference machine's real `~/.claude` and spawned `claude auth
  login` attached to the terminal (observed via a bounded-timeout run
  with stdin closed, to confirm the command launches without completing
  a live OAuth flow in an unattended check).

## Growth Phase 3: three more built-in agents

- **Cursor CLI (`cursor-agent`)**, **Aider**, and **Goose** joined the
  built-in registry (8 agents total, up from 5), each verified the same
  way as the original five: real config file inspection on the reference
  machine (not vendor docs alone), `--help` output for every flag used,
  and a real install command fetched from the vendor's own current docs.
  - **Cursor**: full parity with claude/codex/opencode — MCP sync into
    `~/.cursor/mcp.json`'s real `mcpServers` map (`formats::cursor`, same
    shape as Claude's but with **no** `"type"` field, since no real entry
    on the reference machine has one), `cursor-agent -p` for
    non-interactive runs, `cursor-agent login` for `single agent login`.
    No plugin install — `cursor-agent plugin` only exposes marketplace
    management (add/list/remove/update a git-hosted marketplace), no
    "install a named plugin" command to wire up.
  - **Goose**: MCP sync into `~/.config/goose/config.yaml`'s real
    `extensions` map — the first **YAML** config format this project
    writes (`formats::goose`, via `serde_yaml`; every other format is
    JSON/JSONC/TOML). `goose run --text ... --no-session --quiet` for
    non-interactive runs. `goose configure` wired as `login`, even though
    it's a general provider/credentials setup wizard rather than a narrow
    OAuth flow — it's the closest real entry point goose has.
  - **Aider**: intentionally thin. No MCP (`aider --help` shows no `mcp`
    subcommand or flag — honest gap, not guessed), no `login` (aider
    authenticates via `--api-key`/`--set-env`/`.env` files, not an
    interactive command there's a terminal session to attach to).
    `aider --message "<prompt>" --yes-always` for non-interactive runs.
  - All three get the same isolated-home treatment as the original five
    (`agent_home::real_paths_for` now also bootstraps `.cursor`,
    `.config/goose`, and `.aider.conf.yml`) — verified end-to-end:
    `single doctor` detected all three, `single install-integrations`
    wrote real MCP config into cursor's and goose's isolated homes
    (correctly shaped JSON/YAML), and `single agent login aider`
    correctly reported "unsupported" instead of guessing a flow.

## Growth Phase 4: Copilot, Kiro, Cody — and why Windsurf isn't here

- **GitHub Copilot CLI** joined as a full built-in agent (11 total): MCP
  sync into `~/.copilot/mcp-config.json` (format confirmed by actually
  running `copilot mcp add` against a throwaway `$HOME` and inspecting
  what it wrote, since no config file existed there beforehand to read
  off directly), `copilot -p ... --allow-all-tools` for non-interactive
  runs (the flag is documented as *required* for `-p` to work at all, not
  an optional permission bypass), `copilot login` for `single agent
  login copilot`, and `copilot plugin install <source>` (same
  `plugin@marketplace` convention as claude/codex/agy/cursor).
- **Kiro CLI** (`kiro-cli`) also turned out to be installed on the
  reference machine, so its `chat`/`mcp`/`login` subcommands and flags
  are confirmed via direct `--help` execution, not docs alone. Its real
  `mcp add` command requires being logged in to actually run, though, so
  its on-disk config format couldn't be inspected without authenticating
  a real account just to check a file shape — `configure_mcp` stays
  honestly unsupported rather than guessed.
- **Cody** (Sourcegraph) was **not** installed on the reference machine —
  the only entry in the registry verified from vendor docs alone
  (`cody auth login --web`, `cody chat -m ...`, sourced directly from
  sourcegraph.com, not run locally). Marked `unverified: true` for this
  reason; no MCP support since Sourcegraph doesn't document any.
- **Windsurf was investigated and deliberately excluded.** There is no
  standalone Windsurf agent CLI to add: Windsurf's own install script now
  installs Devin's CLI (Windsurf was acquired and folded into "Devin
  Desktop"), and the other `windsurf-cli` projects found are unofficial
  file-opener tools, not AI agents. See `docs/install-methods.md`'s "Not
  installed" section for the full reasoning — same honesty standard as
  the Perplexity caveat above, not a guessed substitute.

## Growth Phase 5: live task output in the TUI

- **`divisi-agent-sdk::run::run_command_live`** replaces the old
  read-after-wait capture: two background threads drain the child's
  stdout/stderr pipes as the process runs (line by line), each
  accumulating into the final `RunOutcome` and, when a
  `live_output_path` is given, tee-ing every line to that file
  immediately (flushed). Incidentally fixes a latent risk in the old
  code too — a chatty child could fill its pipe buffer and block on
  `write()` while nothing was reading it until `try_wait()` returned;
  continuous draining removes that possibility.
- **`AgentAdapter::run_prompt`** gained a `live_output_path: Option<&Path>`
  parameter (threaded through all 11 built-in adapters plus
  `GenericAdapter`, mechanical given the pattern was already established
  for `home`). `divisi-runtime::task::run` pre-creates the artifacts
  directory and computes a live path
  (`DivisiDirs::task_live_output_path(id)`, `task-<id>.live.txt`) before
  invoking the agent, and removes it once the run finishes and the final
  artifact (`task-<id>.txt`) is written.
- **TUI: task-detail viewer.** `Enter` on a Tasks-tab row opens a modal
  showing status/exit code/summary and the task's output — reading the
  live file directly off disk (same `DivisiDirs` methods the runtime
  used to write it, no new IPC needed since both processes are always
  local) while the task is `Running`, auto-refreshing every 500ms via
  `TaskInspect`, and switching to the final artifact once it finishes.
  Since an `orchestrate` run creates one task row per agent per step,
  this is also how you inspect each agent in a multi-agent run
  individually — select its row, press `Enter`. `divisi_tui::run` now
  takes the resolved `DivisiDirs` (not just the socket path) so the TUI
  can compute these paths itself.
- **Honest scope**: this is disk-file polling, not a push-based live
  event stream — the runtime doesn't notify the TUI when new output
  arrives, the TUI just re-reads the file periodically while a task is
  running. Good enough for a human watching a task in a terminal;
  wouldn't be the right primitive for, say, a sub-100ms-latency use case.
- Verified end-to-end with a real subprocess (a custom TOML agent running
  `sh -c "echo one; sleep 2; echo two; sleep 2; echo three"`): the live
  file showed `one` within 0.5s and `two` within 2.5s of it actually
  being echoed — genuinely incremental, not buffered until exit — and
  was removed once the task completed, leaving only the final artifact.

## Growth Phase 6: `--real-home` for system-configuration tasks

The isolated-home architecture (Growth Phase 2/Isolation section above)
means every task now runs against a SingleCLI-managed sandbox `$HOME` by
default — exactly the point, for agent config/credentials. But it created
a real gap for a legitimate use case: asking an agent, via `single task
run`, to actually configure the real machine (dotfiles, installed
packages, desktop config) — those edits would land in the fake isolated
home instead of the real system, silently.

- **`Request::TaskRun`/`Orchestrate` gained `real_home: bool`** (also
  `RunTaskOptions`/`OrchestrateOptions`). When set, `divisi-runtime::task`
  skips isolated-home materialization entirely and passes `None` as the
  `$HOME` override to `run_prompt` — the agent subprocess then inherits
  the daemon's own real environment, i.e. the actual logged-in user's
  real `$HOME`, same as before the isolation pivot.
- **Exposed as `single task run --real-home` / `single orchestrate
  --real-home`**, and as a `[g]` toggle in the TUI's task-creation flow
  (Tasks tab, `[n]`, agent-picking step). Off by default — this is a
  deliberate, visible opt-in (the CLI prints a warning when used) since
  it gives the agent full access to real credentials and files, which is
  exactly what the isolated-home default exists to prevent in the
  ordinary case.
- Verified for real: `single task run 'echo HOME=$HOME' --agent
  <custom-agent>` printed the isolated home path by default and the real
  `$HOME` with `--real-home`, confirming the override actually reaches
  the subprocess and nothing else changed.

## Growth Phase 7 (v0.1.17): real agent coordination

Three real extensions to how agents work together, on top of what already
existed (the sequential `orchestrate` relay, and memory/notes context
already auto-injected into every task prompt):

- **Real parallel execution.** `divisi-runtime::orchestrate::run_parallel`
  (`single orchestrate-parallel --task <agent>:<description> ...`) runs
  each agent on its own OS thread, its own git worktree, and its own
  SQLite connection to the shared state db — safe because `state::open`
  now sets `PRAGMA journal_mode=WAL` and a busy timeout (previously
  unset; any real concurrent writer would have hit "database is locked"
  immediately). No automatic goal decomposition — the caller supplies
  each agent's own task explicitly, matching this project's rule against
  fabricating a reasoning capability that doesn't exist. Branches are
  never auto-merged; that stays a human decision.
- **Knowledge graph as real shared context.** `task::build_context_preamble`
  (already injecting relevant memory + unread notes into every task
  prompt) now also queries the knowledge graph for relevant entities and
  injects those too. Writing the graph is still manual — no
  auto-promotion of task output into entities, to avoid guessing at
  entity-naming heuristics.
- **`notes.rs` moved from `divisi-runtime` to `divisi-core`**, the same
  reason `preferences`/`permissions` already live there: so `divisi-gateway`
  (a separate binary that doesn't depend on `divisi-runtime`) can use it
  directly. `divisi-gateway`'s gateway gained two real tools —
  `notes_leave`/`notes_read` — implemented directly against
  `divisi_core::notes`, not proxied. Because the gateway process stays
  alive for an agent's entire session (its own module doc explains why),
  these give an agent genuine mid-session messaging: real tool calls
  during a real run, not simulated inter-process signaling. True live
  bidirectional IPC between two running agent processes is still out of
  scope — these CLIs don't expose it, and `orchestrate.rs`'s module doc
  says so plainly rather than pretending otherwise.
- Parallel orchestrate steps also auto-broadcast a short note (agent →
  everyone, project-scoped) summarizing what they did on completion, so
  siblings that ran at the same time — and couldn't see each other in the
  moment, since none of this is live IPC — see it on their next run or
  via `notes_read`.

## Growth Phase 8 (v0.1.18): preset visibility + catalog expansion

A preset catalog (`mcp::presets()`, `lsp::presets()`, `plugins::presets()`,
`providers::presets()`) only ever mattered once something explicitly ran
`add-preset` — nothing surfaced the other ~230 entries anywhere a user
would actually see them (`list` commands, the TUI, `single status`), so in
practice the v0.1.16 catalog expansion was invisible after install. Four
fixes/expansions:

- **Preset visibility fix.** `Context::load()` (the one place every daemon
  request already builds its state) now calls
  `{mcp,lsp,plugins,providers}::sync_missing_presets[_disabled]` on every
  load — idempotent (only adds presets missing by name; never touches an
  entry the user already registered/edited/enabled), so it's cheap enough
  to run unconditionally rather than needing a one-time migration flag.
  MCP presets already default disabled (`to_spec()`); LSP presets are
  forced disabled here too even though `to_spec()` defaults them enabled,
  since a bulk sync must never turn on 100+ file-watching servers at once.
  Plugin/provider presets have no enabled concept — registering one
  doesn't install or activate anything by itself (`plugin sync` and
  setting a provider's secret are separate explicit steps), so there's
  nothing to gate.
- **Tool catalog.** `tools.rs::default_tools()` grew from 26 to 74 —
  security/recon (`nmap`, `sqlmap`, `nuclei`, `trivy`, ...),
  containers/orchestration (`podman`, `kind`, `k9s`, ...), IaC/cloud
  (`terragrunt`, `doctl`, `wrangler`, ...), VCS (`git-lfs`, `svn`, `hg`),
  language toolchains (`mvn`, `dotnet`, `deno`, `bun`, `zig`, ...),
  database clients (`psql`, `redis-cli`, `mongosh`, ...), and
  system/networking tools. Unlike the original 26 (each confirmed present
  via `command -v` on the reference machine), these are verified as real,
  well-known CLIs with correct binary names rather than required to be
  locally installed — this registry has no execution engine yet (Phase 4
  scope), so "real tool, correct name" is the bar that matters.
- **Agent registry.** `registry.rs::builtin_registry()` grew from 11 to
  22 (qwen-code, amp, openhands, droid, codebuff, plandex,
  continue-cli, grok, mistral-vibe, crush). None of these were installed
  on the machine this registry was built on, so every install
  command/capability flag is sourced from the vendor's own current docs
  rather than confirmed by direct execution — `unverified: true` on all
  eleven, the same honest-uncertainty precedent `cody` already set.
  Candidates that didn't meet the bar were dropped rather than guessed at:
  SWE-agent (superseded by mini-swe-agent, stale), Amazon Q Developer CLI
  (no single verified one-line install command across platforms), and
  several IDE-embedded "agents" (Warp, Zed, Devin, Replit Agent, Cline,
  Bolt, v0, Trae) that don't ship a standalone CLI binary separate from
  their IDE/web product.
  - **Generic-adapter fallback for registry entries.** None of the eleven
    new agents have a dedicated Rust adapter, which used to mean
    `for_agent_with_custom` returned `None` for them — `doctor`/`single
    setup` would report them `Unsupported` regardless of whether a real
    bootstrap-install command existed. `for_agent_with_custom` now takes
    the agent registry as a third argument and, after checking the
    hardcoded adapters and any user-defined `custom_agents.toml`
    override, falls back to wrapping the registry entry in the existing
    `GenericAdapter` (previously only reachable via a user's own
    `custom_agents.toml`) — real `command -v` detection, everything else
    honestly `Unsupported` rather than fabricated. This is why a new
    catalog entry gets real detection/install-planning for free instead
    of needing a bespoke adapter written for it.
- **Provider catalog.** `providers.rs::presets()` grew from 4 to 17 (groq,
  deepseek, mistral, xai, google, openrouter, together, fireworks,
  cerebras, sambanova, deepinfra, perplexity, cohere) — every base
  URL/env var pair confirmed against the vendor's own current docs, same
  bar as the original four.

## E28: the free-provider pool

`crates/divisi-runtime/src/pool/` (bandit, ledger, cooldown, backoff,
degrade, handoff, client — ~2,500 lines) plus `divisi_core::free_pool`
(the vendored ~90-provider catalog, `single provider list-free`) and
`divisi_core::pool_keys` (per-provider labeled key storage) implement a
real, working alternative to shelling an agent CLI: `single task run
--agent single-pool "<prompt>"` picks a `(platform, model, key_id)` via a
Thompson-sampling bandit (spec §6.4 — a decay-weighted Beta posterior over
each candidate's 7-day outcome history, half-life 2 days), dispatches
straight to that provider's HTTP API (no CLI process spawned at all — see
`divisi_agent_sdk::adapters::PoolAdapter`'s doc comment for why its
`run_prompt` is a placeholder and `divisi-runtime::task::execute`
special-cases `agent == "single-pool"` before ever reaching adapter
dispatch), and on a rate limit/5xx/auth failure benches that candidate and
retries the next one before the caller ever sees a failure.

- `single provider list-free` / `add-free` / `key-status` / `sync-pool`
  manage the catalog and its keys. `add-free` best-effort validates a key
  at registration time; a key's `valid`/`last_validated_at` fields are
  now *also* updated from real task outcomes (a successful dispatch marks
  it valid, an authoritative auth rejection marks it invalid) rather than
  only from that one-time probe — fixed 2026-09-11 after live
  verification found keys that had already served real, successful
  requests still reporting "keyed, unvalidated" forever.
- Cooldown and headroom in `key-status` are real, live state (per
  `(platform, model, key_id)`, tracked in `pool_outcomes`/cooldown
  tables) for providers with a declared rate limit — "unbounded/unknown"
  for the (larger) set of providers with none, not a placeholder for
  unfinished work.
- **What's honestly still a seam, not a gap**: there is no live
  per-provider model-catalog feed (each provider is treated as offering
  exactly one nominal "model" — its own `provider.id`, spec §2/§17
  non-goal this iteration), and a completed pool task doesn't record
  which `(platform, model, key_id)` actually served it on the task record
  itself (only in the `pool_outcomes` ledger, queryable but not surfaced
  by `single task inspect`).
- **"Stuck for 61.8h waiting for capacity" — investigated 2026-09-11/12,
  turned out to be working as designed, not a scheduler bug.**
  `scheduler::handle_capacity_exhaustion` blocks a goal once
  `capacity_waits`/wall-clock caps are hit; `self_heal::coordinator::
  reeval_blocked_goals` auto-re-ticks a capacity-blocked goal a bounded
  number of times (`max_auto_reevals_per_goal`) and then deliberately
  stops, leaving it `Blocked` for a human to judge — the same "fail
  closed to a human decision, never guess forever" pattern this codebase
  uses everywhere else (approvals, db corruption restore, etc.). Several
  goals sat blocked for 51-61+ hours simply because nothing resumed them
  — not because the pool was actually unhealthy (`single pool status`
  showed only `degraded`, healthy_ratio being a `usable_keys /
  enabled_providers` display snapshot never consulted by the dispatch
  path, ruled out as a cause) or because of a broken threshold. The
  actual gap: `elapsed_minutes` in `handle_capacity_exhaustion` is
  measured from `goal.created_at`, not from the goal's last resume, so
  `single goal resume` on a days-old blocked goal re-trips the exact same
  wall-clock check on the very next tick unless `single goal amend <id>
  capacity-minutes=<N>` (a separate field from `amend ... minutes=<N>`,
  which raises the goal's overall budget, not this one) is used first —
  confirmed live: `goal_dlb44zyx7u7x_0005` re-blocked instantly on resume
  and stayed running once `capacity-minutes` was raised. Fixed
  2026-09-12: the blocked-reason message now names the exact command
  (`capacity-minutes=<N>`, not `minutes=`) instead of leaving that
  distinction to be rediscovered from source. A dispatched fix attempt
  (3 opencode iterations) chasing this as a scheduler defect failed on
  every node before this was understood — there was no defect to find.

  Also found live tonight while testing the brain-role retry fix above:
  a coordinator-dispatched integrator task to `grok` failed 3/3 attempts
  with `spawning grok: No such file or directory (os error 2)` (task
  #1661), even though `single doctor` shows `grok` detected and
  authenticated on this same machine. Ruled out after a live restart to
  v0.17.1 (to pick up the brain-retry fix) did not fix it: the daemon's
  own `$PATH` (checked via `/proc/<pid>/environ`) does include
  `.local/bin`, and `grok`'s symlink chain (`.local/bin/grok` →
  `.grok/bin/grok` → `.grok/downloads/grok-linux-x86_64`) resolves to a
  real, executable file — so this probably isn't a stale-PATH problem
  after all. `run_command_live`'s `.with_context(|| format!("spawning
  {command}"))` wraps the exact same OS error whether `execve` failed on
  the program or `Command::current_dir(cwd)` failed because the cwd
  doesn't exist, and the failing node was a `code`-kind node (which
  defaults to its own git worktree) reassigned by the supervisor twice in
  ~25s on the same live goal — a stale/already-cleaned-up worktree path
  from an earlier supervisor patch is the more likely culprit now. Either
  way, routing picked `grok` as if it were usable and burned a full retry
  budget on a command that doesn't resolve at all — a case the
  retry-on-bad-JSON fix above doesn't help with, since the task never
  produces output to retry-parse.

  **Update (2026-09-12), two real fixes shipped, root cause still open.**
  Directly reproduced grok spawning under the exact conditions the
  production failure used — isolated `$HOME` override, running from
  inside a fresh git worktree, a 3-process concurrent burst matching the
  timing in the failing goal's event log — and it worked every time
  (the burst even correctly surfaced grok's own real rate-limit error,
  not an ENOENT). Docker execution was also ruled out: `docker.toml`
  doesn't exist on this machine, so `docker::is_enabled` returns `false`
  for every agent, meaning the Docker backend was never in play. The
  root cause is still genuinely unknown — but two real improvements
  shipped from the investigation: (1) `run_role` now re-selects a
  *different* agent via `select_agent_excluding` on each JSON-parse
  retry instead of resampling the exact agent that just failed (the
  actual reroute-on-retry fix — before this, a stuck agent burned the
  whole retry budget on itself, confirmed live: the supervisor role kept
  re-selecting `grok` across all 3 attempts every time), and (2)
  `run_command_live`'s spawn error now disambiguates "cwd doesn't exist"
  from "binary not found" from "neither, likely transient" instead of
  returning the identical ambiguous OS error text for all three — the
  ambiguity itself was most of what made this investigation slow. The
  next real occurrence will be far faster to diagnose. A second automated
  fix attempt (before these two fixes existed) already failed on this
  exact goal (`goal_dlaftumiqf9l_0002`, "tried 5 supervisor fixes on this
  goal; need a decision").

  Relatedly, `single acp`'s doc comment says every prompt
  becomes a coordinator goal with no fast path for read-only status
  questions, so a quick "how's it going?" through Zed queues behind this
  same gate — a fast path that answers status questions from existing
  goal/coordinator/pool state without submitting a new goal is worth
  building alongside the capacity-wait fix, not separately.

## ACP bridge (`single acp`)

`crates/divisi-cli/src/acp.rs` implements a newline-delimited JSON-RPC 2.0
stdio server that wraps the SingleCLI coordinator as an
[Agent Client Protocol](https://agentclientprotocol.com) endpoint — the
integration surface Zed (and any other ACP-capable host) uses.

### Protocol version

The bridge implements **ACP v1** (`PROTOCOL_VERSION: u64 = 1`). v2 changed
the notification model significantly (per-turn scoping → a separate idle
notification channel); this codebase tracks v1 only, which matters for the
constraints below.

### What `session/update` is actually emitted for

`session/update` notifications are only ever sent from three places, all
inside an active context:

1. **`run_turn` → `stream_goal`** — the main prompt-turn loop. After
   `session/prompt` arrives, `run_turn` submits a `GoalSubmit` to the
   coordinator, then `stream_goal` long-polls `SessionEvents` at
   1 200 ms intervals and translates each new coordinator event into a
   `session/update` chunk. This is the primary live-output path.
2. **`session_new` / `session_load`** — the ACP session lifecycle methods
   each emit one `available_commands_update` immediately on setup.
3. **`session/set_mode`** — emits a `current_mode_update` when the client
   changes modes.

There is no background goroutine or daemon-side push that emits
`session/update` outside these three call sites.

### Multi-session routing

`Acp` keeps a `sessions: Mutex<HashMap<String, AcpSession>>` keyed by the
coordinator's own `sess_…` id (the ACP session id **is** the coordinator
session id, by design, so a restarted `single acp` process can resume a
thread on `session/load`). Multiple sessions on one `single acp` process
are therefore supported:

- Each `session/prompt` call spawns its own OS thread to run `stream_goal`,
  so two sessions can poll/stream independently without blocking each other.
- **Cursor isolation.** `stream_goal` maintains a **local** cursor rather
  than reading/writing the shared `AcpSession::last_event_id`. Previously,
  if two goals were ever in flight on the same ACP session concurrently,
  both `stream_goal` threads would race on that field and one could silently
  advance the other's starting point past events belonging to its own goal.
  The local cursor closes this race: each `stream_goal` call is correctly
  scoped to its own goal, and the shared `last_event_id` is only updated as
  a high-water mark so a subsequent `session/load` re-attach can skip
  already-replayed history.
- There is no routing across **separate `single acp` processes**: `sessions`
  is an in-process `HashMap`, not a daemon-side table. Two concurrent
  `single acp` invocations have disjoint session maps.

### Proactive update — what's real

The one proactive (unsolicited) notification path that exists is the
**restart re-attach** in `session_load`: if the daemon or the `single acp`
process restarts mid-goal and the given session id resolves to a goal that
is still `running`/`waiting_on_capacity`, `session_load` spawns a new
`stream_goal` thread to re-attach its event stream. The reconnecting Zed
panel then keeps receiving chunks without the user resending the prompt.
This is the only case where `session/update` chunks arrive without a new
`session/prompt` driving them — and it is triggered by client reconnection,
not by the coordinator pushing to an idle channel.

### Why fully unsolicited idle-session push is not possible under ACP v1

Under ACP v1, `session/update` is scoped to an active prompt turn. The
spec's lifecycle delivers all updates between a `session/prompt` request
and its response; there is no separate out-of-band channel for the server
to push to a session that has no in-flight turn. Concretely:

- Coordinator events that arrive while the ACP session has no active
  `stream_goal` loop simply accumulate in the coordinator's `events` table.
  They are not lost, but they are not forwarded until the next
  `session/prompt` or `session/load` re-attach picks them up.
- Adding an always-on background pusher would require either an ACP v2
  upgrade (which defines a proper idle-notification extension) or a
  out-of-spec side channel neither Zed nor any other current ACP host
  would consume. Neither is implemented; the spec limitation is the honest
  ceiling.

### Default agent

A Zed-submitted goal (`run_turn`) used to default `agent` to `single-pool`
when the session had no `/agent` override, on the theory that it's the
lightest-weight default. Removed: `plan_goal` applies that override to
*every* node in the planned graph, not just the planning step, so this
sent a goal's entire task graph — code, test, review, everything — through
`single-pool`, which has no real tool/file/command execution. Live-
verification finding: this fabricated a plausible-but-fictional cargo
test/clippy run when asked to actually run one. `agent` now defaults to
`None`, letting the coordinator's normal per-node-kind routing
(`routing.toml`) pick a real tool-capable agent per step — the same path
every goal submitted via `single goal submit` already takes.
`/agent <name>` still works exactly as before for a session that
deliberately wants one agent pinned for everything.

## Not in Phase 1-6

Per the original spec's own §50 "Development Strategy" (build vertically,
don't implement everything at once): a multi-agent task-graph orchestrator,
an event *stream* (vs. the persisted log that exists), a plugin
*marketplace/discovery* layer (installing a named plugin, including from
a curated preset catalog, is real; live browsing/searching an arbitrary
marketplace is not), task-scoped context selection, workflows, full
model/provider abstraction (discovery, streaming, usage accounting), and
full process
lifecycle management (start/stop/pause/resume/stream a running agent
session, vs. Phase 4's one-shot blocking `run_prompt`) are all
future-phase work. Where the full spec's shape is visible in this
codebase (e.g. `AgentAdapter`'s doc comment, `Envelope<T>` in
`divisi-protocol`, `permissions.rs`), it's a deliberate seam for that
later work, not a stub pretending to be a finished feature.
