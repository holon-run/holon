use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{CallbackDeliveryMode, ToolCapabilityFamily, TrustLevel},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{normalize_optional_non_empty, parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "CreateExternalTrigger";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum CallbackDeliveryModeArgs {
    EnqueueMessage,
    WakeOnly,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateExternalTriggerArgs {
    pub(crate) summary: String,
    pub(crate) source: String,
    pub(crate) condition: String,
    pub(crate) resource: Option<String>,
    pub(crate) delivery_mode: CallbackDeliveryModeArgs,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::ExternalTrigger,
        spec: typed_spec::<CreateExternalTriggerArgs>(
            NAME,
            "Create an external trigger capability for an external system and record the waiting intent in the current agent.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _trust: &TrustLevel,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: CreateExternalTriggerArgs = parse_tool_args(NAME, input)?;
    let summary = validate_non_empty(args.summary, NAME, "summary")?;
    let source = validate_non_empty(args.source, NAME, "source")?;
    let condition = validate_non_empty(args.condition, NAME, "condition")?;
    let resource = normalize_optional_non_empty(args.resource);
    let delivery_mode = match args.delivery_mode {
        CallbackDeliveryModeArgs::EnqueueMessage => CallbackDeliveryMode::EnqueueMessage,
        CallbackDeliveryModeArgs::WakeOnly => CallbackDeliveryMode::WakeOnly,
    };
    let capability = runtime
        .create_callback(summary, source, condition, resource, delivery_mode)
        .await?;
    serialize_success(NAME, &capability)
}
