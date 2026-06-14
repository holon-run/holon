use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NoteMetadata {
    pub title: String,
    pub summary: String,
    pub tags: String,
}

fn parse_frontmatter(content: &str) -> NoteMetadata {
    let mut parsed = NoteMetadata::default();
    if !content.starts_with("---") {
        return parsed;
    }
    let after_start = &content[3..].trim_start();
    let body_start = match after_start.find("---") {
        Some(i) => i + 3,
        None => return parsed,
    };
    let frontmatter = &after_start[..body_start - 3];
    for line in frontmatter.lines() {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            match key {
                "title" if !value.is_empty() => parsed.title = value,
                "summary" if !value.is_empty() => parsed.summary = value,
                "tags" if !value.is_empty() => parsed.tags = value,
                _ => {}
            }
        }
    }
    parsed
}

fn first_heading_or_filename(content: &str, filename: &str) -> String {
    let stripped = if content.starts_with("---\n") || content.starts_with("---\r\n") {
        content
            .split_once("\n---\n")
            .map(|(_, body)| body)
            .or_else(|| content.split_once("\r\n---\r\n").map(|(_, body)| body))
            .unwrap_or(content)
    } else {
        content
    };
    stripped
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with('#'))
        .map(|line| line.trim_start_matches('#').trim().to_string())
        .unwrap_or_else(|| {
            Path::new(filename)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| filename.to_string())
        })
}

fn first_body_paragraph(content: &str) -> String {
    let content_trimmed = if content.starts_with("---") {
        content.trim_start()
    } else {
        content
    };
    let body = if content_trimmed.starts_with("---\n") || content_trimmed.starts_with("---\r\n") {
        let content = content_trimmed;
        content
            .split_once("\n---\n")
            .map(|(_, body)| body)
            .or_else(|| content.split_once("\r\n---\r\n").map(|(_, body)| body))
            .unwrap_or(content)
    } else {
        content
    };
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ")
}

fn read_note_metadata(path: &Path) -> Option<NoteMetadata> {
    let content = fs::read_to_string(path).ok()?;
    let mut meta = parse_frontmatter(&content);
    let filename = path.file_name()?.to_string_lossy().to_string();
    if meta.title.is_empty() {
        meta.title = first_heading_or_filename(&content, &filename);
    }
    if meta.summary.is_empty() {
        meta.summary = first_body_paragraph(&content.trim_start());
    }
    Some(meta)
}

pub fn build_notes_catalog(agent_home: &Path) -> anyhow::Result<Option<String>> {
    let notes_dir = agent_home.join("notes");
    if !notes_dir.is_dir() {
        return Ok(None);
    }
    let mut entries: Vec<(PathBuf, NoteMetadata)> = Vec::new();
    for entry in fs::read_dir(&notes_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Some(ext) = path.extension() {
            if ext != "md" && ext != "markdown" {
                continue;
            }
        } else {
            continue;
        }
        if let Some(meta) = read_note_metadata(&path) {
            entries.push((path, meta));
        }
    }
    if entries.is_empty() {
        return Ok(None);
    }
    entries.sort_by(|a, b| a.0.file_name().cmp(&b.0.file_name()));
    const MAX_ITEMS: usize = 50;
    const MAX_CHARS_PER_ITEM: usize = 500;
    const MAX_TOTAL_CHARS: usize = 4000;
    let total_chars_before =
        entries.len() * 50 + entries.iter().map(|(_, m)| m.title.len()).sum::<usize>();
    let truncate = total_chars_before > MAX_TOTAL_CHARS;
    let mut lines = Vec::new();
    lines.push("agent_home notes catalog:".to_string());
    let mut count = 0usize;
    for (path, meta) in &entries {
        if count >= MAX_ITEMS {
            break;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let title = &meta.title;
        let summary = if truncate && meta.summary.len() > MAX_CHARS_PER_ITEM {
            format!("{}...", &meta.summary[..MAX_CHARS_PER_ITEM])
        } else {
            meta.summary.clone()
        };
        let tags = if meta.tags.is_empty() {
            String::new()
        } else {
            format!(" tags={}", meta.tags)
        };
        lines.push(
            format!("- {name} | title={title}{tags}")
                .replace("\n", " ")
                .replace("\r", ""),
        );
        if !summary.is_empty() {
            lines.push(
                format!("  summary: {summary}")
                    .replace("\n", " ")
                    .replace("\r", ""),
            );
        }
        count += 1;
    }
    let catalog = lines.join("\n");
    if catalog.len() > MAX_TOTAL_CHARS {
        let truncated = &catalog[..MAX_TOTAL_CHARS];
        let last_newline = truncated.rfind('\n').unwrap_or(0);
        return Ok(Some(format!(
            "{}\n... (truncated)",
            &truncated[..last_newline]
        )));
    }
    Ok(Some(catalog))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn no_notes_dir_returns_none() {
        let dir = tempdir().unwrap();
        let result = build_notes_catalog(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn frontmatter_note_is_parsed() {
        let dir = tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        fs::create_dir(&notes_dir).unwrap();
        fs::write(
            &notes_dir.join("note.md"),
            "---\ntitle: My Note\nsummary: A short note.\ntags: foo, bar\n---\n\nBody here.",
        )
        .unwrap();
        let result = build_notes_catalog(dir.path()).unwrap().unwrap();
        assert!(result.contains("My Note"));
        assert!(result.contains("A short note."));
        assert!(result.contains("foo, bar"));
    }

    #[test]
    fn no_frontmatter_uses_heading_and_paragraph() {
        let dir = tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        fs::create_dir(&notes_dir).unwrap();
        fs::write(
            &notes_dir.join("note.md"),
            "# My Heading\n\nFirst paragraph.",
        )
        .unwrap();
        let result = build_notes_catalog(dir.path()).unwrap().unwrap();
        assert!(result.contains("My Heading"));
        assert!(result.contains("First paragraph."));
    }

    #[test]
    fn truncation_works() {
        let dir = tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        fs::create_dir(&notes_dir).unwrap();
        for i in 0..100 {
            fs::write(
                &notes_dir.join(format!("note_{:03}.md", i)),
                format!(
                    "---\ntitle: Note {}\nsummary: {}\n---\n\nBody.",
                    i,
                    "x".repeat(100)
                ),
            )
            .unwrap();
        }
        let result = build_notes_catalog(dir.path()).unwrap().unwrap();
        assert!(result.len() <= 4000 + "... (truncated)".len());
    }

    #[test]
    fn section_is_stable_and_low_priority() {
        let dir = tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        fs::create_dir(&notes_dir).unwrap();
        fs::write(&notes_dir.join("a.md"), "---\ntitle: A\n---\n").unwrap();
        let result = build_notes_catalog(dir.path()).unwrap().unwrap();
        assert!(result.starts_with("agent_home notes catalog:"));
    }
}
