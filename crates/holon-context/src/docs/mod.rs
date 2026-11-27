//! Versioned compile-time embedded operational manuals exposed via `holon docs [topic]`.
//!
//! These manuals serve as the on-demand `L5` procedure reference layer so `L3b`
//! tool guidance cards stay compact while keeping operational contracts aligned
//! with the running binary offline.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DocTopic {
    pub slug: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub content: &'static str,
}

pub const DOCS_TOPICS: &[DocTopic] = &[
    DocTopic {
        slug: "cli",
        title: "Holon CLI Control-Plane Contract",
        summary: "Caller provenance, target scoping, JSON/exit-code discipline, and official docs fallback.",
        content: include_str!("topics/cli.md"),
    },
    DocTopic {
        slug: "skills",
        title: "Managed Skills Workflow",
        summary: "Installing, enabling, checking, and updating skills via `holon skills`.",
        content: include_str!("topics/skills.md"),
    },
    DocTopic {
        slug: "apps",
        title: "Holon Local Apps & SDK Boundary",
        summary: "Same-origin `/apps/{agent_id}/{app_id}/` hosting, `holon.js`, and non-operator provenance.",
        content: include_str!("topics/apps.md"),
    },
    DocTopic {
        slug: "agents",
        title: "Multi-Agent Delegation, Observation, and Cleanup",
        summary: "Delegation patterns, `active_children` vs public roster, bounded `holon tail`, and deletion safety.",
        content: include_str!("topics/agents.md"),
    },
    DocTopic {
        slug: "workspace",
        title: "Workspace & Worktree Lifecycle",
        summary: "Active workspace semantics, worktree isolation, subprocess `cd` boundary, and file references.",
        content: include_str!("topics/workspace.md"),
    },
    DocTopic {
        slug: "agent-home",
        title: "AgentHome Layout & Curation",
        summary: "Agent-owned durable paths (`memory/`, `work-items/`, `media/`, `apps/`, `tmp/`) and `.holon/` boundary.",
        content: include_str!("topics/agent_home.md"),
    },
    DocTopic {
        slug: "models",
        title: "Model Route Syntax & Inspection",
        summary: "Canonical `provider@endpoint/model` refs, single-call overrides, and CLI inspection commands.",
        content: include_str!("topics/models.md"),
    },
];

pub fn get_doc_topic(slug: &str) -> Option<&'static DocTopic> {
    let normalized = slug.trim().to_ascii_lowercase();
    let canonical = match normalized.as_str() {
        "agent_home" | "agenthome" => "agent-home",
        other => other,
    };
    DOCS_TOPICS.iter().find(|topic| topic.slug == canonical)
}

pub fn render_docs_index_markdown() -> String {
    let mut lines = vec![
        "# Holon Embedded Manuals (`holon docs`)".to_string(),
        String::new(),
        "Read any topic offline with `holon docs <topic>` (or `holon docs <topic> --json`):"
            .to_string(),
        String::new(),
    ];
    for topic in DOCS_TOPICS {
        lines.push(format!(
            "- `{}` — **{}**: {}",
            topic.slug, topic.title, topic.summary
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_embedded_doc_topics_resolve_and_are_non_empty() {
        let expected = [
            "cli",
            "skills",
            "apps",
            "agents",
            "workspace",
            "agent-home",
            "models",
        ];
        assert_eq!(
            DOCS_TOPICS.iter().map(|t| t.slug).collect::<Vec<_>>(),
            expected
        );
        for slug in expected {
            let topic = get_doc_topic(slug).unwrap_or_else(|| panic!("missing {slug}"));
            assert!(!topic.title.trim().is_empty());
            assert!(!topic.summary.trim().is_empty());
            assert!(!topic.content.trim().is_empty());
        }
        assert_eq!(
            get_doc_topic("agent_home").map(|t| t.slug),
            Some("agent-home")
        );
        assert!(get_doc_topic("unknown-topic").is_none());
    }
}
