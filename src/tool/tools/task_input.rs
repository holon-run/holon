use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, TaskInputResult, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "TaskInput";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskInputArgs {
    pub(crate) task_id: String,
    pub(crate) input: String,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<TaskInputArgs>(
            NAME,
            include_str!("../tool_descriptions/task_input.md"),
        )?,
    })
}

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
