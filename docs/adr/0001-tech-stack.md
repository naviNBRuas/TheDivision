# ADR 0001: Rust workspace, Unix-socket JSON-lines IPC, SQLite state

> Written when the project was called SingleCLI; names updated after the 0.24.0 rename to divisi. The decision is unchanged.

## Status

Accepted — Phase 1.

## Context

divisi needs a long-lived local runtime daemon, a CLI client, and a TUI
client, all sharing configuration/state, on a Linux-first machine that also
has Go 1.26 and Node 24 available. The full spec (see project root prompt)
suggests Rust, a Unix socket, JSON-RPC-ish IPC, and SQLite, but says to
evaluate alternatives rather than blindly follow that suggestion.

## Decision

**Language/runtime: Rust**, as a Cargo workspace with one crate per layer
(`divisi-protocol`, `divisi-core`, `divisi-agent-sdk`, `divisi-runtime`,
`divisi-cli`, `divisi-tui`).

**IPC: newline-delimited JSON over a Unix domain socket**
(`~/.config/divisi/state/runtime.sock`), not a full RPC framework
(tonic/gRPC, JSON-RPC 2.0 with a schema registry, etc).

**State: SQLite** via `rusqlite` (bundled, no system dependency), at
`~/.config/divisi/state/divisi.db`.

**TUI: `ratatui` + `crossterm`.**

## Alternatives considered

- **Go.** Strong for a daemon + CLI (single static binary, good stdlib
  networking), and available on this machine. Rejected for Phase 1 mainly
  because Rust's stronger type system pays off specifically for the
  adapter/capability-negotiation model (spec sections 39-40): illegal
  states (an agent claiming a capability it can't back) are easier to make
  unrepresentable with Rust's enums/traits than with Go's interfaces. Not a
  strong rejection — Go remains a reasonable choice if a future rewrite is
  ever justified.
- **Node/TypeScript.** Fastest to prototype, and all four real agent CLIs
  investigated for Phase 1 (`claude`, `codex`, `opencode`) are themselves
  Node-adjacent in places. Rejected because a long-lived background daemon
  is a worse fit for Node's runtime characteristics (memory footprint,
  startup cost for many short-lived CLI invocations) than a compiled
  static binary, and distributing a Rust binary to a fresh machine (the
  user's stated fresh-install use case) avoids requiring Node to be present
  at all just to run divisi itself.
- **gRPC/tonic for IPC.** Rejected for Phase 1: it's the right choice once
  there's a real bidirectional event stream (task progress, agent output
  streaming — Phase 4 concerns), but for the current request/response
  surface (status, doctor, agent list, mcp list, setup, integrations) it
  would add a protobuf toolchain and codegen step for no present benefit.
  Newline-delimited JSON is trivially debuggable with `socat -,ignoreeof
  UNIX-CONNECT:runtime.sock` during development, which matters more right
  now than wire efficiency. The wire types already live in one crate
  (`divisi-protocol`) specifically so swapping the framing later doesn't
  require touching call sites.
- **A key-value store or flat files instead of SQLite.** Rejected: Phase 1
  already needs at least one queryable log (the `events` table used for
  audit history, spec section 32), and SQLite's zero-ops embedded model
  fits a local single-user tool better than anything requiring a server
  process.

## Consequences

- Every crate boundary in the workspace mirrors a layer boundary from the
  spec's architecture diagram (`divisi-protocol` = wire types,
  `divisi-core` = config/registry, `divisi-agent-sdk` = adapters,
  `divisi-runtime` = daemon, `divisi-cli`/`divisi-tui` = clients), so
  adding a new agent adapter (spec section 39) means adding one adapter
  implementation plus a registry entry, not touching the runtime or CLI.
- The IPC framing choice means Phase 1 has no true bidirectional event
  stream yet — `divisi-tui`'s dashboard polls/refreshes on a key press
  rather than subscribing to a push feed. `divisi-protocol::Envelope<T>` is
  a deliberately unused-for-now seam for that later addition.
- Because there's no persistent multi-request daemon *required* for
  one-shot CLI commands (`client.rs`'s in-process fallback), a fresh
  install where the user has never manually started `divisid`
  still gets fully working `single doctor`/`single agent list`/etc. Only
  the TUI insists on a real running daemon, since it's the client that
  actually wants live IPC semantics.

## Amendment: Linux release binaries are musl, not glibc

The first published release (v0.1.1) built its `linux-x86_64` binary on
GitHub's `ubuntu-latest` runner using the default
`x86_64-unknown-linux-gnu` target. Tested for real on a fresh Debian 12
container (`docker run debian:12`, the most common "just installed
Debian" baseline) rather than assumed: the binary refused to even start —
`GLIBC_2.39' not found` — because `ubuntu-latest` (Ubuntu 24.04) ships a
newer glibc than Debian 12's 2.36, and a dynamically-linked glibc binary
only runs on a system with an equal-or-newer glibc than it was built
against.

Fix: `release.yml`/`nightly.yml` now build Linux targets as
`x86_64-unknown-linux-musl`/`aarch64-unknown-linux-musl` (fully static,
no glibc dependency at all — `ldd` reports "statically linked"), via
`musl-tools` installed on the same GitHub-hosted runners rather than a
separate cross-compilation toolchain. Verified the fix by copying the
musl-built binary into the same Debian 12 container and confirming
`single --version`/`single doctor` ran correctly. `reqwest` was already
configured with `rustls-tls` (no native OpenSSL) specifically so it
wouldn't reintroduce a similar dynamic-linking fragility on musl.
