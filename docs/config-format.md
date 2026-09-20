# divisi configuration format

This documents the grammar of divisi's own configuration files — the
layer files (global config, profiles, project config) and the `[chat]`
section of the global config. It does **not** cover agent config that
divisi *writes into* on an agent's behalf (`~/.claude.json`,
`~/.codex/config.toml`, `opencode.jsonc`, …) — that's the per-agent
`formats/` side of `divisi-agent-sdk`.

Reference implementation: `crates/divisi-core/src/config.rs` (layers and
merging), `crates/divisi-core/src/profile.rs` (profile switching),
`crates/divisi-runtime/src/assistant/gate.rs` (`[chat]`).

## File locations

| Layer | Path | Format |
|---|---|---|
| Global | `~/.config/divisi/config.toml` | TOML |
| Profile | `~/.config/divisi/profiles/<name>.toml` | TOML |
| Project | `<project-root>/.single/config.yaml` | YAML |

The config root is `~/.config/divisi`, or the value of the
`DIVISI_CONFIG_DIR` environment variable when set (used by tests and
anyone wanting an isolated instance). The first time the root is
resolved, divisi migrates a pre-rename `~/.config/single` onto it as a
move (`crates/divisi-core/src/migrate.rs`) — formats are unchanged by
migration; only `state/single.db` is renamed to `state/divisi.db`.

Only the two TOML layers take part in profile selection; the project
layer is loaded only when a project root is being resolved (and only if
`.single/config.yaml` exists).

## Layer schema

Every layer is the same shape (`ConfigLayer` in `config.rs`):

```toml
# -- TOML layers (global config.toml, profiles/<name>.toml) --
profile = "work"          # top-level, string   (see below)

[agents]
enabled = ["claude", "codex"]

[mcp]
enabled = ["git", "memory"]
```

```yaml
# -- YAML layer (.single/config.yaml) --
agents:
  enabled: ["claude"]
mcp:
  enabled: []
```

Every field is optional in every layer. A layer may use only the keys it
wants; everything else is omitted.

### Keys and value types

| Key | Type | Meaning |
|---|---|---|
| `profile` | string | Name of the active profile (global layer only; see below) |
| `agents.enabled` | array of strings | Agent ids enabled in this layer |
| `mcp.enabled` | array of strings | MCP server names enabled in this layer |

Supported value types come straight from the two parsers: strings
(always double-quoted in TOML) and arrays of strings for the `enabled`
lists. The layer schema itself has no integer/boolean/float fields —
those appear only in the `[chat]` section below. No layer key takes a
nested table beyond `[agents]` and `[mcp]`.

### The `profile` key

`profile` is meaningful only in the **global** layer: it names which
profile file under `profiles/` to load next in the precedence chain.
`divisi profile use <name>` writes it (rewriting the global file
specifically, preserving its other contents). A `profile` key in a
profile or project layer has no effect.

## The `[chat]` section

The global config file additionally carries a `[chat]` table, read
directly (bypassing the layer merge — it never comes from a profile or
project layer) by the daemon's assistant risk gate:

```toml
[chat]
confirm_expiry_secs = 1800    # positive integer; seconds a confirmation stays valid
fanout_cap = 12               # positive integer; plan size that pauses a goal for approval
risky_verbs = ["push", "publish", "deploy", "release", "delete", "drop", "force", "wipe", "destroy"]
default_mode = "auto"         # "auto" | "plan" | "careful" | "dry"
```

| Key | Type | Default | Rules |
|---|---|---|---|
| `confirm_expiry_secs` | integer | `1800` | Only a value `> 0` is accepted |
| `fanout_cap` | integer | `12` | Only a value `> 0` is accepted |
| `risky_verbs` | array of strings | the list above | Stored lower-cased; non-string elements are dropped |
| `default_mode` | string | `"auto"` | Must be one of `auto`/`plan`/`careful`/`dry` |

## Comments

Comments are allowed in both formats, verbatim from the host parser:

- **TOML** (`config.toml`, `profiles/<name>.toml`): `#` to end of line,
  inside or outside tables.
- **YAML** (`.single/config.yaml`): `#` to end of line.

