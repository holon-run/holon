use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

pub const NAME: &str = crate::tool::names::LIST_SUBAGENTS;
pub const LEGACY_NAME: &str = crate::tool::names::LIST_SUBAGENT_CLEANUP;
pub use crate::types::SubagentListPage as ListSubagentsResult;

fn default_limit() -> usize {
    crate::types::DEFAULT_SUBAGENT_LIST_LIMIT
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListSubagentsArgs {
    /// Page size: 1..=100, default 20. Invalid values are rejected, not clamped.
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1, max = 100))]
    pub limit: usize,
    /// Opaque continuation returned by this caller's preceding page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Include the first database cleanup blocker; does not change membership.
    #[serde(default)]
    pub include_cleanup: bool,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AgentCreation,
        spec: typed_spec::<ListSubagentsArgs>(
            NAME,
            include_str!("../tool_descriptions/list_subagents.md"),
        )?,
    })
}

/// Dispatch compatibility only; the catalog hides this spelling from models.
pub fn legacy_definition() -> Result<BuiltinToolDefinition> {
    let mut definition = definition()?;
    definition.spec.name = LEGACY_NAME.into();
    Ok(definition)
}
