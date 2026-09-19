# divisi rebrand: design

Status: approved 2026-09-19. Supersedes the SingleCLI name and identity.
Successor step: implementation plan (writing-plans).

## 1. Foundation

**Name:** divisi. Descriptor: "The Division". Home: `divisi.nbr.company`
(with `the-division.nbr.company` redirecting to it).

**Positioning:** divisi is the orchestration layer for AI agents. You state a
goal. It splits the work into parts, assigns each to whichever agent or model
fits, and keeps the score.

**Audience:** solo power-devs and hackers who run several agent CLIs and are
tired of rate limits, duplicated config and losing track of who is doing what.

**Model:** free, MIT, no revenue. Follows the existing OSS-ventures pattern.

**Endorsement line:** "An independent project, endorsed by NBR Company. Built
by Navin B. Ruas (naviNBRuas)." Used in the footer, README and `--version`.
No NBR-branded chrome.

**Dual sense of the name.** *divisi* is the music term for splitting a section
into parts; *the Division* is a unit of agents. Both are intended.

**Vocabulary** (docs and UI, used where natural, never forced):

| Product concept | Term |
|---|---|
| Goal | the score |
| Task nodes | parts |
| Agents and models | the section |
| Routing and failover | cueing |
| Provider pool | the bench |
| Daemon | the pit |

**Voice:** precise, dry, slightly irreverent. Short sentences. No "AI-powered",
no "supercharge". Example: "Six agents, one goal. Nobody plays over anybody."

**Tagline candidates:** "Split the work." / "One goal. Many parts." /
"The Division of AI agents."

**Name checks run 2026-09-19:** crate `divisi` free on crates.io, no `divisi`
binary on PATH, no notable existing software of that name. Every major TLD is
registered (.com .org .io .ai .dev .app .sh), so the site lives under
`nbr.company`. Trademark not searched; do that before any commercial use.
`division` and `thedivision` crate names are free but are not used.

## 2. Visual identity

**Marks.** The obelus (÷) is the primary logo and the active state. The slash
(/) is the idle state and the prompt, path and command glyph.

**Geometry** (48-unit grid): obelus bar 32 long, 6 thick, corner radius 1.5;
two dots of radius 4.6 at y = 24 ± 14. Slash: the same bar lengthened to 44 and
rotated -62 degrees. Clear space is one dot diameter. Minimum size 12px.
Works in a single colour on any background.

**Colour tokens**

| Token | Hex | Use |
|---|---|---|
| Graphite | `#16181d` | Backgrounds |
| Bone | `#f2f2f0` | Text |
| Signal | `#ff5a1f` | The mark, active states |
| Go | `#3ddc97` | Done, success |
| Caution | `#ffd23f` | Rate-limited, rerouted |
| Fault | `#ff2d55` | Errors |

Signal is reserved for the mark and "active"; errors use Fault so orange never
reads as broken.

**Type.** Wordmark and headings: Space Grotesk 800, lowercase, tight tracking.
Technical text: JetBrains Mono. Body: Inter.

**Motion: Spin & pop.** Loop: hold "/" 0.5s; morph in 1.1s; hold "÷" 0.9s;
morph out 1.1s; rest 0.6s (4.2s total).
- Morph in: bar takes one extra full turn (-62 to -360 degrees) with cubic
  in-out easing while its length goes 44 to 32. Dots appear from progress 0.5
  with an out-back overshoot spring.
- Morph out is the exact reverse.
- State mapping: "/" idle, morph = starting or finishing, "÷" working.
- Reduced motion: honour `prefers-reduced-motion` and `NO_MOTION` by swapping
  between the two static marks with no spin.

**One source of truth.** A motion spec file defines four values per frame
(bar angle, bar length, dot offset, dot radius) as functions of progress. A
build step emits SVG/CSS for the site, precomputed half-block frames for the
TUI (about 10 to 15 per transition, 25 columns by 15 rows), and frame assets
for the notch. The prototype lives in the brainstorm session
(`mark-animation.html`) and is the reference implementation.

**Surfaces.** CLI/TUI: half-block mark in the header and while a goal runs;
`/` as prompt prefix. Notch: 20px mark plus a status word; idle shows "/",
a running goal animates. Site/README: full lockup with a looping hero. Favicon
and avatar: static ÷ on graphite. Under `NO_COLOR` or a dumb terminal the marks
degrade to plain ASCII (`/` and `-:-`).

## 3. Rename and migration

**Name map**

