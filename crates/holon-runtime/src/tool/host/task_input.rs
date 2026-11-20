use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use crate::types::TaskInputResult;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::task_input::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: TaskInputArgs = parse_tool_args(NAME, input)?;
    let task_id = validate_non_empty(args.task_id, NAME, "task_id")?;
    let result: TaskInputResult = runtime
        .managed_tasks()
        .task_input_with_trust(&task_id, &args.input, authority_class)
        .await?;
    serialize_success(NAME, &result)
}
