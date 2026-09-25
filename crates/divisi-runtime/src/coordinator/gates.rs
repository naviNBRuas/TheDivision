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
    // Live finding (2026-09-24): goals were marked done with an empty branch; nothing to merge.
    let changed = git(worktree, &["diff", "--name-only", &format!("{base}...HEAD")]).unwrap_or_default();
    if changed.trim().is_empty() {
        gaps.push("the goal branch has no changes: nothing was implemented or committed".to_string());
        return gaps;
    }
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

/// The first `YYYY-MM-DD` date in `line`.
fn iso_date(line: &str) -> Option<chrono::NaiveDate> {
    let b = line.as_bytes();
    (0..b.len().saturating_sub(9)).find_map(|i| {
        let s = line.get(i..i + 10)?;
        let shape = s.bytes().enumerate().all(|(j, c)| if j == 4 || j == 7 { c == b'-' } else { c.is_ascii_digit() });
        if !shape || (i > 0 && b[i - 1].is_ascii_digit()) {
            return None;
        }
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
    })
}

/// A metadata line that only records when a note was touched (`updated: 2026-08-06`,
/// `**Last updated:** 2026-09-21`).
fn is_date_stamp(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    (l.contains("updated") || l.contains("modified") || l.contains("reviewed")) && iso_date(line).is_some()
}

/// Gaps in the Markdown the goal changed. Live finding (2026-09-25): vault "maintenance" goals were
/// integrated as done while their whole diff was re-stamped `Last updated` dates (some moved back to
/// 2023, some past today), duplicated frontmatter keys, and a table row pasted into itself.
pub fn doc_gaps(worktree: &Path, base: &str, today: chrono::NaiveDate) -> Vec<String> {
    let mut gaps = Vec::new();
    let Some(changed) = git(worktree, &["diff", "--name-only", "--diff-filter=AM", &format!("{base}...HEAD"), "--", "*.md"]) else {
        return gaps;
    };
    for f in changed.lines() {
        let Some(diff) = git(worktree, &["diff", "-U0", &format!("{base}...HEAD"), "--", f]) else { continue };
        let mut removed = Vec::new();
        let mut added = Vec::new();
        for l in diff.lines() {
            if l.starts_with("+++") || l.starts_with("---") {
                continue;
            }
            if let Some(r) = l.strip_prefix('-') {
                removed.push(r);
            } else if let Some(a) = l.strip_prefix('+') {
                added.push(a);
            }
        }
        let edits: Vec<&&str> = removed.iter().chain(added.iter()).filter(|l| !l.trim().is_empty()).collect();
        if !removed.is_empty() && !edits.is_empty() && edits.iter().all(|l| is_date_stamp(l)) {
            gaps.push(format!("{f}: the only change is a re-stamped date; revert it (a date changes only with real content)"));
            continue;
        }
        let old = removed.iter().filter(|l| is_date_stamp(l)).filter_map(|l| iso_date(l)).max();
        for d in added.iter().filter(|l| is_date_stamp(l)).filter_map(|l| iso_date(l)) {
            if d > today {
                gaps.push(format!("{f}: date {d} is in the future (today is {today}); use the real date"));
            } else if old.is_some_and(|o| d < o) {
                gaps.push(format!("{f}: date moved back from {} to {d}; dates never go backwards", old.unwrap()));
            }
        }
        if let Ok(text) = std::fs::read_to_string(worktree.join(f)) {
            if let Some(key) = duplicate_frontmatter_key(&text) {
                gaps.push(format!("{f}: frontmatter key `{key}` appears more than once; keep one"));
            }
        }
    }
    gaps
}

/// The first top-level key repeated in a `---` YAML frontmatter block.
fn duplicate_frontmatter_key(text: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    for l in lines.take_while(|l| l.trim() != "---") {
        if l.starts_with([' ', '\t', '-', '#']) {
            continue;
        }
        if let Some((k, _)) = l.split_once(':') {
            if !seen.insert(k.trim().to_string()) {
                return Some(k.trim().to_string());
            }
        }
    }
    None
}

