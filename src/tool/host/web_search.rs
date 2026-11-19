use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use crate::web::search::search;
use crate::web::search::WebSearchRequest;
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::web_search::*;

pub(crate) async fn list_providers(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    use holon_tools::builtins::list_web_search_providers as tool;

    let _: tool::ListWebSearchProvidersArgs = parse_tool_args(tool::NAME, input)?;
    serialize_success(
        tool::NAME,
        &tool::ListWebSearchProvidersResult {
            providers: holon_web_retrieval::search::list_search_providers(&runtime.web_config()),
        },
    )
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    _agent_id: &str,
    _authority_class: &AuthorityClass,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    let args: WebSearchArgs = parse_tool_args(NAME, input)?;
    let query = validate_non_empty(args.query, NAME, "query")?;
    let web_config = runtime.web_config();
    let result = search(
        WebSearchRequest {
            query,
            max_results: args.max_results,
            provider: args.provider,
        },
        &web_config,
    )
    .await?;
    serialize_success(NAME, &result)
}
