# Acknowledgments

SingleCLI is built in Rust on top of a small, deliberately boring set of
crates. Particular thanks to the maintainers of:

- [Tokio](https://tokio.rs) — the async runtime behind the headless
  `divisid` daemon and every non-interactive agent task.
- [Ratatui](https://github.com/ratatui-org/ratatui) and
  [Crossterm](https://github.com/crossterm-rs/crossterm) — the TUI control
  center (Agents/Tasks/MCP/LSP/Plugins/Tools/Providers/Accounts/Memory
  tabs).
- [Clap](https://github.com/clap-rs/clap) — CLI argument parsing across
  every subcommand.
- [rusqlite](https://github.com/rusqlite/rusqlite) — the local SQLite
  stores backing task history and the shared knowledge-graph memory.
- [serde](https://serde.rs) and the wider Serde ecosystem
  (`serde_json`, `serde_yaml`, `toml`) — every native agent config format
  SingleCLI reads and writes (Claude, Codex, OpenCode, Cursor, and the
  rest).
- [reqwest](https://github.com/seanmonstar/reqwest) — GitHub Releases
  lookups for `single update`, and provider/API traffic.
- [age](https://github.com/str4d/rage) — encrypted backup support.
- [directories](https://github.com/dirs-dev/directories-rs) — cross-platform
  config/data directory resolution.

And to the maintainers of every agent CLI SingleCLI integrates with —
Claude Code, Codex, OpenCode, Antigravity, Cursor CLI, Aider, Goose, GitHub
Copilot CLI, Kiro CLI, Sourcegraph Cody, and Perplexity's `pplx` — whose
own config formats and non-interactive modes this project reverse-engineers
against and keeps in sync with, documented honestly (including the gaps)
in [`docs/install-methods.md`](docs/install-methods.md).

See [CONTRIBUTORS.md](CONTRIBUTORS.md) for people who have contributed code
directly to this repository.
