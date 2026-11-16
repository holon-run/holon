//! Context budget planning and prompt rendering over captured inputs.

pub use holon_domain::PromptStability;
use serde::{Deserialize, Serialize};

pub mod budget;
pub mod episodes;
pub mod fingerprint;
pub mod history;
pub mod planner;
pub mod render;
pub mod system_prompt;
pub mod time;
pub mod tools;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSection {
    pub name: String,
    pub id: String,
    pub content: String,
    pub stability: PromptStability,
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
    sections
        .iter()
        .map(render_section)
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn render_section(section: &PromptSection) -> String {
    format!("## {}\n{}", section.name, section.content)
}

pub fn section(name: &'static str, stability: PromptStability, content: String) -> PromptSection {
    PromptSection {
        name: name.to_string(),
        id: name.to_string(),
        content,
        stability,
    }
}

#[cfg(test)]
mod context_tests;
