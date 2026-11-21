use super::serialize_success;
use super::ToolModelRenderContext;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::get_workspace_state::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let _: GetWorkspaceStateArgs = parse_tool_args(NAME, input)?;
    let state = runtime.workspace_state_result().await?;
    let _serialize = crate::diagnostics::attribution::WORKSPACE_STATE_SERIALIZE.start();
    serialize_success(NAME, &state)
}

pub(crate) fn render_for_model(
    result: &crate::tool::ToolResult,
    context: &ToolModelRenderContext<'_>,
) -> Result<String> {
    let _render = crate::diagnostics::attribution::WORKSPACE_STATE_RENDER.start();
    holon_tools::builtins::get_workspace_state::render_for_model(result, context)
}
