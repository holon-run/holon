use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use crate::types::TaskStatusResult;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::task_status::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: TaskStatusArgs = parse_tool_args(NAME, input)?;
    let task_id = validate_non_empty(args.task_id, NAME, "task_id")?;
    let read = runtime.managed_tasks().task_status_read(&task_id).await?;
    // The remaining read carrier keeps pins through synchronous serialization.
    let snapshot = read.snapshot;
    serialize_success(
        NAME,
        &TaskStatusResult {
            summary_text: snapshot
                .summary
                .clone()
                .or_else(|| Some(format!("task {} status retrieved", snapshot.task_id))),
            task: snapshot,
        },
    )
}
