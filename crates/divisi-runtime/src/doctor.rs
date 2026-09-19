use crate::context::Context;
use divisi_agent_sdk::adapters::for_agent_with_custom;
use divisi_protocol::{AuthState, CheckStatus, DoctorCheck, DoctorReport};

pub fn run(ctx: &Context) -> DoctorReport {
    let mut checks = Vec::new();

    checks.push(DoctorCheck {
        name: "config directory".into(),
        status: CheckStatus::Ok,
        detail: ctx.dirs.root().display().to_string(),
    });

    match std::fs::metadata(ctx.dirs.root()) {
        Ok(meta) if meta.permissions().readonly() => checks.push(DoctorCheck {
            name: "config directory writable".into(),
            status: CheckStatus::Fail,
            detail: "directory is read-only".into(),
        }),
        Ok(_) => checks.push(DoctorCheck {
            name: "config directory writable".into(),
            status: CheckStatus::Ok,
            detail: "writable".into(),
        }),
        Err(e) => checks.push(DoctorCheck {
            name: "config directory writable".into(),
            status: CheckStatus::Fail,
            detail: e.to_string(),
        }),
    }

    for agent in &ctx.registry {
        let Some(adapter) = for_agent_with_custom(&agent.name, &ctx.dirs.agents_dir(), &ctx.registry) else {
            checks.push(DoctorCheck {
                name: format!("agent: {}", agent.name),
                status: CheckStatus::Skipped,
                detail: "no adapter registered".into(),
            });
            continue;
        };
        let discovery = adapter.discover();
        let status = if discovery.detected {
            CheckStatus::Ok
        } else if agent.bootstrap_install.is_some() {
            CheckStatus::Warn
        } else {
            CheckStatus::Skipped
        };
        let detail = if discovery.detected {
            format!(
                "detected at {}{}",
                discovery.resolved_path.as_deref().unwrap_or("?"),
                discovery
                    .version
                    .as_ref()
                    .map(|v| format!(" ({v})"))
                    .unwrap_or_default()
            )
        } else if let Some(install) = &agent.bootstrap_install {
            format!("not installed; `single setup` would run: {}", install.command)
        } else {
            "not installed; no verified install method".into()
        };
        checks.push(DoctorCheck { name: format!("agent: {}", agent.name), status, detail });

        if discovery.detected {
            let isolated_home = ctx.dirs.homes_dir().join(&agent.name);
            let auth = divisi_core::account::is_authenticated(&isolated_home, &agent.name);
            let check = match auth {
                AuthState::Authenticated => DoctorCheck {
                    name: format!("agent: {} auth", agent.name),
                    status: CheckStatus::Ok,
                    detail: "logged in".into(),
                },
                AuthState::NotAuthenticated => DoctorCheck {
                    name: format!("agent: {} auth", agent.name),
                    status: CheckStatus::Skipped,
                    detail: if adapter.login_supported() {
                        format!("not logged in — run `single agent login {}`", agent.name)
                    } else {
                        // `single agent login` would just error here — this
                        // agent's auth state is detectable (support() says
                        // so) but no real login command is wired up for it
                        // (e.g. agy: no auth/login subcommand exists at
                        // all in `agy --help`). Don't point at a command
                        // that doesn't work.
                        format!("not logged in — no `single agent login {}` support yet; run {}'s own login/auth command directly", agent.name, agent.name)
                    },
                },
                AuthState::Unsupported => DoctorCheck {
                    name: format!("agent: {} auth", agent.name),
                    status: CheckStatus::Skipped,
                    detail: "account/auth detection not supported for this agent".into(),
                },
            };
            checks.push(check);
        }
    }

    for (binary, purpose) in [
        ("pdftotext", "PDF text extraction for `single doc ingest` (poppler-utils)"),
        ("pdftoppm", "scanned-PDF rasterization for `single doc ingest` (poppler-utils)"),
        ("tesseract", "OCR for scanned PDFs/images for `single doc ingest`"),
    ] {
        let present = std::process::Command::new("which").arg(binary).output().map(|o| o.status.success()).unwrap_or(false);
        checks.push(DoctorCheck {
            name: format!("tool: {binary}"),
            status: if present { CheckStatus::Ok } else { CheckStatus::Skipped },
            detail: if present { "found".into() } else { format!("not installed — needed for {purpose}") },
        });
    }

    let docker_present = crate::docker::docker_available();
    checks.push(DoctorCheck {
        name: "tool: docker".into(),
        status: if docker_present { CheckStatus::Ok } else { CheckStatus::Skipped },
        detail: if docker_present {
            "found".into()
        } else {
            "not installed — needed only for agents/accounts with `single agent docker enable`".into()
        },
    });
    if docker_present {
        for setting in divisi_core::docker::status(&ctx.dirs.docker_registry_file(), None).unwrap_or_default() {
            if !setting.enabled {
                continue;
            }
            let container = divisi_core::docker::container_name(&setting.agent, setting.account.as_deref());
            let label = match &setting.account {
                Some(a) => format!("{}/{a}", setting.agent),
                None => setting.agent.clone(),
            };
            let (status, detail) = match crate::docker::is_running(&container) {
                Ok(Some(true)) => (CheckStatus::Ok, format!("container {container} running")),
                Ok(Some(false)) => (CheckStatus::Warn, format!("container {container} exists but is stopped")),
                Ok(None) => (CheckStatus::Warn, format!("container {container} not created yet (starts on next task run)")),
                Err(e) => (CheckStatus::Warn, format!("could not check container {container}: {e:#}")),
            };
            checks.push(DoctorCheck { name: format!("docker: {label}"), status, detail });
        }
    }

    match crate::qdrant_backend::resolve_url() {
        Some(url) => {
            let reachable = crate::qdrant_backend::ping(&url).is_ok();
            checks.push(DoctorCheck {
                name: "vector store (qdrant)".into(),
                status: if reachable { CheckStatus::Ok } else { CheckStatus::Warn },
                detail: if reachable { format!("{url} (reachable)") } else { format!("{url} (configured but unreachable)") },
            });
        }
        None => checks.push(DoctorCheck {
            name: "vector store (qdrant)".into(),
            status: CheckStatus::Skipped,
            detail: "not configured — set SINGLE_QDRANT_URL to enable `single memory search --semantic`".into(),
        }),
    }
    let embeddings_configured = crate::embeddings::is_configured();
    checks.push(DoctorCheck {
        name: "semantic memory search (embeddings)".into(),
        status: if embeddings_configured { CheckStatus::Ok } else { CheckStatus::Skipped },
        detail: if embeddings_configured {
            "configured".into()
        } else {
            "not configured — `single secret set embeddings:api_key <key>`; semantic search falls back to substring search until then".into()
        },
    });

    // E28 spec §9.3: agent installs shell package managers -- the
    // category most likely to want off on a shared box, so `doctor`
    // always states plainly whether it's currently enabled.
    let self_heal_cfg = crate::self_heal::SelfHealConfig::load(&ctx.dirs);
    let env_disabled = std::env::var("SINGLE_SELF_HEAL_AGENT_INSTALL").is_ok_and(|v| v == "0");
    let agent_category_on = self_heal_cfg.categories.agent && !env_disabled;
    checks.push(DoctorCheck {
        name: "self-heal: agent category".into(),
        status: if agent_category_on { CheckStatus::Ok } else { CheckStatus::Skipped },
        detail: if !self_heal_cfg.categories.agent {
            "disabled in self_heal.toml — no automatic agent installs/repairs".into()
        } else if env_disabled {
            "disabled via SINGLE_SELF_HEAL_AGENT_INSTALL=0 — no automatic agent installs/repairs".into()
        } else {
            "enabled — missing routable agents may be auto-installed, stale pool keys auto-disabled".into()
        },
    });

    DoctorReport { checks }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx(root: &std::path::Path) -> Context {
        let dirs = divisi_core::DivisiDirs::from_root(root.to_path_buf());
        dirs.ensure_created().unwrap();
        Context { dirs, resolved: divisi_core::ResolvedConfig::default(), registry: divisi_core::builtin_registry() }
    }

    #[test]
    fn doctor_reports_agent_category_on_off_state() {
        let _guard = crate::SELF_HEAL_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        // `agent` defaults off (a real production hang, not just the
        // spec's own safety note, is why -- see `Categories::default()`).
        let report = run(&ctx);
        let check = report.checks.iter().find(|c| c.name == "self-heal: agent category").unwrap();
        assert_eq!(check.status, CheckStatus::Skipped, "off by default");
        assert!(check.detail.contains("disabled in self_heal.toml"), "{}", check.detail);

        // Explicit opt-in flips it to Ok.
        let mut cfg = crate::self_heal::SelfHealConfig::load(&ctx.dirs);
        cfg.categories.agent = true;
        std::fs::write(ctx.dirs.root().join("self_heal.toml"), toml::to_string_pretty(&cfg).unwrap()).unwrap();
        let report = run(&ctx);
        let check = report.checks.iter().find(|c| c.name == "self-heal: agent category").unwrap();
        assert_eq!(check.status, CheckStatus::Ok, "explicitly opted in");
    }
}
