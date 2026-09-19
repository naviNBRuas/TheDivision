//! `single setup`: install any registry agent that isn't detected, using
//! its verified bootstrap install command, then hand off to
//! `integrations::apply_all` to sync SingleCLI's MCP registry into every
//! agent (freshly installed or pre-existing).
//!
//! Running an install script is not easily reversible, so this module never
//! executes anything unless explicitly told `dry_run: false` — callers
//! (the CLI) are expected to require an explicit `--yes`/confirmation
//! before doing that. See docs/architecture.md.

use crate::context::Context;
use divisi_agent_sdk::adapters::for_agent_with_custom;
use divisi_protocol::{SetupAction, SetupActionKind, SetupPlan};
use std::process::Command;

pub fn run(ctx: &Context, dry_run: bool) -> SetupPlan {
    let actions = ctx.registry.iter().map(|agent| plan_one(ctx, agent, dry_run)).collect();
    SetupPlan { actions, dry_run }
}

/// Same per-agent logic as `run`, scoped to a single named agent — the
/// seam the TUI's interactive install flow and `single agent install` use
/// so installing one agent doesn't require planning/touching all five.
pub fn run_one(ctx: &Context, agent_name: &str, dry_run: bool) -> anyhow::Result<SetupAction> {
    let agent = ctx.find_agent(agent_name).ok_or_else(|| anyhow::anyhow!("unknown agent: {agent_name}"))?;
    Ok(plan_one(ctx, agent, dry_run))
}

fn plan_one(ctx: &Context, agent: &divisi_core::registry::AgentDefinition, dry_run: bool) -> SetupAction {
    let Some(adapter) = for_agent_with_custom(&agent.name, &ctx.dirs.agents_dir(), &ctx.registry) else {
        return SetupAction { agent: agent.name.clone(), action: SetupActionKind::Unsupported, detail: "no adapter registered".into(), executed: false };
    };
    let discovery = adapter.discover();

    if discovery.detected {
        return SetupAction {
            agent: agent.name.clone(),
            action: SetupActionKind::AlreadyInstalled,
            detail: discovery.version.clone().unwrap_or_else(|| "installed (version unknown)".into()),
            executed: false,
        };
    }

    let Some(install) = &agent.bootstrap_install else {
        return SetupAction {
            agent: agent.name.clone(),
            action: SetupActionKind::Unsupported,
            detail: match &agent.install_method {
                divisi_protocol::InstallMethod::Unsupported { reason } => reason.clone(),
                _ => "no verified bootstrap install command".into(),
            },
            executed: false,
        };
    };

    if dry_run {
        return SetupAction {
            agent: agent.name.clone(),
            action: SetupActionKind::Install,
            detail: format!("{} (source: {})", install.command, install.source),
            executed: false,
        };
    }

    let result = Command::new("sh").arg("-c").arg(&install.command).status();
    let (executed, detail) = match result {
        Ok(status) if status.success() => (true, format!("ran: {}", install.command)),
        Ok(status) => (false, format!("install command exited with {status}: {}", install.command)),
        Err(e) => (false, format!("failed to run install command: {e}")),
    };
    SetupAction { agent: agent.name.clone(), action: SetupActionKind::Install, detail, executed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use divisi_core::DivisiDirs;

    /// Only ever exercises `dry_run: true` — a test that ran `dry_run:
    /// false` would actually shell out and try to install real software,
    /// which is exactly what spec section 34/section 45 says never to do
    /// from an automated test.
    fn test_context() -> (tempfile::TempDir, Context) {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SINGLE_CONFIG_DIR", dir.path());
        let dirs = DivisiDirs::from_root(dir.path().to_path_buf());
        dirs.ensure_created().unwrap();
        let resolved = divisi_core::ResolvedConfig::default();
        let registry = divisi_core::builtin_registry();
        (dir, Context { dirs, resolved, registry })
    }

    #[test]
    fn dry_run_never_executes_anything() {
        let (_dir, ctx) = test_context();
        let plan = run(&ctx, true);
        assert!(plan.dry_run);
        assert!(plan.actions.iter().all(|a| !a.executed), "dry run must never execute");
    }

    #[test]
    fn every_agent_gets_a_real_action_kind_based_on_actual_detection() {
        // Doesn't assume any particular agent is/isn't installed on the
        // machine running the test (this repo's own dev machine has since
        // installed pplx for real via `single setup --yes`) — only that
        // every agent resolves to a real, non-panicking outcome: detected
        // agents report AlreadyInstalled, undetected ones with a verified
        // bootstrap command report Install, and none of that touches the
        // network since dry_run is true.
        let (_dir, ctx) = test_context();
        let plan = run(&ctx, true);
        for action in &plan.actions {
            let def = ctx.registry.iter().find(|a| a.name == action.agent).unwrap();
            match action.action {
                SetupActionKind::AlreadyInstalled => {}
                SetupActionKind::Install => assert!(def.bootstrap_install.is_some(), "{}", action.agent),
                SetupActionKind::Unsupported => assert!(def.bootstrap_install.is_none(), "{}", action.agent),
                SetupActionKind::ConfigureIntegration => panic!("bootstrap::run never emits this"),
            }
        }
    }

    #[test]
    fn every_action_has_a_recognized_agent() {
        let (_dir, ctx) = test_context();
        let plan = run(&ctx, true);
        let names: Vec<_> = plan.actions.iter().map(|a| a.agent.as_str()).collect();
        let registry_names: Vec<_> = ctx.registry.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, registry_names, "bootstrap plan should cover exactly the agent registry, in order");
    }

    #[test]
    fn run_one_matches_the_corresponding_entry_in_a_full_run() {
        let (_dir, ctx) = test_context();
        let full_plan = run(&ctx, true);
        let single_action = run_one(&ctx, "perplexity", true).unwrap();
        let full_action = full_plan.actions.iter().find(|a| a.agent == "perplexity").unwrap();
        assert_eq!(single_action.action, full_action.action);
        assert_eq!(single_action.detail, full_action.detail);
    }

    #[test]
    fn run_one_errors_for_unknown_agent() {
        let (_dir, ctx) = test_context();
        assert!(run_one(&ctx, "ghost-agent", true).is_err());
    }
}
