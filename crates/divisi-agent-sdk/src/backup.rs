//! Backup-before-write, per spec section 34: "Never overwrite existing
//! configuration without backup and explicit consent."

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Copies `path` to `path.bak-<unix-timestamp>` if `path` exists. Returns
/// `None` if there was nothing to back up (fresh file).
pub fn backup_before_write(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup_path = path.with_extension(format!(
        "{}.bak-{timestamp}",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    std::fs::copy(path, &backup_path)
        .with_context(|| format!("backing up {} to {}", path.display(), backup_path.display()))?;
    Ok(Some(backup_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_backup_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert!(backup_before_write(&path).unwrap().is_none());
    }

    #[test]
    fn backs_up_existing_file_with_content_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"hello":"world"}"#).unwrap();
        let backup = backup_before_write(&path).unwrap().unwrap();
        assert!(backup.exists());
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), r#"{"hello":"world"}"#);
    }
}
