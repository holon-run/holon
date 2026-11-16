use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

pub const NAME: &str = crate::tool::names::GET_AGENT;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetAgentArgs {
    #[serde(default)]
    pub agent_id: Option<String>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<GetAgentArgs>(NAME, include_str!("../tool_descriptions/get_agent.md"))?,
    })
}
