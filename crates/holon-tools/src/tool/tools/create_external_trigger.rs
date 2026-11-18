use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::CREATE_EXTERNAL_TRIGGER;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum CallbackDeliveryModeArgs {
    EnqueueMessage,
    #[serde(alias = "wake_only")]
    WakeHint,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateExternalTriggerArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub delivery_mode: CallbackDeliveryModeArgs,
}

#[allow(dead_code)]
pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::ExternalTrigger,
        spec: typed_spec::<CreateExternalTriggerArgs>(
            NAME,
            include_str!("../tool_descriptions/create_external_trigger.md"),
        )?,
    })
}

#[cfg(test)]
mod tests;
