//! Moves a pre-rename `~/.config/single` onto `~/.config/divisi`.
//! A move, not a conversion: on-disk formats are unchanged; only
//! `state/single.db` is renamed to `state/divisi.db`.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// New dir exists (or nothing legacy is left to move).
    NothingToDo,
    /// Neither dir exists: a fresh install.
    Fresh,
    Migrated,
    /// Both dirs are real directories; left untouched for the user to reconcile.
    BothExist,
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

fn daemon_live(old: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(old.join("state/runtime.sock")).is_ok()
}

fn rename_db(root: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let from = root.join(format!("state/single.db{suffix}"));
        let to = root.join(format!("state/divisi.db{suffix}"));
        if from.exists() && !to.exists() {
            std::fs::rename(&from, &to).with_context(|| format!("renaming {}", from.display()))?;
        }
    }
    Ok(())
}

pub fn migrate_config_dir(old: &Path, new: &Path) -> Result<Outcome> {
    if new.exists() {
        return Ok(if old.exists() && !is_symlink(old) { Outcome::BothExist } else { Outcome::NothingToDo });
    }
    if !old.exists() {
        return Ok(Outcome::Fresh);
    }
    if daemon_live(old) {
        bail!("the runtime daemon is running against {}; stop it, then rerun", old.display());
    }
    std::fs::rename(old, new).with_context(|| format!("moving {} to {}", old.display(), new.display()))?;
    rename_db(new)?;
    std::os::unix::fs::symlink(new, old).with_context(|| format!("linking {} to {}", old.display(), new.display()))?;
    Ok(Outcome::Migrated)
}

/// The config root to use when no `DIVISI_CONFIG_DIR` override is set.
/// Migrates lazily. If migration is refused, keeps using the old dir so the
/// CLI never starts from an empty config next to a populated legacy one.
pub fn resolve_default_root(config_home: &Path) -> PathBuf {
    let (old, new) = (config_home.join("single"), config_home.join("divisi"));
    match migrate_config_dir(&old, &new) {
        Ok(Outcome::BothExist) => {
            eprintln!("divisi: both {} and {} exist; using the latter. Merge or remove the old one.", old.display(), new.display());
            new
        }
        Ok(_) => new,
        Err(e) => {
            eprintln!("divisi: config migration skipped: {e:#}");
            if old.exists() {
                old
            } else {
                new
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn legacy_tree(root: &std::path::Path) {
        fs::create_dir_all(root.join("state")).unwrap();
        fs::write(root.join("config.toml"), "x = 1").unwrap();
        fs::write(root.join("state/single.db"), "db").unwrap();
        fs::write(root.join("state/single.db-wal"), "wal").unwrap();
    }

    #[test]
    fn fresh_install_resolves_to_the_new_dir() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(resolve_default_root(home.path()), home.path().join("divisi"));
        assert!(!home.path().join("divisi").exists(), "must not create anything");
    }

    #[test]
    fn migrates_and_leaves_a_symlink() {
        let home = tempfile::tempdir().unwrap();
        legacy_tree(&home.path().join("single"));
        let root = resolve_default_root(home.path());
        assert_eq!(root, home.path().join("divisi"));
        assert_eq!(fs::read_to_string(root.join("config.toml")).unwrap(), "x = 1");
        assert_eq!(fs::read_to_string(root.join("state/divisi.db")).unwrap(), "db");
        assert_eq!(fs::read_to_string(root.join("state/divisi.db-wal")).unwrap(), "wal");
        assert!(!root.join("state/single.db").exists());
        assert!(fs::symlink_metadata(home.path().join("single")).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(home.path().join("single/config.toml")).unwrap(), "x = 1");
    }

    #[test]
    fn second_run_is_a_no_op() {
        let home = tempfile::tempdir().unwrap();
        legacy_tree(&home.path().join("single"));
        resolve_default_root(home.path());
        assert_eq!(migrate_config_dir(&home.path().join("single"), &home.path().join("divisi")).unwrap(), Outcome::NothingToDo);
    }

    #[test]
    fn refuses_to_move_a_live_daemons_dir_and_keeps_using_the_old_one() {
        let home = tempfile::tempdir().unwrap();
        let old = home.path().join("single");
        legacy_tree(&old);
        let _listener = std::os::unix::net::UnixListener::bind(old.join("state/runtime.sock")).unwrap();
        assert!(migrate_config_dir(&old, &home.path().join("divisi")).is_err());
        assert_eq!(resolve_default_root(home.path()), old);
        assert!(old.join("state/single.db").exists(), "nothing moved");
    }

    #[test]
    fn both_dirs_present_is_reported_not_merged() {
        let home = tempfile::tempdir().unwrap();
        legacy_tree(&home.path().join("single"));
        fs::create_dir_all(home.path().join("divisi")).unwrap();
        assert_eq!(migrate_config_dir(&home.path().join("single"), &home.path().join("divisi")).unwrap(), Outcome::BothExist);
    }
}
