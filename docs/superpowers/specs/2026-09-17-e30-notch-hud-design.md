# E30 — Cross-platform notch HUD: design

Status: draft for review. Builds on E28 (free-provider pool) and the
existing Unix-socket NDJSON protocol (`single-protocol` + `single-runtimed`).
Date: 2026-09-17. Target version: post-`0.17.x` minor (`feat` → `0.18.0`
when implementation lands).

## Goals

1. A small, modern, auto-hiding overlay docked at the **top-center** of the
   screen (macOS Dynamic Island–inspired), available on **macOS and Linux**,
   that shows live SingleCLI free-provider / agent-pool health.
2. Surface, at a glance: which providers are live, **key counts per
   provider** (e.g. `nvidia: 2 keys · google: 2 keys`), rate-limit /
   cooldown state, recent task/goal activity, and agent CLI auth status.
3. Fluid show/hide: slide + fade in on hover or when something notable
   changes; auto-collapse otherwise. Not jarring.
4. Reuse the existing daemon — the notch is a **lightweight client** of
   `runtime.sock`, not a reimplementation of pool logic.
5. Opt-in via `single notch enable|disable|status` (companion process
   lifecycle), consistent with other SingleCLI feature toggles.

## Non-goals

- Push/event-stream IPC (still Phase 4 per ADR 0001). v1 **polls**.
- Windows support (Unix sockets + Linux-first ADR; out of scope).
- Replacing the TUI Pool tab — the TUI remains the full interactive
  surface; the notch is glanceable ambient status.
- Editing pool config from the overlay (no click-to-disable-provider in
  v1). Read-only HUD.
- System tray / menubar icon as the primary surface (optional later
  affordance; notch is the product).
- Perfect pixel-clone of Apple's Dynamic Island hardware cutout geometry.
  We aim for the *interaction language* (collapsed pill → expanded card
  under the top center), not hardware mimicking.
- Bundling a WebView / Tauri stack for v1.

## Survey (what already exists)

| Piece | Location | Notch relevance |
|-------|----------|-----------------|
| Pool catalog + enable flags | `single-core::free_pool` | Provider ids / display names |
| Key metadata | `single-core::pool_keys` | Counts, validity |
| Runtime ledger / cooldowns | `single-runtime::pool::{ledger,cooldown}` | Source of truth inside daemon |
| Wire snapshots | `single-protocol::{PoolStatusInfo,PoolKeyStatusInfo,CoordinatorSnapshot,AgentInfo}` | **What the notch consumes** |
| Socket + framing | `~/.config/single/state/runtime.sock`, NDJSON `Request`/`Response` | Transport |
| CLI client pattern | `single-cli::client`, `single-tui::client` | Copy the TUI's **socket-only** pattern (ensure daemon running) |
| TUI Pool pane | `single-tui` `draw_pool` + poll of `PoolStatus` / `ProviderKeyStatus` | Closest UX reference for data layout |
| Feature toggle pattern | `mcp_gateway.toml` + `McpGatewayCommand::{Enable,Disable,Status}` | Model for `notch.toml` + CLI |
| Desktop GUI | **None** | Greenfield crate |

**Protocol gap (important):** there is no subscription. Daemon comments
note a multiplexed event stream as Phase 4. Only `SessionEvents` is a
poll-with-cursor. A notch must poll. Also `PoolStatusInfo` exposes
degraded / healthy_ratio / benched keys, but **per-provider key counts**
come from aggregating `ProviderKeyStatus` (or a future composite op — see
§Protocol).

**Auth status:** `AgentInfo.authenticated` via `Request::AgentList`;
coordinator CLI-agent rate-limit via `CoordinatorSnapshot.pool`.

## Approaches considered

### A — Pure-Rust iced UI + platform window backends (recommended)

New workspace crate `single-notch` (binary `single-notch`).

- **Shared:** view-model, poller, animation state machine, styling tokens —
  all Rust, depending on `single-protocol` + a tiny socket client (same
  shape as `single-tui::client`).
