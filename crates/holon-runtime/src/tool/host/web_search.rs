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

pub(crate) async fn list_providers(
    runtime: &RuntimeHandle,
    input: &Value,
) -> Result<crate::tool::ToolResult> {
    use holon_tools::builtins::list_web_search_providers as tool;

    let _: tool::ListWebSearchProvidersArgs = parse_tool_args(tool::NAME, input)?;
    let (web_config, derived_ids) = runtime.web_search_tool_config();
    serialize_success(
        tool::NAME,
        &tool::ListWebSearchProvidersResult {
            providers: holon_web_retrieval::search::list_search_providers(&web_config)
                .into_iter()
                .map(|provider_id| {
                    let source = if derived_ids.contains(&provider_id) {
                        "derived"
                    } else if web_config.providers.contains_key(&provider_id) {
                        "configured"
                    } else {
                        "builtin"
                    };
                    tool::WebSearchProviderEntry {
                        provider_id,
                        source: source.to_string(),
                    }
                })
                .collect(),
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
    let (web_config, _derived_ids) = runtime.web_search_tool_config();
    let dispatcher = RuntimeNativeSearchDispatcher { runtime };
    let result = search_with_native_dispatch(
        WebSearchRequest {
            query,
            max_results: args.max_results,
            provider: args.provider,
            language: args.language,
            region: args.region,
        },
        &web_config,
        Some(&dispatcher),
    )
    .await?;
    serialize_success(NAME, &result)
}
