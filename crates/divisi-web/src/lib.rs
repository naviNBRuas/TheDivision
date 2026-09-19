//! Reads the premium-web skill/pattern library (markdown under a
//! `patterns/` directory, see `docs/web-capability-pack-architecture.md`)
//! so `single web patterns list|search` can browse it structurally instead
//! of an agent having to grep the filesystem itself. Deliberately just a
//! reader over plain files — the actual content lives at
//! `~/.config/single/skills/web/premium-web/patterns/**/*.md` (synced
//! there from this repo's own `skills/` directory, a separate step not
//! done by this crate) grouped by category (the immediate parent
//! directory name), same shape as the spec's own
//! `patterns/<category>/<name>.md` layout.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PatternInfo {
    pub name: String,
    pub category: String,
    pub path: PathBuf,
    /// The first non-empty line after the title heading — a one-line
    /// description without needing to load the whole file. Empty if the
    /// file has no such line (title-only, or no title at all).
    pub summary: String,
}

/// Recursively finds every `*.md` file under `patterns_dir` and reads its
/// name/category/summary. `category` is the file's immediate parent
/// directory name relative to `patterns_dir` — a file directly inside
/// `patterns_dir` (no subdirectory) gets category `""`, which callers
/// should treat as "uncategorized" rather than an error; this crate
/// doesn't enforce the two-level layout the spec suggests, just reflects
/// whatever's actually on disk.
pub fn list_patterns(patterns_dir: &Path) -> Result<Vec<PatternInfo>> {
    if !patterns_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    collect_markdown(patterns_dir, patterns_dir, &mut out)?;
    out.sort_by(|a, b| (&a.category, &a.name).cmp(&(&b.category, &b.name)));
    Ok(out)
}

fn collect_markdown(root: &Path, dir: &Path, out: &mut Vec<PatternInfo>) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_markdown(root, &path, out)?;
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        let category = path
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let summary = first_summary_line(&text);
        out.push(PatternInfo { name, category, path, summary });
    }
    Ok(())
}

/// The first non-empty, non-heading line in the file — skips the `# Title`
/// line (and any blank lines around it) to find the first real sentence,
/// which is what a pattern doc's opening paragraph always is in this
/// skill library's style (see e.g. `skills/web/premium-web/SKILL.md`).
fn first_summary_line(text: &str) -> String {
    text.lines().map(str::trim).find(|line| !line.is_empty() && !line.starts_with('#')).unwrap_or_default().to_string()
}

/// Case-insensitive substring match against name, category, or file
/// content — a simple grep, not semantic search (the full spec's
/// "search_patterns" MCP capability implies more; this is the honest,
/// buildable-tonight version, see the architecture doc for what's
/// deferred).
pub fn search_patterns(patterns_dir: &Path, query: &str) -> Result<Vec<PatternInfo>> {
    let query = query.to_lowercase();
    if query.is_empty() {
        return list_patterns(patterns_dir);
    }
    let all = list_patterns(patterns_dir)?;
    let mut matched = Vec::new();
    for p in all {
        if p.name.to_lowercase().contains(&query) || p.category.to_lowercase().contains(&query) || p.summary.to_lowercase().contains(&query) {
            matched.push(p);
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&p.path) {
            if text.to_lowercase().contains(&query) {
                matched.push(p);
            }
        }
    }
    Ok(matched)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pattern(dir: &Path, category: &str, name: &str, body: &str) {
        let cat_dir = dir.join(category);
        std::fs::create_dir_all(&cat_dir).unwrap();
        std::fs::write(cat_dir.join(format!("{name}.md")), body).unwrap();
    }

    #[test]
    fn missing_directory_returns_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert_eq!(list_patterns(&missing).unwrap(), Vec::new());
    }

    #[test]
    fn lists_patterns_grouped_by_category_with_summary() {
        let dir = tempfile::tempdir().unwrap();
        write_pattern(dir.path(), "heroes", "cinematic-hero", "# Cinematic Hero\n\nA full-viewport hero with scroll-driven parallax.\n\nMore detail here.");
        write_pattern(dir.path(), "scroll", "text-reveal", "# Text Reveal\n\nSplit-word reveal on scroll.\n");
        let patterns = list_patterns(dir.path()).unwrap();
        assert_eq!(patterns.len(), 2);
        let hero = patterns.iter().find(|p| p.name == "cinematic-hero").unwrap();
        assert_eq!(hero.category, "heroes");
        assert_eq!(hero.summary, "A full-viewport hero with scroll-driven parallax.");
    }

    #[test]
    fn search_matches_name_category_and_content() {
        let dir = tempfile::tempdir().unwrap();
        write_pattern(dir.path(), "heroes", "cinematic-hero", "# Cinematic Hero\n\nUses GSAP ScrollTrigger for the reveal.");
        write_pattern(dir.path(), "scroll", "pinned-section", "# Pinned Section\n\nPins content while scrolling past it.");
        assert_eq!(search_patterns(dir.path(), "hero").unwrap().len(), 1);
        assert_eq!(search_patterns(dir.path(), "scroll").unwrap().len(), 2, "matches pinned-section's category and cinematic-hero's ScrollTrigger content");
        assert_eq!(search_patterns(dir.path(), "gsap").unwrap().len(), 1, "content-only match");
        assert_eq!(search_patterns(dir.path(), "nonexistent").unwrap().len(), 0);
    }

    #[test]
    fn empty_query_returns_everything() {
        let dir = tempfile::tempdir().unwrap();
        write_pattern(dir.path(), "heroes", "a", "# A\n\nBody.");
        write_pattern(dir.path(), "heroes", "b", "# B\n\nBody.");
        assert_eq!(search_patterns(dir.path(), "").unwrap().len(), 2);
    }

    #[test]
    fn file_with_no_body_gets_empty_summary() {
        let dir = tempfile::tempdir().unwrap();
        write_pattern(dir.path(), "heroes", "stub", "# Stub Only\n");
        let patterns = list_patterns(dir.path()).unwrap();
        assert_eq!(patterns[0].summary, "");
    }
}
