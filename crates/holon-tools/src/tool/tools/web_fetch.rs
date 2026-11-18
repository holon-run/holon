use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use super::BuiltinToolDefinition;

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtractMode {
    Auto,
    Text,
    Raw,
}

impl Default for ExtractMode {
    fn default() -> Self {
        Self::Auto
    }
}

pub const NAME: &str = crate::tool::names::WEB_FETCH;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WebFetchArgs {
    pub url: String,
    pub provider: Option<String>,
    pub max_chars: Option<usize>,
    #[serde(default)]
    pub extract_mode: ExtractMode,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::Web,
        spec: typed_spec::<WebFetchArgs>(NAME, include_str!("../tool_descriptions/web_fetch.md"))?,
    })
}

#[cfg(test)]
mod tests;
