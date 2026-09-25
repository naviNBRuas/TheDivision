//! Filesystem confinement for agent CLIs.
//!
//! Live finding (2026-09-25): agents told to work only in their goal worktree still wrote into the shared
//! checkouts by absolute path, leaving uncommitted edits in almost every repository. Tool-level permissions
//! (e.g. opencode/kilo `external_directory`) do not cover shell commands. When `DIVISI_CONFINE_ROOTS` lists
//! directories (colon-separated) and `bwrap` is installed, every agent process runs in a bubblewrap mount
//! namespace where those roots are read-only, except the agent's own working directory and the git metadata
//! of its repository (so it can still commit); a shared checkout inside the roots stays read-only unless listed
//! in `DIVISI_CONFINE_WRITABLE`. Everything outside the roots stays as it is.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The bubblewrap argv that runs `command args` with `roots` read-only. A working directory outside the roots
/// (a goal worktree) keeps its repo's `git_dir` writable so the agent can commit. A working directory inside
/// the roots is a shared checkout: read-only (planning tasks only read), unless its repository's top level
/// (`toplevel`) is one of `writable` (e.g. the workspace root, whose own goals edit it), in which case it and
/// its `git_dir` are writable. A repository nested inside a writable one (a submodule) is its own checkout
/// and stays read-only. Outside any repository, a `cwd` under a `writable` entry is writable.
pub fn bwrap_argv_with(
    roots: &[PathBuf],
    writable: &[PathBuf],
    cwd: &Path,
    toplevel: Option<&Path>,
    git_dir: Option<&Path>,
    command: &str,
    args: &[String],
) -> Vec<String> {
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let mut argv: Vec<String> = ["--die-with-parent", "--bind", "/", "/", "--dev-bind", "/dev", "/dev", "--proc", "/proc"]
        .iter()
        .map(|a| a.to_string())
        .collect();
    for root in roots {
        argv.extend(["--ro-bind".into(), s(root), s(root)]);
    }
    let inside = |p: &Path| roots.iter().any(|r| p.starts_with(r));
    let cwd_writable = inside(cwd)
        && writable.iter().any(|w| match toplevel {
            Some(top) => top == w.as_path(),
            None => cwd.starts_with(w),
        });
    if cwd_writable {
        argv.extend(["--bind".into(), s(cwd), s(cwd)]);
    }
    if let Some(g) = git_dir {
        if inside(g) && (cwd_writable || !inside(cwd)) {
            argv.extend(["--bind".into(), s(g), s(g)]);
        }
    }
    argv.extend(["--chdir".into(), s(cwd), "--".into(), command.to_string()]);
    argv.extend(args.iter().cloned());
    argv
}

/// `bwrap_argv_with` with no shared checkout writable.
pub fn bwrap_argv(roots: &[PathBuf], cwd: &Path, git_dir: Option<&Path>, command: &str, args: &[String]) -> Vec<String> {
    bwrap_argv_with(roots, &[], cwd, None, git_dir, command, args)
}

fn paths_from_env(var: &str) -> Vec<PathBuf> {
    std::env::var(var)
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.trim().is_empty())
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p)))
        .collect()
}

fn git_common_dir(cwd: &Path) -> Option<PathBuf> {
    git_path(cwd, "--git-common-dir")
}

fn git_toplevel(cwd: &Path) -> Option<PathBuf> {
    git_path(cwd, "--show-toplevel")
}

fn git_path(cwd: &Path, flag: &str) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", flag])
        .current_dir(cwd)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
}

