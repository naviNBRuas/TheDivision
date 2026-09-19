//! `divisi install-integrations` / `divisi uninstall-integrations`: syncs
//! divisi's unified MCP/LSP registries into every enabled agent's
//! **divisi-managed home** (`divisi_core::agent_home` — bootstrapped
//! from the real `$HOME` once, never written into again after that; see
//! that module's doc comment for why).

use crate::context::Context;
use anyhow::Result;
use divisi_agent_sdk::adapters::for_agent_with_custom;
use divisi_protocol::IntegrationResult;

pub fn install_all(ctx: &Context, dry_run: bool, real_home: bool) -> Result<IntegrationResult> {
    let home_root = home_dir()?;
    let registry_servers = divisi_core::mcp::load(&ctx.dirs.mcp_registry_file())?;
    let gateway_spec = divisi_core::mcp::gateway_server_spec();
    // Gateway mode: sync only the divisi-gateway dynamic gateway (it proxies
    // to every enabled server itself) instead of the full registry list —
    // see divisi_core::mcp::gateway_mode's doc comment. Whichever mode is
    // *not* active this run, its names are stale leftovers from a
    // previous sync under the other mode: `configure_mcp` only merges the
    // names it's given, it never prunes names it wasn't told about, so
    // toggling gateway mode and re-syncing would otherwise leave both the
    // gateway entry *and* every individual server registered side by
    // side — defeating gateway mode's whole point (avoid registering N
    // servers individually). Removing the other mode's names first keeps
    // each sync a clean switch, not an accumulation.
    let (mut mcp_servers, mut stale_names): (Vec<_>, Vec<String>) = if divisi_core::mcp::gateway_mode(&ctx.dirs.mcp_gateway_file())? {
        (vec![gateway_spec], registry_servers.iter().map(|s| s.name.clone()).collect())
    } else {
        (registry_servers, vec![gateway_spec.name])
    };
    // divisi-mcp is divisi's own always-on delegation surface, not
    // one of the registry servers gateway mode toggles between — it's
    // appended unconditionally to whichever list the branch above chose.
    mcp_servers.push(divisi_core::mcp::divisi_mcp_server_spec());
    // Entries a pre-rename install registered under the old names.
    stale_names.extend(divisi_core::mcp::LEGACY_SERVER_NAMES.iter().map(|n| n.to_string()));
    let lsp_servers = divisi_core::lsp::load(&ctx.dirs.lsp_registry_file())?;

    let mut writes = Vec::new();
    for agent in &ctx.registry {
        let Some(adapter) = for_agent_with_custom(&agent.name, &ctx.dirs.agents_dir(), &ctx.registry) else { continue };
        let home = if real_home {
            home_root.clone()
        } else {
            divisi_core::agent_home::ensure_bootstrapped(&ctx.dirs.homes_dir(), &home_root, &agent.name)?
        };
        if !stale_names.is_empty() {
            writes.push(adapter.remove_mcp(&home, &stale_names, dry_run)?);
        }
        writes.push(adapter.configure_mcp(&home, &mcp_servers, dry_run)?);
        writes.push(adapter.configure_lsp(&home, &lsp_servers, dry_run)?);
    }
    Ok(IntegrationResult { dry_run, writes })
}

pub fn uninstall_all(ctx: &Context, dry_run: bool, real_home: bool) -> Result<IntegrationResult> {
    let home_root = home_dir()?;
    let mcp_servers = divisi_core::mcp::load(&ctx.dirs.mcp_registry_file())?;
    // Also remove the gateway entry name regardless of which mode is
    // currently active — an uninstall should clean up either shape
    // (direct per-server entries or the single gateway entry) a prior
    // sync may have left behind, not just whichever one gateway_mode()
    // says is "current" right now.
    let mcp_names: Vec<String> = mcp_servers
        .iter()
        .map(|s| s.name.clone())
        .chain(std::iter::once(divisi_core::mcp::gateway_server_spec().name))
        .chain(std::iter::once(divisi_core::mcp::divisi_mcp_server_spec().name))
        .chain(divisi_core::mcp::LEGACY_SERVER_NAMES.iter().map(|n| n.to_string()))
        .collect();
    let lsp_servers = divisi_core::lsp::load(&ctx.dirs.lsp_registry_file())?;
    let lsp_names: Vec<String> = lsp_servers.iter().map(|s| s.name.clone()).collect();

    let mut writes = Vec::new();
    for agent in &ctx.registry {
        let Some(adapter) = for_agent_with_custom(&agent.name, &ctx.dirs.agents_dir(), &ctx.registry) else { continue };
        let home = if real_home {
            home_root.clone()
        } else {
            divisi_core::agent_home::ensure_bootstrapped(&ctx.dirs.homes_dir(), &home_root, &agent.name)?
        };
        writes.push(adapter.remove_mcp(&home, &mcp_names, dry_run)?);
        writes.push(adapter.remove_lsp(&home, &lsp_names, dry_run)?);
    }
    Ok(IntegrationResult { dry_run, writes })
}

