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
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = "AgentGet";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentGetArgs {
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
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: AgentGetArgs = parse_tool_args(NAME, input)?;
    let summary = match args.agent_id {
        None => {
            // Default behavior: return current agent summary.
            runtime.agent_summary().await?
        }
        Some(requested_id) => {
            // Requested agent: resolve through the host bridge, which allows
            // private child agents under the local trusted control boundary.
            runtime.agent_summary_for(&requested_id).await?
        }
    };
    serialize_success(NAME, &AgentGetResult { agent: summary })
}
