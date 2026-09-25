//! Where an agent's prompt-driving process actually runs: directly on the
//! host (today's default, isolated only by `$HOME` — see
//! `divisi_core::agent_home`/`account`), or inside a persistent Docker
//! container (opt-in per agent/account — see `divisi_core::docker` for
//! the enable/disable settings and `divisi-runtime::docker` for container
//! lifecycle). Threaded through `AgentAdapter::run_prompt` and
//! `run::run_command_live` so the same call site works for either backend
//! without the caller needing to know which one is active.

use std::collections::BTreeMap;
use std::path::Path;

pub enum ExecBackend<'a> {
    /// `home: Some(path)` overrides `$HOME` for the child process — same
    /// meaning as the plain `Option<&Path>` this replaced. `None` runs
    /// against the caller's real ambient `$HOME`.
    ///
    /// `extra_env`: provider API keys resolved for this agent (see
    /// `divisi_core::provider_keys::resolve_env_for_agent`), applied on
    /// top of the child's inherited environment — this is how an agent
    /// that authenticates via a plain env var (not OAuth) actually sees
    /// the key divisi has stored for it. `None` for call sites that
    /// don't need this (installs, logins, tests).
    Host { home: Option<&'a Path>, extra_env: Option<&'a BTreeMap<String, String>> },
    /// Runs via `docker exec -w <workdir> <container> <command> <args...>`
    /// against an already-running container (see
    /// `divisi-runtime::docker::ensure_started`, called before this by
    /// whichever daemon handler resolved the backend) — no `$HOME`
    /// override needed, since the container's home is whatever was baked
    /// in when it was created (the isolated home directory, bind-mounted).
    /// `extra_env` is passed as `-e KEY=VALUE` flags, since a host-side
    /// `Command::env()` call has no effect on what `docker exec` sees
    /// inside the container's own namespace.
    Docker { container: &'a str, workdir: &'a Path, extra_env: Option<&'a BTreeMap<String, String>> },
}

impl<'a> ExecBackend<'a> {
    pub fn host(home: Option<&'a Path>) -> Self {
        ExecBackend::Host { home, extra_env: None }
    }

    pub fn host_with_env(home: Option<&'a Path>, extra_env: &'a BTreeMap<String, String>) -> Self {
        ExecBackend::Host { home, extra_env: Some(extra_env) }
    }

    /// The env applied on top of the child's inherited environment, if any.
    pub fn extra_env(&self) -> Option<&'a BTreeMap<String, String>> {
        match self {
            ExecBackend::Host { extra_env, .. } | ExecBackend::Docker { extra_env, .. } => *extra_env,
        }
    }

    /// The same backend, applying `extra_env` instead of its own.
    pub fn with_extra_env<'b>(&self, extra_env: &'b BTreeMap<String, String>) -> ExecBackend<'b>
    where
        'a: 'b,
    {
        match self {
            ExecBackend::Host { home, .. } => ExecBackend::Host { home: *home, extra_env: Some(extra_env) },
            ExecBackend::Docker { container, workdir, .. } => {
                ExecBackend::Docker { container, workdir, extra_env: Some(extra_env) }
            }
        }
    }
}