pub fn home_dir() -> Result<std::path::PathBuf> {
    divisi_core::paths::real_home_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;

    fn test_ctx(dir: &std::path::Path) -> Context {
        let dirs = divisi_core::DivisiDirs::from_root(dir.to_path_buf());
        dirs.ensure_created().unwrap();
        Context { dirs, resolved: divisi_core::ResolvedConfig::default(), registry: divisi_core::builtin_registry() }
    }

    fn claude_mcp_server_names(dir: &std::path::Path) -> Vec<String> {
        let path = dir.join("homes").join("claude").join(".claude.json");
        let root: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut names: Vec<String> = root["mcpServers"].as_object().unwrap().keys().cloned().collect();
        names.sort();
        names
    }

    #[test]
    fn switching_gateway_mode_replaces_rather_than_accumulates_mcp_entries() {
        let _guard = crate::HOME_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());

        install_all(&ctx, false, false).unwrap();
        let direct_names = claude_mcp_server_names(dir.path());
        assert!(direct_names.contains(&"fetch".to_string()));
        assert!(!direct_names.contains(&"divisi-gateway".to_string()));

        divisi_core::mcp::set_gateway_mode(&ctx.dirs.mcp_gateway_file(), true).unwrap();
        install_all(&ctx, false, false).unwrap();
        let gateway_names = claude_mcp_server_names(dir.path());
        assert_eq!(
            gateway_names,
            vec!["divisi-gateway".to_string(), "divisi-mcp".to_string()],
            "enabling gateway mode must remove the old direct entries, not add to them (divisi-mcp stays regardless)"
        );

        divisi_core::mcp::set_gateway_mode(&ctx.dirs.mcp_gateway_file(), false).unwrap();
        install_all(&ctx, false, false).unwrap();
        let back_to_direct = claude_mcp_server_names(dir.path());
        assert_eq!(back_to_direct, direct_names, "disabling gateway mode must remove the divisi-gateway entry, not leave it alongside the direct ones");
    }

    #[test]
    fn uninstall_removes_the_gateway_entry_even_when_gateway_mode_is_off() {
        let _guard = crate::HOME_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());

        divisi_core::mcp::set_gateway_mode(&ctx.dirs.mcp_gateway_file(), true).unwrap();
        install_all(&ctx, false, false).unwrap();
        assert_eq!(claude_mcp_server_names(dir.path()), vec!["divisi-gateway".to_string(), "divisi-mcp".to_string()]);

        divisi_core::mcp::set_gateway_mode(&ctx.dirs.mcp_gateway_file(), false).unwrap();
        uninstall_all(&ctx, false, false).unwrap();
        assert!(claude_mcp_server_names(dir.path()).is_empty(), "uninstall must clean up a gateway entry left over from a prior gateway-mode sync too");
    }

    #[test]
    fn real_home_writes_the_actual_home_not_the_isolated_copy() {
        let _guard = crate::HOME_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let real_home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", real_home.path());

        install_all(&ctx, false, true).unwrap();

        let real_claude_json = real_home.path().join(".claude.json");
        assert!(real_claude_json.exists(), "--real-home must write into the real $HOME, not the isolated homes/ dir");
        let isolated_claude_json = dir.path().join("homes").join("claude").join(".claude.json");
        assert!(!isolated_claude_json.exists(), "--real-home must not also bootstrap/write the isolated home");

        std::env::remove_var("HOME");
    }

    #[test]
    fn without_real_home_still_writes_the_isolated_copy_only() {
        let _guard = crate::HOME_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        install_all(&ctx, false, false).unwrap();
        assert!(dir.path().join("homes").join("claude").join(".claude.json").exists());
    }

    #[test]
    fn divisi_mcp_is_always_included_regardless_of_gateway_mode() {
        let _guard = crate::HOME_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());

        install_all(&ctx, false, false).unwrap();
        assert!(claude_mcp_server_names(dir.path()).contains(&"divisi-mcp".to_string()));

        divisi_core::mcp::set_gateway_mode(&ctx.dirs.mcp_gateway_file(), true).unwrap();
        install_all(&ctx, false, false).unwrap();
        let names = claude_mcp_server_names(dir.path());
        assert!(names.contains(&"divisi-gateway".to_string()));
        assert!(names.contains(&"divisi-mcp".to_string()), "divisi-mcp must survive a gateway-mode switch, unlike the registry servers it isn't one of");
    }
}
