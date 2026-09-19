//! Full-setup backup/restore: everything under `DivisiDirs::root()` plus
//! every secret in the OS keychain, packed into one password-encrypted
//! archive that can be moved to another machine.
//!
//! Deliberately **not** built as a `Request`/`ResponseData` round trip
//! through `divisi-runtime` the way every other divisi feature is —
//! the passphrase that protects live OAuth tokens and API keys must never
//! cross the daemon's Unix socket or land in its event log. This module
//! runs entirely in-process instead, callable directly by both
//! `divisi-cli` (which already links `divisi-runtime` in-process for its
//! own fallback path, see `divisi-cli::client`) and `divisi-tui` (which
//! normally talks to the daemon exclusively over the socket, but has no
//! other way to reach this logic without either a new dependency on
//! `divisi-runtime` or duplicating the engine — `divisi-core` is the one
//! crate both already share, so that's where this lives).
//!
//! Archive format: a tar of every real file under `root()`, plus one
//! synthetic entry (`SECRETS_MANIFEST_NAME`) holding every OS-keychain
//! secret as TOML — chosen as a name no real divisi config file could
//! ever collide with — encrypted as a whole with `age`'s passphrase
//! (scrypt) mode.
//!
//! **Caveat not solved here**: if `divisid` is actively running
//! and writing to `state/divisi.db` during an export, the snapshot could
//! catch it mid-write (SQLite's `-wal`/`-shm` companion files are backed
//! up as plain files, not through a consistent checkpoint). Callers
//! (`divisi backup export`, the TUI's Backup tab) should tell the user to
//! stop the daemon first — enforcing that is a UX concern for those
//! call sites, not this engine.

use crate::paths::DivisiDirs;
use crate::secrets::{SecretStore, SecretTool};
use age::secrecy::SecretString;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

/// Reserved tar entry name for the secrets manifest — not a real relative
/// path under `DivisiDirs::root()`, so it can never collide with an
/// actual config file during either pack or unpack.
const SECRETS_MANIFEST_NAME: &str = "__singlecli_secrets__.toml";

/// Skipped during export: a live socket (meaningless once copied to
/// another machine) and operational log noise (not "setup").
fn is_excluded(relative: &Path) -> bool {
    relative == Path::new("state/runtime.sock") || relative.starts_with("logs")
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupItemResult {
    pub path: String,
    pub success: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BackupReport {
    pub dry_run: bool,
    pub files: Vec<BackupItemResult>,
    pub secrets: Vec<BackupItemResult>,
}

/// Walks `dirs.root()`, tars every real file plus a secrets manifest built
/// from every entry in the OS keychain, encrypts the tar with `passphrase`
/// (age scrypt/passphrase mode), and writes it to `dest_path`. Returns any
/// warnings (currently just "keychain unavailable") — a missing
/// `secret-tool` binary degrades to "no secrets captured" rather than
/// failing the whole export, same as `embeddings.rs`'s treatment of a
/// missing optional external capability elsewhere in this codebase.
pub fn export(dirs: &DivisiDirs, dest_path: &Path, passphrase: &SecretString) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        add_dir_recursive(&mut builder, dirs.root(), dirs.root())?;

        let store = SecretTool;
        let mut secrets = BTreeMap::new();
        match SecretStore::list(&store) {
            Ok(names) => {
                for name in names {
                    match SecretStore::get(&store, &name) {
                        Ok(Some(value)) => {
                            secrets.insert(name, value);
                        }
                        Ok(None) => {}
                        Err(e) => warnings.push(format!("failed to read secret '{name}': {e:#}")),
                    }
                }
            }
            Err(e) => warnings.push(format!("keychain unavailable, no secrets included in this backup: {e:#}")),
        }
        let manifest = SecretsManifest { secrets };
        let rendered = toml::to_string_pretty(&manifest).context("serializing secrets manifest")?;
        let mut header = tar::Header::new_gnu();
        header.set_size(rendered.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        builder.append_data(&mut header, SECRETS_MANIFEST_NAME, rendered.as_bytes())?;
        builder.finish().context("finishing tar archive")?;
    }

    let encryptor = age::Encryptor::with_user_passphrase(passphrase.clone());
    let mut encrypted = Vec::new();
    let mut writer = encryptor.wrap_output(&mut encrypted).context("initializing age encryption")?;
    writer.write_all(&tar_bytes).context("writing archive contents")?;
    writer.finish().context("finalizing age encryption")?;

    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest_path, &encrypted).with_context(|| format!("writing {}", dest_path.display()))?;
    // The archive holds live credentials — same discipline as
    // divisi_core::account's snapshot files.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(warnings)
}

