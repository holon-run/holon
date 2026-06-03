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
    let (page, next_cursor) = page_provider_models(models, cursor.as_deref(), limit);
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

fn page_provider_models(
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

#[cfg(test)]
mod tests {
    use crate::config::ModelRef;
    use crate::types::{ModelAvailability, ProviderModelEntry};

    use super::page_provider_models;

    fn model(model_ref: &str) -> ProviderModelEntry {
        let policy = crate::model_catalog::ResolvedRuntimeModelPolicy {
            model_ref: ModelRef::parse(model_ref).unwrap(),
            ..crate::model_catalog::ResolvedRuntimeModelPolicy::default()
        };
        ProviderModelEntry {
            provider: "test".to_string(),
            id: model_ref.trim_start_matches("test/").to_string(),
            model_ref: model_ref.to_string(),
            display_name: model_ref.to_string(),
            availability: ModelAvailability::Available,
            selectable: true,
            unavailable_reason: None,
            metadata_source: "test".to_string(),
            policy,
            supported_parameters: Vec::new(),
            policy_notes: Vec::new(),
        }
    }

    #[test]
    fn pagination_cursor_resumes_after_last_returned_model() {
        let models = vec![
            model("test/a"),
            model("test/b"),
            model("test/c"),
            model("test/d"),
        ];

        let (first_page, next_cursor) = page_provider_models(models.clone(), None, 2);
        assert_eq!(
            first_page
                .iter()
                .map(|entry| entry.model_ref.as_str())
                .collect::<Vec<_>>(),
            vec!["test/a", "test/b"]
        );
        assert_eq!(next_cursor.as_deref(), Some("test/b"));

        let (second_page, next_cursor) = page_provider_models(models, next_cursor.as_deref(), 2);
        assert_eq!(
            second_page
                .iter()
                .map(|entry| entry.model_ref.as_str())
                .collect::<Vec<_>>(),
            vec!["test/c", "test/d"]
        );
        assert_eq!(next_cursor, None);
    }
}
