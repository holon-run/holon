use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AgentGetResult, AuthorityClass, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::error::ToolError;
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = "AgentGet";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentGetArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent_id: Option<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<AgentGetArgs>(NAME, include_str!("../tool_descriptions/agent_get.md"))?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    current_agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let _args: AgentGetArgs = parse_tool_args(NAME, input)?;
    let summary = match _args.agent_id {
        Some(ref target_agent_id) if target_agent_id == current_agent_id => {
            runtime.agent_summary().await?
        }
        Some(ref target_agent_id) => {
            // Requesting a different agent; use the host bridge to look it up.
            // Under the current simplified trust model, local/operator control
            // API access is trusted, so private child agents may be observed.
            match runtime.lookup_agent_summary(target_agent_id).await? {
                Some(summary) => summary,
                None => {
                    return Ok(crate::tool::ToolResult::error(
                        NAME,
                        ToolError::new(
                            "agent_not_found",
                            format!("agent {} not found", target_agent_id),
                        ),
                    ));
                }
            }
        }
        None => runtime.agent_summary().await?,
    };
    serialize_success(NAME, &AgentGetResult { agent: summary })
}
