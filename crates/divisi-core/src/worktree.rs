//! Git worktree management (spec section 21): give each task its own
//! isolated working tree and branch, off a real repository, via real `git
//! worktree` subprocess calls — not a fabricated isolation mechanism.
//!
//! Phase 4 scope: one agent and one worktree per task. Cross-
//! worktree coordination (locks, merge/conflict resolution across several
//! concurrent agents) is future work once there's more than one agent
//! actually running concurrently against the same repo — see
//! `docs/architecture.md`.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Creates a worktree at `worktree_path` off `repo_root`, on a new branch
/// `branch_name` based on the repo's current HEAD.
pub fn add(repo_root: &Path, worktree_path: &Path, branch_name: &str) -> Result<()> {
    if !is_git_repo(repo_root) {
        bail!("{} is not a git repository", repo_root.display());
    }
    // A branch left by an earlier run (its worktree directory since removed) is reattached, keeping that work;
    // `-b` would refuse because the branch exists. Pruning first drops the removed directory's stale entry.
    let branch_exists = Command::new("git")
        .current_dir(repo_root)
        .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch_name}")])
        .output()
        .is_ok_and(|o| o.status.success());
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_root);
    if branch_exists {
        let _ = Command::new("git").current_dir(repo_root).args(["worktree", "prune"]).output();
        cmd.args(["worktree", "add"]).arg(worktree_path).arg(branch_name);
    } else {
        cmd.args(["worktree", "add", "-b", branch_name]).arg(worktree_path);
    }
    let output = cmd.output().context("spawning git worktree add")?;
    if !output.status.success() {
        bail!("git worktree add failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    link_orphaned_gitlinks(repo_root, worktree_path);
    Ok(())
}

/// Live-verification finding: a directory that was `git add`-ed while it
/// happened to contain its own `.git` (no `git submodule add` ever run,
/// no `.gitmodules` entry) gets recorded as a bare gitlink (mode 160000)
/// -- git's automatic behavior for that case, not something the user
/// necessarily chose. `git worktree add` faithfully reproduces that: an
/// empty directory at the gitlink's path, since it has no `.gitmodules`
/// to know how to populate it. Every task working in an isolated
/// worktree that touches such a path silently sees it as empty and
/// either fabricates work against nothing or (correctly) refuses,
/// blocking real progress on content that verifiably exists right next
/// to the worktree. Best-effort, not a hard failure: for each such path,
/// if `repo_root/<path>` is itself a real, populated git repo, symlink
/// it into the new worktree in place of the empty stub. A real
/// `.gitmodules`-registered submodule is left to git's own (correct)
/// submodule-init handling.
fn link_orphaned_gitlinks(repo_root: &Path, worktree_path: &Path) {
    let Ok(output) = Command::new("git").current_dir(repo_root).args(["ls-files", "-s"]).output() else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let registered_submodules: std::collections::HashSet<String> = std::fs::read_to_string(repo_root.join(".gitmodules"))
        .ok()
        .map(|s| {
            s.lines()
                .filter_map(|l| l.trim().strip_prefix("path = ").map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        // `<mode> <sha> <stage>\t<path>` -- gitlinks are mode 160000.
        let Some((meta, path)) = line.split_once('\t') else { continue };
        if !meta.starts_with("160000") {
            continue;
        }
        if registered_submodules.contains(path) {
            continue; // a real submodule -- git's own init/update path handles this
        }
        let real_dir = repo_root.join(path);
        if !real_dir.join(".git").exists() {
            continue; // gitlink with nothing real behind it locally -- nothing to link
        }
        let target = worktree_path.join(path);
        // `git worktree add` already created an empty dir here; remove it
        // (best-effort -- only if genuinely empty, never touch real content).
        if target.is_dir() && std::fs::read_dir(&target).map(|mut d| d.next().is_none()).unwrap_or(false) {
            let _ = std::fs::remove_dir(&target);
        }
        if !target.exists() {
            let _ = std::os::unix::fs::symlink(&real_dir, &target);
        }
    }
}

/// Removes a worktree. `force` matches `git worktree remove --force`
/// (needed if the worktree has uncommitted changes) — callers decide
/// whether to force based on whether they intend to discard task output,
/// not this function.
pub fn remove(repo_root: &Path, worktree_path: &Path, force: bool) -> Result<()> {
    let mut args = vec!["worktree".to_string(), "remove".to_string()];
    if force {
        args.push("--force".to_string());
    }
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(&args)
        .arg(worktree_path)
        .output()
        .context("spawning git worktree remove")?;
    if !output.status.success() {
        bail!("git worktree remove failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

/// Best-effort cleanup of a worktree path + branch left over from a prior
/// failed attempt at the same task id. `add`'s branch name and path are
/// both derived from the task id, so a leftover from attempt 1 (e.g. a
/// partially-created directory `git worktree add` leaves behind on
/// failure, plus the branch it registers before erroring) collides with
/// attempt 2's `git worktree add -b` on retry -- live-verification
/// finding 2026-09-17 (E30 dispatch, node retried, second attempt failed
/// with "branch already exists" / directory already exists). Safe to
/// call even when nothing is stale; every step here is best-effort and
/// errors are swallowed, since this only ever runs right before a fresh
/// `add` that will surface its own error if cleanup didn't fully work.
pub fn reset_stale(repo_root: &Path, worktree_path: &Path, branch_name: &str) {
    if worktree_path.exists() {
        let _ = remove(repo_root, worktree_path, true);
        let _ = std::fs::remove_dir_all(worktree_path);
    }
    let _ = Command::new("git").current_dir(repo_root).args(["branch", "-D", branch_name]).output();
}

pub fn list(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .context("spawning git worktree list")?;
    if !output.status.success() {
        bail!("git worktree list failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect())
}

/// Diffs `branch` against the repo's current `HEAD` (`git diff
/// HEAD...branch`) — the same three-dot range a GitHub PR view shows,
/// i.e. what actually landed on `branch` since it forked off, not
/// polluted by anything that's happened on the base branch since. Never
/// merges anything — see `merge` below, kept as a deliberately separate
/// call so a caller looks before deciding.
pub fn diff(repo_root: &Path, branch: &str) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["diff", &format!("HEAD...{branch}")])
        .output()
        .context("spawning git diff")?;
    if !output.status.success() {
        bail!("git diff failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Most changed files `commit_pending` will commit on its own; more than this is left for a person.
pub const MAX_AUTO_COMMIT_FILES: usize = 200;
/// Files larger than this are never auto-committed (binaries, dumps).
pub const MAX_AUTO_COMMIT_BYTES: u64 = 5 * 1024 * 1024;

/// Path fragments that are build output, never source.
const BUILD_OUTPUT: &[&str] = &["__pycache__/", ".pyc", "node_modules/", ".pytest_cache/", "/target/", ".DS_Store"];

/// Whether `path` (repo-relative) is build output that must never be committed.
pub fn is_build_output(path: &str) -> bool {
    let p = format!("/{path}");
    BUILD_OUTPUT.iter().any(|j| p.contains(j))
}

/// Commits whatever a goal's agents left uncommitted in `worktree` onto its branch.
///
/// Live finding (2026-09-24): agents wrote files but rarely committed, so finished goals queued
/// empty branches for merge and their work stayed stranded in the worktree. Returns the number of
/// files committed (0 when clean). Refuses (Err, nothing committed) when more than
/// `MAX_AUTO_COMMIT_FILES` changed: one goal once swept 2,909 vault files into a commit. Files over
/// `MAX_AUTO_COMMIT_BYTES` are skipped. `author` is `Name <email>` (else the repo's own identity).
pub fn commit_pending(worktree: &Path, message: &str, author: Option<&str>) -> Result<usize> {
    let git = |args: &[&str]| Command::new("git").current_dir(worktree).args(args).output();
    let out = git(&["status", "--porcelain", "-z", "-uall"]).context("spawning git status")?;
    if !out.status.success() {
        bail!("git status failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let raw = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut paths: Vec<String> = Vec::new();
    let mut entries = raw.split('\0').filter(|e| !e.is_empty());
    while let Some(e) = entries.next() {
        let (code, path) = e.split_at(3.min(e.len()));
        if code.starts_with('R') || code.starts_with('C') {
            entries.next(); // the rename's source path
        }
        paths.push(path.to_string());
    }
    if paths.is_empty() {
        return Ok(0);
    }
    if paths.len() > MAX_AUTO_COMMIT_FILES {
        bail!("{} changed files is more than {MAX_AUTO_COMMIT_FILES}; left uncommitted for a person to review", paths.len());
    }
    let keep: Vec<&String> = paths
        .iter()
        .filter(|p| !is_build_output(p))
        .filter(|p| std::fs::metadata(worktree.join(p)).map(|m| m.len() <= MAX_AUTO_COMMIT_BYTES).unwrap_or(true))
        .collect();
    if keep.is_empty() {
        return Ok(0);
    }
    let mut add = vec!["add", "-A", "--"];
    add.extend(keep.iter().map(|p| p.as_str()));
    let out = git(&add).context("spawning git add")?;
    if !out.status.success() {
        bail!("git add failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let mut cmd = Command::new("git");
    cmd.current_dir(worktree);
    if let Some((name, email)) = author.and_then(|a| a.rsplit_once('<')).map(|(n, e)| (n.trim().to_string(), e.trim_end_matches('>').trim().to_string())) {
        cmd.args(["-c", &format!("user.name={name}"), "-c", &format!("user.email={email}")]);
    }
    let out = cmd.args(["commit", "-q", "--no-verify", "-m", message]).output().context("spawning git commit")?;
    if !out.status.success() {
        bail!("git commit failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(keep.len())
}

/// Merges `branch` into the repo's current `HEAD` (`git merge --no-ff
/// branch`) — `--no-ff` always creates a merge commit, so the fact that
/// this went through an isolated worktree stays visible in history
/// rather than silently fast-forwarding. Per `docs/architecture.md`:
/// "branches are never auto-merged; that stays a human decision" — this
/// function performs the merge once a caller has explicitly decided to
/// (see `diff` above, meant to be called first), it does not decide FOR
/// the caller.
pub fn merge(repo_root: &Path, branch: &str) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["merge", "--no-ff", branch, "-m", &format!("Merge branch '{branch}'")])
        .output()
        .context("spawning git merge")?;
    if !output.status.success() {
        let merge_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        // A real conflict leaves conflict markers in the working tree and
        // `.git/MERGE_HEAD` in place — a genuine mid-merge repo state. This
        // is reachable unattended (an MCP agent calling
        // `worktree_merge_apply`), so leaving that half-done state around
        // for a human to notice later is worse than restoring the
        // pre-merge state ourselves and surfacing a clear error, matching
        // this project's general preference for known-clean over half-done.
        let abort_output = Command::new("git")
            .current_dir(repo_root)
            .args(["merge", "--abort"])
            .output()
            .context("spawning git merge --abort")?;
        if !abort_output.status.success() {
            bail!(
                "git merge failed ({merge_stderr}) AND git merge --abort also failed ({}); repo may be left in a conflicted state, manual cleanup required",
                String::from_utf8_lossy(&abort_output.stderr)
            );
        }
        bail!("git merge failed and was aborted (repo restored to its pre-merge state): {merge_stderr}");
    }
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .current_dir(path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(dir: &Path) {
        let run = |args: &[&str]| {
            let status = Command::new("git").current_dir(dir).args(args).status().unwrap();
            assert!(status.success(), "git {:?} failed", args);
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("README.md"), "hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "initial"]);
    }

    #[test]
    fn add_reattaches_an_existing_branch_whose_worktree_was_removed() {
        // Live finding (2026-09-27): a resumed goal's worktree directory had been cleaned up but its branch
        // (holding the earlier work) was kept, and `git worktree add -b` refused: the branch already exists.
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let parent = tempfile::tempdir().unwrap();
        let wt = parent.path().join("goal-1");
        add(repo.path(), &wt, "divisi/goal-1").unwrap();
        std::fs::write(wt.join("work.txt"), "earlier attempt").unwrap();
        for args in [&["add", "work.txt"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "work"]] {
            assert!(Command::new("git").args(args).current_dir(&wt).status().unwrap().success());
        }
        std::fs::remove_dir_all(&wt).unwrap();

        add(repo.path(), &wt, "divisi/goal-1").unwrap();

        assert_eq!(std::fs::read_to_string(wt.join("work.txt")).unwrap(), "earlier attempt", "the earlier work is kept");
    }

    fn last_commit(dir: &Path) -> String {
        let o = Command::new("git").current_dir(dir).args(["log", "-1", "--format=%an <%ae>|%s"]).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    #[test]
    fn commit_pending_commits_leftover_work_skips_big_files_and_refuses_sweeps() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        assert_eq!(commit_pending(dir.path(), "feat: x", None).unwrap(), 0, "clean tree commits nothing");

        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() {}").unwrap();
        std::fs::write(dir.path().join("README.md"), "changed").unwrap();
        std::fs::write(dir.path().join("big.bin"), vec![0u8; (MAX_AUTO_COMMIT_BYTES + 1) as usize]).unwrap();
        assert_eq!(commit_pending(dir.path(), "feat: goal work", Some("Ann Dev <ann@example.com>")).unwrap(), 2);
        assert_eq!(last_commit(dir.path()), "Ann Dev <ann@example.com>|feat: goal work");
        assert!(dir.path().join("big.bin").exists(), "the big file stays, uncommitted");
        std::fs::create_dir(dir.path().join("__pycache__")).unwrap();
        std::fs::write(dir.path().join("__pycache__/m.cpython-314.pyc"), "b").unwrap();
        assert_eq!(commit_pending(dir.path(), "feat: y", None).unwrap(), 0, "build output is never committed");
        assert!(is_build_output("a/__pycache__/x.pyc") && is_build_output("node_modules/x/index.js"));
        assert!(!is_build_output("src/target_manager.rs") && !is_build_output("docs/node_modules.md"));

        for i in 0..=MAX_AUTO_COMMIT_FILES {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
        }
        assert!(commit_pending(dir.path(), "feat: sweep", None).is_err());
        assert_eq!(last_commit(dir.path()), "Ann Dev <ann@example.com>|feat: goal work", "nothing committed on refusal");
    }

    #[test]
    fn add_creates_a_real_worktree_on_a_new_branch() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktree_parent = tempfile::tempdir().unwrap();
        let worktree_path = worktree_parent.path().join("task-1");

        add(repo.path(), &worktree_path, "divisi/task-1").unwrap();
        assert!(worktree_path.join("README.md").is_file());

        let worktrees = list(repo.path()).unwrap();
        assert!(worktrees.iter().any(|p| p == &worktree_path.canonicalize().unwrap() || p == &worktree_path));
    }

    /// Live-verification regression (E30 dispatch, 2026-09-17): a retried
    /// task reuses the same id, so `add`'s branch name and path collide
    /// with whatever the first failed attempt left behind. `reset_stale`
    /// must clear both so the retry's `add` succeeds cleanly.
    #[test]
    fn reset_stale_clears_a_leftover_worktree_and_branch_so_retry_add_succeeds() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktree_parent = tempfile::tempdir().unwrap();
        let worktree_path = worktree_parent.path().join("task-1");

        add(repo.path(), &worktree_path, "divisi/task-1").unwrap();
        assert!(worktree_path.is_dir(), "first attempt's worktree should exist");

        // a second `add` on the same id, without cleanup, must fail --
        // this reproduces the live bug before asserting the fix.
        assert!(add(repo.path(), &worktree_path, "divisi/task-1").is_err());

        reset_stale(repo.path(), &worktree_path, "divisi/task-1");
        add(repo.path(), &worktree_path, "divisi/task-1").unwrap();
        assert!(worktree_path.join("README.md").is_file(), "retry's worktree should be usable");
    }

    #[test]
    fn reset_stale_is_a_noop_when_nothing_is_stale() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktree_parent = tempfile::tempdir().unwrap();
        let worktree_path = worktree_parent.path().join("task-1");

        // nothing exists yet -- must not error or panic.
        reset_stale(repo.path(), &worktree_path, "divisi/task-1");
        add(repo.path(), &worktree_path, "divisi/task-1").unwrap();
    }

    /// Live-verification finding: a directory `git add`-ed while it
    /// happened to contain its own `.git` becomes a bare gitlink with no
    /// `.gitmodules` entry -- `git worktree add` alone leaves it as an
    /// empty stub in the new worktree, silently hiding real content that
    /// exists right next to it in `repo_root`.
    #[test]
    fn add_links_an_orphaned_gitlink_directory_into_the_new_worktree() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());

        // an inner directory that has its own real, populated `.git` --
        // `git add` on the outer repo records this as a gitlink (160000),
        // not the inner file contents, and no `.gitmodules` is written.
        let inner = repo.path().join("nested-repo");
        std::fs::create_dir(&inner).unwrap();
        init_repo(&inner);
        std::fs::write(inner.join("real-content.md"), "actual content").unwrap();
        Command::new("git").current_dir(&inner).args(["add", "."]).status().unwrap();
        Command::new("git").current_dir(&inner).args(["commit", "-q", "-m", "more content"]).status().unwrap();

        let run = |args: &[&str]| {
            let status = Command::new("git").current_dir(repo.path()).args(args).status().unwrap();
            assert!(status.success(), "git {:?} failed", args);
        };
        run(&["add", "nested-repo"]);
        run(&["commit", "-q", "-m", "add nested-repo as an (accidental) gitlink"]);
        assert!(
            String::from_utf8(Command::new("git").current_dir(repo.path()).args(["ls-files", "-s", "nested-repo"]).output().unwrap().stdout)
                .unwrap()
                .starts_with("160000"),
            "test setup: nested-repo must actually be recorded as a gitlink"
        );

        let worktree_parent = tempfile::tempdir().unwrap();
        let worktree_path = worktree_parent.path().join("task-1");
        add(repo.path(), &worktree_path, "divisi/task-1").unwrap();

        assert_eq!(
            std::fs::read_to_string(worktree_path.join("nested-repo").join("real-content.md")).unwrap(),
            "actual content",
            "the gitlink's real content must be reachable from the worktree, not an empty stub"
        );
    }

    #[test]
    fn remove_deletes_the_worktree() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktree_parent = tempfile::tempdir().unwrap();
        let worktree_path = worktree_parent.path().join("task-2");

        add(repo.path(), &worktree_path, "divisi/task-2").unwrap();
        remove(repo.path(), &worktree_path, false).unwrap();
        assert!(!worktree_path.exists());
    }

    #[test]
    fn add_fails_outside_a_git_repo() {
        let not_a_repo = tempfile::tempdir().unwrap();
        let worktree_path = tempfile::tempdir().unwrap().path().join("task-3");
        assert!(add(not_a_repo.path(), &worktree_path, "divisi/task-3").is_err());
    }

    #[test]
    fn diff_shows_changes_made_on_the_worktree_branch() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktree_parent = tempfile::tempdir().unwrap();
        let worktree_path = worktree_parent.path().join("task-diff");

        add(repo.path(), &worktree_path, "divisi/task-diff").unwrap();
        std::fs::write(worktree_path.join("new-file.txt"), "hello from the worktree").unwrap();
        let status = Command::new("git").current_dir(&worktree_path).args(["add", "."]).status().unwrap();
        assert!(status.success());
        let status = Command::new("git").current_dir(&worktree_path).args(["commit", "-q", "-m", "add new-file"]).status().unwrap();
        assert!(status.success());

        let diff_output = diff(repo.path(), "divisi/task-diff").unwrap();
        assert!(diff_output.contains("new-file.txt"));
        assert!(diff_output.contains("hello from the worktree"));
    }

    #[test]
    fn merge_brings_worktree_changes_into_the_main_branch() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktree_parent = tempfile::tempdir().unwrap();
        let worktree_path = worktree_parent.path().join("task-merge");

        add(repo.path(), &worktree_path, "divisi/task-merge").unwrap();
        std::fs::write(worktree_path.join("merged-file.txt"), "merge me").unwrap();
        let status = Command::new("git").current_dir(&worktree_path).args(["add", "."]).status().unwrap();
        assert!(status.success());
        let status = Command::new("git").current_dir(&worktree_path).args(["commit", "-q", "-m", "add merged-file"]).status().unwrap();
        assert!(status.success());

        merge(repo.path(), "divisi/task-merge").unwrap();
        assert!(repo.path().join("merged-file.txt").is_file());
    }

    #[test]
    fn merge_aborts_and_restores_a_clean_state_on_a_real_conflict() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktree_parent = tempfile::tempdir().unwrap();
        let worktree_path = worktree_parent.path().join("task-conflict");

        add(repo.path(), &worktree_path, "divisi/task-conflict").unwrap();

        // Conflicting edit on the worktree branch.
        std::fs::write(worktree_path.join("README.md"), "changed on the worktree branch").unwrap();
        let status = Command::new("git").current_dir(&worktree_path).args(["add", "."]).status().unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .current_dir(&worktree_path)
            .args(["commit", "-q", "-m", "conflicting change on worktree branch"])
            .status()
            .unwrap();
        assert!(status.success());

        // Conflicting edit on the main branch (repo_root), same lines.
        std::fs::write(repo.path().join("README.md"), "changed on the main branch").unwrap();
        let status = Command::new("git").current_dir(repo.path()).args(["add", "."]).status().unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .current_dir(repo.path())
            .args(["commit", "-q", "-m", "conflicting change on main branch"])
            .status()
            .unwrap();
        assert!(status.success());

        let result = merge(repo.path(), "divisi/task-conflict");
        assert!(result.is_err(), "expected a real conflict to produce an Err");
        let message = format!("{:#}", result.unwrap_err());
        assert!(
            message.contains("aborted"),
            "expected the error to mention the merge was aborted, got: {message}"
        );

        // The repo must be back in a clean, non-mid-merge state.
        assert!(
            !repo.path().join(".git").join("MERGE_HEAD").exists(),
            "MERGE_HEAD should not exist after an aborted merge"
        );
        let status_output = Command::new("git")
            .current_dir(repo.path())
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&status_output.stdout).trim().is_empty(),
            "expected a clean working tree after the aborted merge"
        );
    }

    #[test]
    fn diff_fails_for_a_nonexistent_branch() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        assert!(diff(repo.path(), "no-such-branch").is_err());
    }
}
