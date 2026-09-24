//! One place that decides how an agent or provider's credentials are
//! described, so the TUI and the notch never disagree.
//!
//! Honesty rules: a state is only claimed when something actually checked it.
//! Only a handful of agents have login detection (`account::support`); every
//! other installed agent is `unverified`, never "authed" or "no auth needed".
//! No agent in the built-in registry is confirmed to run without any login,
//! so `no_auth_needed` only appears where the catalog says a *provider* is
//! keyless (`free_pool::Auth::Keyless`).

use divisi_protocol::{AgentInfo, AuthState, PoolKeyStatusInfo};

pub struct KeyCounts {
    pub total: u32,
    pub valid: u32,
    pub invalid: u32,
    pub unvalidated: u32,
    pub disabled: u32,
}

/// `authed | unverified | invalid | disabled | no_key | blocked | no_auth_needed`
pub fn provider_auth_state(keyless: bool, catalog_block: Option<&str>, keys: &KeyCounts) -> &'static str {
    if keys.valid > 0 {
        return "authed";
    }
    if keyless {
        return "no_auth_needed";
    }
    if catalog_block.is_some() {
        return "blocked";
    }
    if keys.total == 0 {
        return "no_key";
    }
    let live = keys.total - keys.disabled;
    if live == 0 {
        "disabled"
    } else if keys.unvalidated > 0 {
        "unverified"
    } else {
        "invalid"
    }
}

pub struct AgentClass {
    /// `authed | needs_login | unverified | no_auth_needed | not_installed`
    pub class: &'static str,
    pub why: String,
}

fn from_provider(p: &PoolKeyStatusInfo) -> AgentClass {
    let (class, why) = match p.auth_state.as_str() {
        "authed" => ("authed", format!("via {} key", p.platform)),
        "no_auth_needed" => ("no_auth_needed", format!("{} needs no key", p.platform)),
        "unverified" => ("unverified", format!("{} key never validated", p.platform)),
        "invalid" => ("needs_login", format!("{} key rejected", p.platform)),
        "disabled" => ("needs_login", format!("{} key disabled", p.platform)),
        "blocked" => ("needs_login", format!("{} signup blocked", p.platform)),
        _ => ("needs_login", format!("no {} key", p.platform)),
    };
    AgentClass { class, why }
}

/// Classifies one agent. `providers` is the pool's per-provider status list;
/// the `divisi-*` agents are provider proxies, so their credential is the
/// matching pool key rather than a vendor login.
pub fn classify_agent(agent: &AgentInfo, providers: &[PoolKeyStatusInfo]) -> AgentClass {
    if !agent.detected {
        return AgentClass { class: "not_installed", why: "binary not found".into() };
    }
    if crate::agent_names::is_pool(&agent.name) || crate::agent_names::is_native(&agent.name) {
        let ok = providers.iter().filter(|p| matches!(p.auth_state.as_str(), "authed" | "no_auth_needed")).count();
        return if ok > 0 {
            AgentClass { class: "authed", why: format!("{ok} providers usable") }
        } else {
            AgentClass { class: "needs_login", why: "no usable provider key".into() }
        };
    }
    if let Some(id) = crate::agent_names::provider_of(&agent.name) {
        let id = id.as_str();
        return match providers.iter().find(|p| p.platform == id) {
            Some(p) => from_provider(p),
            None => AgentClass { class: "unverified", why: format!("provider {id} not in the pool catalog") },
        };
    }
    match agent.authenticated {
        AuthState::Authenticated => AgentClass { class: "authed", why: "login found".into() },
        AuthState::NotAuthenticated => AgentClass { class: "needs_login", why: "no login in single's isolated home (a login in your real home does not count)".into() },
        AuthState::Unsupported => AgentClass { class: "unverified", why: "login state can't be detected".into() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(total: u32, valid: u32, invalid: u32, unvalidated: u32, disabled: u32) -> KeyCounts {
        KeyCounts { total, valid, invalid, unvalidated, disabled }
    }

    #[test]
    fn provider_state_precedence() {
        assert_eq!(provider_auth_state(false, None, &counts(3, 1, 1, 1, 0)), "authed");
        assert_eq!(provider_auth_state(true, None, &counts(0, 0, 0, 0, 0)), "no_auth_needed");
        assert_eq!(provider_auth_state(false, Some("needs a China account"), &counts(0, 0, 0, 0, 0)), "blocked");
        assert_eq!(provider_auth_state(false, None, &counts(0, 0, 0, 0, 0)), "no_key");
        assert_eq!(provider_auth_state(false, None, &counts(2, 0, 0, 0, 2)), "disabled");
        assert_eq!(provider_auth_state(false, None, &counts(2, 0, 1, 1, 0)), "unverified");
        assert_eq!(provider_auth_state(false, None, &counts(2, 0, 2, 0, 0)), "invalid");
    }

    fn agent(name: &str, detected: bool, auth: AuthState) -> AgentInfo {
        let mut a = crate::registry::builtin_registry().into_iter().next().map(|d| AgentInfo {
            name: d.name,
            adapter: d.adapter,
            command: d.command,
            detected: true,
            version: None,
            install_method: d.install_method,
            bootstrap_install: d.bootstrap_install,
            unverified: d.unverified,
            home_requirement: d.home_requirement,
            max_concurrency: d.max_concurrency,
            capabilities: d.capabilities,
            config_paths: d.config_paths,
            notes: d.notes,
            authenticated: AuthState::Unsupported,
        }).unwrap();
        a.name = name.into();
        a.detected = detected;
        a.authenticated = auth;
        a
    }

    fn prov(id: &str, state: &str) -> PoolKeyStatusInfo {
        PoolKeyStatusInfo { platform: id.into(), auth_state: state.into(), ..Default::default() }
    }

    #[test]
    fn agents_are_classified_by_evidence_only() {
        let ps = [prov("nvidia", "authed"), prov("google", "unverified"), prov("cohere", "no_key")];
        assert_eq!(classify_agent(&agent("claude", false, AuthState::Authenticated), &ps).class, "not_installed");
        assert_eq!(classify_agent(&agent("claude", true, AuthState::Authenticated), &ps).class, "authed");
        assert_eq!(classify_agent(&agent("claude", true, AuthState::NotAuthenticated), &ps).class, "needs_login");
        assert_eq!(classify_agent(&agent("opencode", true, AuthState::Unsupported), &ps).class, "unverified");
        assert_eq!(classify_agent(&agent("divisi-nvidia", true, AuthState::Unsupported), &ps).class, "authed");
        assert_eq!(classify_agent(&agent("divisi-google", true, AuthState::Unsupported), &ps).class, "unverified");
        assert_eq!(classify_agent(&agent("single-cohere", true, AuthState::Unsupported), &ps).class, "needs_login");
        assert_eq!(classify_agent(&agent("single-nowhere", true, AuthState::Unsupported), &ps).class, "unverified");
        assert_eq!(classify_agent(&agent("divisi-pool", true, AuthState::Unsupported), &ps).class, "authed");
    }
}
