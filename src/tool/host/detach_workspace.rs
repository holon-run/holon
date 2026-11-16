use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::detach_workspace::*;

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
