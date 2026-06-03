use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    runtime::RuntimeHandle,
    tool::spec::typed_spec,
    types::{AuthorityClass, ModelAvailability, ProviderModelEntry, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};
use crate::tool::helpers::{invalid_tool_input, normalize_optional_non_empty, parse_tool_args};

pub(crate) const NAME: &str = "ListProviderModels";
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListProviderModelsArgs {
    pub(crate) provider: String,
    #[serde(default)]
    pub(crate) cursor: Option<String>,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    #[serde(default)]
    pub(crate) include_unavailable: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ListProviderModelsResult {
    pub(crate) provider: String,
    pub(crate) include_unavailable: bool,
    pub(crate) limit: usize,
    pub(crate) returned: usize,
    pub(crate) next_cursor: Option<String>,
    pub(crate) models: Vec<ProviderModelEntry>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<ListProviderModelsArgs>(
            NAME,
            "List models for a provider. Use provider from ListModelProviders; cursor is a model_ref returned by a previous page.",
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: ListProviderModelsArgs = parse_tool_args(NAME, input)?;
    let provider = normalize_optional_non_empty(Some(args.provider)).ok_or_else(|| {
        invalid_tool_input(
            NAME,
            "ListProviderModels requires a non-empty provider",
            serde_json::json!({
                "field": "provider",
                "validation_error": "must not be empty",
            }),
            "call ListModelProviders first and pass one returned provider id",
        )
    })?;
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let cursor = normalize_optional_non_empty(args.cursor);
    let mut models = runtime.provider_models(&provider).await?;
    if !args.include_unavailable {
        models.retain(|model| model.availability != ModelAvailability::Unavailable);
    }
    let start = cursor
        .as_deref()
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
    let next_cursor = if page.len() > limit {
        page.pop().map(|model| model.model_ref)
    } else {
        None
    };
    let returned = page.len();
    serialize_success(
        NAME,
        &ListProviderModelsResult {
            provider,
            include_unavailable: args.include_unavailable,
            limit,
            returned,
            next_cursor,
            models: page,
        },
    )
}
