use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{TaskInputResult, ToolCapabilityFamily, TrustLevel},
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
            "Send input to a managed task handle. Command tasks accept stdin or tty text here when they were created for interactive continuation, and parent-supervised child handles accept bounded follow-up input on the same surface.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: TaskInputArgs = parse_tool_args(NAME, input)?;
    let task_id = validate_non_empty(args.task_id, NAME, "task_id")?;
    let result: TaskInputResult = runtime
        .task_input_with_trust(&task_id, &args.input, trust)
        .await?;
    serialize_success(NAME, &result)
}
