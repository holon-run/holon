use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::types::AuthorityClass;
use crate::types::CallbackDeliveryMode;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::create_external_trigger::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: CreateExternalTriggerArgs = parse_tool_args(NAME, input)?;
    let delivery_mode = match args.delivery_mode {
        CallbackDeliveryModeArgs::EnqueueMessage => CallbackDeliveryMode::EnqueueMessage,
        CallbackDeliveryModeArgs::WakeHint => CallbackDeliveryMode::WakeHint,
    };
    let capability = runtime.default_external_trigger(delivery_mode).await?;
    serialize_success(NAME, &capability)
}
