use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, CallbackDeliveryMode, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::parse_tool_args;

pub(crate) const NAME: &str = crate::tool::names::CREATE_EXTERNAL_TRIGGER;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum CallbackDeliveryModeArgs {
    EnqueueMessage,
    #[serde(alias = "wake_only")]
    WakeHint,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateExternalTriggerArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<String>,
    pub(crate) delivery_mode: CallbackDeliveryModeArgs,
}

#[allow(dead_code)]
pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::ExternalTrigger,
        spec: typed_spec::<CreateExternalTriggerArgs>(
            NAME,
            include_str!("../tool_descriptions/create_external_trigger.md"),
        )?,
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_mode_accepts_legacy_wake_only_alias() {
        let args: CreateExternalTriggerArgs = serde_json::from_value(serde_json::json!({
            "description": "Check external queue",
            "source": "test",
            "delivery_mode": "wake_only"
        }))
        .expect("legacy wake_only alias should deserialize");

        assert!(matches!(
            args.delivery_mode,
            CallbackDeliveryModeArgs::WakeHint
        ));
    }
}
