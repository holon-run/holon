use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::Value;

use crate::tool::ToolError;
use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AgentGetResult, AuthorityClass, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = "AgentGet";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentGetArgs {
    /// Optional agent id to inspect. When omitted, returns the current agent
    /// summary. When provided, returns the summary of the requested agent,
    /// including private child agents under the current local trusted control
    /// boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<AgentGetArgs>(NAME, include_str!("../tool_descriptions/agent_get.md"))?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    caller_agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: AgentGetArgs = parse_tool_args(NAME, input)?;

    let summary = if let Some(requested_id) = args.agent_id.as_deref() {
        if requested_id == caller_agent_id {
            runtime.agent_summary().await?
        } else {
            match runtime.agent_summary_for(requested_id).await? {
                Some(summary) => summary,
                None => {
                    return Err(ToolError::new(
                        "agent_not_found",
                        format!("agent `{requested_id}` not found or not active"),
                    )
                    .with_details(json!({ "requested_agent_id": requested_id }))
                    .into());
                }
            }
        }
    } else {
        runtime.agent_summary().await?
    };

    serialize_success(NAME, &AgentGetResult { agent: summary })
}
