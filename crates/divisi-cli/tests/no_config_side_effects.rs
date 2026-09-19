//! Commands that do not need the config dir must not resolve, migrate or create it.
//! `divisi migrate` without `--apply` is a dry run, and `divisi logo` only prints.

use std::fs;
use std::path::Path;
use std::process::Command;

fn legacy_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let old = home.path().join("cfg/single");
    fs::create_dir_all(old.join("state")).unwrap();
    fs::write(old.join("state/single.db"), "db").unwrap();
    fs::write(old.join("config.toml"), "x = 1").unwrap();
    home
}

fn run(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_divisi"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("cfg"))
        .env_remove("DIVISI_CONFIG_DIR")
        .env_remove("SINGLE_CONFIG_DIR")
        .env("NO_COLOR", "1")
        .output()
        .expect("run divisi")
}

fn assert_untouched(home: &Path) {
    let cfg = home.join("cfg");
    let meta = fs::symlink_metadata(cfg.join("single")).unwrap();
    assert!(meta.is_dir() && !meta.file_type().is_symlink(), "legacy dir must stay a real directory");
    assert!(!cfg.join("divisi").exists(), "no divisi config dir may be created or migrated");
    assert_eq!(fs::read_to_string(cfg.join("single/state/single.db")).unwrap(), "db");
    assert!(!cfg.join("single/state/divisi.db").exists());
}

#[test]
fn logo_does_not_touch_the_config_dir() {
    let home = legacy_home();
    let out = run(home.path(), &["logo"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_untouched(home.path());
}

#[test]
fn migrate_without_apply_changes_nothing() {
    let home = legacy_home();
    let out = run(home.path(), &["migrate"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Dry run"));
    assert_untouched(home.path());
}
