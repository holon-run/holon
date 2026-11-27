use super::serialize_success;
use crate::provider::model_query;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::types::AuthorityClass;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::list_model_providers::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: ListModelProvidersArgs = parse_tool_args(NAME, input)?;
    let mut providers = runtime.model_providers().await?;
    let model_discovery_cache = runtime.model_discovery_status().await?;
    providers.retain(|provider| {
        model_query::provider_matches(provider, args.operation, args.include_unavailable)
    });
    serialize_success(
        NAME,
        &ListModelProvidersResult {
            media_defaults: runtime.media_tool_defaults(None).await?,
            operation: args.operation,
            include_unavailable: args.include_unavailable,
            model_discovery_cache,
            providers,
        },
    )
}
