use anyhow::Result;

use crate::{tool::tool_error_from_anyhow, web::WebConfig};

pub use holon_web_retrieval::search::{
    WebCitation, WebSearchCommandAttempt, WebSearchProviderAttempt, WebSearchProviderAttemptStatus,
    WebSearchRequest, WebSearchResponse, WebSearchResult,
};

pub async fn search(request: WebSearchRequest, config: &WebConfig) -> Result<WebSearchResponse> {
    holon_web_retrieval::search::search(tool_error_from_anyhow, request, config).await
}

#[cfg(all(test, holon_lib_test_shard = "d"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn search_preserves_the_runtime_error_contract() {
        let mut config = WebConfig::default();
        config.search.enabled = true;
        let error = search(
            WebSearchRequest {
                query: "test query".into(),
                max_results: None,
                provider: Some("missing-search-provider".into()),
            },
            &config,
        )
        .await
        .unwrap_err();
        let error = tool_error_from_anyhow(&error);
        assert_eq!(error.kind, "provider_unavailable");
        assert_eq!(
            error.recovery_hint.as_deref(),
            Some("configure web.providers or use provider=duckduckgo")
        );
        let details = error.details.unwrap();
        assert_eq!(
            details["attempted_providers"],
            json!(["missing-search-provider"])
        );
        assert_eq!(
            details["provider_attempts"][0]["error_kind"],
            "provider_unavailable"
        );
        assert!(details["winning_provider"].is_null());
    }
}
