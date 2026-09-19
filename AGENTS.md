# Agent instructions for divisi

These rules apply to every agent — human-directed or autonomous — that commits code in this repository, regardless of which CLI or model is doing the work.

## Commit format

`<type>: <description>` — imperative, concise, no scope prefix unless absolutely necessary.

Allowed types only: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `build`, `ci`. Never `style`, `revert`, `update`, or anything outside this list.

Commits are atomic and focused — one coherent logical change per commit. The message describes *what changed*, not the debugging or implementation process. Never write `update`, `changes`, `misc`, `stuff`, or other filler as a description.

## Authorship

Every commit is sole-authored by `Navin B. Ruas <founder@nbr.company>`.

- Never add `Co-authored-by`.
- Never attribute any part of a commit to an AI model, agent, or bot (no `Claude-Session:`, no `Generated with`, nothing naming the tool that did the work).
- Never rely on a tool's default commit templating — strip any automatic attribution trailer before committing.

## Versioning

This project follows SemVer and stays pre-1.0 (`0.x.x`) during active development:

| Segment | Meaning | When it bumps |
|---|---|---|
| `0.x.x` | Pre-1.0 / unstable | Default while the API/architecture can still change |
| `x.0.0` | Major | Breaking/incompatible changes (only after an explicit decision to cut `1.0.0`) |
| `0.x.0` | Minor | New backwards-compatible functionality |
| `0.0.x` | Patch | Fixes |

Map commit types to version bumps: `feat` → minor (or major only post-1.0 if breaking), `fix`/`perf` → patch, `refactor`/`docs`/`test`/`chore`/`build`/`ci` → normally no bump on their own. Pre-1.0, breaking changes still bump minor, not major.

Bump the workspace version in `Cargo.toml` (`[workspace.package].version`) and rebuild before committing a release-worthy change, so `Cargo.lock` and the commit land together.
