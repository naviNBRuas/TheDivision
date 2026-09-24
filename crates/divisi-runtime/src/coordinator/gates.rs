//! Deterministic checks on a goal's branch that the LLM integrator cannot talk its way past.
//!
//! Live finding (2026-09-24): of 71 merges queued for a human, none was worth landing. Pool coders
//! wrote Python stubs into Go repositories, committed `__pycache__`, and swapped `<img>` for an
//! un-imported `<Image>`; the integrator, reading only the agents' own summaries, marked these goals
//! done. These gates look at the branch itself. Every gap they return makes the goal not met, so
//! a continuation round is planned to fix exactly that.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Source extensions and the language they belong to. A new file in a language the base tree has
/// no file of is almost always an agent that ignored the repo's stack.
const LANGS: &[(&str, &str)] = &[
    ("py", "Python"),
    ("go", "Go"),
    ("rs", "Rust"),
    ("js", "JavaScript"),
    ("jsx", "JavaScript"),
    ("ts", "TypeScript"),
    ("tsx", "TypeScript"),
    ("java", "Java"),
    ("rb", "Ruby"),
    ("php", "PHP"),
];

use divisi_core::worktree::is_build_output as is_junk;

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git").current_dir(dir).args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Gaps in the goal branch checked out at `worktree`, compared with `base` (a commit-ish).
pub fn branch_gaps(worktree: &Path, base: &str) -> Vec<String> {
    let mut gaps = Vec::new();
    let Some(added) = git(worktree, &["diff", "--name-only", "--diff-filter=A", &format!("{base}...HEAD")]) else {
        return gaps;
    };
    let base_files = git(worktree, &["ls-tree", "-r", "--name-only", base]).unwrap_or_default();
    let has_ext = |ext: &str| base_files.lines().any(|f| f.ends_with(&format!(".{ext}")));
    let repo_langs: Vec<&str> = LANGS.iter().filter(|(e, _)| has_ext(e)).map(|(_, l)| *l).collect();
    for f in added.lines() {
        if is_junk(f) {
            gaps.push(format!("{f} is build output and must not be committed; remove it"));
            continue;
        }
        let Some(ext) = Path::new(f).extension().and_then(|e| e.to_str()) else { continue };
        let Some((_, lang)) = LANGS.iter().find(|(e, _)| *e == ext) else { continue };
        if !repo_langs.is_empty() && !repo_langs.contains(lang) {
            gaps.push(format!(
                "{f} is {lang}, but this repository is written in {}; delete it and implement the change in the repository's own language and existing structure",
                repo_langs.join("/")
            ));
        }
    }
    gaps
}

/// Runs the repository's own quick build check when it has an obvious one (Go, Rust). Returns a gap
/// with the tail of the output when it fails; None when it passes or there is nothing to run.
pub fn build_gap(worktree: &Path, timeout: Duration) -> Option<String> {
    let (prog, args): (&str, &[&str]) = if worktree.join("go.mod").exists() {
        ("go", &["build", "./..."])
    } else if worktree.join("Cargo.toml").exists() {
        ("cargo", &["check", "--quiet", "--all-targets"])
    } else {
        return None;
    };
    let mut child = Command::new(prog)
        .current_dir(worktree)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return None,
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                return None; // too slow to judge here; not a failure of the goal
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(500)),
            Err(_) => return None,
        }
    }
    let mut err = String::new();
    if let Some(mut s) = child.stderr.take() {
        use std::io::Read;
        let _ = s.read_to_string(&mut err);
    }
    let tail: Vec<&str> = err.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect();
    Some(format!("`{prog} {}` fails in the goal worktree:\n{}", args.join(" "), tail.join("\n")))
}

/// The repository's main stack, from its manifest files (checked from `dir` up to the repo root).
pub fn stack_of(dir: &Path) -> Option<&'static str> {
    const MANIFESTS: &[(&str, &str)] = &[
        ("go.mod", "Go"),
        ("Cargo.toml", "Rust"),
        ("package.json", "TypeScript/JavaScript"),
        ("pyproject.toml", "Python"),
        ("requirements.txt", "Python"),
    ];
    let mut d = Some(dir);
    while let Some(p) = d {
        if let Some((_, lang)) = MANIFESTS.iter().find(|(m, _)| p.join(m).exists()) {
            return Some(lang);
        }
        if p.join(".git").exists() {
            break;
        }
        d = p.parent();
    }
    None
}

/// A line for planner and node prompts naming the stack. Live finding (2026-09-24): planners asked
/// for `data_core.py` in a Go repository because nothing told them what the repository is written in.
pub fn stack_note(dir: &Path) -> String {
    match stack_of(dir) {
        Some(lang) => format!(
            "\n\nREPOSITORY STACK: this repository is written in {lang}. Write all code in {lang}, inside its existing \
            packages and layout; never add standalone files in another language."
        ),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        assert!(Command::new("git").current_dir(dir).args(args).status().unwrap().success(), "git {args:?}");
    }

    #[test]
    fn stack_comes_from_the_nearest_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("internal/x")).unwrap();
        assert_eq!(stack_of(dir.path()), None);
        assert_eq!(stack_note(dir.path()), "");
        std::fs::write(dir.path().join("go.mod"), "module m").unwrap();
        assert_eq!(stack_of(&dir.path().join("internal/x")), Some("Go"));
        assert!(stack_note(dir.path()).contains("written in Go"));
    }

    #[test]
    fn flags_foreign_language_files_and_build_output_but_not_native_code() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        run(d, &["init", "-q"]);
        run(d, &["config", "user.email", "t@example.com"]);
        run(d, &["config", "user.name", "T"]);
        std::fs::write(d.join("main.go"), "package main").unwrap();
        run(d, &["add", "."]);
        run(d, &["commit", "-q", "-m", "base"]);
        let base = git(d, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        std::fs::create_dir_all(d.join("internal/cdc/__pycache__")).unwrap();
        std::fs::write(d.join("internal/cdc/cdc.py"), "x = 1").unwrap();
        std::fs::write(d.join("internal/cdc/__pycache__/cdc.cpython-314.pyc"), "b").unwrap();
        std::fs::write(d.join("internal/cdc/cdc.go"), "package cdc").unwrap();
        std::fs::write(d.join("internal/cdc/README.md"), "docs").unwrap();
        run(d, &["add", "."]);
        run(d, &["commit", "-q", "-m", "goal"]);

        let gaps = branch_gaps(d, &base);
        assert_eq!(gaps.len(), 2, "{gaps:?}");
        assert!(gaps.iter().any(|g| g.starts_with("internal/cdc/cdc.py is Python") && g.contains("written in Go")));
        assert!(gaps.iter().any(|g| g.contains("__pycache__") && g.contains("build output")));
    }

}
