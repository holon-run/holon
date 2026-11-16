use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::CANCEL_EXTERNAL_TRIGGER;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelExternalTriggerArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_trigger_id: Option<String>,
}

#[allow(dead_code)]
pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::ExternalTrigger,
        spec: typed_spec::<CancelExternalTriggerArgs>(
            NAME,
            include_str!("../tool_descriptions/cancel_external_trigger.md"),
        )?,
    })
}
