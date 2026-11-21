use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use crate::types::TaskStatus;
use crate::types::TaskStopResult;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::task_stop::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: TaskStopArgs = parse_tool_args(NAME, input)?;
    let task_id = validate_non_empty(args.task_id, NAME, "task_id")?;
    let task = runtime
        .managed_tasks()
        .stop_task(&task_id, authority_class)
        .await?;
    let force_stop_requested = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("force_stop_requested"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let snapshot = crate::types::TaskStatusSnapshot::from_task_record(&task);
    serialize_success(
        NAME,
        &TaskStopResult {
            summary_text: Some(match task.status {
                TaskStatus::Cancelling => format!("stop requested for task {}", task.id),
                TaskStatus::Cancelled => format!("cancelled task {}", task.id),
                _ => format!("updated task {}", task.id),
            }),
            task: snapshot,
            stop_requested: true,
            force_stop_requested,
        },
    )
}
