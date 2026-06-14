use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::warn;

use crate::types::{NoteCatalogEntry, NoteScope};

/// Maximum number of notes projected in the catalog.
pub const NOTES_CATALOG_MAX_ITEMS: usize = 20;
/// Maximum characters for the entire notes catalog section.
pub const NOTES_CATALOG_MAX_CHARS: usize = 2000;
/// Maximum characters for a single note's metadata entry in the catalog.
pub const NOTE_ENTRY_MAX_CHARS: usize = 200;

const NOTES_DIR: &str = "notes";

/// Load note catalog entries from `agent_home/notes/`.
///
/// Only `.md` files are considered. Metadata is read from YAML frontmatter
/// (`title`, `summary`, `tags`). When frontmatter is missing, the title falls
/// back to the first Markdown heading or the file stem.
pub fn load_notes_catalog(agent_home: &Path) -> Result<Vec<NoteCatalogEntry>> {
    let notes_dir = agent_home.join(NOTES_DIR);
    if !notes_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(&notes_dir)
        .with_context(|| format!("reading notes dir {}", notes_dir.display()))?
    {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                warn!(?err, "skipping unreadable entry in notes dir");
                continue;
            }
        };
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(err) => {
                warn!(path = %path.display(), ?err, "skipping unreadable note file");
                continue;
            }
        };

        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string();

        let parsed = parse_note_frontmatter(&content);
        let title = parsed.title.or_else(|| extract_heading_title(&content));
        let summary = parsed.summary.or_else(|| extract_first_paragraph(&content));
        let tags = parsed.tags;

        entries.push(NoteCatalogEntry {
            name,
            path,
            title,
            summary: summary.map(|s| truncate_text(&s, NOTE_ENTRY_MAX_CHARS)),
            tags,
            scope: NoteScope::Agent,
        });
    }

    // Stable sort by name for deterministic output.
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

#[derive(Default)]
struct ParsedNoteMetadata {
    title: Option<String>,
    summary: Option<String>,
    tags: Vec<String>,
}

/// Parse YAML frontmatter (`---` delimited) for `title`, `summary`, and `tags`.
fn parse_note_frontmatter(content: &str) -> ParsedNoteMetadata {
    let mut parsed = ParsedNoteMetadata::default();
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return parsed;
    }

    for line in lines {
        if line == "---" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim() {
            "title" if !value.is_empty() => parsed.title = Some(value.to_string()),
            "summary" if !value.is_empty() => parsed.summary = Some(value.to_string()),
            "tags" if !value.is_empty() => parsed.tags = parse_tag_list(value),
            _ => {}
        }
    }
    parsed
}

/// Parse YAML list syntax `"[a, b, c]"` or comma-separated `"a, b, c"`.
fn parse_tag_list(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    inner
        .split(',')
        .map(|t| t.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Extract the first Markdown H1/H2 heading as a fallback title.
fn extract_heading_title(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let trimmed = line.trim_start();
        let heading = trimmed
            .strip_prefix("# ")
            .or_else(|| trimmed.strip_prefix("## "))?;
        let title = heading.trim();
        if title.is_empty() {
            None
        } else {
            Some(title.to_string())
        }
    })
}

/// Extract the first non-empty paragraph as a fallback summary.
fn extract_first_paragraph(content: &str) -> Option<String> {
    let body = strip_frontmatter(content);
    let paragraph = body
        .split("\n\n")
        .map(str::trim)
        .find(|p| !p.is_empty() && !p.starts_with('#'))?;
    Some(paragraph.replace('\n', " "))
}

/// Remove frontmatter block if present, returning body text.
fn strip_frontmatter(content: &str) -> &str {
    if content.starts_with("---\n") || content.starts_with("---\r\n") {
        content
            .split_once("\n---\n")
            .or_else(|| content.split_once("\r\n---\r\n"))
            .map(|(_, body)| body)
            .unwrap_or("")
    } else {
        content
    }
}

