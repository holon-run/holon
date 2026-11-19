use super::serialize_success;
use crate::runtime::RuntimeHandle;
use crate::tool::helpers::parse_tool_args;
use crate::tool::helpers::validate_non_empty;
use crate::types::AuthorityClass;
use crate::web::search::search_with_native_dispatch;
use crate::web::search::{WebSearchRequest, WebSearchResult};
use anyhow::Result;
use serde_json::Value;

pub(crate) use holon_tools::builtins::web_search::*;

struct RuntimeNativeSearchDispatcher<'a> {
    runtime: &'a RuntimeHandle,
}

#[async_trait::async_trait]
impl holon_web_retrieval::search::WebSearchNativeDispatcher for RuntimeNativeSearchDispatcher<'_> {
    async fn dispatch(
        &self,
        provider_id: &str,
        model_route: &str,
        query: &str,
        max_results: usize,
    ) -> std::result::Result<
        Vec<WebSearchResult>,
        holon_web_retrieval::search::WebSearchNativeDispatchError,
    > {
        self.runtime
            .dispatch_native_web_search(provider_id, model_route, query, max_results)
            .await
    }
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
    let dispatcher = RuntimeNativeSearchDispatcher { runtime };
    let result = search_with_native_dispatch(
        WebSearchRequest {
            query,
            max_results: args.max_results,
            provider: args.provider,
        },
        &web_config,
        Some(&dispatcher),
    )
    .await?;
    serialize_success(NAME, &result)
}
