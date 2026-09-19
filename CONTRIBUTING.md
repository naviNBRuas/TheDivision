# Contributing

Thanks for considering a contribution to divisi.

## Workflow

1. Fork the repo and branch from `main`: `feat/<slug>`, `fix/<slug>`,
   `chore/<slug>`, `docs/<slug>`.
2. Make your change. Add or update tests for any behavioral change.
3. Commit using [Conventional Commits](https://www.conventionalcommits.org/)
   — see [Commit format](#commit-format) below. This repo's own
   [`AGENTS.md`](AGENTS.md) states the same rules for any AI agent
   committing here — human contributors follow the identical format.
4. Open a PR against `main`. Describe what changed and why, and link any
   related issue.
5. Squash-merge once reviewed.

## Local checks (run before opening a PR)

```bash
cargo build --workspace   # all seven crates
cargo test --workspace
```

No API keys are required to build, test, or run `divisi doctor`/`single
agent list`. Commands that touch real files or run real installers
(`divisi setup --yes`, `divisi install-integrations --yes`, `single
account use`, `divisi provider sync --yes`) are the exception — see the
README's "Development" section. The optional Redis/Qdrant memory-backend
tests skip cleanly when no such service is reachable locally.

## Commit format

`<type>: <description>` — imperative, concise.

Allowed types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`,
`build`, `ci`.

## Versioning

This project follows [SemVer](https://semver.org/) and stays pre-1.0
(`0.x.x`) during active development:

| Segment | Meaning | When it bumps |
|---|---|---|
| `0.x.x` | Pre-1.0 / unstable | Default while the API/architecture can still change |
| `x.0.0` | Major | Breaking changes (only after an explicit decision to cut `1.0.0`) |
| `0.x.0` | Minor | New backwards-compatible functionality |
| `0.0.x` | Patch | Fixes |

Bump `[workspace.package].version` in `Cargo.toml` and rebuild (so
`Cargo.lock` moves with it) as part of any release-worthy commit.

## Adding a new agent integration

divisi supports two paths for adding agent support:

- **Declarative** (no Rust) — describe the CLI in
  `~/.config/divisi/agents/<name>.toml` (command, install command, prompt
  mode, MCP format). Start here; see the README's "Phase 5" note.
- **Built-in** (Rust) — for agents needing deeper integration (real login
  flow, native LSP/plugin sync). Look at an existing adapter in
  `crates/divisi-agent-sdk` as a template, and read
  [`docs/install-methods.md`](docs/install-methods.md) first — it documents
  the verified install/login/MCP commands for every currently-supported
  agent, including the ones that were investigated and left out (e.g.
  Windsurf) and why.

## Reporting security issues

See [SECURITY.md](SECURITY.md) — do NOT open a public issue for a
vulnerability in divisi itself.
