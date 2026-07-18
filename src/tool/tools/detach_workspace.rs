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
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = crate::tool::names::DETACH_WORKSPACE;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DetachWorkspaceArgs {
    pub(crate) workspace_id: String,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::AuthorityExpanding,
        spec: typed_spec::<DetachWorkspaceArgs>(
            NAME,
            include_str!("../tool_descriptions/detach_workspace.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: DetachWorkspaceArgs = parse_tool_args(NAME, input)?;
    let workspace_id = validate_non_empty(args.workspace_id, NAME, "workspace_id")?;
    serialize_success(
        NAME,
        &runtime
            .detach_workspace_with_fallback(&workspace_id)
            .await?,
    )
}
