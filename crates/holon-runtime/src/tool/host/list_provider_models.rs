use super::serialize_success;
use crate::provider::model_query;
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
    let limit = model_query::clamp_page_limit(args.limit);
    let cursor = normalize_optional_non_empty(args.cursor);
    let mut models = runtime.provider_models(&provider).await?;
    let discovery_cache = runtime
        .model_discovery_status()
        .await?
        .into_iter()
        .find(|status| status.provider.as_str() == provider);
    models.retain(|model| {
        model_query::model_matches(model, args.operation, args.include_unavailable)
    });
    let (page, next_cursor) =
        model_query::page_models(models, cursor.as_deref(), limit).map_err(|_| {
            invalid_tool_input(
                NAME,
                "ListProviderModels cursor does not match a model in the filtered list",
                serde_json::json!({
                    "field": "cursor",
                    "validation_error": "unknown_cursor",
                    "code": "invalid_cursor",
                }),
                "restart pagination without a cursor to receive a fresh next_cursor",
            )
        })?;
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
