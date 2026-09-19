//! Skill registry (spec section 14): directories under
//! `~/.config/single/skills/<name>/`. Phase 2 scope is local-only —
//! `install` copies a local source directory in; there is no marketplace/
//! network fetch yet (that would need the plugin system from spec section
//! 29, which is later-phase work). A skill is just a directory; SingleCLI
//! doesn't yet interpret its contents (translating it into each agent's
//! native skill mechanism is also later-phase — see docs/architecture.md).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

pub fn list(skills_dir: &Path) -> Result<Vec<String>> {
    if !skills_dir.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(skills_dir).with_context(|| format!("reading {}", skills_dir.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Copies `source` (a directory) into `skills_dir/<name>`. Refuses to
/// overwrite an existing skill of the same name — remove it first.
pub fn install(skills_dir: &Path, name: &str, source: &Path) -> Result<PathBuf> {
    if !source.is_dir() {
        bail!("{} is not a directory", source.display());
    }
    let dest = skills_dir.join(name);
    if dest.exists() {
        bail!("skill '{name}' already exists at {}; remove it first", dest.display());
    }
    std::fs::create_dir_all(skills_dir)?;
    copy_dir_recursive(source, &dest)?;
    Ok(dest)
}

/// Removes `skills_dir/<name>`. Refuses anything that would escape
/// `skills_dir` (defense against a `name` containing `..`).
pub fn remove(skills_dir: &Path, name: &str) -> Result<bool> {
    let dest = skills_dir.join(name);
    let canonical_dir = skills_dir.canonicalize().unwrap_or_else(|_| skills_dir.to_path_buf());
    let canonical_dest = match dest.canonicalize() {
        Ok(p) => p,
        Err(_) => return Ok(false), // doesn't exist
    };
    if !canonical_dest.starts_with(&canonical_dir) {
        bail!("refusing to remove a path outside the skills directory: {}", dest.display());
    }
    std::fs::remove_dir_all(&canonical_dest)?;
    Ok(true)
}

pub fn inspect(skills_dir: &Path, name: &str) -> Result<Option<Vec<String>>> {
    let dir = skills_dir.join(name);
    if !dir.is_dir() {
        return Ok(None);
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        if let Some(n) = entry?.file_name().to_str() {
            entries.push(n.to_string());
        }
    }
    entries.sort();
    Ok(Some(entries))
}

/// Copies a SingleCLI-managed skill into Claude Code's real skill
/// directory (`~/.claude/skills/<name>/SKILL.md`) — confirmed real via
/// `claude plugin init --help` output on the reference machine, which
/// shows new skills scaffolded at exactly that path. If a directory of
/// the same name already exists there, it's renamed aside with a
/// timestamp first (same backup-before-overwrite discipline as every
/// other config write in this project) rather than merged or clobbered.
pub fn sync_to_claude(skills_dir: &Path, claude_skills_dir: &Path, name: &str) -> Result<PathBuf> {
    let source = skills_dir.join(name);
    if !source.is_dir() {
        bail!("no such skill: {name}");
    }
    std::fs::create_dir_all(claude_skills_dir)?;
    let dest = claude_skills_dir.join(name);
    if dest.exists() {
        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let backup = claude_skills_dir.join(format!("{name}.bak-{timestamp}"));
        std::fs::rename(&dest, &backup)?;
    }
    copy_dir_recursive(&source, &dest)?;
    Ok(dest)
}

/// A small curated starter set — real, original write-ups of well-known
/// practices (not a redistribution of any other project's skill content,
/// to sidestep any licensing question), embedded at compile time so a
/// fresh install has something useful immediately without a network fetch
/// (spec section 14's marketplace/network fetch is still later-phase work,
/// same as `install`'s doc comment above says).
pub struct StarterSkill {
    pub name: &'static str,
    pub description: &'static str,
    content: &'static str,
}

pub fn starter_set() -> Vec<StarterSkill> {
    vec![
        StarterSkill {
            name: "tdd-workflow",
            description: "Red-green-refactor: write the failing test before the implementation.",
            content: include_str!("../skills-starter/tdd-workflow/SKILL.md"),
        },
        StarterSkill {
            name: "systematic-debugging",
            description: "Find the root cause before writing a fix, instead of patching the first symptom found.",
            content: include_str!("../skills-starter/systematic-debugging/SKILL.md"),
        },
        StarterSkill {
            name: "git-commit-conventions",
            description: "Commit messages that explain why, not just what — for the reader six months from now.",
            content: include_str!("../skills-starter/git-commit-conventions/SKILL.md"),
        },
        StarterSkill {
            name: "code-review-checklist",
            description: "Review for correctness, edge cases, and blast radius — not style.",
            content: include_str!("../skills-starter/code-review-checklist/SKILL.md"),
        },
        StarterSkill {
            name: "security-review-basics",
            description: "A first-pass checklist for input handling, auth, secrets, and injection risk.",
            content: include_str!("../skills-starter/security-review-basics/SKILL.md"),
        },
    ]
}

pub fn starter(name: &str) -> Option<StarterSkill> {
    starter_set().into_iter().find(|s| s.name == name)
}

/// Installs a starter skill by name into `skills_dir/<name>/SKILL.md` —
/// same refuse-to-overwrite behavior as `install`, since this goes through
/// the same directory layout.
pub fn install_starter(skills_dir: &Path, name: &str) -> Result<PathBuf> {
    let Some(starter) = starter(name) else {
        bail!("no starter skill named '{name}'");
    };
    let dest_dir = skills_dir.join(name);
    if dest_dir.exists() {
        bail!("skill '{name}' already exists at {}; remove it first", dest_dir.display());
    }
    std::fs::create_dir_all(&dest_dir)?;
    std::fs::write(dest_dir.join("SKILL.md"), starter.content)?;
    Ok(dest_dir)
}

fn copy_dir_recursive(source: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let dest_path = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_then_list_then_inspect() {
        let root = tempfile::tempdir().unwrap();
        let skills_dir = root.path().join("skills");
        let source = root.path().join("source-skill");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("SKILL.md"), "# hi").unwrap();

        install(&skills_dir, "greeting", &source).unwrap();
        assert_eq!(list(&skills_dir).unwrap(), vec!["greeting"]);
        assert_eq!(inspect(&skills_dir, "greeting").unwrap().unwrap(), vec!["SKILL.md"]);
    }

    #[test]
    fn install_refuses_to_overwrite_existing() {
        let root = tempfile::tempdir().unwrap();
        let skills_dir = root.path().join("skills");
        let source = root.path().join("source-skill");
        std::fs::create_dir_all(&source).unwrap();
        install(&skills_dir, "dup", &source).unwrap();
        assert!(install(&skills_dir, "dup", &source).is_err());
    }

    #[test]
    fn remove_deletes_and_reports_missing() {
        let root = tempfile::tempdir().unwrap();
        let skills_dir = root.path().join("skills");
        let source = root.path().join("source-skill");
        std::fs::create_dir_all(&source).unwrap();
        install(&skills_dir, "temp", &source).unwrap();
        assert!(remove(&skills_dir, "temp").unwrap());
        assert!(!remove(&skills_dir, "temp").unwrap());
    }

    #[test]
    fn remove_refuses_path_traversal_when_target_exists_outside_skills_dir() {
        let root = tempfile::tempdir().unwrap();
        let skills_dir = root.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::create_dir_all(root.path().join("outside")).unwrap();
        // "../outside" canonicalizes to a real directory outside skills_dir —
        // must error, not silently remove it or report "not found".
        assert!(remove(&skills_dir, "../outside").is_err());
        assert!(root.path().join("outside").exists());
    }

    #[test]
    fn sync_to_claude_copies_skill_and_backs_up_existing() {
        let root = tempfile::tempdir().unwrap();
        let skills_dir = root.path().join("skills");
        let claude_skills_dir = root.path().join("claude-skills");
        let source = root.path().join("source-skill");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("SKILL.md"), "# v1").unwrap();
        install(&skills_dir, "greeting", &source).unwrap();

        let dest = sync_to_claude(&skills_dir, &claude_skills_dir, "greeting").unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("SKILL.md")).unwrap(), "# v1");

        // A second sync after the skill changes must back up the old copy, not clobber silently.
        std::fs::write(skills_dir.join("greeting/SKILL.md"), "# v2").unwrap();
        sync_to_claude(&skills_dir, &claude_skills_dir, "greeting").unwrap();
        assert_eq!(std::fs::read_to_string(claude_skills_dir.join("greeting/SKILL.md")).unwrap(), "# v2");
        let backups: Vec<_> = std::fs::read_dir(&claude_skills_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("greeting.bak-"))
            .collect();
        assert_eq!(backups.len(), 1);
    }

    #[test]
    fn starter_set_has_real_nonempty_content_and_installs_cleanly() {
        let names: Vec<_> = starter_set().iter().map(|s| s.name).collect();
        for expected in ["tdd-workflow", "systematic-debugging", "git-commit-conventions", "code-review-checklist", "security-review-basics"]
        {
            assert!(names.contains(&expected), "missing starter skill {expected}");
        }
        for s in starter_set() {
            assert!(!s.content.trim().is_empty(), "{} has empty content", s.name);
            assert!(!s.description.is_empty(), "{} has no description", s.name);
        }
        assert!(starter("nonexistent").is_none());

        let root = tempfile::tempdir().unwrap();
        let skills_dir = root.path().join("skills");
        let dest = install_starter(&skills_dir, "tdd-workflow").unwrap();
        let content = std::fs::read_to_string(dest.join("SKILL.md")).unwrap();
        assert!(content.contains("TDD Workflow"));

        assert!(install_starter(&skills_dir, "tdd-workflow").is_err(), "should refuse to overwrite");
    }

    #[test]
    fn remove_reports_false_for_nonexistent_traversal_target() {
        let root = tempfile::tempdir().unwrap();
        let skills_dir = root.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        // No "outside" directory exists at all -> canonicalize fails -> Ok(false).
        assert!(!remove(&skills_dir, "../nonexistent").unwrap());
    }
}
