use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::task_list::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    execute_with_name(NAME, runtime, input).await
}

pub(crate) async fn execute_legacy(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    execute_with_name(LEGACY_NAME, runtime, input).await
}

async fn execute_with_name(
    tool_name: &str,
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: ListTasksArgs = parse_tool_args(tool_name, input)?;
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let read = runtime.active_task_list_snapshot(limit).await?;
    serialize_success(
        tool_name,
        &ListTasksResult {
            total_active: read.total_active,
            returned: read.entries.len(),
            limit,
            tasks: read.entries,
        },
    )
}