/// Runs the repository's own quick build check when it has an obvious one (Go, Rust). Returns a gap
/// with the errors that point at files the goal changed; None when it passes, there is nothing to
/// run, or every error is in code the goal did not touch. Live finding (2026-09-25): six Go mainlines
/// did not compile, so an unscoped check would have failed every goal there for breakage it did not
/// cause.
pub fn build_gap(worktree: &Path, base: &str, timeout: Duration) -> Option<String> {
    let (prog, args): (&str, &[&str]) = if worktree.join("go.mod").exists() {
        // Compiles every package and its tests without running them (`-exec true`): `go build`
        // skips _test.go files, which let a goal land a test that no longer compiled (2026-09-25).
        ("go", &["test", "-count=1", "-exec", "true", "./..."])
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
    let changed = git(worktree, &["diff", "--name-only", &format!("{base}...HEAD")]).unwrap_or_default();
    let ours = errors_in_changed_files(&err, &changed);
    if ours.is_empty() {
        return None;
    }
    Some(format!("`{prog} {}` fails in files this goal changed:\n{}", args.join(" "), ours.join("\n")))
}

/// Error lines of a build's output that name one of `changed` (git paths, one per line). Matches the
/// full path or its last two components, since tools print paths relative to the module or crate.
fn errors_in_changed_files<'a>(output: &'a str, changed: &str) -> Vec<&'a str> {
    let needles: Vec<String> = changed
        .lines()
        .filter(|f| !f.is_empty())
        .flat_map(|f| {
            let parts: Vec<&str> = f.rsplitn(3, '/').collect();
            let short = if parts.len() >= 2 { format!("{}/{}", parts[1], parts[0]) } else { f.to_string() };
            [format!("{f}:"), format!("{short}:")]
        })
        .collect();
    output.lines().filter(|l| needles.iter().any(|n| l.contains(n.as_str()))).take(15).collect()
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
        assert_eq!(branch_gaps(d, &base), ["the goal branch has no changes: nothing was implemented or committed"]);
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

    #[test]
    fn flags_date_churn_backdating_future_dates_and_duplicate_keys_but_not_real_edits() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        run(d, &["init", "-q"]);
        run(d, &["config", "user.email", "t@example.com"]);
        run(d, &["config", "user.name", "T"]);
        let note = |date: &str, body: &str| format!("---\ntype: note\nupdated: {date}\n---\n\n# N\n\n**Last updated:** {date}\n\n{body}\n");
        for f in ["churn.md", "back.md", "future.md", "dup.md", "real.md"] {
            std::fs::write(d.join(f), note("2026-09-20", "Body.")).unwrap();
        }
        run(d, &["add", "."]);
        run(d, &["commit", "-q", "-m", "base"]);
        let base = git(d, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        std::fs::write(d.join("churn.md"), note("2026-09-24", "Body.")).unwrap();
        std::fs::write(d.join("back.md"), note("2023-11-15", "New body.")).unwrap();
        std::fs::write(d.join("future.md"), note("2026-12-01", "New body.")).unwrap();
        std::fs::write(d.join("dup.md"), note("2026-09-20", "New body.").replacen("type: note\n", "type: note\ntype: note\n", 1)).unwrap();
        std::fs::write(d.join("real.md"), note("2026-09-25", "A real new paragraph.")).unwrap();
        run(d, &["commit", "-qam", "goal"]);

        let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap();
        let gaps = doc_gaps(d, &base, today);
        let has = |f: &str, s: &str| gaps.iter().any(|g| g.starts_with(f) && g.contains(s));
        assert!(has("churn.md", "re-stamped"), "{gaps:?}");
        assert!(has("back.md", "moved back"), "{gaps:?}");
        assert!(has("future.md", "in the future"), "{gaps:?}");
        assert!(has("dup.md", "`type`"), "{gaps:?}");
        assert!(!gaps.iter().any(|g| g.starts_with("real.md")), "{gaps:?}");
    }

    #[test]
    fn build_errors_count_only_in_files_the_goal_changed() {
        let out = "# m/internal/license\ninternal/license/keystore.go:136:12: k.loadExisting undefined\n\
                   # m/internal/agent/schema\ninternal/agent/schema/stock_agent_test.go:10:16: undefined: ModelPrefs\n";
        let changed = "internal/agent/schema/stock_agent_test.go\ninternal/agent/schema/stock_agent.go\n";
        assert_eq!(errors_in_changed_files(out, changed), ["internal/agent/schema/stock_agent_test.go:10:16: undefined: ModelPrefs"]);
        assert!(errors_in_changed_files(out, "README.md\n").is_empty());
        let nested = "nbr-sterling/internal/mcp/client.go:62:2: no required module";
        assert_eq!(errors_in_changed_files(nested, "internal/mcp/client.go\n").len(), 1);
    }

}
