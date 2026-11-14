use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{serialize_success, BuiltinToolDefinition};
use crate::{
    runtime::RuntimeHandle,
    tool::{helpers::parse_tool_args, spec::typed_spec},
    types::ToolCapabilityFamily,
};

pub(crate) const NAME: &str = crate::tool::names::LIST_SUBAGENT_CLEANUP;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListSubagentCleanupArgs {}

/// Bounded, read-only lifecycle responsibility; never a deletion decision.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SubagentCleanupCandidate {
    pub agent_id: String,
    pub incarnation: u64,
    pub blocker: Option<String>,
    pub observed_since: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ListSubagentCleanupResult {
    subagent_cleanup: Vec<SubagentCleanupCandidate>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AgentCreation,
        spec: typed_spec::<ListSubagentCleanupArgs>(
            NAME,
            include_str!("../tool_descriptions/list_subagent_cleanup.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    parse_tool_args::<ListSubagentCleanupArgs>(NAME, input)?;
    serialize_success(
        NAME,
        &ListSubagentCleanupResult {
            subagent_cleanup: runtime.subagent_cleanup_candidates().await?,
        },
    )
}
