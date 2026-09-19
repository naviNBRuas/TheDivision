# Security Policy

## Supported Versions

SingleCLI is pre-1.0 (`0.x.x`); the latest release on the `stable` channel
receives security fixes. There is no long-term-support branch yet.

## Reporting a Vulnerability

Email **security@nbr.company** with details and reproduction steps.
Do NOT open a public issue. Expect acknowledgement within 72 hours and a
remediation timeline within 7 days.

Please include:

- The affected version/commit and install channel (`stable` or `nightly`).
- Whether the issue involves credential/secret handling (OS keychain,
  captured account profiles, provider API keys), the isolated-home model
  (`~/.config/divisi/homes/<agent>/`), or the headless daemon's Unix
  socket — these get priority given what they touch.
- Steps to reproduce, or a PoC if one exists.

## Scope

In scope: SingleCLI's own code — the CLI, TUI, headless daemon
(`divisid`), agent adapters, and the registries/stores under
`~/.config/divisi/`.

Out of scope: vulnerabilities in a third-party agent CLI SingleCLI
integrates with (Claude Code, Codex, OpenCode, etc.) — report those to the
respective project. If SingleCLI's *integration* with one of them
introduces a vulnerability (e.g. leaking a secret into process argv, as
fixed previously — see CHANGELOG), that is in scope.

## Disclosure

Coordinated disclosure. We credit reporters unless anonymity is requested.
