use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::MEMORY_SEARCH;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemorySearchArgs {
    pub query: String,
    pub limit: Option<usize>,
    #[serde(default)]
    pub include_all_workspaces: bool,
    /// Optional explicit agent scope. Omitted, empty, or blank-only values
    /// keep the default caller-only search; a non-empty list searches only
    /// the listed agents (whitespace-trimmed, deduplicated; unknown ids
    /// simply match nothing and never implicitly add the caller).
    #[serde(default)]
    pub agent_ids: Option<Vec<String>>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<MemorySearchArgs>(
            NAME,
            include_str!("../tool_descriptions/memory_search.md"),
        )?,
    })
}
