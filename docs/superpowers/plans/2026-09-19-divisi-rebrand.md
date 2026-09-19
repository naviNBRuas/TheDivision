# divisi Rebrand Implementation Plan (Phase A, local)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename SingleCLI to divisi across the workspace, with compatibility shims and migrations, plus the new mark, its animation, and its TUI and notch surfaces, entirely locally.

**Architecture:** One new crate, `divisi-brand`, holds the mark geometry, the Spin & pop motion spec and its renderers (SVG constants, half-block characters, single-cell glyph, notch frame JSON), so every surface reads the same numbers. The rename is a staged, explicit-table Python script (never a blind sed) guarded by a `check` stage. Compatibility lives in `divisi-core` (`env`, `migrate`, `shim`) and a `divisi migrate` command.

**Tech Stack:** Rust 2021 workspace (cargo 1.97), ratatui TUI, GNOME Shell extension (GJS, St, Cairo), Python 3 stdlib for the rename script, git.

**Spec:** `docs/superpowers/specs/2026-09-19-divisi-rebrand-design.md`

## Global Constraints

- **Working dir / branch:** `/home/navinbruas/Projects/The-Company/nbr-vault/Development/Repositories/naviNBRuas/Active/SingleCLI`, branch `rebrand/divisi`. Never push, tag, rename the GitHub repo, or use `gh`. Phase B (below) is out of scope.
- **Daemon stays stopped** (`single-runtimed` disabled and not running) until Task 13. Never run the real `~/.config/single` through the new binaries before Task 13; tests use `DIVISI_CONFIG_DIR` temp dirs.
- **Commits:** subject `type: description` with type in `feat|fix|refactor|docs|test|chore|perf|build|ci`; sole author `Navin B. Ruas <founder@nbr.company>` (already the git config); no `Co-Authored-By` or any trailer; message describes what changed. One atomic, building commit per task step marked "Commit".
- **Version:** 0.24.0 (set in Task 11). No git tag.
- **Name map:** bin `single`→`divisi`; crate `single-cli`→`divisi-cli`; crates `single-{core,protocol,runtime,agent-sdk,native-agent,tui,web,lsp,notch}`→`divisi-*`; package `single-agent`→`divisi-agent`; daemon `single-runtimed`→`divisid`; `singlecli-mcp`→`divisi-mcp`; `single-mcp`→`divisi-gateway`; `SINGLE_*`→`DIVISI_*`; `~/.config/single`→`~/.config/divisi`; notch UUID `single-notch@nbr.company`→`divisi-notch@nbr.company`; `SingleDirs`→`DivisiDirs`.
- **Legacy names kept in 0.24.0** (persisted; renaming needs a data migration): agent ids `"single-pool"` and `"single-agent"` (the `divisi-agent` *binary* is renamed; only the id and adapter key stay), opencode provider namespace `single-<provider>`, Qdrant collection `single_memory`, keyring entry `single-redact-master-key`, MCP permission resource prefix `singlecli:<tool>`, backup manifest name `__singlecli_secrets__.toml`, and the repository URL `naviNBRuas/SingleCLI`.
- **Colour tokens:** graphite `#16181d`, bone `#f2f2f0`, signal `#ff5a1f`, go `#3ddc97`, caution `#ffd23f`, fault `#ff2d55`.
- **Mark geometry (48-unit grid):** obelus bar 32 long, 6 thick, rx 1.5; dots radius 4.6 at y = 24 ± 14. Slash: bar length 44, rotated -62 degrees.
- **Motion:** loop 4.2s: hold "/" 0.5s, morph in 1.1s, hold "÷" 0.9s, morph out 1.1s, rest 0.6s. Morph in: angle -62 → -360 with cubic in-out easing, bar length 44 → 32, dots appear from progress 0.5 with an out-back spring. `NO_MOTION` and `NO_COLOR` must degrade to static or plain output.
- **Test runs:** `cargo test --workspace`. If rustc SIGSEGVs under memory pressure, rerun with `-j1`.
- **Test hygiene (learned in execution):** the integration tests bootstrap full copies of every agent home into `/tmp` (about 1.5 GB), and the suite leaks `.tmp*` dirs there. If `/tmp` (tmpfs) has under about 2 GB free, `copy_dir_recursive` swallows the ENOSPC and leaves zero-byte files, so tests fail with `EOF while parsing` on an agent config. Before a run: `df -h /tmp`, and clear idle leftovers with `find /tmp -maxdepth 1 -name '.tmp*' -user "$USER" -mmin +5 -exec rm -rf {} +`.
- **Config safety:** `.cargo/config.toml` sandboxes `XDG_CONFIG_HOME` under `target/xdg-config` for cargo-launched processes, because tests call `DivisiDirs::discover()` and it now migrates the legacy config dir. Never run `divisi migrate --apply` or an installed new binary against the real home before Task 13.
- **Guard exit code:** `python3 scripts/rename_divisi.py check | tail -1` hides the exit status. Use `python3 scripts/rename_divisi.py check >/dev/null; echo $?` (0 = clean) before committing.

---

### Task 1: Baseline

**Files:** none changed.

- [ ] **Step 1: Confirm state**

Run:
```bash
cd /home/navinbruas/Projects/The-Company/nbr-vault/Development/Repositories/naviNBRuas/Active/SingleCLI
git branch --show-current; git status --short | wc -l
systemctl --user is-active single-runtimed; systemctl --user is-enabled single-runtimed
```
Expected: `rebrand/divisi`, `0`, `inactive`, `disabled`. If the daemon is active, stop here and tell the user.

- [ ] **Step 2: Build and test the untouched workspace**

Run: `cargo build --workspace 2>&1 | tail -3 && cargo test --workspace 2>&1 | grep -E "^test result|FAILED|error" | sort | uniq -c`
Expected: build succeeds; every `test result:` line is `ok`, no `FAILED`. Record the total passed count (`cargo test --workspace 2>&1 | grep -E "^test result" | awk '{s+=$4} END {print s}'`) as the baseline number; later tasks must not reduce it except by the tests deleted with a renamed file.

If a test fails before any change, stop and report it; do not proceed on a red baseline.

Also record the clippy baseline: `cargo clippy --workspace --all-targets 2>&1 | grep -E "^(warning|error)" | sort | uniq -c > /tmp/clippy-baseline.txt; wc -l /tmp/clippy-baseline.txt`.

---

### Task 2: `divisi-brand` crate (mark, motion, renderers)

**Files:**
- Create: `crates/divisi-brand/Cargo.toml`
- Create: `crates/divisi-brand/src/lib.rs`, `src/motion.rs`, `src/raster.rs`, `src/glyph.rs`, `src/tokens.rs`, `src/frames.rs`
- Create: `crates/divisi-brand/assets/mark-obelus.svg`, `assets/mark-slash.svg`, `assets/tokens.css`
- Create: `crates/divisi-brand/examples/emit_frames.rs`
- Modify: `Cargo.toml` (workspace `members`)

**Interfaces:**
- Produces (used by Tasks 9, 10):
  - `divisi_brand::motion::{SLASH_ANGLE, BAR_SLASH_LEN, BAR_OBELUS_LEN, DOT_OFFSET, DOT_RADIUS, LOOP_SECS}` (`f32` consts)
  - `motion::MarkState { angle_deg: f32, bar_len: f32, dot_offset: f32, dot_radius: f32 }` (`Debug, Clone, Copy, PartialEq`)
  - `motion::state_at(progress: f32) -> MarkState` (0.0 = "/", 1.0 = "÷")
  - `motion::MarkPhase { Idle, Starting, Working, Finishing }` with `as_str(self) -> &'static str`
  - `motion::LoopFrame { progress: f32, phase: MarkPhase, state: MarkState }` and `motion::loop_at(t_secs: f32) -> LoopFrame`
  - `raster::render(state: &MarkState, rows: usize) -> Vec<String>` (`rows` character rows, half-block characters)
  - `glyph::glyph(progress: f32) -> char`, `glyph::ascii(progress: f32) -> &'static str`
  - `tokens::ALL: [(&str, &str); 6]`
  - `frames::loop_frames(fps: u32) -> Vec<LoopFrame>`, `frames::to_json(fps: u32, frames: &[LoopFrame]) -> String`

- [ ] **Step 1: Scaffold the crate and register it**

`crates/divisi-brand/Cargo.toml`:
```toml
[package]
name = "divisi-brand"
version.workspace = true
edition.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
description = "divisi mark geometry, motion spec and renderers"
```
`crates/divisi-brand/src/lib.rs`:
```rust
//! The divisi mark and its motion, defined once and rendered everywhere
//! (SVG, terminal half-blocks, single-cell glyph, notch frame data).

pub mod frames;
pub mod glyph;
pub mod motion;
pub mod raster;
pub mod tokens;
```
In root `Cargo.toml`, add `"crates/divisi-brand",` as the last entry of `[workspace] members`.

- [ ] **Step 2: Write the failing motion tests**

`crates/divisi-brand/src/motion.rs`:
```rust
//! Spin & pop: "/" morphs to "÷" (progress 0.0 -> 1.0) and back.

pub const SLASH_ANGLE: f32 = -62.0;
pub const BAR_SLASH_LEN: f32 = 44.0;
pub const BAR_OBELUS_LEN: f32 = 32.0;
pub const DOT_OFFSET: f32 = 14.0;
pub const DOT_RADIUS: f32 = 4.6;
pub const LOOP_SECS: f32 = 4.2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_zero_is_the_slash() {
        let s = state_at(0.0);
        assert_eq!(s.angle_deg, -62.0);
        assert_eq!(s.bar_len, 44.0);
        assert!(s.dot_radius.abs() < 1e-4, "dots hidden at rest: {}", s.dot_radius);
    }

    #[test]
    fn progress_one_is_the_obelus() {
        let s = state_at(1.0);
        assert!((s.angle_deg - -360.0).abs() < 1e-3);
        assert!((s.bar_len - 32.0).abs() < 1e-3);
        assert!((s.dot_radius - DOT_RADIUS).abs() < 1e-3);
        assert_eq!(s.dot_offset, DOT_OFFSET);
    }

    #[test]
    fn dots_stay_hidden_until_halfway_then_overshoot() {
        assert!(state_at(0.5).dot_radius.abs() < 1e-4);
        assert!(state_at(0.3).dot_radius.abs() < 1e-4);
        let peak = (0..=100).map(|i| state_at(i as f32 / 100.0).dot_radius).fold(0.0_f32, f32::max);
        assert!(peak > DOT_RADIUS, "spring should overshoot, peak was {peak}");
    }

    #[test]
    fn the_bar_only_turns_one_way() {
        let mut prev = state_at(0.0).angle_deg;
        for i in 1..=100 {
            let a = state_at(i as f32 / 100.0).angle_deg;
            assert!(a <= prev + 1e-4, "angle went back up at {i}: {prev} -> {a}");
            prev = a;
        }
    }

    #[test]
    fn progress_is_clamped() {
        assert_eq!(state_at(-3.0), state_at(0.0));
        assert_eq!(state_at(9.0), state_at(1.0));
    }

    #[test]
    fn loop_follows_the_timeline() {
        let idle = loop_at(0.2);
        assert_eq!((idle.progress, idle.phase), (0.0, MarkPhase::Idle));
        let starting = loop_at(1.05);
        assert!((starting.progress - 0.5).abs() < 1e-3);
        assert_eq!(starting.phase, MarkPhase::Starting);
        let working = loop_at(2.0);
        assert_eq!((working.progress, working.phase), (1.0, MarkPhase::Working));
        let finishing = loop_at(3.05);
        assert!((finishing.progress - 0.5).abs() < 1e-3);
        assert_eq!(finishing.phase, MarkPhase::Finishing);
        assert_eq!(loop_at(4.0).phase, MarkPhase::Idle);
    }

    #[test]
    fn loop_wraps() {
        assert!((loop_at(4.2 + 1.05).progress - loop_at(1.05).progress).abs() < 1e-3);
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p divisi-brand 2>&1 | tail -15`
Expected: FAIL to compile: `cannot find function state_at` / `MarkPhase` / `loop_at`.

