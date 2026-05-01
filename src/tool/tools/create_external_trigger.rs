use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{CallbackDeliveryMode, ExternalTriggerScope, ToolCapabilityFamily, TrustLevel},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{parse_tool_args, validate_non_empty};

pub(crate) const NAME: &str = "CreateExternalTrigger";

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum CallbackDeliveryModeArgs {
    EnqueueMessage,
    #[serde(alias = "wake_only")]
    WakeHint,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum ExternalTriggerScopeArgs {
    WorkItem,
    Agent,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateExternalTriggerArgs {
    pub(crate) description: String,
    pub(crate) source: String,
    pub(crate) scope: ExternalTriggerScopeArgs,
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
    let description = validate_non_empty(args.description, NAME, "description")?;
    let source = validate_non_empty(args.source, NAME, "source")?;
    let scope = match args.scope {
        ExternalTriggerScopeArgs::WorkItem => ExternalTriggerScope::WorkItem,
        ExternalTriggerScopeArgs::Agent => ExternalTriggerScope::Agent,
    };
    let delivery_mode = match args.delivery_mode {
        CallbackDeliveryModeArgs::EnqueueMessage => CallbackDeliveryMode::EnqueueMessage,
        CallbackDeliveryModeArgs::WakeHint => CallbackDeliveryMode::WakeHint,
    };
    let capability = runtime
        .create_external_trigger(description, source, scope, delivery_mode, None, None)
        .await?;
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
            "scope": "agent",
            "delivery_mode": "wake_only"
        }))
        .expect("legacy wake_only alias should deserialize");

        assert!(matches!(
            args.delivery_mode,
            CallbackDeliveryModeArgs::WakeHint
        ));
    }
}
