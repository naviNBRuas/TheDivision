//! `divisi migrate`: move a pre-rename install onto divisi names.
//! Dry run by default; `--apply` changes things. Never starts the daemon.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

pub fn rewrite_unit(text: &str) -> String {
    // The repository keeps its old name until the first divisi release, so its URL must survive.
    const REPO: &str = "naviNBRuas/SingleCLI";
    const KEEP: &str = "\u{0}REPO\u{0}";
    text.replace(REPO, KEEP)
        .replace("SingleCLI", "divisi")
        .replace(KEEP, REPO)
        .replace("single-runtimed", "divisid")
        .replace(".config/single", ".config/divisi")
        .replace("SINGLE_", "DIVISI_")
}

fn systemctl(args: &[&str]) -> Option<String> {
    let out = Command::new("systemctl").arg("--user").args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn unit_dir() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config/systemd/user"))
}

pub fn run(apply: bool) -> Result<()> {
    let verb = if apply { "" } else { "would " };

    // 1. Config dir: DivisiDirs::discover() migrates lazily, so touching it is enough.
    if apply {
        let dirs = divisi_core::DivisiDirs::discover()?;
        println!("config dir: {}", dirs.root().display());
    } else {
        println!("config dir: would move ~/.config/single to ~/.config/divisi on first run (needs the daemon stopped)");
    }

    // 2. systemd unit: copy to divisid.service, keep the enabled/disabled state, never start.
    let dir = unit_dir()?;
    let old_unit = dir.join("single-runtimed.service");
    if old_unit.exists() {
        let new_unit = dir.join("divisid.service");
        let was_enabled = systemctl(&["is-enabled", "single-runtimed"]).as_deref() == Some("enabled");
        println!("systemd: {verb}write {} (old unit was {})", new_unit.display(), if was_enabled { "enabled" } else { "disabled" });
        if apply {
            std::fs::write(&new_unit, rewrite_unit(&std::fs::read_to_string(&old_unit)?))?;
            if was_enabled {
                systemctl(&["disable", "single-runtimed"]);
            }
            systemctl(&["daemon-reload"]);
            if was_enabled {
                systemctl(&["enable", "divisid"]);
            }
            println!("systemd: old unit left in place at {}; delete it once you are happy", old_unit.display());
        }
    } else {
        println!("systemd: no single-runtimed.service found, nothing to do");
    }

    // 3. GNOME notch extension.
    if crate::notch_proc::is_gnome() {
        match crate::notch_proc::gnome_migrate_legacy(apply)? {
            Some(msg) => println!("notch: {msg}"),
            None => println!("notch: no legacy extension installed"),
        }
    }
    if !apply {
        println!("\nDry run. Rerun with --apply to make these changes. The daemon is never started.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_a_legacy_unit() {
        let unit = "[Unit]\nDescription=SingleCLI runtime daemon\n[Service]\nExecStart=%h/.local/bin/single-runtimed\nEnvironment=SINGLE_CONFIG_DIR=%h/.config/single\nEnvironment=PATH=%h/.opencode/bin:%h/.local/bin:/usr/bin\n";
        let out = rewrite_unit(unit);
        assert!(out.contains("Description=divisi runtime daemon"), "{out}");
        assert!(out.contains("ExecStart=%h/.local/bin/divisid"), "{out}");
        assert!(out.contains("DIVISI_CONFIG_DIR=%h/.config/divisi"), "{out}");
        assert!(out.contains("PATH=%h/.opencode/bin:%h/.local/bin:/usr/bin"), "PATH must be untouched: {out}");
        assert!(!out.contains("single"), "{out}");
    }

    #[test]
    fn keeps_the_repository_url_until_the_repo_is_renamed() {
        let out = rewrite_unit("Description=SingleCLI daemon\nDocumentation=https://github.com/naviNBRuas/SingleCLI\n");
        assert!(out.contains("Description=divisi daemon"), "{out}");
        assert!(out.contains("Documentation=https://github.com/naviNBRuas/SingleCLI"), "{out}");
    }
}
