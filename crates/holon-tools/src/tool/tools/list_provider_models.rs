use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::ModelAvailability;
use crate::types::ProviderModelEntry;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use holon_domain::MediaOperation;
use holon_provider::model_discovery::ModelDiscoveryCacheStatus;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const NAME: &str = crate::tool::names::LIST_PROVIDER_MODELS;

pub const DEFAULT_LIMIT: usize = 100;

pub const MAX_LIMIT: usize = 500;

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

pub fn model_matches_operation(
    model: &ProviderModelEntry,
    operation: Option<MediaOperation>,
    include_unavailable: bool,
) -> bool {
    include_unavailable
        || match operation {
            Some(operation) => model.media.operation(operation).call_selectable,
            None => model.availability != ModelAvailability::Unavailable,
        }
}

pub fn page_provider_models(
    models: Vec<ProviderModelEntry>,
    cursor: Option<&str>,
    limit: usize,
) -> (Vec<ProviderModelEntry>, Option<String>) {
    let start = cursor
        .and_then(|cursor| {
            models
                .iter()
                .position(|model| model.model_ref == cursor)
                .map(|index| index + 1)
        })
        .unwrap_or(0);
    let mut page = models
        .into_iter()
        .skip(start)
        .take(limit + 1)
        .collect::<Vec<_>>();
    if page.len() <= limit {
        return (page, None);
    }

    page.truncate(limit);
    let next_cursor = page.last().map(|model| model.model_ref.clone());
    (page, next_cursor)
}
