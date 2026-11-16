use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::SubagentCleanupCandidate;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::LIST_SUBAGENT_CLEANUP;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListSubagentCleanupArgs {}

#[derive(Serialize, JsonSchema)]
pub struct ListSubagentCleanupResult {
    pub subagent_cleanup: Vec<SubagentCleanupCandidate>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AgentCreation,
        spec: typed_spec::<ListSubagentCleanupArgs>(
            NAME,
            include_str!("../tool_descriptions/list_subagent_cleanup.md"),
        )?,
    })
}