- [ ] **Step 4: Implement motion**

Insert above the `#[cfg(test)]` block in `motion.rs`:
```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkState {
    pub angle_deg: f32,
    pub bar_len: f32,
    pub dot_offset: f32,
    pub dot_radius: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkPhase {
    Idle,
    Starting,
    Working,
    Finishing,
}

impl MarkPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            MarkPhase::Idle => "idle",
            MarkPhase::Starting => "starting",
            MarkPhase::Working => "working",
            MarkPhase::Finishing => "finishing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoopFrame {
    pub progress: f32,
    pub phase: MarkPhase,
    pub state: MarkState,
}

fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

fn ease_in_out_cubic(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

fn ease_out_back(t: f32) -> f32 {
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

/// Mark geometry at `progress` (0.0 = "/", 1.0 = "÷"); the reverse morph is
/// the same function walked backwards.
pub fn state_at(progress: f32) -> MarkState {
    let r = clamp01(progress);
    let e = ease_in_out_cubic(r);
    let dots = ease_out_back(clamp01((r - 0.5) / 0.5)).max(0.0);
    MarkState {
        angle_deg: SLASH_ANGLE + (-360.0 - SLASH_ANGLE) * e,
        bar_len: BAR_SLASH_LEN + (BAR_OBELUS_LEN - BAR_SLASH_LEN) * e,
        dot_offset: DOT_OFFSET,
        dot_radius: DOT_RADIUS * dots,
    }
}

/// The 4.2s loop: hold "/" 0.5s, morph in 1.1s, hold "÷" 0.9s, morph out 1.1s, rest 0.6s.
pub fn loop_at(t_secs: f32) -> LoopFrame {
    let t = t_secs.rem_euclid(LOOP_SECS);
    let (progress, phase) = if t < 0.5 {
        (0.0, MarkPhase::Idle)
    } else if t < 1.6 {
        ((t - 0.5) / 1.1, MarkPhase::Starting)
    } else if t < 2.5 {
        (1.0, MarkPhase::Working)
    } else if t < 3.6 {
        (1.0 - (t - 2.5) / 1.1, MarkPhase::Finishing)
    } else {
        (0.0, MarkPhase::Idle)
    };
    LoopFrame { progress, phase, state: state_at(progress) }
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p divisi-brand motion 2>&1 | tail -12`
Expected: 7 tests pass.

- [ ] **Step 6: Write the failing raster, glyph and token tests, then implement**

`crates/divisi-brand/src/raster.rs`:
```rust
//! Half-block (`▀ ▄ █`) rendering of the mark for terminals. A terminal cell is
//! about 0.6 wide per 1.0 tall and a half-block row is 0.5 tall, so one
//! sub-row is `48 / (rows * 2)` world units tall and one column is 1.2x that wide.

use crate::motion::MarkState;

const WORLD: f32 = 48.0;
const CELL_ASPECT: f32 = 1.2;
const BAR_HALF: f32 = 3.1;

fn seg_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((px - (ax + t * dx)).powi(2) + (py - (ay + t * dy)).powi(2)).sqrt()
}

/// Renders `state` into `rows` character rows (columns follow from the aspect ratio).
pub fn render(state: &MarkState, rows: usize) -> Vec<String> {
    let sub = rows * 2;
    let uy = WORLD / sub as f32;
    let ux = uy * CELL_ASPECT;
    let cols = (WORLD / ux).round() as usize;
    let reach = uy * 0.5;
    let rad = state.angle_deg.to_radians();
    let (hx, hy) = (rad.cos() * state.bar_len / 2.0, rad.sin() * state.bar_len / 2.0);
    let dot_r = state.dot_radius.max(0.0);

    let on = |x: usize, y: usize| -> bool {
        let (px, py) = ((x as f32 + 0.5) * ux, (y as f32 + 0.5) * uy);
        if seg_dist(px, py, 24.0 - hx, 24.0 - hy, 24.0 + hx, 24.0 + hy) <= BAR_HALF.max(reach) {
            return true;
        }
        dot_r > 0.05 && {
            let r = dot_r.max(reach);
            let (up, down) = (24.0 - state.dot_offset, 24.0 + state.dot_offset);
            ((px - 24.0).powi(2) + (py - up).powi(2)).sqrt() <= r || ((px - 24.0).powi(2) + (py - down).powi(2)).sqrt() <= r
        }
    };

    (0..rows)
        .map(|r| {
            (0..cols)
                .map(|c| match (on(c, 2 * r), on(c, 2 * r + 1)) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::state_at;

    #[test]
    fn full_size_is_25_columns_by_15_rows() {
        let lines = render(&state_at(1.0), 15);
        assert_eq!(lines.len(), 15);
        assert!(lines.iter().all(|l| l.chars().count() == 25));
    }

    #[test]
    fn slash_leans_up_and_to_the_right() {
        let lines = render(&state_at(0.0), 15);
        for row in 0..7 {
            let left: String = lines[row].chars().take(10).collect();
            assert!(left.trim().is_empty(), "row {row} has ink on the left: {left:?}");
        }
        assert!(lines[1].chars().skip(15).any(|c| c != ' '), "top right should be inked");
        assert!(lines[13].chars().take(10).any(|c| c != ' '), "bottom left should be inked");
    }

    #[test]
    fn obelus_has_a_bar_and_two_dots() {
        let lines = render(&state_at(1.0), 15);
        let bar = lines[7].chars().filter(|c| *c != ' ').count();
        assert!(bar >= 14, "bar row too short: {bar}");
        assert_ne!(lines[3].chars().nth(12), Some(' '), "upper dot missing");
        assert_ne!(lines[11].chars().nth(12), Some(' '), "lower dot missing");
    }
}
```
`crates/divisi-brand/src/glyph.rs`:
```rust
//! Single-cell rendering for headers and status lines: a spinning bar that
//! resolves into "÷". `ascii` is the fallback for `NO_COLOR` / dumb terminals.

pub fn glyph(progress: f32) -> char {
    let r = progress.clamp(0.0, 1.0);
    if r >= 0.85 {
        return '÷';
    }
    ['/', '|', '\\', '-'][((r / 0.85) * 4.0) as usize % 4]
}

pub fn ascii(progress: f32) -> &'static str {
    if progress < 0.5 {
        "/"
    } else {
        "-:-"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spins_then_resolves() {
        assert_eq!(glyph(0.0), '/');
        assert_eq!(glyph(0.25), '|');
        assert_eq!(glyph(0.5), '\\');
        assert_eq!(glyph(0.75), '-');
        assert_eq!(glyph(1.0), '÷');
    }

    #[test]
    fn ascii_fallback() {
        assert_eq!(ascii(0.0), "/");
        assert_eq!(ascii(1.0), "-:-");
    }
}
```
`crates/divisi-brand/src/tokens.rs`:
```rust
pub const GRAPHITE: &str = "#16181d";
pub const BONE: &str = "#f2f2f0";
pub const SIGNAL: &str = "#ff5a1f";
pub const GO: &str = "#3ddc97";
pub const CAUTION: &str = "#ffd23f";
pub const FAULT: &str = "#ff2d55";

pub const ALL: [(&str, &str); 6] =
    [("graphite", GRAPHITE), ("bone", BONE), ("signal", SIGNAL), ("go", GO), ("caution", CAUTION), ("fault", FAULT)];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_matches_the_constants() {
        let css = include_str!("../assets/tokens.css");
        for (name, hex) in ALL {
            assert!(css.contains(&format!("--{name}:{hex}")), "tokens.css is missing --{name}:{hex}");
        }
    }

    #[test]
    fn svgs_use_signal_and_the_48_grid() {
        for svg in [include_str!("../assets/mark-obelus.svg"), include_str!("../assets/mark-slash.svg")] {
            assert!(svg.contains(SIGNAL));
            assert!(svg.contains("viewBox=\"0 0 48 48\""));
        }
    }
}
```
Assets:

`crates/divisi-brand/assets/tokens.css`:
```css
:root{--graphite:#16181d;--bone:#f2f2f0;--signal:#ff5a1f;--go:#3ddc97;--caution:#ffd23f;--fault:#ff2d55;}
```
`crates/divisi-brand/assets/mark-obelus.svg`:
```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48" width="48" height="48"><g fill="#ff5a1f"><circle cx="24" cy="10" r="4.6"/><rect x="8" y="21" width="32" height="6" rx="1.5"/><circle cx="24" cy="38" r="4.6"/></g></svg>
```
`crates/divisi-brand/assets/mark-slash.svg`:
```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48" width="48" height="48"><g fill="#ff5a1f" transform="translate(24 24) rotate(-62)"><rect x="-22" y="-3" width="44" height="6" rx="1.5"/></g></svg>
```
`crates/divisi-brand/src/frames.rs` is written in Task 10; for now create it with `//! Notch frame data; see Task 10.` so the crate compiles.

