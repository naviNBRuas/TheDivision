# Governance

divisi is maintained under a **BDFL (Benevolent Dictator For Life)**
model.

## Decision-making

- **Navin B. Ruas** ([@naviNBRuas](https://github.com/naviNBRuas)) is the
  sole maintainer and has final say on scope, architecture, and what merges.
- Design discussions happen in GitHub Issues/Discussions and are open to
  anyone; the maintainer weighs community input but is not bound by vote.
- There is no formal committee, working group, or voting process at this
  project's current size. This will be revisited if the contributor base
  grows enough to justify it.

## Contribution acceptance

- Pull requests are reviewed against [CONTRIBUTING.md](CONTRIBUTING.md) and
  merged at the maintainer's discretion.
- New built-in agent integrations, changes to the isolated-home model, and
  anything touching account/credential handling get extra scrutiny — see
  `docs/architecture.md` and `docs/adr/0001-tech-stack.md` for the
  reasoning behind the current design before proposing a change to it.

## Releases

- Versioning follows [SemVer](https://semver.org/), pre-1.0 — see
  `AGENTS.md`'s "Versioning" section for the exact scheme, also summarized
  in [CONTRIBUTING.md](CONTRIBUTING.md).
- Releases ship on a `stable` (tagged) and `nightly` (rebuilt on every push
  to `main`) channel — see `.github/workflows/`. The maintainer owns `main`
  and release tagging; no one else has merge or release authority at this
  time.

## Stepping down / succession

If the maintainer becomes unavailable long-term, ownership transfer will be
handled via the repository's GitHub organization settings, favoring an
active, trusted contributor if one exists at that time. There is no
standing succession plan beyond that, given the project's current size.
