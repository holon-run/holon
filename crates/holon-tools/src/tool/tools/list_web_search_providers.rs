use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;

pub const NAME: &str = crate::tool::names::LIST_WEB_SEARCH_PROVIDERS;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListWebSearchProvidersArgs {}

#[derive(Serialize, JsonSchema)]
pub struct ListWebSearchProvidersResult {
    pub providers: Vec<String>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        // Share WebSearch's model visibility and dispatch authorization.
        family: ToolCapabilityFamily::Web,
        spec: typed_spec::<ListWebSearchProvidersArgs>(
            NAME,
            include_str!("../tool_descriptions/list_web_search_providers.md"),
        )?,
    })
}