- **Linux Wayland:** `iced` + layer-shell integration (`iced_layershell` or
  equivalent) so the surface is an **Overlay** layer-shell surface,
  anchored top-center, exclusive zone 0, no server-side decoration.
- **Linux X11 fallback:** undecorated, always-on-top, skip-taskbar window
  positioned at top-center (best-effort; compositor-dependent).
- **macOS:** borderless, non-activating, always-on-top window at
  `WindowLevel::statusBar` / floating panel semantics via iced/winit +
  `objc2` tweaks (collection behavior: join all spaces, full-screen
  auxiliary). Vibrancy/blur via `NSVisualEffectView` shim if iced's
  transparent clear-color alone looks flat.

**Pros:** One UI codebase; fits the Rust workspace ADR; no WebView; ships
as one more binary beside `single-runtimed`.  
**Cons:** macOS panel polish may need a thin `objc2` assist; Wayland
layer-shell iced crates are younger than GTK's; short spike required to
confirm blur + click-through collapsed hit target.

### B — Dual native shells (GTK4 layer-shell + SwiftUI NSPanel)

Rust owns poller + view-model (JSON snapshot over a pipe or control
socket). Linux UI in GTK4 + `gtk4-layer-shell`; macOS UI in SwiftUI
`NSPanel`.

**Pros:** Best native blur/animation/input feel on each OS.  
**Cons:** Two UIs to maintain; Swift toolchain in the macOS release
matrix; fights "one Rust workspace" preference. Rejected for v1;
kept as **escape hatch** if Approach A fails the spike (§Build order).

### C — Tauri / WebView overlay

HTML/CSS HUD in a transparent always-on-top webview.

**Pros:** Fast visual iteration.  
**Cons:** Heavy for a ~40px pill; WebView runtime on every user machine;
contradicts lean CLI/daemon ethos and musl-friendly releases. Rejected.

### Decision

**Lock Approach A for v1.** Spike gate (≤1–2 days of implementation work,
still no product polish): prove (1) transparent rounded pill on macOS +
Linux Wayland, (2) top-center anchoring, (3) hover hit-testing while
mostly collapsed. If the spike fails on macOS feel, fall back to
Approach B **only for the macOS shell**, keeping the Rust view-model.

## Architecture

```
┌────────────────────┐     NDJSON poll      ┌──────────────────┐
│  single-notch      │ ───────────────────► │  single-runtimed │
│  (companion HUD)   │   runtime.sock       │  pool + coord    │
│                    │ ◄─────────────────── │                  │
│  view-model        │   PoolStatus,        └──────────────────┘
│  animation SM      │   ProviderKeyStatus,
│  iced UI           │   CoordinatorStatus,
└─────────┬──────────┘   AgentList
          │
          │ control (optional notch.sock)
          ▼
┌────────────────────┐
│  single notch …    │  enable/disable/status/show/hide
│  (CLI, clap)       │  reads/writes notch.toml, manages process
└────────────────────┘
```

### New crate: `crates/single-notch`

| Module | Responsibility |
|--------|----------------|
| `client` | Unix socket NDJSON send (socket-only; spawn/ensure daemon like TUI) |
| `poll` | Periodic fetch + diff → `NotchSnapshot` + `NotableEvent`s |
| `model` | Aggregate key counts, benches, goals, agent auth into UI state |
| `anim` | Collapsed ↔ Expanded transitions (easing, timers) |
| `ui` | iced application: pill + expanded card |
| `platform` | `cfg`-gated window setup (layer-shell / NSPanel flags / X11) |
| `control` | Optional local `notch.sock` for `show`/`hide`/`quit` from CLI |
| `config` | Load `notch.toml` |

`single-cli` gains a `Notch` subcommand; it does **not** embed the GUI.
`single-core` gains `NotchConfig` + `SingleDirs::notch_file()` /
`notch_pid_file()` / `notch_socket_path()` mirroring other feature files.

### Process model

- **Opt-in companion**, not folded into `single-runtimed`. The daemon stays
  headless and safe on servers/CI; the HUD is a user-session GUI process.
- `single notch enable`: write `enabled = true`, ensure `single-runtimed`
  is up, spawn `single-notch` if not running (pidfile + live check).