Run: `cargo test -p divisi-brand 2>&1 | tail -20`
Expected: all tests pass (motion 7, raster 3, glyph 2, tokens 2).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/divisi-brand
git commit -m "feat: add divisi-brand crate with mark geometry, motion spec and renderers"
```

---

### Task 3: Rename tooling and guard

**Files:**
- Create: `scripts/rename_divisi.py`
- Create: `scripts/legacy-allowlist.txt`

**Interfaces:**
- Produces (used by Tasks 4-12): `python3 scripts/rename_divisi.py <stage> [--dry-run]` with stages `crates`, `idents`, `names`, `env`, `paths`, `prose`, `check`. `check` exits 1 and lists `path:line: text` for every legacy token not covered by `scripts/legacy-allowlist.txt`.

- [ ] **Step 1: Write the script**

`scripts/rename_divisi.py`:
```python
#!/usr/bin/env python3
"""Staged single -> divisi rename. Explicit tables only; never a blind sed.

Usage: scripts/rename_divisi.py <crates|idents|names|env|paths|prose|check> [--dry-run]
"""
import fnmatch
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Never rewritten: vendored code, build output, lockfile, history, and the docs
# that describe the rename itself.
SKIP_PREFIXES = ("vendor/", "target/", ".git/", "docs/adr/", "docs/superpowers/")
SKIP_FILES = {"Cargo.lock", "CHANGELOG.md", "scripts/rename_divisi.py", "scripts/legacy-allowlist.txt"}
# Files that must keep legacy names on purpose (created after the rename stages).
STAGE_SKIP = {
    "env": {"crates/divisi-core/src/env.rs"},
}

# Persisted identifiers that keep their legacy names in 0.24.0 (spec section 4).
PROTECT = [
    re.compile(p)
    for p in (
        r"naviNBRuas/SingleCLI",
        r"single-redact-master-key",
        r'"single-pool"',
        r'"single-agent"',
        r'"single-(?:openrouter|nvidia|gemini|widgetsai)"',
        r"single-\{",
        r'"single_memory"',
    )
]

CRATE_DIRS = {
    "single-core": "divisi-core",
    "single-protocol": "divisi-protocol",
    "single-runtime": "divisi-runtime",
    "single-agent-sdk": "divisi-agent-sdk",
    "single-native-agent": "divisi-native-agent",
    "single-cli": "divisi-cli",
    "single-tui": "divisi-tui",
    "single-web": "divisi-web",
    "single-lsp": "divisi-lsp",
    "single-notch": "divisi-notch",
    "single-mcp": "divisi-gateway",
    "singlecli-mcp": "divisi-mcp",
}
# Cargo.toml tokens: crate dirs/packages plus the bins and the native-agent package.
CARGO_TOKENS = dict(CRATE_DIRS, **{"single-runtimed": "divisid", "single-agent": "divisi-agent"})

IDENTS = [
    (r"\bsingle_agent_sdk\b", "divisi_agent_sdk"),
    (r"\bsingle_native_agent\b", "divisi_native_agent"),
    (r"\bsingle_core\b", "divisi_core"),
    (r"\bsingle_protocol\b", "divisi_protocol"),
    (r"\bsingle_runtime\b", "divisi_runtime"),
    (r"\bsingle_notch\b", "divisi_notch"),
    (r"\bsingle_web\b", "divisi_web"),
    (r"\bsingle_tui\b", "divisi_tui"),
    (r"\bsingle_lsp\b", "divisi_lsp"),
    (r"\bsingle_mcp\b", "divisi_gateway"),
    (r"\bsinglecli_mcp\b", "divisi_mcp"),
    (r"\bSingleDirs\b", "DivisiDirs"),
    (r"\bSingleCliServer\b", "DivisiServer"),
    (r"\bSingleAgentAdapter\b", "DivisiAgentAdapter"),
]

NAME_PAIRS = [
    ("single-runtimed", "divisid"),
    ("singlecli-mcp", "divisi-mcp"),
    ("single-mcp", "divisi-gateway"),
    ("single-lsp", "divisi-lsp"),
    ("single-notch", "divisi-notch"),
    ("single-cli", "divisi-cli"),
    ("single-tui", "divisi-tui"),
    ("single-web", "divisi-web"),
    ("single-runtime", "divisi-runtime"),
    ("single-protocol", "divisi-protocol"),
    ("single-core", "divisi-core"),
    ("single-agent-sdk", "divisi-agent-sdk"),
    ("single-native-agent", "divisi-native-agent"),
    ("single.db", "divisi.db"),
]

def token(old):
    return re.compile(r"(?<![\w.-])" + re.escape(old) + r"(?![\w-])")

PATHS = [
    (re.compile(r"\.config/single(?![\w-])"), ".config/divisi"),
    (re.compile(r'join\("single"\)'), 'join("divisi")'),
]

PROSE = [
    (re.compile(r"SingleCLI"), "divisi"),
    (re.compile(r"(?<=`)single(?=[` ])"), "divisi"),
    (re.compile(r"(?<=\$ )single(?= )"), "divisi"),
    (re.compile(r'(?<=")single(?=",)'), "divisi"),
    (re.compile(r'Command::new\("single"\)'), 'Command::new("divisi")'),
    (re.compile(r'name = "single"'), 'name = "divisi"'),
]

LEGACY = re.compile(
    r"SingleCLI|singlecli"
    r"|(?<![\w.-])single-(?:core|protocol|runtimed|runtime|agent-sdk|native-agent|cli|tui|web|lsp|notch|mcp)(?![\w-])"
    r"|\bSINGLE_[A-Z]|\.config/single(?![\w-])"
    r"|\bSingle(?:Dirs|CliServer|AgentAdapter)\b"
    r"|\bsingle_(?:core|protocol|runtime|agent_sdk|native_agent|notch|web|tui|lsp|mcp)\b"
)