Comments are not preserved by writers. `divisi profile use` reparses and
re-serializes the global file, so any hand-written comments in
`config.toml` are lost on the next profile switch (same tradeoff as the
documented agent-format writers).

## Nested sections

Two levels only: a top-level key (`profile`) or a named table (`[agents]`,
`[mcp]`, `[chat]`), each holding scalar/array members. There are no
deeper nesting levels in any layer.

## Precedence and merging

Layers are merged in the order **defaults → global → profile → project**;
later layers win (`config.rs::merge_layers`). Merging is per-field and
non-destructive:

- A field that a layer **omits** falls through to the previous layer,
  and finally to the built-in defaults (an omitted `enabled` is `None`;
  an explicitly empty list `[]` is a real value).
- A field a layer *does* specify replaces the whole previous value.
  `enabled` lists are replaced, never concatenated — so `enabled = []`
  is a deliberate "nothing enabled here", not an inheritance.

Resolved defaults when nothing sets a field:

| Resolved field | Default |
|---|---|
| `active_profile` | `"default"` |
| `enabled_agents` | `["claude", "codex", "opencode", "agy", "perplexity"]` |
| `enabled_mcp` | `[]` |

So, in practice: a project layer can enable MCP servers for one repo
without touching the global list, a profile can narrow the agent list
without losing the global MCP list, and so on.

A profile named by the global layer that has no file is silently skipped
(falls through to defaults), the same as a profile name that doesn't
exist.

## Error semantics

Two different readers, two different philosophies.

### Layer files (`ConfigLayer`)

`config.rs::load_layer` is strict — any problem other than a missing
file is a hard error that aborts config resolution:

| Situation | Outcome |
|---|---|
| File does not exist | Layer skipped (`Ok(None)`); resolution continues with remaining layers |
| File exists but unreadable (I/O error) | Error: `reading <path>: <io error>` |
| File exists but doesn't parse | Error: `parsing <path> as TOML: <detail>` (or `… as YAML: …`) |
| Value of a known key has the wrong type | Parse error (same as above) |
| Unknown key | **Ignored silently** — `ConfigLayer` does not use `deny_unknown_fields`, so the file may carry keys divisi doesn't yet understand (e.g. incumbent sections), and forward-compatible extra tables are fine. Nothing is warned about; it's simply not selected |

Because unknown keys are ignored, a `config.toml` containing `[chat]`
and `[agents]` parses cleanly as a layer and as a chat source — the two
readers are additive views of the same file.

Notable per-layer behaviour:

- **Global absent, profile named** — impossible by construction: a
  profile is only loaded when the global layer exists *and* carries a
  `profile` key.
- **Profile named but file absent** — silent no-op; no error.
- **Project layer** — only read when a project root is being resolved.
  Without a project root, the project layer never participates.

### The `[chat]` section

`ChatConfig::load` is deliberately tolerant — the chat gate must never
take divisi down because of a config typo. Every failure mode falls back
to defaults instead of erroring:

| Situation | Outcome |
|---|---|
| `config.toml` missing, unreadable, or not valid TOML | All chat defaults |
| No `[chat]` table | All chat defaults |
| `confirm_expiry_secs`/`fanout_cap` `<= 0` or non-integer | That key falls back to its default |
| `risky_verbs` present | Used as-is, lower-cased, non-strings dropped |
| `default_mode` not one of the four modes | Falls back to `"auto"` |

A broken `[chat]` value never produces an error and never breaks a chat
run — the opposite contract from the layer files above.

## Examples

Complete global file:

```toml
# ~/.config/divisi/config.toml
profile = "work"

[agents]
enabled = ["claude", "codex"]

[mcp]
enabled = ["git", "memory", "sequential-thinking"]

[chat]
confirm_expiry_secs = 900
fanout_cap = 8
risky_verbs = ["deploy", "destroy"]
default_mode = "plan"
```

Overriding profile:

```toml
# ~/.config/divisi/profiles/work.toml
[agents]
enabled = ["claude"]          # this machine's work machine only runs claude
# mcp omitted -> inherits the global list
```

Repo-scoped override:

```yaml
# <repo>/.single/config.yaml
mcp:
  enabled: ["git"]
```