- `single notch disable`: set `enabled = false`, signal quit via
  `notch.sock` or SIGTERM using pidfile.
- `single notch status`: print enabled flag, HUD pid alive?, last snapshot
  age if available.
- `single notch show` / `hide`: force expand / collapse (control socket).
- Autostart: v1 documents manual enable + optional user-level
  systemd/launchd unit examples; no forced install of session agents.

If `enabled = true` but the HUD dies, the next `single` invocation that
touches notch (or a lightweight check from `daemon ensure` — **optional,
v1.1**) may respawn it. v1: only explicit `enable` / documented
session-autostart respawns.

## Protocol & data

### v1 — compose existing ops (no wire break)

Poll loop (default **1000 ms** collapsed / idle; **400 ms** while expanded
or within 5s of a notable event):

1. `Request::PoolStatus` → degraded, healthy_ratio, benched[]
2. `Request::ProviderKeyStatus { platform: None }` → per-platform
   keyed/valid/cooldown/headroom  
   **Key counts:** group by `platform` where `keyed == true` (and ideally
   `valid`); render `nvidia: 2 keys · google: 2 keys`.
3. `Request::CoordinatorStatus` → running/queued/waiting goals + CLI
   agent `rate_limited` flags (recent activity strip).
4. `Request::AgentList` → auth dots (`Authenticated` / not / unsupported).

Diff consecutive snapshots to emit `NotableEvent`:

| Event | Expand? |
|-------|---------|
| New bench / bench cleared | yes |
| `degraded` flipped | yes |
| Goal entered terminal success/fail (from coordinator summaries) | yes |
| Agent auth lost (`Authenticated` → not) | yes |
| Healthy ratio drop ≥ 0.25 absolute | yes |
| Pure headroom tick / unchanged | no |

### Optional additive op (same release if cheap)

```text
Request::NotchSnapshot
ResponseData::NotchSnapshot(NotchSnapshotInfo)
```

One round-trip bundling the four queries above, computed in
`handlers.rs`. **Nice-to-have**, not required to ship — reduces socket
chatter and keeps aggregation server-side. If added, notch prefers it and
falls back to the four-op poll on older daemons.

**Still no push stream in E30.**

## UX & animation

### Layout

**Collapsed (default):** ~128×28 px pill, top-center, ~8–12 px below the
top screen edge (or just under the macOS menu bar / in the notch band when
the display has one — use visible frame APIs; if unknown, 8 px from top of
primary monitor). Contents:

- Status dot: teal = healthy, amber = any bench/cooldown, red = degraded
- Compact tally: `3 providers · 7 keys` or the two busiest platforms
- Optional tiny spinner glyph when a goal is `running`

**Expanded (hover or notable):** ~340×148 px card, same anchor, growing
downward:

1. Header: Pool healthy / DEGRADED · ratio%
2. Provider rows: `nvidia  2 keys  clear  40/50 rpd`
3. Benches: `openai/gpt-…  key_ab12  42s  authoritative`
4. Activity: up to 3 recent goals (truncate text)
5. Agents: colored dots + short names for auth state

### Motion (locked defaults — adjustable in `notch.toml`)

- Expand: **220 ms** ease-out-cubic on height/width + opacity 0.0→1.0 for
  expanded content
- Collapse: **280 ms** ease-in-cubic (slightly slower — feels calmer)
- Notable auto-expand holds **2.5 s** after last event, then collapses if
  pointer is not inside
- Hover: expand while pointer inside hit target; collapse **400 ms** after
  leave
- No spring/bounce in v1 (compositor-friendly; less motion-sickness)
- Reduced-motion: if OS accessibility flag is set, snap expand/collapse
  with opacity only (no geometry tween)

### Visual language

- Fill: near-black `#0D0D0F` at ~88% opacity; 1 px hairline
  `rgba(255,255,255,0.08)`; corner radius 14 (pill) / 16 (card)
- Blur/vibrancy when the platform shim allows; otherwise solid translucent
  fill — never a loud gradient
- Accent: teal `#2EC4B6` healthy, amber `#E9A319` cooldown, red `#E85D4C`
  degraded — **no purple**
