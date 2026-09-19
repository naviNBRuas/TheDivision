# Web Capability Pack — integration plan

Written 2026-08-24, ahead of the overnight build run. Grounds the
"premium web development" feature request in SingleCLI's actual
architecture (crates: `divisi-core`, `divisi-protocol`, `divisi-runtime`,
`divisi-agent-sdk`, `divisi-cli`, `divisi-tui`, `divisi-gateway`) rather than
inventing a parallel system.

## What the feature request actually asks for, restated honestly

Two combined prompts ask for a full "digital agency" capability: premium
web design knowledge, a pattern/component library, motion/3D expertise,
autonomous browser-driven visual QA (screenshots, Lighthouse, axe),
asset intelligence (discovery, generation, optimization, licensing), and
an 8-role agent orchestrator, wired into SingleCLI as a removable
"Web Capability Pack." Both prompts explicitly forbid fabricating
functionality, test results, or quality scores. Given that constraint,
this plan only claims a piece is "built" once it's real and verified —
everything else is marked designed-but-deferred.

## Where each piece actually belongs

**Not new "agents."** SingleCLI's `agent` registry
(`divisi-agent-sdk::adapters`) is for real coding-agent CLIs (claude,
codex, opencode, ...). The spec's 8 roles (WebArchitect, FrontendEngineer,
MotionDesigner, ThreeDDesigner, UXDesigner, AccessibilityEngineer,
PerformanceEngineer, VisualQA) are **prompt personas**, not new binaries
to integrate. Each becomes a skill fragment
(`skills/web/premium-web/roles/<role>.md`) whose content gets prepended
to a `single task run`/`orchestrate-graph` dispatch — reusing the exact
mechanism already proven in this repo's own dogfooding (see
`crates/divisi-runtime/src/orchestrate_graph.rs`), not a parallel
orchestrator.

**Skills**: `~/.config/single/skills/web/premium-web/` — pure markdown,
managed by the existing `single skill` command (`divisi_core::skills`).
No code changes needed to consume these once written; any agent doing
web work can load them today.

**"Design MCP" / "Asset MCP"**: the honest interpretation is *register
real existing MCP servers* into SingleCLI's already-real dynamic MCP
registry (`divisi_core::mcp`, `~/.config/single/mcp.toml`,
`single mcp add`), not hand-write a design/asset protocol server from
scratch overnight. Priority one: a Playwright MCP server for browser
automation — the one section of the spec with a genuinely mature,
off-the-shelf solution (this Claude Code session's own
`mcp__plugin_playwright_playwright__*` tools are an existing proof this
shape of integration works). A bespoke "asset intelligence" MCP with
semantic search, generation, and licensing verification is multi-week
work; not attempted tonight beyond a local-directory scanner.

**`divisi-web` crate (new, real)**: a thin workspace member, same shape
as `divisi-gateway` — reads the skill/pattern markdown and exposes
`single web patterns list|search`, `single web design-system get`.
Scoped and tested like the `task-hooks` feature shipped tonight
(`cargo build --workspace` / `cargo test --workspace` must pass, no
placeholder implementations).

**Browser-driven visual QA loop, Lighthouse/axe scoring, image
generation, full 3D asset pipeline, `web create/analyze/audit/qa` CLI
surface**: designed here, **not implemented tonight**. Each needs either
a provider integration decision the user should confirm (image
generation costs money; Lighthouse/axe need a real running project to
test against) or enough surface area to be its own dedicated session.
Recorded as open work in `OVERNIGHT-BUILD-PLAN-2026-08-24.md`'s progress
log rather than silently dropped or claimed done.

## Dependency policy note (per the spec's own section 28)

`rmcp`/`rmcp-macros` are already workspace dependencies (used by
`divisi-gateway`) — no new MCP SDK needed if a bespoke server is ever built.
No new dependency has been added for this plan beyond what's already in
the workspace; anything a future phase needs (Playwright bindings,
image libraries, etc.) should be verified against current published
docs before adding, per house policy, not assumed from training data.
