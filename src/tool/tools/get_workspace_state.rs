use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = crate::tool::names::GET_WORKSPACE_STATE;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetWorkspaceStateArgs {}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::LocalEnvironment,
        spec: typed_spec::<GetWorkspaceStateArgs>(
            NAME,
            include_str!("../tool_descriptions/get_workspace_state.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let _: GetWorkspaceStateArgs = parse_tool_args(NAME, input)?;
    serialize_success(NAME, &runtime.workspace_state_result().await?)
}
