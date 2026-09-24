//! divisi's own agent names, and the one place legacy names map onto them.
//!
//! Before the rebrand the pool agent was `single-pool`, the native agent `single-agent`, and each
//! provider-backed agent `single-<provider>`. Those names are persisted in routing.toml, fallback.toml,
//! custom agent files, task rows and goal graphs, so every reader passes names through [`canonical`]
//! instead of the old names being kept alive in code.

/// The free-provider pool as an agent.
pub const POOL: &str = "divisi-pool";
/// divisi's native provider-direct agent.
pub const NATIVE: &str = "divisi-agent";
/// Prefix of provider-backed agents (`divisi-<provider>`).
pub const PROVIDER_PREFIX: &str = "divisi-";

const LEGACY_PREFIX: &str = "single-";

/// `name` under its current divisi name.
pub fn canonical(name: &str) -> String {
    match name {
        "single-pool" => POOL.to_string(),
        "single-agent" => NATIVE.to_string(),
        n => match n.strip_prefix(LEGACY_PREFIX) {
            Some(rest) if !rest.is_empty() => format!("{PROVIDER_PREFIX}{rest}"),
            _ => n.to_string(),
        },
    }
}

pub fn is_pool(name: &str) -> bool {
    canonical(name) == POOL
}

pub fn is_native(name: &str) -> bool {
    canonical(name) == NATIVE
}

/// The provider behind a provider-backed agent (`divisi-nvidia` -> `nvidia`), or `None` for the pool,
/// the native agent and CLI agents.
pub fn provider_of(name: &str) -> Option<String> {
    let c = canonical(name);
    if c == POOL || c == NATIVE {
        return None;
    }
    c.strip_prefix(PROVIDER_PREFIX).map(str::to_string)
}

/// Whether `name` is one of divisi's own HTTP-backed agents (never a shelled CLI).
pub fn is_divisi_backed(name: &str) -> bool {
    canonical(name).starts_with(PROVIDER_PREFIX)
}

/// Every name in `names`, canonical, de-duplicated with order kept.
pub fn canonical_list(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let c = canonical(n);
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_names_map_onto_divisi_names() {
        assert_eq!(canonical("single-pool"), "divisi-pool");
        assert_eq!(canonical("single-agent"), "divisi-agent");
        assert_eq!(canonical("single-nvidia"), "divisi-nvidia");
        assert_eq!(canonical("opencode"), "opencode");
        assert_eq!(canonical("divisi-pool"), "divisi-pool");
        assert!(is_pool("single-pool") && is_pool("divisi-pool"));
        assert_eq!(provider_of("single-google").as_deref(), Some("google"));
        assert_eq!(provider_of("divisi-pool"), None);
        assert_eq!(provider_of("grok"), None);
        assert_eq!(canonical_list(&["single-pool".into(), "divisi-pool".into(), "grok".into()]), vec!["divisi-pool", "grok"]);
    }
}
