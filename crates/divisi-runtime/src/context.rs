use divisi_core::{registry::AgentDefinition, ResolvedConfig, DivisiDirs};

/// Shared state for handling a request: resolved config, the agent
/// registry, and where divisi's directories live. Built once per
/// request in Phase 1 (cheap: local file reads) rather than kept as a long
/// lived mutable daemon state — there is no in-memory state that outlives a
/// single request yet, since orchestration/task state is Phase 4.
#[derive(Clone)]
pub struct Context {
    pub dirs: DivisiDirs,
    pub resolved: ResolvedConfig,
    pub registry: Vec<AgentDefinition>,
}

impl Context {
    pub fn load() -> anyhow::Result<Self> {
        let dirs = DivisiDirs::discover()?;
        dirs.ensure_created()?;
        let resolved = divisi_core::resolve(&dirs, None)?;
        let mut registry = divisi_core::builtin_registry();

        // User-defined agents (~/.config/divisi/agents/*.toml) join the
        // registry alongside the five built-in ones. A malformed custom
        // agent file is skipped here (surfaced properly via `doctor`,
        // which calls `custom_agents::load_all` directly to see the
        // errors) rather than failing every request.
        if let Ok((custom, _errors)) = divisi_core::custom_agents::load_all(&dirs.agents_dir()) {
            registry.extend(custom.iter().map(divisi_core::custom_agents::to_agent_definition));
        }

        // One-time (per-preset) migration: bring the real registries up to
        // date with the built-in preset catalogs, disabled, so `list`/the
        // TUI actually show what's available instead of only the original
        // hand-picked entries. Gated to once per daemon process (see
        // `PRESETS_SYNCED` below) rather than run on every `Context::load()`
        // — each preset catalog now runs to hundreds of entries, so
        // "cheap to run on every load" stopped being true: this was
        // re-parsing every registry file in full on every single request
        // (`server.rs` builds a fresh `Context` per connection), which
        // compounds badly once a client fires a dozen requests on startup
        // (see `divisi-tui`'s `App::refresh`). New built-in presets only
        // ship with a divisi upgrade, which already needs `divisi daemon
        // restart` to be picked up (same reasoning as `$PATH` in
        // `cached_discover`), so once-per-process loses nothing real.
        // Errors here (e.g. an unwritable config dir) are surfaced
        // elsewhere (`doctor`) and shouldn't block every other request, so
        // they're swallowed rather than propagated.
        static PRESETS_SYNCED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        PRESETS_SYNCED.get_or_init(|| {
            let _ = divisi_core::mcp::sync_missing_presets_disabled(&dirs.mcp_registry_file());
            let _ = divisi_core::lsp::sync_missing_presets_disabled(&dirs.lsp_registry_file());
            let _ = divisi_core::plugins::sync_missing_presets(&dirs.plugins_registry_file());
            let _ = divisi_core::providers::sync_missing_presets(&dirs.providers_registry_file());
        });

        Ok(Self { dirs, resolved, registry })
    }

    pub fn find_agent(&self, name: &str) -> Option<&AgentDefinition> {
        let name = divisi_core::agent_names::canonical(name);
        self.registry.iter().find(|a| a.name == name)
    }
}
