use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::SEND_AGENT_MESSAGE;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendAgentMessageArgs {
    pub agent_id: String,
    pub message: String,
    #[serde(default)]
    pub forward: bool,
    #[serde(default)]
    pub reply_context_id: Option<String>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<SendAgentMessageArgs>(
            NAME,
            include_str!("../tool_descriptions/send_agent_message.md"),
        )?,
    })
}

#[cfg(test)]
mod tests;
