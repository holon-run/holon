use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use crate::web::fetch::fetch_with_provider;
use crate::web::fetch::WebFetchRequest;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::web_fetch::*;

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: WebFetchArgs = parse_tool_args(NAME, input)?;
    let url = validate_non_empty(args.url, NAME, "url")?;
    let web_config = runtime.web_config();
    let result = fetch_with_provider(
        WebFetchRequest {
            url,
            max_chars: args.max_chars,
            extract_mode: args.extract_mode,
        },
        args.provider.as_deref(),
        &web_config,
    )
    .await?;
    serialize_success(NAME, &result)
}

pub(crate) async fn list_providers(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    use holon_tools::builtins::list_web_fetch_providers as tool;

    let _: tool::ListWebFetchProvidersArgs = parse_tool_args(tool::NAME, input)?;
    let providers = holon_web_retrieval::fetch::list_fetch_providers(&runtime.web_config())
        .into_iter()
        .map(|info| tool::WebFetchProviderSummary {
            provider_id: info.provider_id,
            kind: info.kind.to_string(),
            extract_modes: info.extract_modes.into_iter().map(str::to_string).collect(),
        })
        .collect();
    serialize_success(tool::NAME, &tool::ListWebFetchProvidersResult { providers })
}
