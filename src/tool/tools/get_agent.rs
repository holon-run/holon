use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, GetAgentResult, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = crate::tool::names::GET_AGENT;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetAgentArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<GetAgentArgs>(NAME, include_str!("../tool_descriptions/get_agent.md"))?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: GetAgentArgs = parse_tool_args(NAME, input)?;
    let summary = match args.agent_id {
        None => {
            // Default behavior: return current agent summary.
            runtime.agent_summary().await?
        }
        Some(requested_id) => {
            // Requested agent: use the host's canonical storage-backed detail
            // projection so read-only inspection never starts the target.
            runtime.agent_summary_for(&requested_id).await?
        }
    };
    serialize_success(NAME, &GetAgentResult { agent: summary })
}
