use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{TaskStatus, TaskStopResult, ToolCapabilityFamily, TrustLevel},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "TaskStop";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskStopArgs {
    pub(crate) task_id: String,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<TaskStopArgs>(
            NAME,
            "Stop a currently running background task by id. command_task may first enter cancelling before the final cancelled result arrives.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: TaskStopArgs = parse_tool_args(NAME, input)?;
    let task_id = validate_non_empty(args.task_id, NAME, "task_id")?;
    let task = runtime.stop_task(&task_id, trust).await?;
    let force_stop_requested = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("force_stop_requested"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    serialize_success(
        NAME,
        &TaskStopResult {
            summary_text: Some(match task.status {
                TaskStatus::Cancelling => format!("stop requested for task {}", task.id),
                TaskStatus::Cancelled => format!("cancelled task {}", task.id),
                _ => format!("updated task {}", task.id),
            }),
            task,
            stop_requested: true,
            force_stop_requested,
        },
    )
}