- Typography: system UI font stack with medium weight for tallies; avoid
  Inter-as-brand; keep type tiny and dense (11–12 px body)

### Input

- Hover / pointer enter-leave drives expand (primary)
- Click on expanded card: no-op in v1 (or focus steal avoided —
  non-activating panel on macOS)
- Escape while expanded from notable event: collapse immediately
- Right-click / context menu: deferred

## CLI & config

### Commands

```text
single notch enable
single notch disable
single notch status
single notch show      # force expand briefly
single notch hide      # force collapse
```

Clap shape mirrors `McpGatewayCommand` (no name argument).

### Config — `~/.config/single/notch.toml`

```toml
enabled = false
poll_ms_idle = 1000
poll_ms_active = 400
auto_hide_ms = 2500
hover_leave_ms = 400
# position fixed top-center in v1; field reserved
# position = "top-center"
```

Env override (debug): `SINGLE_NOTCH_POLL_MS`.

### Paths

| Path | Role |
|------|------|
| `…/notch.toml` | Feature config |
| `…/state/notch.pid` | HUD pid |
| `…/state/notch.sock` | Control socket (show/hide/quit) |

## Build order (implementation plan later)

1. **Spike (throwaway or behind feature flag):** iced window — transparent,
   rounded, top-center, Wayland layer-shell + macOS floating — empty UI.
2. **`NotchConfig` + CLI enable/disable/status** without GUI (spawn stub
   that exits 0).
3. **Poller + `NotchSnapshot` model** with unit tests on aggregation /
   notable-event diff (pure Rust, no GUI).
4. **Collapsed pill UI** wired to live daemon (or fixture).
5. **Expanded card + animation SM.**
6. **Platform polish** (blur, click-through, multi-monitor primary).
7. **Optional `Request::NotchSnapshot`** if profiling shows four polls
   hurt.
8. Docs: FAQ blurb + session-autostart examples (systemd user unit /
   launchd plist).

Hard gate from brainstorming: **no GUI product code until this design is
greenlit**; the spike is explicitly allowed only after approval, and
remains discardable.

## Testing strategy

- Unit: snapshot aggregation, notable-event detection, animation timer
  math (determinism via injectable clock).
- Integration: socket round-trip against test daemon (extend
  `socket_roundtrip` patterns) for poller.
- Manual matrix: macOS arm64 (notched + non-notched laptop), Linux
  Wayland (Mutter / KWin), one X11 session.
- CI: pure model/anim tests only — no headed GUI in CI for v1.

## Assumptions (underspecified → chosen)

1. Epic id **E30**; version bump to **0.18.0** when the feature merges.
2. Default **off** — opt-in via `enable`.
3. Primary monitor only in v1.
4. Key count = number of `ProviderKeyStatus` rows with `keyed == true`
   per platform (not distinct secret refs beyond what the API already
   flattens).
5. "Recent task activity" = coordinator goal summaries (running /
   waiting / recently terminal if present), not the full events table.
6. Collapse-by-default idle; no always-expanded mode in v1.
7. Single notch instance globally (second launch exits if pidfile live).
8. Docs live at `docs/superpowers/specs/` per existing epic convention.

## Deferred

- True push events from `single-runtimed` (Phase 4) — notch becomes
  event-driven then.
- Click actions (open TUI on Pool tab, copy key id, disable provider).
- Multi-monitor follow-cursor / follow-focused-window.
- Tray icon mirror for environments where overlays are blocked.
- Windows.
- Approach B macOS SwiftUI shell (only if Approach A spike fails).
- Composite `NotchSnapshot` wire op if four-poll is "good enough".

## Success criteria

- On an enabled Linux Wayland or macOS session with `single-runtimed`
  running and free-pool keys configured, the pill appears top-center,
  shows accurate provider key tallies within one poll interval, expands
  smoothly on hover and on an induced bench, and auto-hides without
  stealing focus from the focused app.
- `single notch disable` removes the overlay and leaves the daemon alone.
- No pool logic duplicated outside `single-runtime` / `single-core`.
