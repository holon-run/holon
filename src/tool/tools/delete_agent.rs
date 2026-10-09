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

pub(crate) const NAME: &str = crate::tool::names::DELETE_AGENT;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteAgentArgs {
    pub agent_id: String,
    /// Use identity.incarnation from GetAgent; prevents deleting a recreated agent.
    pub incarnation: u64,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct DeleteAgentResult {
    pub deletion: crate::types::AgentDeletionJob,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AgentCreation,
        spec: typed_spec::<DeleteAgentArgs>(
            NAME,
            include_str!("../tool_descriptions/delete_agent.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: DeleteAgentArgs = parse_tool_args(NAME, input)?;
    let job = runtime
        .delete_supervised_child(&args.agent_id, args.incarnation)
        .await?;
    serialize_success(NAME, &DeleteAgentResult { deletion: job })
}
