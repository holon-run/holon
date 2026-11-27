use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ProviderModelEntry;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use holon_domain::MediaOperation;
use holon_provider::model_discovery::ModelDiscoveryCacheStatus;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::LIST_PROVIDER_MODELS;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListProviderModelsArgs {
    pub provider: String,
    #[serde(default)]
    pub operation: Option<MediaOperation>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[schemars(
        description = "Diagnostic-only. Defaults to false so normal selection paths only see usable models; set true only to inspect blocked models and unavailable reasons."
    )]
    #[serde(default)]
    pub include_unavailable: bool,
}

#[derive(Serialize, Deserialize)]
pub struct ListProviderModelsResult {
    #[serde(default)]
    pub media_defaults: holon_domain::MediaToolDefaults,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<MediaOperation>,
    pub include_unavailable: bool,
    pub limit: usize,
    pub returned: usize,
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_cache: Option<ModelDiscoveryCacheStatus>,
    pub models: Vec<ProviderModelEntry>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<ListProviderModelsArgs>(
            NAME,
            include_str!("../tool_descriptions/list_provider_models.md"),
        )?,
    })
}
