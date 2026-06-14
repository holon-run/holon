//! Agent home notes catalog projection.
//!
//! Scans `agent_home/notes/` for Markdown files and builds a bounded metadata
//! catalog for prompt injection. The catalog is a low-priority reference index;
//! it never overrides operator instructions, system/developer guidance,
//! AGENTS.md, or current WorkItem context.

use std::path::Path;

/// Maximum number of note entries to include in the catalog.
pub const MAX_CATALOG_ITEMS: usize = 20;

/// Maximum total character count for the rendered catalog section.
pub const MAX_CATALOG_CHARS: usize = 2000;

/// Parsed frontmatter metadata from a note file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteMetadata {
    pub title: Option<String>,
    pub summary: Option<String>,
    pub tags: Vec<String>,
}

/// A single entry in the notes catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteCatalogEntry {
    pub relative_path: String,
    pub title: String,
    pub summary: Option<String>,
    pub tags: Vec<String>,
}

/// Scan the agent home notes directory and return a bounded catalog of note
/// metadata entries.
pub fn scan_agent_notes(agent_home: &Path) -> Vec<NoteCatalogEntry> {
    let notes_dir = agent_home.join("notes");
    if !notes_dir.is_dir() {
        return Vec::new();
    }

    let mut entries = Vec::new();
    let read_dir = match std::fs::read_dir(&notes_dir) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some("md") | Some("markdown") => {}
            _ => continue,
        }

        let relative = format!(
            "agent_home/notes/{}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let metadata = parse_frontmatter(&content);
        let title = metadata
            .title
            .unwrap_or_else(|| fallback_title(&path, &content));
        let summary = metadata.summary;
        let tags = metadata.tags;

        entries.push(NoteCatalogEntry {
            relative_path: relative,
            title,
            summary,
            tags,
        });

        if entries.len() >= MAX_CATALOG_ITEMS {
            break;
        }
    }

    // Sort by relative path for deterministic output.
    entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    entries
}

/// Render a bounded notes catalog section for prompt injection.
///
/// Returns `None` when the catalog is empty.
pub fn render_notes_catalog(entries: &[NoteCatalogEntry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    lines.push(
        "Available agent home notes (low-priority reference index; read relevant notes on demand):"
            .to_string(),
    );

    let mut total_chars = lines[0].len();
    let mut included = 0;

    for entry in entries {
        let mut entry_lines = Vec::new();
        entry_lines.push(format!("- {}", entry.relative_path));
        entry_lines.push(format!("  title: {}", entry.title));
        if let Some(summary) = &entry.summary {
            entry_lines.push(format!("  summary: {}", summary));
        }
        if !entry.tags.is_empty() {
            entry_lines.push(format!("  tags: {}", entry.tags.join(", ")));
        }

        let block = entry_lines.join("\n");
        let block_len = block.len() + 1; // +1 for newline separator

        if total_chars + block_len > MAX_CATALOG_CHARS {
            lines.push(format!(
                "... ({} more notes omitted; catalog truncated at {} items, {} chars)",
                entries.len() - included,
                MAX_CATALOG_ITEMS,
                MAX_CATALOG_CHARS
            ));
            break;
        }

        lines.push(block);
        total_chars += block_len;
        included += 1;
    }

    Some(lines.join("\n"))
}

/// Parse simple YAML-like frontmatter from a Markdown document.
///
/// Frontmatter is delimited by `---` at the start of the file and supports
/// only the `title`, `summary`, and `tags` fields.
fn parse_frontmatter(content: &str) -> NoteMetadata {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return NoteMetadata::default();
    }

    let after_opening = &trimmed[3..];
    // Find the closing `---`
    let closing = match after_opening.find("\n---") {
        Some(pos) => pos,
        None => return NoteMetadata::default(),
    };

    let yaml_block = &after_opening[..closing];
    let mut metadata = NoteMetadata::default();

    for line in yaml_block.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(value) = extract_field(line, "title") {
            metadata.title = Some(unquote(&value));
        } else if let Some(value) = extract_field(line, "summary") {
            metadata.summary = Some(unquote(&value));
        } else if let Some(value) = extract_field(line, "tags") {
            metadata.tags = parse_tags_value(&value);
        }
    }

    metadata
}