def tracked():
    out = subprocess.run(["git", "ls-files"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.split("\n")
    return [p for p in out if p and not p.startswith(SKIP_PREFIXES) and p not in SKIP_FILES]

def read(rel):
    try:
        return (ROOT / rel).read_text(encoding="utf-8")
    except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
        return None

def protect(text):
    saved = []
    def stash(m):
        saved.append(m.group(0))
        return f"\x00{len(saved) - 1}\x00"
    for pat in PROTECT:
        text = pat.sub(stash, text)
    return text, saved

def restore(text, saved):
    return re.sub(r"\x00(\d+)\x00", lambda m: saved[int(m.group(1))], text)

def rewrite(files, transform, dry):
    changed = 0
    for rel in files:
        text = read(rel)
        if text is None:
            continue
        new = transform(rel, text)
        if new != text:
            changed += 1
            print(("would change " if dry else "changed ") + rel)
            if not dry:
                (ROOT / rel).write_text(new, encoding="utf-8")
    print(f"{changed} file(s) {'would change' if dry else 'changed'}")

def git_mv(src, dst, dry):
    if not (ROOT / src).exists():
        return
    print(f"mv {src} -> {dst}")
    if not dry:
        subprocess.run(["git", "mv", src, dst], cwd=ROOT, check=True)

def stage_crates(dry):
    for old, new in CRATE_DIRS.items():
        git_mv(f"crates/{old}", f"crates/{new}", dry)
    git_mv("crates/divisi-runtime/src/bin/single-runtimed.rs", "crates/divisi-runtime/src/bin/divisid.rs", dry)
    git_mv("extensions/gnome-shell/single-notch@nbr.company", "extensions/gnome-shell/divisi-notch@nbr.company", dry)
    tokens = [(token(o), n) for o, n in CARGO_TOKENS.items()]
    def cargo(rel, text):
        for pat, new in tokens:
            text = pat.sub(new, text)
        return text
    rewrite([p for p in tracked() if p.endswith("Cargo.toml")], cargo, dry)

def stage_idents(dry):
    pats = [(re.compile(p), n) for p, n in IDENTS]
    def f(rel, text):
        for pat, new in pats:
            text = pat.sub(new, text)
        return text
    rewrite([p for p in tracked() if p.endswith((".rs", ".md", ".toml"))], f, dry)

def regex_stage(pairs, dry, skip=()):
    def f(rel, text):
        text, saved = protect(text)
        for pat, new in pairs:
            text = pat.sub(new, text)
        return restore(text, saved)
    rewrite([p for p in tracked() if p not in skip], f, dry)

def stage_check():
    allow = []
    allow_file = ROOT / "scripts/legacy-allowlist.txt"
    if allow_file.exists():
        allow = [l.split("\t")[0].strip() for l in allow_file.read_text().splitlines() if l.strip() and not l.startswith("#")]
    bad = 0
    for rel in subprocess.run(["git", "ls-files"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.split("\n"):
        if not rel or rel.startswith(("vendor/", "target/")) or rel in SKIP_FILES or rel == "Cargo.lock":
            continue
        if any(fnmatch.fnmatch(rel, g) for g in allow):
            continue
        text = read(rel)
        if text is None:
            continue
        for n, line in enumerate(text.splitlines(), 1):
            stripped, _ = protect(line)
            if LEGACY.search(stripped):
                bad += 1
                print(f"{rel}:{n}: {line.strip()[:140]}")
    print(f"{bad} legacy reference(s) outside the allowlist")
    return 1 if bad else 0

def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    dry = "--dry-run" in sys.argv
    if len(args) != 1:
        sys.exit(__doc__)
    stage = args[0]
    if stage == "crates":
        stage_crates(dry)
    elif stage == "idents":
        stage_idents(dry)
    elif stage == "names":
        regex_stage([(token(o), n) for o, n in NAME_PAIRS], dry)
    elif stage == "env":
        regex_stage([(re.compile(r"\bSINGLE_(?=[A-Z])"), "DIVISI_")], dry, skip=STAGE_SKIP["env"])
    elif stage == "paths":
        regex_stage(PATHS, dry)
    elif stage == "prose":
        regex_stage(PROSE, dry)
    elif stage == "check":
        sys.exit(stage_check())
    else:
        sys.exit(__doc__)

if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Write the allowlist**

`scripts/legacy-allowlist.txt` (glob, TAB, reason; these files legitimately mention legacy names):
```
# Files that must keep legacy names. Phase B rows are removed when those files ship.
CHANGELOG.md	history
crates/divisi-core/src/env.rs	SINGLE_* adoption (Task 5)
crates/divisi-core/src/migrate.rs	old config dir and db (Task 6)
crates/divisi-core/src/shim.rs	deprecation shims (Task 7)
crates/divisi-cli/src/bin/*	single* shim binaries (Task 7)
crates/divisi-cli/src/migrate_cmd.rs	pre-rename install migration (Task 8)
crates/divisi-cli/src/notch_proc.rs	legacy notch UUID (Task 8)
install.sh	Phase B: ships with the first divisi release
.github/workflows/*	Phase B: release artifact names
docker/*	Phase B: image names
README.md	install section keeps the old release names until Phase B
```

- [ ] **Step 3: Verify the check stage sees the current mess**

Run: `python3 scripts/rename_divisi.py check | tail -3; echo "exit=$?"`
Expected: a large count (hundreds) of legacy references and a non-zero exit from the script (`python3 scripts/rename_divisi.py check >/dev/null; echo $?` prints `1`).

- [ ] **Step 4: Commit**

```bash
chmod +x scripts/rename_divisi.py
git add scripts/rename_divisi.py scripts/legacy-allowlist.txt
git commit -m "build: add staged single-to-divisi rename script and legacy-name guard"
```

---

### Task 4: Run the rename

**Files:** every tracked file the stages touch; no hand edits except the fixes in Step 3.

- [ ] **Step 1: Dry-run each stage and review**

Run:
```bash
for s in crates idents names; do echo "== $s"; python3 scripts/rename_divisi.py $s --dry-run | tail -5; done
```
Expected: `crates` lists 12 `mv` lines plus `divisid.rs` and the extension dir, then Cargo.toml files; `idents` and `names` list `.rs`/docs files. Eyeball that no path under `vendor/`, `docs/superpowers/` or `CHANGELOG.md` appears.

- [ ] **Step 2: Apply crates, idents and names, then build**

Run:
```bash
python3 scripts/rename_divisi.py crates
python3 scripts/rename_divisi.py idents
python3 scripts/rename_divisi.py names
cargo build --workspace 2>&1 | grep -E "^(error|warning: unused)" | head -20; cargo build --workspace 2>&1 | tail -2
```
Expected: `Finished`. `Cargo.lock` regenerates on this build.

- [ ] **Step 3: Fix what the script cannot know**

Run: `cargo build --workspace 2>&1 | grep -E "^error" -A6 | head -40` and fix each error by hand. Known cases to check even if the build passes:
```bash
grep -n 'name = ' crates/divisi-cli/Cargo.toml crates/divisi-runtime/Cargo.toml crates/divisi-native-agent/Cargo.toml | head
git grep -nE 'divisi-runtimed|divisid' -- crates/divisi-cli/src/daemon.rs | head -5
```
Expected: `[[bin]] name = "single"` in `crates/divisi-cli/Cargo.toml` may still read `single` (Cargo.toml tokens only map `single-*`); change it to `divisi` by hand (the `prose` stage in Step 6 also handles `name = "single"`). The native-agent package must read `divisi-agent`. The daemon lookup in `daemon.rs` must now be `divisid`.

- [ ] **Step 4: Test and commit the crate and identifier rename**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED|error\[" | sort | uniq -c`
Expected: all `ok`, count matches the Task 1 baseline.
```bash
git add -A
git commit -m "refactor: rename crates, binaries and identifiers from single to divisi"
```

- [ ] **Step 5: Env vars**

Run:
```bash
python3 scripts/rename_divisi.py env
cargo test --workspace 2>&1 | grep -E "^test result|FAILED" | sort | uniq -c
git add -A
git commit -m "refactor: rename SINGLE_ environment variables to DIVISI_"
```
Expected: tests pass. (Legacy `SINGLE_*` support is added in Task 5.)

- [ ] **Step 6: Paths and prose**

Run:
```bash
python3 scripts/rename_divisi.py paths
cargo test --workspace 2>&1 | grep -E "^test result|FAILED" | sort | uniq -c
git add -A
git commit -m "refactor: move default config directory name to divisi"
python3 scripts/rename_divisi.py prose
cargo test --workspace 2>&1 | grep -E "^test result|FAILED" | sort | uniq -c
git add -A
git commit -m "docs: rename SingleCLI to divisi in code comments, help text and docs"
```
Expected: tests pass both times. A test that asserts on help text or the `single` clap name is the likeliest failure; fix by updating the expected string.

- [ ] **Step 7: Run the guard**

Run: `python3 scripts/rename_divisi.py check`
Expected: exit 0 or a short list. For each remaining hit decide: rename by hand (then commit `refactor: rename remaining single references`), or add a path with a reason to `scripts/legacy-allowlist.txt`. Do not add a path whose legacy mention is a bug.

Also inspect what the rename left of the residual English word: `git grep -niE '\bsingle\b' -- crates | grep -vE "single (source|line|value|flag|entry|file|process|thread|instance|command|provider|agent|task|goal|account|key|char|shot|run|pass|test|match|one|item)" | head -30` and fix any leftover product references by hand.

- [ ] **Step 8: Commit any guard fixes**

```bash
git add -A
git commit -m "chore: allowlist intentional legacy names in the rename guard"
```
(Skip if nothing changed.)

---

### Task 5: Legacy `SINGLE_*` adoption

**Files:**
- Create: `crates/divisi-core/src/env.rs`
- Modify: `crates/divisi-core/src/lib.rs` (add `pub mod env;`)
- Modify: the `main` of each binary (see Step 5)

**Interfaces:**
- Produces: `divisi_core::env::plan_adoption(vars: &[(String, String)]) -> Vec<(String, String, String)>` (tuples of `(old_name, new_name, value)`), `divisi_core::env::adopt_legacy_env()`.

- [ ] **Step 1: Write the failing tests**

`crates/divisi-core/src/env.rs`:
```rust
//! Honour the pre-rename `SINGLE_*` environment variables when the matching
//! `DIVISI_*` is unset. Removed together with the `single*` shims.

const OLD: &str = "SINGLE_";
const NEW: &str = "DIVISI_";

#[cfg(test)]
mod tests {
    use super::*;

    fn v(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, val)| (k.to_string(), val.to_string())).collect()
    }

    #[test]
    fn adopts_a_legacy_var_when_the_new_one_is_absent() {
        let plan = plan_adoption(&v(&[("SINGLE_CONFIG_DIR", "/x")]));
        assert_eq!(plan, vec![("SINGLE_CONFIG_DIR".into(), "DIVISI_CONFIG_DIR".into(), "/x".into())]);
    }

    #[test]
    fn the_new_name_wins() {
        let plan = plan_adoption(&v(&[("SINGLE_CONFIG_DIR", "/old"), ("DIVISI_CONFIG_DIR", "/new")]));
        assert!(plan.is_empty());
    }

    #[test]
    fn ignores_unrelated_and_bare_prefix() {
        let plan = plan_adoption(&v(&[("PATH", "/bin"), ("SINGLE_", "x"), ("MY_SINGLE_X", "y")]));
        assert!(plan.is_empty());
    }
}
```
Add `pub mod env;` to `crates/divisi-core/src/lib.rs`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p divisi-core env:: 2>&1 | tail -8`
Expected: FAIL: `cannot find function plan_adoption`.

- [ ] **Step 3: Implement**

Insert above the tests in `env.rs`:
```rust
use std::collections::HashSet;
use std::io::IsTerminal;

/// `(old_name, new_name, value)` for every `SINGLE_x` whose `DIVISI_x` is not set.
pub fn plan_adoption(vars: &[(String, String)]) -> Vec<(String, String, String)> {
    let present: HashSet<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
    vars.iter()
        .filter_map(|(k, v)| {
            let rest = k.strip_prefix(OLD).filter(|r| !r.is_empty())?;
            let new = format!("{NEW}{rest}");
            (!present.contains(new.as_str())).then(|| (k.clone(), new, v.clone()))
        })
        .collect()
}

/// Copies legacy `SINGLE_*` variables to their `DIVISI_*` names. Call first
/// thing in `main`, before any thread reads the environment.
pub fn adopt_legacy_env() {
    let vars: Vec<(String, String)> =
        std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect();
    let plan = plan_adoption(&vars);
    for (_, new, value) in &plan {
        std::env::set_var(new, value);
    }
    if !plan.is_empty() && std::io::stderr().is_terminal() {
        let names: Vec<&str> = plan.iter().map(|(old, _, _)| old.as_str()).collect();
        eprintln!("divisi: {} still work but are deprecated; rename them to DIVISI_*", names.join(", "));
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p divisi-core env:: 2>&1 | tail -6`
Expected: 3 passed.

- [ ] **Step 5: Call it from every binary**

Run: `git grep -n "fn main" -- 'crates/*/src/main.rs' 'crates/*/src/bin/*.rs'`
Expected mains: `divisi-notch`, `divisi-native-agent`, `divisi-gateway`, `divisi-mcp`, `divisi-runtime/src/bin/divisid.rs`, `divisi-lsp`, `divisi-cli`. Insert as the first statement of each `main` body:
```rust
    divisi_core::env::adopt_legacy_env();
```
Each of those crates must depend on `divisi-core`; check with `grep -L divisi-core crates/divisi-{notch,native-agent,gateway,mcp,runtime,lsp,cli}/Cargo.toml` and add `divisi-core = { path = "../divisi-core" }` under `[dependencies]` for any listed.

- [ ] **Step 6: Build, test, commit**

Run: `cargo build --workspace 2>&1 | tail -2 && cargo test --workspace 2>&1 | grep -E "^test result|FAILED" | sort | uniq -c`
Expected: build finishes, tests pass.
```bash
git add -A
git commit -m "feat: honour legacy SINGLE_ environment variables during the rename window"
```

---

### Task 6: Config-dir migration

**Files:**
- Create: `crates/divisi-core/src/migrate.rs`
- Modify: `crates/divisi-core/src/lib.rs` (`pub mod migrate;`)
- Modify: `crates/divisi-core/src/paths.rs` (`DivisiDirs::discover`, `db_path`)

**Interfaces:**
- Produces: `divisi_core::migrate::{Outcome, migrate_config_dir(old: &Path, new: &Path) -> anyhow::Result<Outcome>, resolve_default_root(config_home: &Path) -> PathBuf}`.
- Consumes: none from earlier tasks. `DivisiDirs::discover()` (already renamed in Task 4) calls `resolve_default_root`.

- [ ] **Step 1: Write the failing tests**

`crates/divisi-core/src/migrate.rs`:
```rust
//! Moves a pre-rename `~/.config/single` onto `~/.config/divisi`.
//! A move, not a conversion: on-disk formats are unchanged; only
//! `state/single.db` is renamed to `state/divisi.db`.

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn legacy_tree(root: &std::path::Path) {
        fs::create_dir_all(root.join("state")).unwrap();
        fs::write(root.join("config.toml"), "x = 1").unwrap();
        fs::write(root.join("state/single.db"), "db").unwrap();
        fs::write(root.join("state/single.db-wal"), "wal").unwrap();
    }

    #[test]
    fn fresh_install_resolves_to_the_new_dir() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(resolve_default_root(home.path()), home.path().join("divisi"));
        assert!(!home.path().join("divisi").exists(), "must not create anything");
    }

    #[test]
    fn migrates_and_leaves_a_symlink() {
        let home = tempfile::tempdir().unwrap();
        legacy_tree(&home.path().join("single"));
        let root = resolve_default_root(home.path());
        assert_eq!(root, home.path().join("divisi"));
        assert_eq!(fs::read_to_string(root.join("config.toml")).unwrap(), "x = 1");
        assert_eq!(fs::read_to_string(root.join("state/divisi.db")).unwrap(), "db");
        assert_eq!(fs::read_to_string(root.join("state/divisi.db-wal")).unwrap(), "wal");
        assert!(!root.join("state/single.db").exists());
        assert!(fs::symlink_metadata(home.path().join("single")).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(home.path().join("single/config.toml")).unwrap(), "x = 1");
    }

    #[test]
    fn second_run_is_a_no_op() {
        let home = tempfile::tempdir().unwrap();
        legacy_tree(&home.path().join("single"));
        resolve_default_root(home.path());
        assert_eq!(migrate_config_dir(&home.path().join("single"), &home.path().join("divisi")).unwrap(), Outcome::NothingToDo);
    }

    #[test]
    fn refuses_to_move_a_live_daemons_dir_and_keeps_using_the_old_one() {
        let home = tempfile::tempdir().unwrap();
        let old = home.path().join("single");
        legacy_tree(&old);
        let _listener = std::os::unix::net::UnixListener::bind(old.join("state/runtime.sock")).unwrap();
        assert!(migrate_config_dir(&old, &home.path().join("divisi")).is_err());
        assert_eq!(resolve_default_root(home.path()), old);
        assert!(old.join("state/single.db").exists(), "nothing moved");
    }

    #[test]
    fn both_dirs_present_is_reported_not_merged() {
        let home = tempfile::tempdir().unwrap();
        legacy_tree(&home.path().join("single"));
        fs::create_dir_all(home.path().join("divisi")).unwrap();
        assert_eq!(migrate_config_dir(&home.path().join("single"), &home.path().join("divisi")).unwrap(), Outcome::BothExist);
    }
}
```
Add `pub mod migrate;` to `lib.rs`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p divisi-core migrate:: 2>&1 | tail -8`
Expected: FAIL: `cannot find function resolve_default_root`.

- [ ] **Step 3: Implement**

Insert above the tests in `migrate.rs`:
```rust
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// New dir exists (or nothing legacy is left to move).
    NothingToDo,
    /// Neither dir exists: a fresh install.
    Fresh,
    Migrated,
    /// Both dirs are real directories; left untouched for the user to reconcile.
    BothExist,
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

fn daemon_live(old: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(old.join("state/runtime.sock")).is_ok()
}

fn rename_db(root: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let from = root.join(format!("state/single.db{suffix}"));
        let to = root.join(format!("state/divisi.db{suffix}"));
        if from.exists() && !to.exists() {
            std::fs::rename(&from, &to).with_context(|| format!("renaming {}", from.display()))?;
        }
    }
    Ok(())
}

pub fn migrate_config_dir(old: &Path, new: &Path) -> Result<Outcome> {
    if new.exists() {
        return Ok(if old.exists() && !is_symlink(old) { Outcome::BothExist } else { Outcome::NothingToDo });
    }
    if !old.exists() {
        return Ok(Outcome::Fresh);
    }
    if daemon_live(old) {
        bail!("the runtime daemon is running against {}; stop it, then rerun", old.display());
    }
    std::fs::rename(old, new).with_context(|| format!("moving {} to {}", old.display(), new.display()))?;
    rename_db(new)?;
    std::os::unix::fs::symlink(new, old).with_context(|| format!("linking {} to {}", old.display(), new.display()))?;
    Ok(Outcome::Migrated)
}

/// The config root to use when no `DIVISI_CONFIG_DIR` override is set.
/// Migrates lazily. If migration is refused, keeps using the old dir so the
/// CLI never starts from an empty config next to a populated legacy one.
pub fn resolve_default_root(config_home: &Path) -> PathBuf {
    let (old, new) = (config_home.join("single"), config_home.join("divisi"));
    match migrate_config_dir(&old, &new) {
        Ok(Outcome::BothExist) => {
            eprintln!("divisi: both {} and {} exist; using the latter. Merge or remove the old one.", old.display(), new.display());
            new
        }
        Ok(_) => new,
        Err(e) => {
            eprintln!("divisi: config migration skipped: {e:#}");
            if old.exists() { old } else { new }
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p divisi-core migrate:: 2>&1 | tail -10`
Expected: 5 passed.

- [ ] **Step 5: Wire into `DivisiDirs`**

In `crates/divisi-core/src/paths.rs`, replace the body of `discover` after the `DIVISI_CONFIG_DIR` early return so it reads:
```rust
    pub fn discover() -> Result<Self> {
        if let Ok(dir) = std::env::var("DIVISI_CONFIG_DIR") {
            return Ok(Self { root: PathBuf::from(dir) });
        }
        let base = directories::BaseDirs::new()
            .context("could not determine home/config directory for this platform")?;
        Ok(Self { root: crate::migrate::resolve_default_root(base.config_dir()) })
    }
```
and change `db_path` to return `self.state_dir().join("divisi.db")`.

- [ ] **Step 6: Full test, guard, commit**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED" | sort | uniq -c; python3 scripts/rename_divisi.py check | tail -2`
Expected: tests pass; guard exit 0 (`migrate.rs` is allowlisted). If a test asserts the old `single.db` name, update it to `divisi.db`.
```bash
git add -A
git commit -m "feat: migrate the legacy config directory and database name on first run"
```

---

### Task 7: `single*` shims

**Files:**
- Create: `crates/divisi-core/src/shim.rs`
- Modify: `crates/divisi-core/src/lib.rs` (`pub mod shim;`)
- Create: `crates/divisi-cli/src/bin/single.rs`, `single-runtimed.rs`, `single-mcp.rs`, `singlecli-mcp.rs`, `single-lsp.rs`, `single-notch.rs`

**Interfaces:**
- Produces: `divisi_core::shim::{deprecation_line(old: &str, new: &str) -> String, resolve_target(current_exe: &Path, new: &str) -> PathBuf, run(old: &str, new: &str) -> !}`.

- [ ] **Step 1: Write the failing tests**

`crates/divisi-core/src/shim.rs`:
```rust
//! Deprecated `single*` command names that exec their divisi replacement.
//! Removed in 0.26.0 (two minor versions after the rename).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deprecation_line_names_both() {
        let l = deprecation_line("single", "divisi");
        assert!(l.contains("single") && l.contains("`divisi`") && l.contains("0.26"), "{l}");
    }

    #[test]
    fn prefers_the_sibling_binary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("divisi"), "").unwrap();
        assert_eq!(resolve_target(&dir.path().join("single"), "divisi"), dir.path().join("divisi"));
    }

    #[test]
    fn falls_back_to_path_lookup() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_target(&dir.path().join("single"), "divisi"), std::path::PathBuf::from("divisi"));
    }
}
```
Add `pub mod shim;` to `lib.rs`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p divisi-core shim:: 2>&1 | tail -6`
Expected: FAIL: `cannot find function deprecation_line`.

- [ ] **Step 3: Implement**

Insert above the tests in `shim.rs`:
```rust
use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

pub fn deprecation_line(old: &str, new: &str) -> String {
    format!("{old}: renamed, use `{new}`. This alias is removed in 0.26.")
}

/// The replacement binary next to this one when it exists, else a bare name for PATH lookup.
pub fn resolve_target(current_exe: &Path, new: &str) -> PathBuf {
    if let Some(dir) = current_exe.parent() {
        let candidate = dir.join(new);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(new)
}

/// Replaces this process with `new`, forwarding all arguments. The
/// deprecation line is only printed on a terminal so daemons and agents that
/// still spawn the old name are not spammed.
pub fn run(old: &str, new: &str) -> ! {
    if std::io::stderr().is_terminal() {
        eprintln!("{}", deprecation_line(old, new));
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let target = resolve_target(&exe, new);
    let err = std::process::Command::new(&target).args(std::env::args_os().skip(1)).exec();
    eprintln!("{old}: cannot run {}: {err}", target.display());
    std::process::exit(127)
}
```

- [ ] **Step 4: Add the six shim binaries**

Add a seventh shim for the renamed native agent: `single-agent.rs` → `("single-agent", "divisi-agent")`. Each file is one line of logic. `crates/divisi-cli/src/bin/single.rs`:
```rust
fn main() {
    divisi_core::shim::run("single", "divisi")
}
```
Same shape for the others, changing only the two strings: `single-runtimed.rs` → `("single-runtimed", "divisid")`, `single-mcp.rs` → `("single-mcp", "divisi-gateway")`, `singlecli-mcp.rs` → `("singlecli-mcp", "divisi-mcp")`, `single-lsp.rs` → `("single-lsp", "divisi-lsp")`, `single-notch.rs` → `("single-notch", "divisi-notch")`. Cargo auto-discovers `src/bin/*.rs`; the explicit `[[bin]] name = "divisi"` for `src/main.rs` stays.

- [ ] **Step 5: Verify**

Run:
```bash
cargo test -p divisi-core shim:: 2>&1 | tail -5
cargo build --workspace 2>&1 | tail -2
ls target/debug | grep -E '^(single|singlecli|divisi|divisid)[-a-z]*$'
DIVISI_CONFIG_DIR=$(mktemp -d) target/debug/single --version 2>&1 | head -3
```
Expected: 3 tests pass; build finishes; six `single*` binaries plus the `divisi*` ones are listed; `single --version` prints the divisi version (the deprecation line appears only on a terminal).

- [ ] **Step 6: Commit**

```bash
python3 scripts/rename_divisi.py check | tail -1
git add -A
git commit -m "feat: add deprecated single command aliases that exec their divisi replacements"
```

---

### Task 8: `divisi migrate` (systemd unit and notch extension)

**Files:**
- Create: `crates/divisi-cli/src/migrate_cmd.rs`
- Modify: `crates/divisi-cli/src/main.rs` (add `mod migrate_cmd;`, a `Migrate` variant in `enum Command`, and its match arm)
- Modify: `crates/divisi-cli/src/notch_proc.rs` (legacy-extension migration)

**Interfaces:**
- Produces: `migrate_cmd::rewrite_unit(text: &str) -> String`, `migrate_cmd::run(apply: bool) -> anyhow::Result<()>`; `notch_proc::strip_uuid(current: &str, uuid: &str) -> String`, `notch_proc::gnome_migrate_legacy(apply: bool) -> anyhow::Result<Option<String>>`.

- [ ] **Step 1: Write the failing tests**

`crates/divisi-cli/src/migrate_cmd.rs`:
```rust
//! `divisi migrate`: move a pre-rename install onto divisi names.
//! Dry run by default; `--apply` changes things. Never starts the daemon.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_a_legacy_unit() {
        let unit = "[Unit]\nDescription=SingleCLI runtime daemon\n[Service]\nExecStart=%h/.local/bin/single-runtimed\nEnvironment=SINGLE_CONFIG_DIR=%h/.config/single\nEnvironment=PATH=%h/.opencode/bin:%h/.local/bin:/usr/bin\n";
        let out = rewrite_unit(unit);
        assert!(out.contains("Description=divisi runtime daemon"), "{out}");
        assert!(out.contains("ExecStart=%h/.local/bin/divisid"), "{out}");
        assert!(out.contains("DIVISI_CONFIG_DIR=%h/.config/divisi"), "{out}");
        assert!(out.contains("PATH=%h/.opencode/bin:%h/.local/bin:/usr/bin"), "PATH must be untouched: {out}");
        assert!(!out.contains("single"), "{out}");
    }
}
```
In `notch_proc.rs`, add to its existing test module (create `#[cfg(test)] mod tests { use super::*; ... }` if none exists):
```rust
    #[test]
    fn strip_uuid_removes_only_that_entry() {
        assert_eq!(strip_uuid("['a@b', 'single-notch@nbr.company']", "single-notch@nbr.company"), "['a@b']");
        assert_eq!(strip_uuid("@as []", "single-notch@nbr.company"), "[]");
        assert_eq!(strip_uuid("['x@y']", "single-notch@nbr.company"), "['x@y']");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p divisi-cli 2>&1 | grep -E "error|FAILED" | head -5`
Expected: FAIL to compile: `cannot find function rewrite_unit` / `strip_uuid`.

- [ ] **Step 3: Implement `rewrite_unit` and `run`**

Insert above the tests in `migrate_cmd.rs`:
```rust
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

pub fn rewrite_unit(text: &str) -> String {
    text.replace("SingleCLI", "divisi")
        .replace("single-runtimed", "divisid")
        .replace(".config/single", ".config/divisi")
        .replace("SINGLE_", "DIVISI_")
}

fn systemctl(args: &[&str]) -> Option<String> {
    let out = Command::new("systemctl").arg("--user").args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn unit_dir() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config/systemd/user"))
}

pub fn run(apply: bool) -> Result<()> {
    let verb = if apply { "" } else { "would " };

    // 1. Config dir: DivisiDirs::discover() migrates lazily, so touching it is enough.
    if apply {
        let dirs = divisi_core::paths::DivisiDirs::discover()?;
        println!("config dir: {}", dirs.root().display());
    } else {
        println!("config dir: would move ~/.config/single to ~/.config/divisi on first run (needs the daemon stopped)");
    }

    // 2. systemd unit: copy to divisid.service, keep the enabled/disabled state, never start.
    let dir = unit_dir()?;
    let old_unit = dir.join("single-runtimed.service");
    if old_unit.exists() {
        let new_unit = dir.join("divisid.service");
        let was_enabled = systemctl(&["is-enabled", "single-runtimed"]).as_deref() == Some("enabled");
        println!("systemd: {verb}write {} (old unit was {})", new_unit.display(), if was_enabled { "enabled" } else { "disabled" });
        if apply {
            std::fs::write(&new_unit, rewrite_unit(&std::fs::read_to_string(&old_unit)?))?;
            if was_enabled {
                systemctl(&["disable", "single-runtimed"]);
            }
            systemctl(&["daemon-reload"]);
            if was_enabled {
                systemctl(&["enable", "divisid"]);
            }
            println!("systemd: old unit left in place at {}; delete it once you are happy", old_unit.display());
        }
    } else {
        println!("systemd: no single-runtimed.service found, nothing to do");
    }

    // 3. GNOME notch extension.
    if crate::notch_proc::is_gnome() {
        match crate::notch_proc::gnome_migrate_legacy(apply)? {
            Some(msg) => println!("notch: {msg}"),
            None => println!("notch: no legacy extension installed"),
        }
    }
    if !apply {
        println!("\nDry run. Rerun with --apply to make these changes. The daemon is never started.");
    }
    Ok(())
}
```

- [ ] **Step 4: Implement the notch migration**

In `crates/divisi-cli/src/notch_proc.rs`, add next to the other GNOME functions:
```rust
const LEGACY_GNOME_UUID: &str = "single-notch@nbr.company";

/// `current` is the raw `gsettings get org.gnome.shell enabled-extensions` value.
pub fn strip_uuid(current: &str, uuid: &str) -> String {
    let inner = current.trim().trim_start_matches("@as ").trim_start_matches('[').trim_end_matches(']');
    let kept: Vec<&str> = inner.split(',').map(str::trim).filter(|s| !s.is_empty() && s.trim_matches('\'') != uuid).collect();
    format!("[{}]", kept.join(", "))
}

/// Disables and removes the pre-rename extension, then enables the new one if the old one was on.
pub fn gnome_migrate_legacy(apply: bool) -> Result<Option<String>> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let legacy = std::path::PathBuf::from(home).join(".local/share/gnome-shell/extensions").join(LEGACY_GNOME_UUID);
    if !legacy.exists() {
        return Ok(None);
    }
    if !apply {
        return Ok(Some(format!("would disable and remove {LEGACY_GNOME_UUID}, then enable {GNOME_UUID}")));
    }
    let settings = std::process::Command::new("gsettings").args(["get", "org.gnome.shell", "enabled-extensions"]).output()?;
    let current = String::from_utf8_lossy(&settings.stdout).into_owned();
    let was_enabled = current.contains(LEGACY_GNOME_UUID);
    let _ = gnome_extensions(&["disable", LEGACY_GNOME_UUID]);
    std::fs::remove_dir_all(&legacy).with_context(|| format!("removing {}", legacy.display()))?;
    if was_enabled {
        let cleaned = strip_uuid(&current, LEGACY_GNOME_UUID);
        let _ = std::process::Command::new("gsettings").args(["set", "org.gnome.shell", "enabled-extensions", &cleaned]).status();
        gnome_enable()?;
    }
    Ok(Some(format!("removed {LEGACY_GNOME_UUID}{}; log out and back in for GNOME to load {GNOME_UUID}", if was_enabled { ", enabled the new extension" } else { "" })))
}
```

- [ ] **Step 5: Add the subcommand**

In `crates/divisi-cli/src/main.rs`: add `mod migrate_cmd;` beside the other `mod` lines; add to `enum Command` (beside `Doctor`):
```rust
    /// Move a pre-rename (SingleCLI) install onto divisi names: config dir, systemd unit, notch extension.
    Migrate {
        /// Apply the changes (default: print what would change).
        #[arg(long)]
        apply: bool,
    },
```
and add to the `match cli.command` dispatch in `main` (beside `Command::Doctor { fix } => {`):
```rust
        Command::Migrate { apply } => migrate_cmd::run(apply)?,
```
If `divisi_core::paths` is not a public module path, use the path `grep -n "pub mod paths" crates/divisi-core/src/lib.rs` shows.

- [ ] **Step 6: Verify and commit**

Run:
```bash
cargo test -p divisi-cli 2>&1 | grep -E "test result|FAILED"
DIVISI_CONFIG_DIR=$(mktemp -d) cargo run -q -p divisi-cli -- migrate 2>&1 | tail -8
```
Expected: tests pass; the dry run prints the config, systemd and notch lines and ends with "Dry run". Nothing on disk changes (`systemctl --user is-enabled single-runtimed` is still `disabled`).
```bash
git add -A
git commit -m "feat: add divisi migrate for the systemd unit and legacy notch extension"
```

---

### Task 9: TUI mark and `divisi logo`

**Files:**
- Modify: `crates/divisi-tui/Cargo.toml` (add `divisi-brand`)
- Modify: `crates/divisi-tui/src/ui.rs` (header)
- Modify: `crates/divisi-tui/src/app.rs` (`mark_progress`)
- Create: `crates/divisi-cli/src/logo.rs`
- Modify: `crates/divisi-cli/Cargo.toml` (add `divisi-brand`), `crates/divisi-cli/src/main.rs` (`Logo` subcommand)

**Interfaces:**
- Consumes: `divisi_brand::{glyph::glyph, glyph::ascii, motion::{loop_at, state_at, LOOP_SECS}, raster::render}` (Task 2).
- Produces: `divisi_tui::ui::mark_char(busy: bool, t_secs: f32, no_motion: bool) -> char`; `logo::print_logo(animate: bool)`.

- [ ] **Step 1: Write the failing test**

Append to `crates/divisi-tui/src/ui.rs`:
```rust
#[cfg(test)]
mod mark_tests {
    use super::mark_char;

    #[test]
    fn idle_shows_the_slash() {
        assert_eq!(mark_char(false, 1.0, false), '/');
        assert_eq!(mark_char(false, 1.0, true), '/');
    }

    #[test]
    fn busy_holds_the_obelus_and_spins_between() {
        assert_eq!(mark_char(true, 2.0, false), '÷');
        assert_eq!(mark_char(true, 0.2, false), '/');
        assert_ne!(mark_char(true, 1.05, false), '÷');
    }

    #[test]
    fn no_motion_skips_the_spin() {
        assert_eq!(mark_char(true, 1.05, true), '÷');
        assert_eq!(mark_char(true, 0.2, true), '÷');
    }

    #[test]
    fn dumb_terminals_get_plain_ascii() {
        assert_eq!(mark_text(false, 0.0, false, true), "/");
        assert_eq!(mark_text(true, 2.0, false, true), "-:-");
        assert_eq!(mark_text(true, 2.0, false, false), "÷");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p divisi-tui mark_tests 2>&1 | tail -6`
Expected: FAIL: `cannot find function mark_char`.

- [ ] **Step 3: Implement the glyph and header**

Add `divisi-brand = { path = "../divisi-brand" }` under `[dependencies]` in `crates/divisi-tui/Cargo.toml`. In `ui.rs` add:
```rust
/// The header mark: "/" at rest, spinning into "÷" while work is running.
/// `no_motion` (env `NO_MOTION`) shows a static "÷" while busy.
pub fn mark_char(busy: bool, t_secs: f32, no_motion: bool) -> char {
    if !busy {
        return '/';
    }
    if no_motion {
        return '÷';
    }
    divisi_brand::glyph::glyph(divisi_brand::motion::loop_at(t_secs).progress)
}

/// `mark_char` as text, or the plain-ASCII fallback (`/`, `-:-`) for `TERM=dumb`.
pub fn mark_text(busy: bool, t_secs: f32, no_motion: bool, ascii: bool) -> String {
    if ascii {
        return divisi_brand::glyph::ascii(if busy { 1.0 } else { 0.0 }).to_string();
    }
    mark_char(busy, t_secs, no_motion).to_string()
}
```
In `crates/divisi-tui/src/app.rs`, inside `impl App` next to `spinner_frame`:
```rust
    /// Header mark: animated while a refresh is in flight or a goal is running.
    pub fn mark(&self) -> String {
        let busy = self.loading || self.goals.as_ref().is_some_and(|g| g.iter().any(|x| x.status == "running"));
        let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
        crate::ui::mark_text(busy, self.started_at.elapsed().as_secs_f32(), std::env::var_os("NO_MOTION").is_some(), dumb)
    }
```
Confirm the running status string first: `git grep -n '"running"' -- crates/divisi-runtime/src | head -3`; if goals use a different value, use that value in the closure. In `draw_header`, prefix each of the three header strings with the mark: change the three `format!`/`to_string()` texts so they begin `format!("{}  divisi  ·  …", app.mark(), …)` (keep existing fields), e.g.
```rust
            format!(
                "{}  divisi  ·  profile: {}  ·  agents: {}/{} detected  ·  v{}",
                app.mark(), s.active_profile, s.agents_detected, s.agents_known, s.version
            ),
```
and `(None, true)` → `format!("{}  divisi  ·  {} connecting…", app.mark(), app.spinner_frame())`, `(None, false)` → `format!("{}  divisi  ·  runtime unreachable", app.mark())`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p divisi-tui 2>&1 | grep -E "test result|FAILED|error"`
Expected: tests pass, including 3 `mark_tests`.

- [ ] **Step 4b: Prompt prefix**

The spec puts `/` in front of prompts. Find any text-input prompt in the TUI: `git grep -nE 'Prompt|prompt|input' -- crates/divisi-tui/src/ui.rs | head`. If an input line is rendered, prefix it with `/ ` styled `ACCENT`; commit that with this task. If the TUI has no text input today, add nothing: the prefix arrives with the conversational ACP input, which is specified separately. Record which case applied in the commit body-free message you write in Step 6.

- [ ] **Step 5: Add `divisi logo`**

Add `divisi-brand = { path = "../divisi-brand" }` to `crates/divisi-cli/Cargo.toml`. Create `crates/divisi-cli/src/logo.rs`:
```rust
//! `divisi logo [--animate]`: the half-block mark, static or one animated loop.

use divisi_brand::{motion, raster};
use std::io::IsTerminal;

const ROWS: usize = 15;

pub fn print_logo(animate: bool) {
    let color = std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();
    let paint = |s: &str| if color { format!("\x1b[38;2;255;90;31m{s}\x1b[0m") } else { s.to_string() };
    if !animate || !std::io::stdout().is_terminal() || std::env::var_os("NO_MOTION").is_some() {
        for line in raster::render(&motion::state_at(1.0), ROWS) {
            println!("{}", paint(&line));
        }
        return;
    }
    let start = std::time::Instant::now();
    print!("\x1b[?25l");
    let mut first = true;
    while start.elapsed().as_secs_f32() < motion::LOOP_SECS {
        let frame = motion::loop_at(start.elapsed().as_secs_f32());
        if !first {
            print!("\x1b[{ROWS}A");
        }
        first = false;
        for line in raster::render(&frame.state, ROWS) {
            println!("{}", paint(&line));
        }
        std::thread::sleep(std::time::Duration::from_millis(33));
    }
    print!("\x1b[?25h");
}
```
In `main.rs` add `mod logo;`, this variant to `enum Command`:
```rust
    /// Print the divisi mark (`--animate` plays one loop).
    Logo {
        #[arg(long)]
        animate: bool,
    },
```
and the arm `Command::Logo { animate } => logo::print_logo(animate),`.

- [ ] **Step 6: Verify and commit**

Run: `cargo run -q -p divisi-cli -- logo | head -16; NO_COLOR=1 cargo run -q -p divisi-cli -- logo --animate | head -3`
Expected: a 25x15 half-block ÷; the second prints the static mark (no animation when stdout is piped).
```bash
git add -A
git commit -m "feat: show the animated divisi mark in the TUI header and add divisi logo"
```

---

### Task 10: Notch mark and frame data

**Files:**
- Modify: `crates/divisi-brand/src/frames.rs`, create `crates/divisi-brand/examples/emit_frames.rs`
- Create: `extensions/gnome-shell/divisi-notch@nbr.company/mark-frames.json` (generated)
- Modify: `extensions/gnome-shell/divisi-notch@nbr.company/extension.js`, `metadata.json`
- Modify: `crates/divisi-cli/src/notch_proc.rs` (install the frames file)

**Interfaces:**
- Consumes: `divisi_brand::motion::{loop_at, LoopFrame, LOOP_SECS}`.
- Produces: `frames::loop_frames(fps: u32) -> Vec<LoopFrame>`; `frames::to_json(fps: u32, frames: &[LoopFrame]) -> String`; the JSON shape `{"fps":30,"loop_secs":4.2,"frames":[{"a":-62.0,"l":44.0,"o":14.0,"r":0.0,"p":"idle"}, …]}` (`a` angle degrees, `l` bar length, `o` dot offset, `r` dot radius, `p` phase).

- [ ] **Step 1: Write the failing tests**

`crates/divisi-brand/src/frames.rs`:
```rust
//! Precomputed loop frames for surfaces that cannot run Rust (the GNOME Shell
//! extension). The committed `mark-frames.json` must equal `to_json(30, ...)`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thirty_fps_gives_126_frames() {
        assert_eq!(loop_frames(30).len(), 126);
    }

    #[test]
    fn first_frame_is_the_idle_slash() {
        let f = &loop_frames(30)[0];
        assert_eq!(f.phase.as_str(), "idle");
        assert_eq!(f.state.angle_deg, -62.0);
    }

    #[test]
    fn json_carries_every_frame() {
        let frames = loop_frames(30);
        let json = to_json(30, &frames);
        assert!(json.starts_with("{\"fps\":30,\"loop_secs\":4.2,\"frames\":["));
        assert_eq!(json.matches("\"a\":").count(), 126);
        assert!(json.contains("\"p\":\"working\""));
    }

    #[test]
    fn committed_notch_frames_are_current() {
        let committed = include_str!("../../../extensions/gnome-shell/divisi-notch@nbr.company/mark-frames.json");
        assert_eq!(committed.trim_end(), to_json(30, &loop_frames(30)), "regenerate with: cargo run -p divisi-brand --example emit_frames");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p divisi-brand frames:: 2>&1 | tail -6`
Expected: FAIL: `cannot find function loop_frames` (and the include path missing).

- [ ] **Step 3: Implement**

Insert above the tests in `frames.rs`:
```rust
use crate::motion::{loop_at, LoopFrame, LOOP_SECS};

pub fn loop_frames(fps: u32) -> Vec<LoopFrame> {
    let n = (LOOP_SECS * fps as f32).round() as u32;
    (0..n).map(|i| loop_at(i as f32 / fps as f32)).collect()
}

pub fn to_json(fps: u32, frames: &[LoopFrame]) -> String {
    let body: Vec<String> = frames
        .iter()
        .map(|f| {
            format!(
                "{{\"a\":{:.2},\"l\":{:.2},\"o\":{:.2},\"r\":{:.2},\"p\":\"{}\"}}",
                f.state.angle_deg,
                f.state.bar_len,
                f.state.dot_offset,
                f.state.dot_radius,
                f.phase.as_str()
            )
        })
        .collect();
    format!("{{\"fps\":{fps},\"loop_secs\":{LOOP_SECS},\"frames\":[{}]}}", body.join(","))
}
```
`crates/divisi-brand/examples/emit_frames.rs`:
```rust
fn main() {
    use divisi_brand::frames::{loop_frames, to_json};
    println!("{}", to_json(30, &loop_frames(30)));
}
```
Note `{:.2}` formats `-62` as `-62.00`, so the test's `starts_with` and the JS reader both work on that shape; the test in Step 1 only checks counts and the header.

- [ ] **Step 4: Generate the file and pass the tests**

Run:
```bash
cargo run -q -p divisi-brand --example emit_frames > extensions/gnome-shell/divisi-notch@nbr.company/mark-frames.json
cargo test -p divisi-brand 2>&1 | grep -E "test result|FAILED"
```
Expected: all divisi-brand tests pass (motion 7, raster 3, glyph 2, tokens 2, frames 4).

- [ ] **Step 5: Ship the frames with the extension**

In `crates/divisi-cli/src/notch_proc.rs`, add beside the other `include_str!` constants:
```rust
const GNOME_FRAMES: &str = include_str!("../../../extensions/gnome-shell/divisi-notch@nbr.company/mark-frames.json");
```
and in `gnome_install`, after the `metadata.json` write:
```rust
    std::fs::write(dir.join("mark-frames.json"), GNOME_FRAMES)?;
```

- [ ] **Step 6: Draw the mark in the extension**

In `extension.js` add near the other imports: `import Cairo from 'cairo';` (skip if a `Cairo` import already exists). Add these constants after `NOTABLE_PEEK_MS`:
```js
const SIGNAL = [0xff / 255, 0x5a / 255, 0x1f / 255];
const MARK_SIZE = 20;
```
In `enable()`, after `this._cancellable = new Gio.Cancellable();` add:
```js
        this._frames = null;
        this._markArea = null;
        this._markRunning = false;
        try {
            const [, bytes] = Gio.File.new_for_path(`${this.path}/mark-frames.json`).load_contents(null);
            this._frames = JSON.parse(new TextDecoder().decode(bytes));
        } catch (e) {
            console.warn(`divisi notch: mark-frames.json unavailable (${e.message}); using a static mark`);
        }
        this._addTimer(33, () => {
            if (this._markArea?.mapped && this._markRunning)
                this._markArea.queue_repaint();
            return GLib.SOURCE_CONTINUE;
        });
```
Add these methods beside `_dot`:
```js
    _markFrame() {
        const f = this._frames;
        if (!f)
            return {a: 0, l: 32, o: 14, r: 4.6};
        if (!this._markRunning)
            return f.frames[0];
        if (!St.Settings.get().enable_animations)
            return f.frames[Math.floor(2.0 * f.fps)];
        const t = (GLib.get_monotonic_time() / 1e6) % f.loop_secs;
        return f.frames[Math.min(f.frames.length - 1, Math.floor(t * f.fps))];
    }

    _markWidget(running) {
        this._markRunning = running;
        const area = new St.DrawingArea({width: MARK_SIZE, height: MARK_SIZE, y_align: Clutter.ActorAlign.CENTER});
        area.connect('repaint', () => {
            const cr = area.get_context();
            const [w, h] = area.get_surface_size();
            const fr = this._markFrame();
            cr.scale(Math.min(w, h) / 48, Math.min(w, h) / 48);
            cr.setSourceRGBA(SIGNAL[0], SIGNAL[1], SIGNAL[2], 1);
            cr.save();
            cr.translate(24, 24);
            cr.rotate(fr.a * Math.PI / 180);
            cr.rectangle(-fr.l / 2, -3, fr.l, 6);
            cr.fill();
            cr.restore();
            if (fr.r > 0.05) {
                cr.arc(24, 24 - fr.o, fr.r, 0, 2 * Math.PI);
                cr.fill();
                cr.arc(24, 24 + fr.o, fr.r, 0, 2 * Math.PI);
                cr.fill();
            }
            cr.$dispose();
        });
        area.connect('destroy', () => {
            if (this._markArea === area)
                this._markArea = null;
        });
        this._markArea = area;
        return area;
    }
```
In `_renderCard`, replace `header.add_child(this._dot(color, 11, running));` with `header.add_child(this._markWidget(running));`. The label already reads `divisi` from the prose stage (verify: `grep -n "'divisi'" extension.js`). In `disable()` set `this._frames = this._markArea = null;`. In `metadata.json` set `"name": "divisi Notch"` and description "Edge notch showing divisi pool health. Hover to reveal, click to expand." (the `uuid` was renamed by the `names` stage; confirm it reads `divisi-notch@nbr.company`).

- [ ] **Step 7: Verify in a real shell**

Read `/home/navinbruas/.claude/projects/-home-navinbruas-Projects-The-Company/memory/project_notch_gnome_extension.md` and run its headless test recipe against the new extension directory (install with `cargo run -q -p divisi-cli -- notch enable` only inside a throwaway `HOME`, never the real one, since Task 13 handles the live install). Expected: the extension loads without errors in `journalctl --user -g gnome-shell -n 50`, the card header shows the mark, and with an idle snapshot the mark is a static "/". If the headless recipe cannot render Cairo, report that as unverified rather than claiming it works.

- [ ] **Step 8: Guard and commit**

Run: `cargo test --workspace 2>&1 | grep -E "test result|FAILED" | sort | uniq -c; python3 scripts/rename_divisi.py check | tail -1`
Expected: tests pass; guard exit 0.
```bash
git add -A
git commit -m "feat: animate the divisi mark in the GNOME notch from shared frame data"
```

---

### Task 11: Docs, endorsement line, version 0.24.0

**Files:**
- Modify: `README.md`, `CHANGELOG.md`, `docs/architecture.md`, `docs/FAQ.md`, `docs/install-methods.md`, `crates/divisi-cli/src/main.rs` (`about`), `Cargo.toml` (`version`)

- [ ] **Step 1: README**

Rewrite the first paragraph and the Install section of `README.md`:
- Lead with: `# divisi` then `**The Division of AI agents.** divisi is the orchestration layer for AI agents: you state a goal, it splits the work into parts, assigns each to whichever agent or model fits, and keeps the score.` and keep the existing capability description below it (already renamed by the prose stage).
- Add an "Install" note: `divisi binaries ship with the first divisi release. Until then build from source: cargo install --path crates/divisi-cli` (leave the existing curl line under a heading "Current release (still named single)").
- Add a "Renamed from SingleCLI" section: the name map from the spec, the `single*` alias removal in 0.26, `DIVISI_*` and `~/.config/divisi`, and `divisi migrate`.
- Add the footer line: `An independent project, endorsed by NBR Company. Built by Navin B. Ruas (naviNBRuas).`

- [ ] **Step 2: Endorsement in `--version` and `about`**

In `crates/divisi-cli/src/main.rs`, change the `about = "…"` on the `Cli` derive to the line below and add `long_version` next to it (`-V` stays the bare version, `--version` carries the endorsement):
```rust
    about = "divisi — the orchestration layer for AI agents. An independent project, endorsed by NBR Company.",
    long_version = concat!(env!("CARGO_PKG_VERSION"), "\nAn independent project, endorsed by NBR Company. Built by Navin B. Ruas (naviNBRuas)."),
```

- [ ] **Step 3: CHANGELOG and version**

Add on top of `CHANGELOG.md` (above the 0.23.0 entry, matching its heading style):
```markdown
## 0.24.0

Renamed from SingleCLI to divisi.

- Binaries: `single` → `divisi`, `single-runtimed` → `divisid`, `singlecli-mcp` → `divisi-mcp`, `single-mcp` → `divisi-gateway`, `single-lsp` → `divisi-lsp`, `single-notch` → `divisi-notch`. The old names remain as aliases until 0.26.
- Crates `single-*` → `divisi-*`; environment `SINGLE_*` → `DIVISI_*` (old names still read); config `~/.config/single` → `~/.config/divisi` (moved automatically on first run, symlink left behind).
- New: the divisi mark (÷ active, / idle), `divisi logo`, the animated TUI and notch mark, `divisi migrate`.
- Unchanged for now: agent ids `single-pool` and `single-agent`, the opencode provider namespace, the Qdrant collection `single_memory`, the `single-redact-master-key` keyring entry, the MCP permission resource prefix `singlecli:<tool>` (saved rules keep working), and the `__singlecli_secrets__.toml` backup manifest name.
- Integration sync now also removes the old `single-mcp` and `singlecli-mcp` entries from agent configs.
```
Set `version = "0.24.0"` in `[workspace.package]` of the root `Cargo.toml`.

- [ ] **Step 4: Verify and commit**

Run: `cargo build --workspace 2>&1 | tail -2; cargo run -q -p divisi-cli -- --version; python3 scripts/rename_divisi.py check | tail -1`
Expected: build finishes; `--version` prints `0.24.0` then the endorsement line; guard exit 0.
```bash
git add -A
git commit -m "docs: document the divisi rename and bump version to 0.24.0"
```

---

### Task 12: Final verification

**Files:** none changed.

- [ ] **Step 1: Everything green**

Run:
```bash
cargo build --workspace --release 2>&1 | tail -2
cargo test --workspace 2>&1 | grep -E "^test result|FAILED" | sort | uniq -c
cargo clippy --workspace --all-targets 2>&1 | grep -E "^(warning|error)" | sort | uniq -c > /tmp/clippy-after.txt; diff /tmp/clippy-baseline.txt /tmp/clippy-after.txt
python3 scripts/rename_divisi.py check | tail -1
git status --short | wc -l
```
Expected: release build finishes; all tests `ok` (count at least the Task 1 baseline plus the new tests); the clippy `diff` shows no new warning categories (rename-only differences in message text are fine); guard exit 0; working tree clean.

- [ ] **Step 2: Smoke the binaries in an isolated home**

Run:
```bash
export DIVISI_CONFIG_DIR=$(mktemp -d)
target/release/divisi --version
target/release/divisi doctor 2>&1 | head -8
target/release/divisi logo | head -3
SINGLE_CONFIG_DIR=$(mktemp -d) target/release/single --version
```
Expected: version `0.24.0`; doctor runs without panicking; the logo prints; the `single` alias runs `divisi`.

- [ ] **Step 3: Report**

Summarise for the user: commits on `rebrand/divisi` (`git log --oneline main..HEAD`), test count vs baseline, anything unverified (notch rendering if the headless recipe could not draw), and that Task 13 and Phase B are waiting.

---

### Task 13: Cutover (only on the user's explicit go)

This task changes the live machine. Do not start it without an explicit instruction. It is the first time any new binary touches the real `~/.config/single`.

**Files:** none in the repo.

- [ ] **Step 1: Back up and confirm the daemon is down**

Run:
```bash
cp -a ~/.config/single ~/.config/single.pre-divisi-backup
systemctl --user is-active single-runtimed
```
Expected: `inactive`. If active, stop and ask.

- [ ] **Step 2: Install the binaries**

Run:
```bash
cargo build --workspace --release
for b in divisi divisid divisi-mcp divisi-gateway divisi-lsp divisi-notch divisi-agent single single-runtimed single-mcp singlecli-mcp single-lsp single-notch single-agent; do install -m755 target/release/$b ~/.local/bin/$b; done
divisi --version
```
Expected: `divisi 0.24.0`. The `single*` names are now the aliases.

- [ ] **Step 3: Migrate**

Run: `divisi migrate` (review), then `divisi migrate --apply`.
Expected: config dir moved to `~/.config/divisi` with a symlink at `~/.config/single`; `divisid.service` written, autostart still disabled; the legacy notch extension removed if present. `ls -la ~/.config | grep -E "single|divisi"` shows both.

- [ ] **Step 4: Re-sync integrations and check for stale entries**

Run: `divisi install-integrations --real-home --yes`, then
```bash
grep -rn "single-mcp\|singlecli-mcp\|single-runtimed" ~/.claude.json ~/.config/zed/settings.json ~/.claude/settings.json 2>/dev/null | head
```
Expected: no stale references (the sync itself now removes the old `single-mcp` and `singlecli-mcp` entries; zed and `~/.claude/settings.json` are not covered by it). Remove any that remain by hand (they still work through the aliases, but should not linger). Update `~/.claude/CLAUDE.md` names (`single-mcp`, `singlecli-mcp`, `single install-integrations`) and the memory notes that name the old binaries.

- [ ] **Step 5: Resume the daemon deliberately**

Run: `systemctl --user enable --now divisid && divisi daemon status && divisi notch enable`
Expected: daemon running; notch loads after a re-login on GNOME/Wayland. Only then restart epic dispatch.

---

## Phase B (deferred until the forge exists; not part of this plan's execution)

Waits for E28 (local forge), then GitHub and GitLab.

- Repo rename to `naviNBRuas/divisi`; update `repository` in the root `Cargo.toml`, `install.sh` `REPO`, README URLs, and the extension `metadata.json` `url`.
- `install.sh` and `.github/workflows/release.yml`: asset names `singlecli-<target>.tar.gz` → `divisi-<target>.tar.gz`, binaries copied (`divisi`, `divisid`, `divisi-gateway`, plus the `single*` aliases), then remove the `install.sh`, `.github/workflows/*`, `docker/*` and `README.md` rows from `scripts/legacy-allowlist.txt`. Docker image `singlecli-agents` → `divisi-agents`.
- Tag `v0.24.0` and publish the release; publish `divisi.nbr.company` and the `the-division` redirect; note the old install URL in the README.
- Remove the `single*` aliases and `SINGLE_*` adoption in 0.26.0.
- Follow-up migration for the kept persisted names (`single-pool`, `single-agent`, opencode `single-<provider>` namespace, `single_memory`, `single-redact-master-key`).
