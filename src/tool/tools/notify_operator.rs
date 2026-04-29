use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{NotifyOperatorResult, ToolCapabilityFamily, TrustLevel},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "NotifyOperator";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NotifyOperatorArgs {
    pub(crate) message: String,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<NotifyOperatorArgs>(
            NAME,
            "Create an operator-facing notification record/event without stopping the current turn. Call Sleep explicitly afterward only if the agent should wait.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: NotifyOperatorArgs = parse_tool_args(NAME, input)?;
    let message = validate_non_empty(args.message, NAME, "message")?;
    let notification = runtime.notify_operator(message).await?;
    serialize_success(NAME, &NotifyOperatorResult { notification })
}
