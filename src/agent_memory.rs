use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::types::{
    AgentMemorySource, AgentMemorySourceView, LoadedAgentMemory, LoadedAgentMemoryView,
};

/// Default per-file character budget for the auto-loaded slice of
/// `agent_home/memory/{operator,self}.md`. Keeps the prompt injection bounded
/// while still letting a compact high-priority slice ride along with the
/// always-loaded guidance. Larger files remain available via
/// `MemorySearch` / `MemoryGet`.
pub const DEFAULT_MEMORY_BUDGET_CHARS: usize = 1500;

const OPERATOR_MEMORY_FILENAME: &str = "operator.md";
const SELF_MEMORY_FILENAME: &str = "self.md";

pub fn load_agent_memory(agent_home: &Path) -> Result<LoadedAgentMemory> {
    load_agent_memory_with_budget(agent_home, DEFAULT_MEMORY_BUDGET_CHARS)
}

pub fn load_agent_memory_with_budget(
    agent_home: &Path,
    budget_chars: usize,
) -> Result<LoadedAgentMemory> {
    let operator = load_source(
        agent_home.join("memory").join(OPERATOR_MEMORY_FILENAME),
        budget_chars,
    )?;
    let self_md = load_source(
        agent_home.join("memory").join(SELF_MEMORY_FILENAME),
        budget_chars,
    )?;
    Ok(LoadedAgentMemory {
        operator_source: operator,
        self_source: self_md,
        budget_chars,
    })
}

fn load_source(path: PathBuf, budget_chars: usize) -> Result<Option<AgentMemorySource>> {
    let raw = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let trimmed = raw.trim_start();
    if trimmed.is_empty() {
        return Ok(Some(AgentMemorySource {
            path,
            content: String::new(),
            truncated: false,
            total_chars: 0,
        }));
    }
    let total_chars = trimmed.chars().count();
    let (content, truncated) = if total_chars <= budget_chars {
        (trimmed.to_string(), false)
    } else {
        let truncated_text: String = trimmed.chars().take(budget_chars).collect();
        (truncated_text, true)
    };
    Ok(Some(AgentMemorySource {
        path,
        content,
        truncated,
        total_chars,
    }))
}

impl From<&LoadedAgentMemory> for LoadedAgentMemoryView {
    fn from(value: &LoadedAgentMemory) -> Self {
        Self {
            operator_source: value
                .operator_source
                .as_ref()
                .map(AgentMemorySourceView::from),
            self_source: value.self_source.as_ref().map(AgentMemorySourceView::from),
            budget_chars: value.budget_chars,
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn loads_missing_files_as_none() {
        let dir = tempdir().unwrap();
        let loaded = load_agent_memory_with_budget(dir.path(), 1500).unwrap();
        assert!(loaded.operator_source.is_none());
        assert!(loaded.self_source.is_none());
    }

    #[test]
    fn loads_short_files_under_budget_without_truncation() {
        let dir = tempdir().unwrap();
        let memory = dir.path().join("memory");
        std::fs::create_dir_all(&memory).unwrap();
        write(
            &memory,
            "operator.md",
            "## Operator preferences\nUse English.\n",
        );
        write(
            &memory,
            "self.md",
            "## Self notes\nCross-workspace habit X.\n",
        );

        let loaded = load_agent_memory_with_budget(dir.path(), 1500).unwrap();

        let operator = loaded.operator_source.expect("operator source");
        assert!(!operator.truncated);
        assert_eq!(
            operator.total_chars,
            "## Operator preferences\nUse English.\n".chars().count()
        );
        assert_eq!(operator.content, "## Operator preferences\nUse English.\n");

        let self_md = loaded.self_source.expect("self source");
        assert!(!self_md.truncated);
        assert!(self_md.content.contains("Cross-workspace habit X."));
    }

    #[test]
    fn truncates_long_files_at_budget_boundary() {
        let dir = tempdir().unwrap();
        let memory = dir.path().join("memory");
        std::fs::create_dir_all(&memory).unwrap();
        let body: String = "x".repeat(3000);
        write(&memory, "operator.md", &body);

        let loaded = load_agent_memory_with_budget(dir.path(), 100).unwrap();

        let operator = loaded.operator_source.expect("operator source");
        assert!(operator.truncated);
        assert_eq!(operator.total_chars, 3000);
        assert_eq!(operator.content.chars().count(), 100);
    }

    #[test]
    fn treats_whitespace_only_files_as_empty() {
        let dir = tempdir().unwrap();
        let memory = dir.path().join("memory");
        std::fs::create_dir_all(&memory).unwrap();
        write(&memory, "operator.md", "   \n\t\n");

        let loaded = load_agent_memory_with_budget(dir.path(), 100).unwrap();

        let operator = loaded.operator_source.expect("operator source");
        assert!(!operator.truncated);
        assert_eq!(operator.total_chars, 0);
        assert_eq!(operator.content, "");
    }

    #[test]
    fn view_drops_content_and_keeps_provenance() {
        let dir = tempdir().unwrap();
        let memory = dir.path().join("memory");
        std::fs::create_dir_all(&memory).unwrap();
        write(&memory, "operator.md", "operator body");
        write(&memory, "self.md", "self body");

        let loaded = load_agent_memory_with_budget(dir.path(), 1500).unwrap();
        let view: LoadedAgentMemoryView = (&loaded).into();

        assert_eq!(view.budget_chars, 1500);
        assert_eq!(
            view.operator_source.as_ref().map(|s| s.path.clone()),
            Some(memory.join("operator.md")),
        );
        assert_eq!(
            view.self_source.as_ref().map(|s| s.path.clone()),
            Some(memory.join("self.md")),
        );
    }

    #[test]
    fn default_loader_uses_default_budget() {
        let dir = tempdir().unwrap();
        let memory = dir.path().join("memory");
        std::fs::create_dir_all(&memory).unwrap();
        let body: String = "a".repeat(DEFAULT_MEMORY_BUDGET_CHARS + 50);
        write(&memory, "operator.md", &body);

        let loaded = load_agent_memory(dir.path()).unwrap();

        assert_eq!(loaded.budget_chars, DEFAULT_MEMORY_BUDGET_CHARS);
        let operator = loaded.operator_source.expect("operator source");
        assert!(operator.truncated);
        assert_eq!(
            operator.content.chars().count(),
            DEFAULT_MEMORY_BUDGET_CHARS
        );
    }
}
