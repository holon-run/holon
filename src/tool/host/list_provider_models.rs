use super::serialize_success;
#[cfg(test)]
use crate::provider::media::MediaOperation;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::invalid_tool_input;
use crate::tool::helpers::normalize_optional_non_empty;
use crate::tool::helpers::parse_tool_args;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::list_provider_models::*;

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
    let discovery_cache = runtime
        .model_discovery_status()
        .await?
        .into_iter()
        .find(|status| status.provider.as_str() == provider);
    models.retain(|model| model_matches_operation(model, args.operation, args.include_unavailable));
    let (page, next_cursor) = page_provider_models(models, cursor.as_deref(), limit);
    let returned = page.len();
    serialize_success(
        NAME,
        &ListProviderModelsResult {
            media_defaults: runtime.media_tool_defaults(None).await?,
            provider,
            operation: args.operation,
            include_unavailable: args.include_unavailable,
            limit,
            returned,
            next_cursor,
            discovery_cache,
            models: page,
        },
    )
}

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use crate::config::ModelRef;
    use crate::types::{ModelAvailability, ProviderModelEntry};

    use super::{model_matches_operation, page_provider_models, MediaOperation};

    #[test]
    fn operation_filter_is_independent_of_text_turn_availability() {
        let mut image_only = model("test/image");
        image_only.availability = ModelAvailability::Unavailable;
        image_only.selectable = false;
        image_only.media.generate_image.call_selectable = true;
        assert!(model_matches_operation(
            &image_only,
            Some(MediaOperation::GenerateImage),
            false
        ));
        assert!(!model_matches_operation(&image_only, None, false));
        assert!(!model_matches_operation(
            &image_only,
            Some(MediaOperation::ViewImage),
            false
        ));
        image_only.media.deny_tool_access();
        assert!(!model_matches_operation(
            &image_only,
            Some(MediaOperation::GenerateImage),
            false
        ));
        assert!(model_matches_operation(
            &image_only,
            Some(MediaOperation::GenerateImage),
            true
        ));
        assert!(!image_only.media.generate_image.call_selectable);
    }

    #[test]
    fn old_model_result_deserializes_without_media_projection() {
        let mut value = serde_json::to_value(model("test/image")).unwrap();
        value.as_object_mut().unwrap().remove("media");
        let old: ProviderModelEntry = serde_json::from_value(value).unwrap();
        assert_eq!(old.media, Default::default());
    }

    fn model(model_ref: &str) -> ProviderModelEntry {
        let policy = crate::model_catalog::ResolvedRuntimeModelPolicy {
            model_ref: ModelRef::parse(model_ref).unwrap(),
            ..crate::model_catalog::ResolvedRuntimeModelPolicy::default()
        };
        ProviderModelEntry {
            provider: "test".to_string(),
            provider_family: "test".to_string(),
            endpoint: "default".to_string(),
            route_provider: "test".to_string(),
            id: model_ref.trim_start_matches("test/").to_string(),
            model_ref: model_ref.to_string(),
            display_name: model_ref.to_string(),
            availability: ModelAvailability::Available,
            selectable: true,
            unavailable_reason: None,
            metadata_source: "test".to_string(),
            policy,
            parameter_contracts: Vec::new(),
            supported_parameters: Vec::new(),
            media: Default::default(),
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
