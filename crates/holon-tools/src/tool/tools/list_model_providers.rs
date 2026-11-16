use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ModelProviderEntry;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use holon_domain::MediaOperation;
use holon_provider::model_discovery::ModelDiscoveryCacheStatus;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::LIST_MODEL_PROVIDERS;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListModelProvidersArgs {
    #[serde(default)]
    pub operation: Option<MediaOperation>,
    #[schemars(
        description = "Diagnostic-only. Defaults to false so normal selection paths only see usable providers; set true only to inspect blocked provider options and unavailable reasons."
    )]
    #[serde(default)]
    pub include_unavailable: bool,
}

#[derive(Serialize, Deserialize)]
pub struct ListModelProvidersResult {
    #[serde(default)]
    pub media_defaults: holon_domain::MediaToolDefaults,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<MediaOperation>,
    pub include_unavailable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_discovery_cache: Vec<ModelDiscoveryCacheStatus>,
    pub providers: Vec<ModelProviderEntry>,
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<ListModelProvidersArgs>(
            NAME,
            include_str!("../tool_descriptions/list_model_providers.md"),
        )?,
    })
}
