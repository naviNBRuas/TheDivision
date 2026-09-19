//! Deprecated `single*` command names that exec their divisi replacement.
//! Removed in 0.26.0 (two minor versions after the rename).

use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

pub fn deprecation_line(old: &str, new: &str) -> String {
    format!("{old}: renamed, use `{new}`. This alias is removed in 0.26.")
}

/// The replacement binary next to this one when it exists, else a bare name for PATH lookup.
pub fn resolve_target(current_exe: &Path, new: &str) -> PathBuf {
    if let Some(dir) = current_exe.parent() {
        let candidate = dir.join(new);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(new)
}

/// Replaces this process with `new`, forwarding all arguments. The
/// deprecation line is only printed on a terminal so daemons and agents that
/// still spawn the old name are not spammed.
pub fn run(old: &str, new: &str) -> ! {
    if std::io::stderr().is_terminal() {
        eprintln!("{}", deprecation_line(old, new));
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let target = resolve_target(&exe, new);
    let err = std::process::Command::new(&target).args(std::env::args_os().skip(1)).exec();
    eprintln!("{old}: cannot run {}: {err}", target.display());
    std::process::exit(127)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deprecation_line_names_both() {
        let l = deprecation_line("single", "divisi");
        assert!(l.contains("single") && l.contains("`divisi`") && l.contains("0.26"), "{l}");
    }

    #[test]
    fn prefers_the_sibling_binary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("divisi"), "").unwrap();
        assert_eq!(resolve_target(&dir.path().join("single"), "divisi"), dir.path().join("divisi"));
    }

    #[test]
    fn falls_back_to_path_lookup() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_target(&dir.path().join("single"), "divisi"), std::path::PathBuf::from("divisi"));
    }
}