/// Truncate text to at most `max` bytes, adding ellipsis if truncated.
fn truncate_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    // Reserve room for the ellipsis character (3 bytes in UTF-8).
    let ellipsis_len = "…".len();
    let mut end = max.saturating_sub(ellipsis_len);
    if end == 0 {
        return "…".to_string();
    }
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        return "…".to_string();
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn no_notes_dir_returns_empty() {
        let dir = tempdir().unwrap();
        let entries = load_notes_catalog(dir.path()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn empty_notes_dir_returns_empty() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("notes")).unwrap();
        let entries = load_notes_catalog(dir.path()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn frontmatter_note_metadata() {
        let dir = tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        fs::create_dir_all(&notes_dir).unwrap();
        fs::write(
            notes_dir.join("release.md"),
            "---\ntitle: Release workflow debugging notes\nsummary: Things to check when GitHub release assets are missing.\ntags: [release, github-actions]\n---\n\n# Release\n\nBody content here.",
        )
        .unwrap();

        let entries = load_notes_catalog(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        let note = &entries[0];
        assert_eq!(note.name, "release");
        assert_eq!(
            note.title.as_deref(),
            Some("Release workflow debugging notes")
        );
        assert_eq!(
            note.summary.as_deref(),
            Some("Things to check when GitHub release assets are missing.")
        );
        assert_eq!(note.tags, vec!["release", "github-actions"]);
    }

    #[test]
    fn no_frontmatter_falls_back_to_heading_and_paragraph() {
        let dir = tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        fs::create_dir_all(&notes_dir).unwrap();
        fs::write(
            notes_dir.join("agent-memory.md"),
            "# Agent Memory Notes\n\nWhen to use AGENTS.md vs memory.\n\nMore detail here.",
        )
        .unwrap();

        let entries = load_notes_catalog(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        let note = &entries[0];
        assert_eq!(note.name, "agent-memory");
        assert_eq!(note.title.as_deref(), Some("Agent Memory Notes"));
        assert_eq!(
            note.summary.as_deref(),
            Some("When to use AGENTS.md vs memory.")
        );
        assert!(note.tags.is_empty());
    }

    #[test]
    fn no_frontmatter_no_heading_uses_stem_as_title() {
        let dir = tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        fs::create_dir_all(&notes_dir).unwrap();
        fs::write(
            notes_dir.join("scratch.md"),
            "Just a paragraph without heading.",
        )
        .unwrap();

        let entries = load_notes_catalog(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        let note = &entries[0];
        assert_eq!(note.name, "scratch");
        assert!(note.title.is_none());
        assert_eq!(
            note.summary.as_deref(),
            Some("Just a paragraph without heading.")
        );
    }

    #[test]
    fn non_markdown_files_ignored() {
        let dir = tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        fs::create_dir_all(&notes_dir).unwrap();
        fs::write(notes_dir.join("data.json"), "{}").unwrap();
        fs::write(notes_dir.join("readme.txt"), "text").unwrap();

        let entries = load_notes_catalog(dir.path()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn comma_separated_tags_without_brackets() {
        let dir = tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        fs::create_dir_all(&notes_dir).unwrap();
        fs::write(
            notes_dir.join("config.md"),
            "---\ntitle: Config\ntags: a, b, c\n---\nbody",
        )
        .unwrap();

        let entries = load_notes_catalog(dir.path()).unwrap();
        assert_eq!(entries[0].tags, vec!["a", "b", "c"]);
    }

    #[test]
    fn long_summary_is_truncated() {
        let dir = tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        fs::create_dir_all(&notes_dir).unwrap();
        let long_summary = "x".repeat(500);
        fs::write(
            notes_dir.join("big.md"),
            format!("---\nsummary: {}\n---\nbody", long_summary),
        )
        .unwrap();

        let entries = load_notes_catalog(dir.path()).unwrap();
        let summary = entries[0].summary.as_ref().unwrap();
        assert!(summary.len() <= NOTE_ENTRY_MAX_CHARS);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn entries_sorted_by_name() {
        let dir = tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        fs::create_dir_all(&notes_dir).unwrap();
        fs::write(notes_dir.join("zebra.md"), "# Zebra").unwrap();
        fs::write(notes_dir.join("alpha.md"), "# Alpha").unwrap();
        fs::write(notes_dir.join("mid.md"), "# Mid").unwrap();

        let entries = load_notes_catalog(dir.path()).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mid", "zebra"]);
    }
}
