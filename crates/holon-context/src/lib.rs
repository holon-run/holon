//! Context budget planning and prompt rendering over captured inputs.

pub use holon_domain::PromptStability;
use serde::{Deserialize, Serialize};

pub mod assembly;
pub mod budget;
pub mod episodes;
pub mod fingerprint;
pub mod history;
pub mod planner;
pub mod projection_eval;
pub mod records;
pub mod render;
pub mod system_prompt;
pub mod time;
pub mod tools;

/// Physical prompt placement, independent of a section's cache invalidation scope.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptLayout {
    SharedPrefix,
    #[default]
    ScopedSuffix,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSection {
    pub name: String,
    pub id: String,
    pub content: String,
    pub stability: PromptStability,
    #[serde(default)]
    pub layout: PromptLayout,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptCacheIdentity {
    pub agent_id: String,
    pub prompt_cache_key: String,
    pub context_fingerprint: String,
    pub compression_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptCacheScopeDiagnostics {
    pub cacheable_system_sections: usize,
    pub cacheable_context_sections: usize,
    pub turn_scoped_system_sections: usize,
    pub turn_scoped_context_sections: usize,
    pub cacheable_system_chars: usize,
    pub cacheable_context_chars: usize,
    pub turn_scoped_system_chars: usize,
    pub turn_scoped_context_chars: usize,
    pub cacheable_system_fingerprint: String,
    pub cacheable_context_fingerprint: String,
    pub turn_scoped_fingerprint: String,
}

pub fn render_sections(sections: &[PromptSection]) -> String {
    render_section_blocks(sections).concat()
}

/// Render canonical blocks, including each block's following separator.
/// A shared-only prompt keeps its final separator so adding scoped sections
/// cannot change any shared-prefix block's bytes.
pub fn render_section_blocks(sections: &[PromptSection]) -> Vec<String> {
    let shared_end = shared_prefix_len(sections);
    sections
        .iter()
        .enumerate()
        .map(|(index, section)| {
            let mut block = render_section(section);
            if index + 1 < sections.len() || index < shared_end {
                block.push_str("\n\n");
            }
            block
        })
        .collect()
}

pub fn render_section(section: &PromptSection) -> String {
    format!("## {}\n{}", section.name, section.content)
}

/// Count only the contiguous, explicitly audited shared prefix.
/// Later shared labels cannot extend the boundary across scoped content.
pub fn shared_prefix_len(sections: &[PromptSection]) -> usize {
    sections
        .iter()
        .take_while(|section| section.layout == PromptLayout::SharedPrefix)
        .count()
}

pub fn section(name: &'static str, stability: PromptStability, content: String) -> PromptSection {
    PromptSection {
        name: name.to_string(),
        id: name.to_string(),
        content,
        stability,
        layout: PromptLayout::ScopedSuffix,
    }
}

/// Use only for fixed runtime contracts with no agent, workspace, or task inputs.
pub(crate) fn shared_section(
    name: &'static str,
    stability: PromptStability,
    content: String,
) -> PromptSection {
    PromptSection {
        layout: PromptLayout::SharedPrefix,
        ..section(name, stability, content)
    }
}

#[cfg(test)]
mod context_tests;