fn add_dir_recursive<W: Write>(builder: &mut tar::Builder<W>, base: &Path, dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(base).unwrap_or(&path).to_path_buf();
        if is_excluded(&relative) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            add_dir_recursive(builder, base, &path)?;
        } else if file_type.is_file() {
            builder.append_path_with_name(&path, &relative).with_context(|| format!("archiving {}", path.display()))?;
        }
        // Symlinks are skipped, same reasoning agent_home::ensure_bootstrapped
        // already documents for its own directory walk: not worth the
        // risk of an unexpected target on the machine being restored to.
    }
    Ok(())
}

/// Decrypts `src_path` with `passphrase`, unpacks the tar, and writes
/// every file back under `dirs.root()` (through the same
/// backup-before-overwrite discipline every other divisi config writer
/// uses) and every secret back into the OS keychain. `dry_run` reports
/// what would happen without writing anything.
pub fn import(dirs: &DivisiDirs, src_path: &Path, passphrase: &SecretString, dry_run: bool) -> Result<BackupReport> {
    let encrypted = std::fs::read(src_path).with_context(|| format!("reading {}", src_path.display()))?;
    let decryptor = age::Decryptor::new(&encrypted[..]).context("this file isn't a valid age-encrypted archive")?;
    let mut tar_bytes = Vec::new();
    let mut reader = decryptor
        .decrypt(std::iter::once(&age::scrypt::Identity::new(passphrase.clone()) as &dyn age::Identity))
        .context("decryption failed — wrong passphrase, or the archive is corrupted")?;
    reader.read_to_end(&mut tar_bytes).context("reading decrypted archive")?;

    let mut report = BackupReport { dry_run, ..Default::default() };
    let mut archive = tar::Archive::new(&tar_bytes[..]);
    for entry in archive.entries().context("reading tar entries")? {
        let mut entry = entry?;
        let entry_path = entry.path()?.into_owned();

        if !is_safe_entry_name(&entry_path) {
            report.files.push(BackupItemResult {
                path: entry_path.display().to_string(),
                success: false,
                detail: "rejected: unsafe archive entry name (must be a relative path with no '..')".into(),
            });
            continue;
        }

        if entry_path == Path::new(SECRETS_MANIFEST_NAME) {
            let mut contents = String::new();
            entry.read_to_string(&mut contents).context("reading secrets manifest")?;
            let manifest: SecretsManifest = toml::from_str(&contents).context("parsing secrets manifest")?;
            restore_secrets(manifest, dry_run, &mut report);
            continue;
        }

        let target = dirs.root().join(&entry_path);
        let label = entry_path.display().to_string();
        if dry_run {
            report.files.push(BackupItemResult { path: label, success: true, detail: "would restore".into() });
            continue;
        }
        match restore_one_file(&mut entry, &target) {
            Ok(()) => report.files.push(BackupItemResult { path: label, success: true, detail: "restored".into() }),
            Err(e) => report.files.push(BackupItemResult { path: label, success: false, detail: format!("{e:#}") }),
        }
    }
    Ok(report)
}

/// True only for entry names that can be safely joined under
/// `DivisiDirs::root()`: plain relative paths. Rejects absolute names
/// (which `Path::join` would treat as a full replacement of the root) and
/// any archive name containing prefix/root/parent components, so a hostile
/// tar can never make restore write outside the config root.
fn is_safe_entry_name(name: &Path) -> bool {
    !name.is_absolute()
        && name.components().all(|component| {
            matches!(component, Component::Normal(_) | Component::CurDir)
        })
}

fn restore_one_file<R: Read>(entry: &mut tar::Entry<R>, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    backup_file_before_write(target)?;
    let mut contents = Vec::new();
    entry.read_to_end(&mut contents)?;
    std::fs::write(target, contents).with_context(|| format!("writing {}", target.display()))
}

/// Same one-file-backup-before-overwrite discipline
/// `divisi-agent-sdk::backup::backup_before_write` already applies to
/// every agent config write — duplicated here rather than imported since
/// `divisi-agent-sdk` depends on `divisi-core`, not the other way around.
fn backup_file_before_write(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup_path = PathBuf::from(format!("{}.bak-{timestamp}", path.display()));
    std::fs::copy(path, &backup_path).with_context(|| format!("backing up {} to {}", path.display(), backup_path.display()))?;
    Ok(())
}