/// Extract a YAML field value like `field: value` or `field: "value"`.
fn extract_field(line: &str, field: &str) -> Option<String> {
    let prefix = format!("{}:", field);
    let trimmed = line.trim_start();
    if !trimmed.starts_with(&prefix) {
        return None;
    }
    let rest = &trimmed[prefix.len()..];
    let value = rest.trim().to_string();
    if value.is_empty() {
        return None;
    }
    Some(value)
}

/// Remove surrounding quotes from a YAML string value.
fn unquote(value: &str) -> String {
    let v = value.trim();
    if (v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')) {
        return v[1..v.len() - 1].to_string();
    }
    v.to_string()
}

/// Parse a tags value which can be:
/// - A YAML inline list: `[tag1, tag2, tag3]`
/// - A comma-separated string: `tag1, tag2, tag3`
fn parse_tags_value(value: &str) -> Vec<String> {
    let v = value.trim();
    let inner = if v.starts_with('[') && v.ends_with(']') {
        &v[1..v.len() - 1]
    } else {
        // Remove quotes if present
        &unquote(v)
    };

    inner
        .split(',')
        .map(|s| unquote(s.trim()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Derive a title from the filename or first heading when frontmatter is absent.
fn fallback_title(path: &Path, content: &str) -> String {
    // Try first heading
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix('#') {
            let heading = heading.trim();
            if !heading.is_empty() {
                return heading.to_string();
            }
        }
    }

    // Fall back to filename stem
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_agent_home_with_notes(notes: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        let notes_dir = dir.path().join("notes");
        fs::create_dir_all(&notes_dir).unwrap();
        for (name, content) in notes {
            fs::write(notes_dir.join(name), content).unwrap();
        }
        dir
    }

    #[test]
    fn no_notes_directory() {
        let dir = TempDir::new().unwrap();
        let entries = scan_agent_notes(dir.path());
        assert!(entries.is_empty());
        assert!(render_notes_catalog(&entries).is_none());
    }

    #[test]
    fn note_with_frontmatter() {
        let content = "---\ntitle: Release workflow debugging notes\nsummary: Things to check when GitHub release assets are missing.\ntags: [release, github-actions]\n---\n\nSome body content here.\n";
        let dir = create_agent_home_with_notes(&[("release.md", content)]);
        let entries = scan_agent_notes(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Release workflow debugging notes");
        assert_eq!(
            entries[0].summary.as_deref(),
            Some("Things to check when GitHub release assets are missing.")
        );
        assert_eq!(entries[0].tags, vec!["release", "github-actions"]);
        assert_eq!(entries[0].relative_path, "agent_home/notes/release.md");
    }

    #[test]
    fn note_without_frontmatter_uses_heading() {
        let content = "# My Custom Note\n\nSome body text.\n";
        let dir = create_agent_home_with_notes(&[("custom.md", content)]);
        let entries = scan_agent_notes(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "My Custom Note");
        assert!(entries[0].summary.is_none());
        assert!(entries[0].tags.is_empty());
    }

    #[test]
    fn note_without_frontmatter_or_heading_uses_filename() {
        let content = "Just plain text without any heading or frontmatter.\n";
        let dir = create_agent_home_with_notes(&[("plain.md", content)]);
        let entries = scan_agent_notes(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "plain");
        assert!(entries[0].summary.is_none());
    }

    #[test]
    fn truncation_respects_char_limit() {
        // Create many notes with long summaries to trigger truncation
        let mut notes = Vec::new();
        for i in 0..25 {
            let content = format!(
                "---\ntitle: Note number {}\nsummary: This is a moderately long summary for note number {} that adds characters.\ntags: [tag-a, tag-b]\n---\nBody.\n",
                i, i
            );
            notes.push((format!("note_{:02}.md", i), content));
        }
        let note_refs: Vec<(&str, &str)> = notes
            .iter()
            .map(|(n, c)| (n.as_str(), c.as_str()))
            .collect();
        let dir = create_agent_home_with_notes(&note_refs);
        let entries = scan_agent_notes(dir.path());

        // Should be capped at MAX_CATALOG_ITEMS
        assert!(entries.len() <= MAX_CATALOG_ITEMS);

        let rendered = render_notes_catalog(&entries).unwrap();
        assert!(rendered.len() <= MAX_CATALOG_CHARS + 200); // some slack for the truncation message
        assert!(rendered.contains("truncated"));
    }

    #[test]
    fn catalog_does_not_include_full_body() {
        let content = "---\ntitle: Short Title\nsummary: Brief summary.\ntags: [test]\n---\n\nThis is the full body of the note which should NOT appear in the catalog output at all because we only project metadata.\n";
        let dir = create_agent_home_with_notes(&[("body-test.md", content)]);
        let entries = scan_agent_notes(dir.path());
        let rendered = render_notes_catalog(&entries).unwrap();
        assert!(!rendered.contains("full body of the note"));
        assert!(rendered.contains("Short Title"));
        assert!(rendered.contains("Brief summary."));
    }

    #[test]
    fn frontmatter_with_quoted_values() {
        let content = "---\ntitle: \"Quoted Title\"\nsummary: 'Single quoted summary'\ntags: [\"tag-one\", \"tag-two\"]\n---\n";
        let dir = create_agent_home_with_notes(&[("quoted.md", content)]);
        let entries = scan_agent_notes(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Quoted Title");
        assert_eq!(entries[0].summary.as_deref(), Some("Single quoted summary"));
        assert_eq!(entries[0].tags, vec!["tag-one", "tag-two"]);
    }

    #[test]
    fn frontmatter_with_comma_separated_tags() {
        let content = "---\ntitle: Comma Tags\nsummary: A note.\ntags: alpha, beta, gamma\n---\n";
        let dir = create_agent_home_with_notes(&[("comma.md", content)]);
        let entries = scan_agent_notes(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tags, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn ignores_non_markdown_files() {
        let dir = TempDir::new().unwrap();
        let notes_dir = dir.path().join("notes");
        fs::create_dir_all(&notes_dir).unwrap();
        fs::write(notes_dir.join("readme.txt"), "not a note").unwrap();
        fs::write(notes_dir.join("data.json"), "{}").unwrap();
        fs::write(
            notes_dir.join("valid.md"),
            "---\ntitle: Valid Note\n---\nbody\n",
        )
        .unwrap();
        let entries = scan_agent_notes(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Valid Note");
    }

    #[test]
    fn entries_sorted_by_path() {
        let notes = vec![
            ("zebra.md", "---\ntitle: Zebra\n---\n"),
            ("alpha.md", "---\ntitle: Alpha\n---\n"),
            ("middle.md", "---\ntitle: Middle\n---\n"),
        ];
        let dir = create_agent_home_with_notes(&notes);
        let entries = scan_agent_notes(dir.path());
        assert_eq!(entries[0].relative_path, "agent_home/notes/alpha.md");
        assert_eq!(entries[1].relative_path, "agent_home/notes/middle.md");
        assert_eq!(entries[2].relative_path, "agent_home/notes/zebra.md");
    }

    #[test]
    fn incomplete_frontmatter_returns_default() {
        let content = "---\ntitle: No closing delimiter\n";
        let metadata = parse_frontmatter(content);
        assert_eq!(metadata, NoteMetadata::default());
    }

    #[test]
    fn empty_frontmatter_returns_default() {
        let content = "---\n---\n# Just a heading\n";
        let metadata = parse_frontmatter(content);
        assert_eq!(metadata, NoteMetadata::default());
    }

    #[test]
    fn render_empty_catalog_returns_none() {
        assert!(render_notes_catalog(&[]).is_none());
    }

    #[test]
    fn catalog_header_present() {
        let entries = vec![NoteCatalogEntry {
            relative_path: "agent_home/notes/test.md".into(),
            title: "Test".into(),
            summary: Some("A test note.".into()),
            tags: vec!["test".into()],
        }];
        let rendered = render_notes_catalog(&entries).unwrap();
        assert!(rendered.contains("Available agent home notes"));
        assert!(rendered.contains("low-priority reference index"));
    }

    #[test]
    fn parse_tags_empty_brackets() {
        let tags = parse_tags_value("[]");
        assert!(tags.is_empty());
    }

    #[test]
    fn parse_tags_single_item() {
        let tags = parse_tags_value("[solo]");
        assert_eq!(tags, vec!["solo"]);
    }
}
