//! Honour the pre-rename `SINGLE_*` environment variables when the matching
//! `DIVISI_*` is unset. Removed together with the `single*` shims.

const OLD: &str = "SINGLE_";
const NEW: &str = "DIVISI_";

use std::collections::HashSet;
use std::io::IsTerminal;

/// `(old_name, new_name, value)` for every `SINGLE_x` whose `DIVISI_x` is not set.
pub fn plan_adoption(vars: &[(String, String)]) -> Vec<(String, String, String)> {
    let present: HashSet<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
    vars.iter()
        .filter_map(|(k, v)| {
            let rest = k.strip_prefix(OLD).filter(|r| !r.is_empty())?;
            let new = format!("{NEW}{rest}");
            (!present.contains(new.as_str())).then(|| (k.clone(), new, v.clone()))
        })
        .collect()
}

/// Copies legacy `SINGLE_*` variables to their `DIVISI_*` names. Call first
/// thing in `main`, before any thread reads the environment.
pub fn adopt_legacy_env() {
    let vars: Vec<(String, String)> =
        std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect();
    let plan = plan_adoption(&vars);
    for (_, new, value) in &plan {
        std::env::set_var(new, value);
    }
    if !plan.is_empty() && std::io::stderr().is_terminal() {
        let names: Vec<&str> = plan.iter().map(|(old, _, _)| old.as_str()).collect();
        eprintln!("divisi: {} still work but are deprecated; rename them to DIVISI_*", names.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, val)| (k.to_string(), val.to_string())).collect()
    }

    #[test]
    fn adopts_a_legacy_var_when_the_new_one_is_absent() {
        let plan = plan_adoption(&v(&[("SINGLE_CONFIG_DIR", "/x")]));
        assert_eq!(plan, vec![("SINGLE_CONFIG_DIR".into(), "DIVISI_CONFIG_DIR".into(), "/x".into())]);
    }

    #[test]
    fn the_new_name_wins() {
        let plan = plan_adoption(&v(&[("SINGLE_CONFIG_DIR", "/old"), ("DIVISI_CONFIG_DIR", "/new")]));
        assert!(plan.is_empty());
    }

    #[test]
    fn ignores_unrelated_and_bare_prefix() {
        let plan = plan_adoption(&v(&[("PATH", "/bin"), ("SINGLE_", "x"), ("MY_SINGLE_X", "y")]));
        assert!(plan.is_empty());
    }
}