fn restore_secrets(manifest: SecretsManifest, dry_run: bool, report: &mut BackupReport) {
    let store = SecretTool;
    for (name, value) in manifest.secrets {
        if dry_run {
            report.secrets.push(BackupItemResult { path: name, success: true, detail: "would restore".into() });
            continue;
        }
        match SecretStore::set(&store, &name, &value) {
            Ok(()) => report.secrets.push(BackupItemResult { path: name, success: true, detail: "restored".into() }),
            Err(e) => report.secrets.push(BackupItemResult { path: name, success: false, detail: format!("{e:#}") }),
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct SecretsManifest {
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_then_import_round_trips_files_and_secrets_byte_identical() {
        let src_dir = tempfile::tempdir().unwrap();
        let dirs = DivisiDirs::from_root(src_dir.path().to_path_buf());
        std::fs::create_dir_all(dirs.root().join("profiles")).unwrap();
        std::fs::write(dirs.config_file(), "answer = 42\n").unwrap();
        std::fs::write(dirs.root().join("profiles").join("default.toml"), "name = \"default\"\n").unwrap();
        // A live socket must never survive a round trip.
        std::fs::create_dir_all(dirs.state_dir()).unwrap();
        std::fs::write(dirs.socket_path(), "not a real socket").unwrap();

        let archive_path = tempfile::tempdir().unwrap().path().join("backup.age");
        let passphrase = SecretString::from("correct horse battery staple".to_string());
        export(&dirs, &archive_path, &passphrase).unwrap();
        assert!(archive_path.exists());

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_dirs = DivisiDirs::from_root(dest_dir.path().to_path_buf());
        let report = import(&dest_dirs, &archive_path, &passphrase, false).unwrap();
        assert!(!report.dry_run);
        assert!(report.files.iter().all(|f| f.success), "every file should restore cleanly: {:?}", report.files);

        assert_eq!(std::fs::read_to_string(dest_dirs.config_file()).unwrap(), "answer = 42\n");
        assert_eq!(std::fs::read_to_string(dest_dirs.root().join("profiles").join("default.toml")).unwrap(), "name = \"default\"\n");
        assert!(!dest_dirs.socket_path().exists(), "runtime.sock must be excluded from the archive");
    }

    #[test]
    fn wrong_passphrase_fails_cleanly_not_partially() {
        let src_dir = tempfile::tempdir().unwrap();
        let dirs = DivisiDirs::from_root(src_dir.path().to_path_buf());
        std::fs::write(dirs.config_file(), "answer = 42\n").unwrap();

        let archive_path = tempfile::tempdir().unwrap().path().join("backup.age");
        export(&dirs, &archive_path, &SecretString::from("right-passphrase".to_string())).unwrap();

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_dirs = DivisiDirs::from_root(dest_dir.path().to_path_buf());
        let result = import(&dest_dirs, &archive_path, &SecretString::from("wrong-passphrase".to_string()), false);
        assert!(result.is_err());
        assert!(!dest_dirs.config_file().exists(), "nothing should be written on decryption failure");
    }

    #[test]
    fn dry_run_writes_nothing() {
        let src_dir = tempfile::tempdir().unwrap();
        let dirs = DivisiDirs::from_root(src_dir.path().to_path_buf());
        std::fs::write(dirs.config_file(), "answer = 42\n").unwrap();

        let archive_path = tempfile::tempdir().unwrap().path().join("backup.age");
        let passphrase = SecretString::from("dry-run-pass".to_string());
        export(&dirs, &archive_path, &passphrase).unwrap();

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_dirs = DivisiDirs::from_root(dest_dir.path().to_path_buf());
        let report = import(&dest_dirs, &archive_path, &passphrase, true).unwrap();
        assert!(report.dry_run);
        assert!(!report.files.is_empty());
        assert!(!dest_dirs.config_file().exists(), "dry run must not write any file");
    }

    #[test]
    fn importing_onto_an_existing_file_backs_it_up_first() {
        let src_dir = tempfile::tempdir().unwrap();
        let dirs = DivisiDirs::from_root(src_dir.path().to_path_buf());
        std::fs::write(dirs.config_file(), "from-backup = true\n").unwrap();

        let archive_path = tempfile::tempdir().unwrap().path().join("backup.age");
        let passphrase = SecretString::from("existing-file-pass".to_string());
        export(&dirs, &archive_path, &passphrase).unwrap();

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_dirs = DivisiDirs::from_root(dest_dir.path().to_path_buf());
        std::fs::create_dir_all(dest_dirs.root()).unwrap();
        std::fs::write(dest_dirs.config_file(), "already-here = true\n").unwrap();

        import(&dest_dirs, &archive_path, &passphrase, false).unwrap();
        assert_eq!(std::fs::read_to_string(dest_dirs.config_file()).unwrap(), "from-backup = true\n");
        let backups: Vec<_> = std::fs::read_dir(dest_dirs.root())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".bak-"))
            .collect();
        assert_eq!(backups.len(), 1, "the pre-existing config.toml should have been backed up before being overwritten");
    }

    #[test]
    fn entry_name_validation_rejects_absolute_parent_root_and_prefix_paths() {
        for safe in ["config.toml", "profiles/default.toml", "./nested/ok.toml"] {
            assert!(is_safe_entry_name(Path::new(safe)), "{safe:?} should be accepted");
        }
        for hostile in ["/", "/etc/passwd", "/absolute.txt", "..", "../up.txt", "profiles/../../escape.txt"] {
            assert!(!is_safe_entry_name(Path::new(hostile)), "{hostile:?} should be rejected");
        }
        // Drive prefixes only parse as `Component::Prefix` on Windows.
        #[cfg(windows)]
        assert!(!is_safe_entry_name(Path::new(r"C:\evil.txt")));
    }

    /// Appends one ustar entry carrying a completely raw name. Unlike
    /// `tar::Builder` — which refuses non-relative names on write — this
    /// mirrors what a hand-crafted hostile archive actually contains.
    fn append_raw_ustar_entry(out: &mut Vec<u8>, name: &str, contents: &[u8]) {
        assert!(name.len() <= 100, "test names must fit the ustar name field");
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        let octal = |field: &mut [u8], value: usize| {
            let digits = field.len() - 1;
            let rendered = format!("{:0width$o}", value, width = digits);
            field[..digits].copy_from_slice(rendered.as_bytes());
            field[digits] = b'\0';
        };
        octal(&mut header[100..108], 0o600);
        octal(&mut header[108..116], 0);
        octal(&mut header[116..124], 0);
        octal(&mut header[124..136], contents.len());
        octal(&mut header[136..148], 0);
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: u32 = header
            .iter()
            .enumerate()
            .map(|(i, b)| if (148..156).contains(&i) { u32::from(b' ') } else { u32::from(*b) })
            .sum();
        header[148..154].copy_from_slice(format!("{checksum:06o}").as_bytes());
        header[154] = b'\0';
        header[155] = b' ';
        out.extend_from_slice(&header);
        out.extend_from_slice(contents);
        out.extend(std::iter::repeat_n(0u8, (512 - contents.len() % 512) % 512));
    }

    /// Builds an age-encrypted tar by hand — bypassing `export`, which can
    /// only ever emit sanitized relative names — so hostile entry names can
    /// be smuggled into `import`.
    fn write_encrypted_archive(dest_path: &Path, passphrase: &SecretString, entries: &[(&str, &[u8])]) {
        let mut tar_bytes = Vec::new();
        for (name, contents) in entries {
            append_raw_ustar_entry(&mut tar_bytes, name, contents);
        }
        tar_bytes.extend_from_slice(&[0u8; 1024]);
        let encryptor = age::Encryptor::with_user_passphrase(passphrase.clone());
        let mut encrypted = Vec::new();
        let mut writer = encryptor.wrap_output(&mut encrypted).unwrap();
        writer.write_all(&tar_bytes).unwrap();
        writer.finish().unwrap();
        std::fs::write(dest_path, &encrypted).unwrap();
    }

    #[test]
    fn malicious_archive_entries_cannot_write_outside_root() {
        let scratch = tempfile::tempdir().unwrap();
        // Root sits three levels deep so "../.." from it lands back inside
        // `scratch`, letting the traversal case be asserted on real paths.
        let root = scratch.path().join("a/b/root");
        let dirs = DivisiDirs::from_root(root);
        std::fs::create_dir_all(dirs.root()).unwrap();

        let absolute_target = scratch.path().join("absolute-escape.txt");
        let passphrase = SecretString::from("hostile-archive-pass".to_string());
        let archive_dir = tempfile::tempdir().unwrap();
        let archive_path = archive_dir.path().join("hostile.age");
        write_encrypted_archive(
            &archive_path,
            &passphrase,
            &[
                ("config.toml", b"answer = 42\n".as_slice()),
                (absolute_target.to_str().unwrap(), b"pwned\n"),
                ("../../traversal-escape.txt", b"pwned\n"),
                ("/etc/passwd", b"pwned\n"),
                ("..", b"pwned\n"),
            ],
        );

        let report = import(&dirs, &archive_path, &passphrase, false).unwrap();

        let by_label = |name: &str| {
            report
                .files
                .iter()
                .find(|f| f.path == name)
                .unwrap_or_else(|| panic!("entry {name:?} missing from report: {:?}", report.files))
        };
        assert!(by_label("config.toml").success, "benign entries must still restore: {:?}", report.files);
        for hostile in [
            absolute_target.to_str().unwrap(),
            "../../traversal-escape.txt",
            "/etc/passwd",
            "..",
        ] {
            assert!(!by_label(hostile).success, "entry {hostile:?} must be rejected: {:?}", report.files);
        }

        assert_eq!(std::fs::read_to_string(dirs.config_file()).unwrap(), "answer = 42\n");
        assert!(!absolute_target.exists());
        for escaped in ["traversal-escape.txt"] {
            for base in [scratch.path().to_path_buf(), scratch.path().join("a"), scratch.path().join("a/b")] {
                assert!(!base.join(escaped).exists(), "{} must not exist anywhere above root", base.join(escaped).display());
            }
        }
    }
}
