# FAQ

## Does SingleCLI touch my real `~/.claude`, `~/.codex`, etc.?

Not after the first run. Each agent gets its own SingleCLI-managed home
under `~/.config/single/homes/<agent>/`, bootstrapped from the real one
exactly once. Every `task run`, `install-integrations`, `plugin sync`,
`provider sync`, and `account capture`/`use` after that operates only
inside that isolated copy. See `docs/architecture.md`'s "Isolation"
section.

The one deliberate exception is `--real-home`, for the specific case of
using an agent *through* SingleCLI to configure your actual machine
(dotfiles, installing tools). It's off by default and prints a warning
when used, since it gives the agent real credentials and file access.

## Do I need API keys to try it?

No. `single doctor`, `single agent list`, and most read/inspect commands
work with nothing configured. Commands that install software or sync real
config (`single setup --yes`, `single install-integrations --yes`) touch
real files by design — that's their job.

## Can I run two accounts of the same agent at once?

Yes — `single account capture <agent> <label>` snapshots a logged-in
session, and `single task run --account <name>` runs against a
materialized, isolated `$HOME` for that account, so multiple accounts of
the same agent run concurrently without clobbering each other's login
state. See the README's "Auth" section.

## Is `single orchestrate` a real multi-agent chat?

No — it's a sequential relay: each agent in the list runs in turn on one
goal, sharing a git worktree, and each one receives the previous agent's
real captured output. There's no live parallel conversation between
agents. See the README's "Multi-agent orchestration" note and
`docs/architecture.md` for the honest scope.

## What happens if an agent CLI isn't installed yet?

`single setup --yes` installs missing agent CLIs using each agent's own
verified installer. `single agent list` shows live detection status
either way, so you can see what's present before deciding.

## Does SingleCLI store my provider API keys in plaintext?

No — provider keys go through the OS keychain (see
`docs/architecture.md`), then get synced into each agent's own config
slot (e.g. Codex's `OPENAI_API_KEY`, Claude Code's `env` settings) with a
masked-entry flow in the TUI's Providers tab.

## Redis/Qdrant memory backends aren't configured — will tests fail?

No — their tests skip cleanly rather than fail when
`DIVISI_REDIS_URL`/`DIVISI_QDRANT_URL` aren't set. See the README's
"Development" section for how to run them locally against real instances
if you want to exercise that code path.

## What is `single notch`?

An optional top-center overlay (macOS / Linux) that shows live free-provider
pool health — key tallies, benches, recent goals, agent auth dots. It is
off by default. Enable with `single notch enable` (starts a companion
`divisi-notch` process that polls `runtime.sock`). Disable with
`single notch disable`. It does not replace the TUI Pool tab and does not
edit pool config. Session autostart examples: `docs/examples/notch.*`.

Placement depends on the desktop:

- **GNOME (Wayland or X11):** the notch is a GNOME Shell extension
  (`extensions/gnome-shell/`), drawn as compositor chrome on the middle of the
  right screen edge, so tiling extensions never treat it as a window. It stays
  a faint sliver, reveals a summary pill on hover, shows a tooltip when you
  hover an item, and expands into a detail card on click; it hides again when
  the pointer leaves. `single notch enable` installs and enables it. GNOME on
  Wayland only discovers a newly installed extension at login, so log out and
  back in once. The extension gets its data from `divisi-notch --snapshot`.
- **wlroots compositors (Sway, Hyprland, river) and macOS:** the `divisi-notch`
  process draws a top-center overlay itself.
- **KDE (KWin):** no layer-shell and no extension yet, so it exits with an
  explanatory error; `divisi-notch --window` runs it as an ordinary window.

## Where do I report a bug or ask something not covered here?

See [SUPPORT.md](../SUPPORT.md).
