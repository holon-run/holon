use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ToolCapabilityFamily;

pub const NAME: &str = crate::tool::names::LIST_WEB_FETCH_PROVIDERS;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListWebFetchProvidersArgs {}

/// One WebFetch route: the default local HTTP route (empty `provider_id`)
/// or a configured remote provider ID, with the extract modes it accepts.
#[derive(Serialize, JsonSchema)]
pub struct WebFetchProviderSummary {
    /// Empty for the built-in local HTTP route.
    pub provider_id: String,
    pub kind: String,
    pub extract_modes: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct ListWebFetchProvidersResult {
    pub providers: Vec<WebFetchProviderSummary>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        // Share WebFetch's model visibility and dispatch authorization.
        family: ToolCapabilityFamily::Web,
        spec: typed_spec::<ListWebFetchProvidersArgs>(
            NAME,
            include_str!("../tool_descriptions/list_web_fetch_providers.md"),
        )?,
    })
}
