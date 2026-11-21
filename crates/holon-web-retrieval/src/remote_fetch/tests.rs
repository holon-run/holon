use super::*;
use crate::fetch::ExtractMode;
use axum::{extract::Request, http::StatusCode, routing::post, Router};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn request() -> WebFetchRequest {
    WebFetchRequest {
        url: "https://93.184.216.34/docs".into(),
        max_chars: None,
        extract_mode: ExtractMode::Auto,
    }
}

async fn server(
    status: StatusCode,
    body: &str,
) -> (
    WebProviderConfig,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let body = body.to_owned();
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = calls.clone();
    let router = Router::new().route(
        "/v1/tools/fetch",
        post(move |request: Request| {
            let body = body.clone();
            let calls = handler_calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(request.headers()["authorization"], "Bearer fixture-key");
                let bytes = axum::body::to_bytes(request.into_body(), 100_000)
                    .await
                    .unwrap();
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(value, json!({"url": "https://93.184.216.34/docs"}));
                (status, body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (
        WebProviderConfig {
            kind: super::super::WebProviderKind::DeepSeek,
            base_url: Some(base_url),
            engine_id: None,
            api_key: "fixture-key".into(),
            model_route: None,
            command: None,
            output: None,
            limits: Default::default(),
        },
        calls,
        handle,
    )
}

#[tokio::test]
async fn deepseek_fetch_maps_markdown_provenance_and_unicode_limit() {
    let content = "# 文档\n内容";
    let (provider, _calls, handle) = server(
        StatusCode::OK,
        &json!({"title": "文档", "content": content}).to_string(),
    )
    .await;
    let config = WebFetchConfig {
        max_chars: 4,
        ..Default::default()
    };
    let mut request = request();
    request.max_chars = Some(100);
    let page = fetch(request, "fixture", &provider, &config).await.unwrap();
    handle.abort();
    assert_eq!(page.final_url, None);
    assert_eq!(page.source, "deepseek_remote");
    assert_eq!(page.provider_id.as_deref(), Some("fixture"));
    assert_eq!(page.status_source, "provider_api");
    assert_eq!(page.status, 200);
    assert_eq!(page.content_type.as_deref(), Some("text/markdown"));
    assert_eq!(page.bytes_read, content.len());
    assert_eq!(
        page.sha256,
        format!("{:x}", Sha256::digest(content.as_bytes()))
    );
    assert!(page.truncated);
    assert!(page.text.contains("# 文档"));
    assert!(!page.text.contains("内容"));
    assert!(page.text.starts_with("<external_content source="));
    assert!(page.text.contains("untrusted"));
}

#[tokio::test]
async fn deepseek_fetch_rejects_unsafe_or_unenforceable_requests() {
    let (mut provider, calls, handle) = server(StatusCode::OK, "{}").await;
    let config = WebFetchConfig::default();
    for url in [
        "http://127.0.0.1/private",
        "http://169.254.169.254/latest/meta-data",
        "http://10.1.2.3/private",
        "file:///etc/passwd",
        "https://user:secret@93.184.216.34",
        "not a url",
    ] {
        let mut input = request();
        input.url = url.into();
        let error = fetch(input, "fixture", &provider, &config)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("secret"));
    }
    let mut input = request();
    input.url = format!("https://93.184.216.34/{}", "a".repeat(8192));
    assert!(fetch(input, "fixture", &provider, &config).await.is_err());
    for mode in [ExtractMode::Raw, ExtractMode::Text] {
        let mut input = request();
        input.extract_mode = mode;
        // Mode gating lives in the shared route layer before any request.
        let mut shared = super::super::WebConfig::default();
        shared.providers.insert("fixture".into(), provider.clone());
        assert!(
            super::super::fetch::fetch_with_provider(input, Some("fixture"), &shared)
                .await
                .is_err()
        );
    }
    for config in [
        WebFetchConfig {
            enabled: false,
            ..Default::default()
        },
        WebFetchConfig {
            allowed_hosts: vec!["93.184.216.34".into()],
            ..Default::default()
        },
        WebFetchConfig {
            denied_hosts: vec!["example.com".into()],
            ..Default::default()
        },
        WebFetchConfig {
            max_redirects: 0,
            ..Default::default()
        },
    ] {
        assert!(fetch(request(), "fixture", &provider, &config)
            .await
            .is_err());
    }
    provider.api_key.clear();
    assert!(fetch(request(), "fixture", &provider, &config)
        .await
        .is_err());
    handle.abort();
    // Every rejection above must happen before the provider is contacted.
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn deepseek_fetch_errors_are_bounded_redacted_and_retryable() {
    for (status, body, retryable) in [
        (StatusCode::UNAUTHORIZED, "fixture-key", false),
        (StatusCode::PAYMENT_REQUIRED, "fixture-key", false),
        (StatusCode::TOO_MANY_REQUESTS, "fixture-key", true),
        (StatusCode::INTERNAL_SERVER_ERROR, "fixture-key", true),
        (StatusCode::TEMPORARY_REDIRECT, "fixture-key", false),
        (StatusCode::OK, "{\"content\":\"fixture-key", false),
        (StatusCode::OK, "{}", false),
        (StatusCode::OK, "{\"content\":\"\"}", false),
    ] {
        let (provider, _calls, handle) = server(status, body).await;
        let error = fetch(request(), "fixture", &provider, &WebFetchConfig::default())
            .await
            .unwrap_err();
        handle.abort();
        let error = error.downcast_ref::<ToolError>().unwrap();
        let value = serde_json::to_value(error).unwrap();
        assert_eq!(value["retryable"], retryable);
        assert!(!value.to_string().contains("fixture-key"));
    }
    let (provider, _calls, handle) = server(StatusCode::OK, "{\"content\":\"too big\"}").await;
    let config = WebFetchConfig {
        max_response_bytes: 8,
        ..Default::default()
    };
    assert!(fetch(request(), "fixture", &provider, &config)
        .await
        .unwrap_err()
        .to_string()
        .contains("max_response_bytes"));
    handle.abort();
}

#[tokio::test]
async fn fetch_provider_selection_rejects_unknown_or_unsupported_kind() {
    let (mut provider, calls, handle) = server(StatusCode::OK, "{}").await;
    let mut config = super::super::WebConfig::default();
    assert!(
        super::super::fetch::fetch_with_provider(request(), Some("absent"), &config)
            .await
            .is_err()
    );
    provider.kind = super::super::WebProviderKind::Brave;
    config.providers.insert("brave".into(), provider.clone());
    assert!(
        super::super::fetch::fetch_with_provider(request(), Some("brave"), &config)
            .await
            .is_err()
    );
    config.fetch.enabled = false;
    assert!(
        super::super::fetch::fetch_with_provider(request(), Some("brave"), &config)
            .await
            .is_err()
    );
    handle.abort();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

// ---- Tavily Extract ----

fn tavily_provider(base_url: String) -> WebProviderConfig {
    WebProviderConfig {
        kind: super::super::WebProviderKind::Tavily,
        base_url: Some(base_url),
        engine_id: None,
        api_key: "fixture-key".into(),
        model_route: None,
        command: None,
        output: None,
        limits: Default::default(),
    }
}

async fn tavily_server(
    status: StatusCode,
    body: &str,
) -> (
    WebProviderConfig,
    Arc<std::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let body = body.to_owned();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let handler_seen = seen.clone();
    let router = Router::new().route(
        "/extract",
        post(move |request: Request| {
            let body = body.clone();
            let seen = handler_seen.clone();
            async move {
                assert_eq!(request.headers()["authorization"], "Bearer fixture-key");
                let bytes = axum::body::to_bytes(request.into_body(), 100_000)
                    .await
                    .unwrap();
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                seen.lock().unwrap().push(value);
                (status, body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (tavily_provider(base_url), seen, handle)
}

#[tokio::test]
async fn tavily_extract_maps_markdown_and_text_provenance() {
    let content = "# 页面\n正文";
    let (provider, seen, handle) = tavily_server(
        StatusCode::OK,
        &json!({"results": [{"url": "https://93.184.216.34/docs", "raw_content": content}]})
            .to_string(),
    )
    .await;
    let config = WebFetchConfig {
        max_chars: 4,
        ..Default::default()
    };
    let mut first = request();
    first.max_chars = Some(100);
    let page = fetch(first, "fixture", &provider, &config).await.unwrap();
    handle.abort();
    assert_eq!(page.final_url, None);
    assert_eq!(page.source, "tavily_remote");
    assert_eq!(page.provider_id.as_deref(), Some("fixture"));
    assert_eq!(page.status_source, "provider_api");
    assert_eq!(page.status, 200);
    assert_eq!(page.content_type.as_deref(), Some("text/markdown"));
    assert_eq!(page.bytes_read, content.len());
    assert_eq!(
        page.sha256,
        format!("{:x}", Sha256::digest(content.as_bytes()))
    );
    assert!(page.truncated);
    assert!(page.text.starts_with("<external_content source="));
    assert!(page.text.contains("untrusted"));
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[json!({"urls": ["https://93.184.216.34/docs"], "format": "markdown"})]
    );

    // text mode maps to the documented plain-text format.
    let (provider, seen, handle) = tavily_server(
        StatusCode::OK,
        &json!({"results": [{"url": "https://93.184.216.34/docs", "raw_content": "plain body"}]})
            .to_string(),
    )
    .await;
    let mut input = request();
    input.extract_mode = ExtractMode::Text;
    let page = fetch(input, "fixture", &provider, &WebFetchConfig::default())
        .await
        .unwrap();
    handle.abort();
    assert_eq!(page.content_type.as_deref(), Some("text/plain"));
    assert!(!page.truncated);
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[json!({"urls": ["https://93.184.216.34/docs"], "format": "text"})]
    );
}

#[tokio::test]
async fn tavily_extract_rejects_provider_failures_without_fallback() {
    let url = "https://93.184.216.34/docs";
    for (status, body) in [
        // HTTP 200 with the requested URL in failed_results.
        (
            StatusCode::OK,
            json!({
                "results": [],
                "failed_results": [{"url": url, "error": "extraction failed"}]
            })
            .to_string(),
        ),
        // HTTP 200 with no results at all.
        (
            StatusCode::OK,
            json!({"results": [], "failed_results": []}).to_string(),
        ),
        // HTTP 200 with empty content for the requested URL.
        (
            StatusCode::OK,
            json!({"results": [{"url": url, "raw_content": "  "}]}).to_string(),
        ),
        // HTTP 200 whose only result is for a different URL.
        (
            StatusCode::OK,
            json!({"results": [{"url": "https://other.example/x", "raw_content": "body"}]})
                .to_string(),
        ),
        // Transport-level provider errors.
        (StatusCode::UNAUTHORIZED, "unauthorized".into()),
        (StatusCode::TOO_MANY_REQUESTS, "rate limited".into()),
        (StatusCode::BAD_GATEWAY, "bad gateway".into()),
        // Invalid JSON body.
        (StatusCode::OK, "{\"results\":".into()),
    ] {
        let (provider, seen, handle) = tavily_server(status, &body).await;
        let error = fetch(request(), "fixture", &provider, &WebFetchConfig::default())
            .await
            .unwrap_err();
        handle.abort();
        let tool_error = error.downcast_ref::<ToolError>().unwrap();
        let value = serde_json::to_value(tool_error).unwrap();
        // Rate limiting and upstream 5xx map to retryable; content and
        // authentication failures do not.
        let expected_retryable = matches!(
            status,
            StatusCode::TOO_MANY_REQUESTS | StatusCode::BAD_GATEWAY
        );
        assert_eq!(value["retryable"], expected_retryable, "{status}");
        assert_eq!(value["kind"], "provider_error", "{status}");
        assert!(!value.to_string().contains("fixture-key"));
        // Exactly one /extract call happened and no local fallback retried it.
        assert_eq!(seen.lock().unwrap().len(), 1, "{status}");
    }
}

#[tokio::test]
async fn tavily_extract_rejects_raw_mode_before_sending() {
    let (provider, seen, handle) = tavily_server(StatusCode::OK, "{}").await;
    let mut config = super::super::WebConfig::default();
    config.providers.insert("fixture".into(), provider.clone());
    let mut input = request();
    input.extract_mode = ExtractMode::Raw;
    assert!(
        super::super::fetch::fetch_with_provider(input, Some("fixture"), &config)
            .await
            .is_err()
    );
    let mut provider = provider;
    provider.api_key.clear();
    config.providers.insert("fixture".into(), provider);
    assert!(fetch(
        request(),
        "fixture",
        &config.providers["fixture"],
        &config.fetch
    )
    .await
    .is_err());
    handle.abort();
    // Nothing left the process for either rejection.
    assert!(seen.lock().unwrap().is_empty());
}

async fn exa_server(
    status: StatusCode,
    body: String,
) -> (
    crate::WebConfig,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = calls.clone();
    let router = Router::new().route(
        "/api/contents",
        post(move |outbound: Request| {
            let calls = handler_calls.clone();
            let body = body.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(outbound.headers()["x-api-key"], "fixture-key");
                let bytes = axum::body::to_bytes(outbound.into_body(), 100_000)
                    .await
                    .unwrap();
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(value, json!({"ids": [request().url], "text": true}));
                (status, body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut provider = tavily_provider(format!("http://{}/api/", listener.local_addr().unwrap()));
    provider.kind = WebProviderKind::Exa;
    let mut config = crate::WebConfig::default();
    config.providers.insert("exa-fixture".into(), provider);
    let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (config, calls, handle)
}

async fn fetch_exa(input: WebFetchRequest, config: &crate::WebConfig) -> Result<WebFetchResponse> {
    crate::fetch::fetch_with_provider(input, Some("exa-fixture"), config).await
}

#[test]
fn exa_contents_uses_the_default_endpoint_and_sanitizes_invalid_base_urls() {
    let mut provider = tavily_provider("https://unused.example".into());
    provider.kind = WebProviderKind::Exa;
    provider.base_url = None;
    assert_eq!(
        exa_contents_url(&provider).unwrap().as_str(),
        "https://api.exa.ai/contents"
    );
    provider.base_url = Some("https://name:fixture-secret@exa.example".into());
    let error = exa_contents_url(&provider).unwrap_err();
    let receipt = serde_json::to_value(error.downcast_ref::<ToolError>().unwrap()).unwrap();
    assert_eq!(receipt["kind"], "provider_unavailable");
    assert!(!receipt.to_string().contains("fixture-secret"));
}

#[tokio::test]
async fn exa_contents_extracts_only_text_and_keeps_provider_provenance() {
    let mut input = request();
    input.max_chars = Some(500);
    let content = "网页正文 with more text";
    let (mut config, calls, handle) = exa_server(
        StatusCode::OK,
        json!({
            "results": [{
                "id": input.url,
                "url": "https://exa-reported.example/page",
                "text": content,
                "summary": "not delivered"
            }],
            "statuses": [{"id": input.url, "status": "success", "source": "cached"}],
        })
        .to_string(),
    )
    .await;
    config.fetch.max_chars = 4;
    let response = fetch_exa(input, &config).await.unwrap();
    handle.abort();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(response.url, request().url);
    assert!(response.final_url.is_none());
    assert_eq!(response.status, 200);
    assert_eq!(response.status_source, "provider_api");
    assert_eq!(response.source, "exa_remote");
    assert_eq!(response.provider_id.as_deref(), Some("exa-fixture"));
    assert_eq!(response.content_type.as_deref(), Some("text/plain"));
    assert_eq!(response.bytes_read, content.len());
    assert_eq!(response.sha256, format!("{:x}", Sha256::digest(content)));
    assert!(response.truncated);
    assert_eq!(
        response.text,
        external_content_wrapper(&Url::parse(&request().url).unwrap(), "网页正文...")
    );
}

#[tokio::test]
async fn exa_contents_rejects_unsuccessful_or_unexpected_responses() {
    let url = request().url;
    let good_status = json!({"id": url, "status": "success"});
    let good_result = json!({"id": url, "text": "body"});
    let failed_status = json!({
        "id": url,
        "status": "error",
        "error": {"tag": "secret-provider-detail", "httpStatusCode": 404}
    });
    let foreign_result = json!({"id": "https://other.example", "text": "body"});
    for (name, statuses, results) in [
        (
            "status failure wins over usable text",
            vec![failed_status.clone()],
            vec![good_result.clone()],
        ),
        (
            "conflicting statuses",
            vec![good_status.clone(), failed_status],
            vec![good_result.clone()],
        ),
        ("missing status", vec![], vec![good_result.clone()]),
        (
            "wrong status ID",
            vec![json!({"id": "https://other.example", "status": "success"})],
            vec![good_result.clone()],
        ),
        ("missing results", vec![good_status.clone()], vec![]),
        (
            "missing text",
            vec![good_status.clone()],
            vec![json!({"id": url})],
        ),
        (
            "blank text",
            vec![good_status.clone()],
            vec![json!({"id": url, "text": "  "})],
        ),
        (
            "unrelated result",
            vec![good_status.clone()],
            vec![foreign_result.clone()],
        ),
        (
            "extra result",
            vec![good_status],
            vec![good_result, foreign_result],
        ),
    ] {
        let (config, calls, handle) = exa_server(
            StatusCode::OK,
            json!({"statuses": statuses, "results": results}).to_string(),
        )
        .await;
        let error = fetch_exa(request(), &config).await.unwrap_err();
        handle.abort();
        let receipt = serde_json::to_value(error.downcast_ref::<ToolError>().unwrap()).unwrap();
        assert_eq!(receipt["kind"], "provider_error", "{name}");
        assert_eq!(receipt["retryable"], false, "{name}");
        assert!(
            !receipt.to_string().contains("secret-provider-detail"),
            "{name}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{name}");
    }
}

#[tokio::test]
async fn exa_contents_rejects_http_errors_and_caps_json_without_leaking_details() {
    for (status, body_limit, retryable, message) in [
        (StatusCode::UNAUTHORIZED, 4096, false, "HTTP 401"),
        (StatusCode::TOO_MANY_REQUESTS, 4096, true, "HTTP 429"),
        (StatusCode::OK, 4, false, "max_response_bytes"),
    ] {
        let (mut config, calls, handle) =
            exa_server(status, "fixture-key and private response body".into()).await;
        config.fetch.max_response_bytes = body_limit;
        let error = fetch_exa(request(), &config).await.unwrap_err();
        handle.abort();
        let receipt = serde_json::to_value(error.downcast_ref::<ToolError>().unwrap()).unwrap();
        assert_eq!(receipt["kind"], "provider_error");
        assert_eq!(receipt["retryable"], retryable);
        let receipt = receipt.to_string();
        assert!(receipt.contains(message));
        assert!(!receipt.contains("fixture-key"));
        assert!(!receipt.contains("private response body"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn exa_contents_never_sends_unsupported_modes_or_policy_rejections() {
    let (mut config, calls, handle) = exa_server(StatusCode::OK, "{}".into()).await;
    for mode in [ExtractMode::Text, ExtractMode::Raw] {
        let mut input = request();
        input.extract_mode = mode;
        let error = fetch_exa(input, &config).await.unwrap_err();
        let receipt = serde_json::to_value(error.downcast_ref::<ToolError>().unwrap()).unwrap();
        assert_eq!(receipt["kind"], "invalid_argument");
    }
    config.fetch.allowed_hosts = vec!["example.com".into()];
    let error = fetch_exa(request(), &config).await.unwrap_err();
    let receipt = serde_json::to_value(error.downcast_ref::<ToolError>().unwrap()).unwrap();
    assert_eq!(receipt["kind"], "network_denied");
    handle.abort();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
fn firecrawl_provider(base_url: String) -> WebProviderConfig {
    WebProviderConfig {
        kind: super::super::WebProviderKind::Firecrawl,
        base_url: Some(base_url),
        engine_id: None,
        api_key: "fixture-key".into(),
        model_route: None,
        command: None,
        output: None,
        limits: Default::default(),
    }
}

async fn firecrawl_server(
    status: StatusCode,
    body: &str,
) -> (
    WebProviderConfig,
    Arc<std::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let body = body.to_owned();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let handler_seen = seen.clone();
    let router = Router::new().route(
        "/v2/scrape",
        post(move |request: Request| {
            let body = body.clone();
            let seen = handler_seen.clone();
            async move {
                assert_eq!(request.headers()["authorization"], "Bearer fixture-key");
                let bytes = axum::body::to_bytes(request.into_body(), 100_000)
                    .await
                    .unwrap();
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                seen.lock().unwrap().push(value);
                (status, body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (firecrawl_provider(base_url), seen, handle)
}

#[tokio::test]
async fn firecrawl_scrape_maps_markdown_and_provider_final_url() {
    let content = "# 页面\n正文";
    let (provider, seen, handle) = firecrawl_server(
        StatusCode::OK,
        &json!({
            "success": true,
            "data": {
                "markdown": content,
                "metadata": {"url": "https://93.184.216.34/final", "statusCode": 404}
            }
        })
        .to_string(),
    )
    .await;
    let config = WebFetchConfig {
        max_chars: 4,
        ..Default::default()
    };
    let mut input = request();
    input.max_chars = Some(100);
    let page = fetch(input, "fixture", &provider, &config).await.unwrap();
    handle.abort();
    // final_url follows the provider-reported redirect target; the page's
    // own statusCode never replaces the Firecrawl API HTTP status.
    assert_eq!(
        page.final_url.as_deref(),
        Some("https://93.184.216.34/final")
    );
    assert_eq!(page.source, "firecrawl_remote");
    assert_eq!(page.provider_id.as_deref(), Some("fixture"));
    assert_eq!(page.status_source, "provider_api");
    assert_eq!(page.status, 200);
    assert_eq!(page.content_type.as_deref(), Some("text/markdown"));
    assert_eq!(page.bytes_read, content.len());
    assert_eq!(
        page.sha256,
        format!("{:x}", Sha256::digest(content.as_bytes()))
    );
    assert!(page.truncated);
    assert!(page.text.starts_with("<external_content source="));
    assert!(page.text.contains("untrusted"));
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[json!({
            "url": "https://93.184.216.34/docs",
            "formats": ["markdown"],
            "onlyMainContent": true,
        })]
    );

    // Without a provider-reported final URL the receipt keeps final_url null.
    let (provider, _seen, handle) = firecrawl_server(
        StatusCode::OK,
        &json!({"success": true, "data": {"markdown": "plain body"}}).to_string(),
    )
    .await;
    let page = fetch(request(), "fixture", &provider, &WebFetchConfig::default())
        .await
        .unwrap();
    handle.abort();
    assert_eq!(page.final_url, None);
    assert!(!page.truncated);
}

#[tokio::test]
async fn firecrawl_scrape_rejects_provider_failures_without_fallback() {
    for (status, body) in [
        // HTTP 200 with the documented success=false error object.
        (
            StatusCode::OK,
            json!({"success": false, "error": "Invalid API key"}).to_string(),
        ),
        // HTTP 200 with a page-level metadata.error report.
        (
            StatusCode::OK,
            json!({
                "success": true,
                "data": {"markdown": "body", "metadata": {"error": "target failed"}}
            })
            .to_string(),
        ),
        // HTTP 200 with missing markdown.
        (
            StatusCode::OK,
            json!({"success": true, "data": {"metadata": {}}}).to_string(),
        ),
        // HTTP 200 with blank markdown.
        (
            StatusCode::OK,
            json!({"success": true, "data": {"markdown": "  "}}).to_string(),
        ),
        // HTTP 200 with no data at all.
        (StatusCode::OK, json!({"success": true}).to_string()),
        // Transport-level provider errors.
        (StatusCode::UNAUTHORIZED, "unauthorized".into()),
        (StatusCode::TOO_MANY_REQUESTS, "rate limited".into()),
        (StatusCode::BAD_GATEWAY, "bad gateway".into()),
        // Invalid JSON body.
        (StatusCode::OK, "{\"success\":".into()),
    ] {
        let (provider, seen, handle) = firecrawl_server(status, &body).await;
        let error = fetch(request(), "fixture", &provider, &WebFetchConfig::default())
            .await
            .unwrap_err();
        handle.abort();
        let tool_error = error.downcast_ref::<ToolError>().unwrap();
        let value = serde_json::to_value(tool_error).unwrap();
        // Rate limiting and upstream 5xx map to retryable; content and
        // authentication failures do not.
        let expected_retryable = matches!(
            status,
            StatusCode::TOO_MANY_REQUESTS | StatusCode::BAD_GATEWAY
        );
        assert_eq!(value["retryable"], expected_retryable, "{status}");
        assert_eq!(value["kind"], "provider_error", "{status}");
        assert!(!value.to_string().contains("fixture-key"), "{status}");
        // Exactly one /v2/scrape call happened and no local fallback retried it.
        assert_eq!(seen.lock().unwrap().len(), 1, "{status}");
    }
}

#[tokio::test]
async fn firecrawl_scrape_rejects_text_raw_and_missing_key_before_sending() {
    let (provider, seen, handle) = firecrawl_server(StatusCode::OK, "{}").await;
    let mut config = super::super::WebConfig::default();
    config.providers.insert("fixture".into(), provider.clone());
    for mode in [ExtractMode::Text, ExtractMode::Raw] {
        let mut input = request();
        input.extract_mode = mode;
        assert!(
            super::super::fetch::fetch_with_provider(input, Some("fixture"), &config)
                .await
                .is_err(),
            "{mode:?}"
        );
    }
    let mut provider = provider;
    provider.api_key.clear();
    config.providers.insert("fixture".into(), provider);
    assert!(fetch(
        request(),
        "fixture",
        &config.providers["fixture"],
        &config.fetch
    )
    .await
    .is_err());
    handle.abort();
    // Nothing left the process for any rejection.
    assert!(seen.lock().unwrap().is_empty());
}