fn bwrap_available() -> bool {
    Command::new("bwrap").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// `(program, args)` to spawn for `command args` in `cwd`: wrapped in bubblewrap when confinement is
/// configured and available, unchanged otherwise.
pub fn wrap(command: &str, args: &[String], cwd: &Path) -> (String, Vec<String>) {
    let roots = paths_from_env("DIVISI_CONFINE_ROOTS");
    if roots.is_empty() || !bwrap_available() {
        return (command.to_string(), args.to_vec());
    }
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let git_dir = git_common_dir(&cwd);
    let writable = paths_from_env("DIVISI_CONFINE_WRITABLE");
    ("bwrap".to_string(), bwrap_argv_with(&roots, &writable, &cwd, git_toplevel(&cwd).as_deref(), git_dir.as_deref(), command, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_pair(argv: &[String], flag: &str, path: &str) -> bool {
        argv.windows(3).any(|w| w[0] == flag && w[1] == path && w[2] == path)
    }

    #[test]
    fn roots_are_read_only_and_only_the_agents_own_dirs_are_writable() {
        let roots = [PathBuf::from("/work/company")];
        let argv = bwrap_argv_with(
            &roots,
            &[PathBuf::from("/work/company/repo")],
            Path::new("/work/company/repo/sub"),
            Some(Path::new("/work/company/repo")),
            Some(Path::new("/work/company/repo/.git")),
            "kilo",
            &["run".into(), "go".into()],
        );
        assert!(has_pair(&argv, "--ro-bind", "/work/company"));
        assert!(has_pair(&argv, "--bind", "/work/company/repo/sub"));
        assert!(has_pair(&argv, "--bind", "/work/company/repo/.git"));
        let sep = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(&argv[sep + 1..], ["kilo", "run", "go"]);
        // The writable binds come after the read-only root, so they win.
        let ro = argv.iter().position(|a| a == "--ro-bind").unwrap();
        let rw = argv.iter().rposition(|a| a == "--bind").unwrap();
        assert!(rw > ro);
    }

    #[test]
    fn a_shared_checkout_is_read_only_unless_listed_as_writable() {
        // Planning tasks run in a repo's main checkout; they only read.
        let roots = [PathBuf::from("/work/company")];
        let argv = bwrap_argv_with(&roots, &[], Path::new("/work/company/nbr-core"), Some(Path::new("/work/company/nbr-core")), Some(Path::new("/work/company/nbr-core/.git")), "kilo", &[]);
        assert!(!has_pair(&argv, "--bind", "/work/company/nbr-core"));
        assert!(!has_pair(&argv, "--bind", "/work/company/nbr-core/.git"));
        // The workspace root's own goals edit it, so it is listed as writable.
        let writable = [PathBuf::from("/work/company/ws")];
        let argv = bwrap_argv_with(&roots, &writable, Path::new("/work/company/ws/docs"), Some(Path::new("/work/company/ws")), Some(Path::new("/work/company/ws/.git")), "kilo", &[]);
        assert!(has_pair(&argv, "--bind", "/work/company/ws/docs"));
        assert!(has_pair(&argv, "--bind", "/work/company/ws/.git"));
        // A module repo nested inside the writable workspace is its own repo and stays read-only.
        let argv = bwrap_argv_with(&roots, &writable, Path::new("/work/company/ws/nbr-core"), Some(Path::new("/work/company/ws/nbr-core")), Some(Path::new("/work/company/ws/.git/modules/nbr-core")), "kilo", &[]);
        assert!(!has_pair(&argv, "--bind", "/work/company/ws/nbr-core"));
        assert!(!has_pair(&argv, "--bind", "/work/company/ws/.git/modules/nbr-core"));
    }

    #[test]
    fn a_worktree_outside_the_roots_needs_no_bind_but_its_repo_git_dir_does() {
        let roots = [PathBuf::from("/work/company")];
        let argv = bwrap_argv(
            &roots,
            Path::new("/state/worktrees/goal-1"),
            Some(Path::new("/work/company/repo/.git")),
            "kilo",
            &[],
        );
        assert!(!has_pair(&argv, "--bind", "/state/worktrees/goal-1"));
        assert!(has_pair(&argv, "--bind", "/work/company/repo/.git"));
    }

    /// Real bubblewrap: a write under a root fails, a write in the working directory succeeds.
    #[test]
    fn bwrap_really_makes_the_roots_read_only() {
        if !bwrap_available() {
            eprintln!("bwrap not installed; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let work = root.join("work");
        let other = root.join("other");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let script = format!("echo ok > {}/in.txt; echo no > {}/out.txt", work.display(), other.display());
        let argv = bwrap_argv_with(&[root.clone()], &[work.clone()], &work, None, None, "sh", &["-c".into(), script]);
        let status = Command::new("bwrap").args(&argv).status().unwrap();
        assert!(!status.success(), "the write outside the working directory must fail");
        assert!(work.join("in.txt").exists());
        assert!(!other.join("out.txt").exists());
    }
}
