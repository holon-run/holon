use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::DELETE_AGENT;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteAgentArgs {
    pub agent_id: String,
    /// Use identity.incarnation from GetAgent; prevents deleting a recreated agent.
    pub incarnation: u64,
}

#[derive(Serialize, JsonSchema)]
pub struct DeleteAgentResult {
    pub deletion: crate::types::AgentDeletionJob,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AgentCreation,
        spec: typed_spec::<DeleteAgentArgs>(
            NAME,
            include_str!("../tool_descriptions/delete_agent.md"),
        )?,
    })
}