| Today | After |
|---|---|
| product SingleCLI | divisi ("The Division") |
| bin `single`, crate `single-cli` | `divisi`, `divisi-cli` |
| crates `single-*` (core, runtime, tui, protocol, agent-sdk, native-agent, web, lsp, agent) | `divisi-*` |
| daemon `single-runtimed` | `divisid` (unit `divisid.service`) |
| `single-notch`, UUID `single-notch@nbr.company` | `divisi-notch`, `divisi-notch@nbr.company` |
| `singlecli-mcp` (agent/task/orchestrate tools) | `divisi-mcp` |
| `single-mcp` (dynamic registry gateway) | `divisi-gateway` |
| `SINGLE_*` env vars | `DIVISI_*` |
| `~/.config/single` | `~/.config/divisi` |
| repo `naviNBRuas/SingleCLI` | `naviNBRuas/divisi` |

No `division` alias is shipped (two names for one tool split search and docs).

**Surface measured 2026-09-19** (excluding `vendor/` and the lockfile): 256
tracked files; `single-…` in about 120 files (about 1,200 hits); `SingleCLI` in
85; `SINGLE_*` in 39; `~/.config/single` in 37; `single-runtimed` in 33. The
word "single" is also ordinary English, so the rename is a scripted,
pattern-specific transform verified by a full build and test run, never a blind
find-and-replace.

**Compatibility**
- A `single` shim remains for two minor versions. It execs `divisi` and prints
  one stderr deprecation line, then is removed.
- `SINGLE_*` is read when `DIVISI_*` is unset, with a one-time warning.
- On first run `divisi` moves `~/.config/single` to `~/.config/divisi` and
  leaves a symlink back. On-disk formats are unchanged, so this is a move, not
  a conversion.
- `divisi notch enable` disables the old extension UUID and enables the new one.
- A migration step disables `single-runtimed` and installs `divisid`,
  preserving the current disabled-autostart state; nothing is restarted.
- GitHub redirects the old repo and clone URLs; a new `install.sh` URL is
  published and the old one is noted in the README.
- The CHANGELOG keeps its history and gains a "formerly SingleCLI" line.

**Outside the repo, re-synced after the rename:** `~/.claude.json` MCP entries
and global `CLAUDE.md`; Zed `agent_servers`; systemd `PATH` and unit names; the
`nbr-workspace` epic queue; memory notes. MCP and Zed entries are re-synced with
`divisi install-integrations --real-home --yes`.

**Versioning:** 0.24.0 (minor bump, per the pre-1.0 SemVer rule).

**Rollout, one atomic commit per step**

Phase A: local only. Needs no GitHub or GitLab access; commits stay on the
`rebrand/divisi` branch. Nothing is pushed.
0. Branch and baseline (build and full tests pass before touching anything;
   the daemon stays stopped).
1. Brand kit: logo SVGs, colour tokens, motion spec and renderers.
2. Mechanical rename: crates, binaries, identifiers, env vars, paths; build
   and full tests pass after each stage.
3. Compatibility and migrations: legacy `SINGLE_*` adoption, config-dir move,
   `single*` shims, `divisi migrate` (systemd unit, notch UUID).
4. TUI and notch: animated mark and the `/` prompt glyph.
5. Docs and CHANGELOG, version 0.24.0 (no tag).
6. Cutover, on explicit go only: install binaries, run `divisi migrate
   --apply`, re-sync integrations, resume the daemon.

Phase B: deferred until the forge is available (E28 local forge first, then
GitHub and GitLab). Repo rename to `naviNBRuas/divisi` and the `repository`
URL; `install.sh`, `release.yml` and Docker image names (they must ship
together with the first divisi release, since installers fetch release
assets); tag and 0.24.0 release; `divisi.nbr.company` site publish;
GitHub redirect notes in the README.

## 4. Risks and known open items

- The `gh` keyring token is invalid and authenticated API calls return
  "account was suspended". Decision 2026-09-19: work around it. All remote
  work is Phase B and waits for the E28 local forge, then GitHub and GitLab.
  Until then the `repository` URL and the release and install paths keep the
  SingleCLI names.
- Persisted identifiers keep their legacy names in 0.24.0 and get a later
  migration: agent ids `single-pool` and `single-agent`, the opencode provider
  namespace `single-<provider>`, the Qdrant collection `single_memory`, and the
  keyring entry `single-redact-master-key`. Renaming them without a data
  migration would orphan stored state or secrets. `state/single.db` is renamed
  to `divisi.db` as part of the config-dir migration.
- `divisi.dev` is held by a third party (registered 2021, expires
  2027-05-17); acquiring it is optional and out of scope.
- Unresolved carry-overs from the pause note that the rename must not disturb:
  the release-build standalone HUD socket issue, agent login detection against
  the real home, the token parser, `target/` growth.
- Persisted state and wire strings may embed the old names; step 2 must audit
  `single-protocol` and stored state before renaming, and keep formats readable.
