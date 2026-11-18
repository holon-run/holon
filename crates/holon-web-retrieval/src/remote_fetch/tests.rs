use super::*;
use axum::{extract::Request, http::StatusCode, routing::post, Router};
use serde_json::Value;

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
) -> (WebProviderConfig, tokio::task::JoinHandle<()>) {
    let body = body.to_owned();
    let router = Router::new().route(
        "/v1/tools/fetch",
        post(move |request: Request| {
            let body = body.clone();
            async move {
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
            api_key: "fixture-key".into(),
            command: None,
            output: None,
            limits: Default::default(),
        },
        handle,
    )
}

#[tokio::test]
async fn deepseek_fetch_maps_markdown_provenance_and_unicode_limit() {
    let content = "# 文档\n内容";
    let (provider, handle) = server(
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
    let page = fetch(request, &provider, &config).await.unwrap();
    handle.abort();
    assert_eq!(page.final_url, None);
    assert_eq!(page.source, "deepseek_remote");
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
}

#[tokio::test]
async fn deepseek_fetch_rejects_unsafe_or_unenforceable_requests() {
    let (mut provider, handle) = server(StatusCode::OK, "{}").await;
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
        let error = fetch(input, &provider, &config).await.unwrap_err();
        assert!(!error.to_string().contains("secret"));
    }
    let mut input = request();
    input.url = format!("https://93.184.216.34/{}", "a".repeat(8192));
    assert!(fetch(input, &provider, &config).await.is_err());
    for mode in [ExtractMode::Raw, ExtractMode::Text] {
        let mut input = request();
        input.extract_mode = mode;
        assert!(fetch(input, &provider, &config).await.is_err());
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
        assert!(fetch(request(), &provider, &config).await.is_err());
    }
    provider.api_key.clear();
    assert!(fetch(request(), &provider, &config).await.is_err());
    handle.abort();
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
        let (provider, handle) = server(status, body).await;
        let error = fetch(request(), &provider, &WebFetchConfig::default())
            .await
            .unwrap_err();
        handle.abort();
        let error = error.downcast_ref::<ToolError>().unwrap();
        let value = serde_json::to_value(error).unwrap();
        assert_eq!(value["retryable"], retryable);
        assert!(!value.to_string().contains("fixture-key"));
    }
    let (provider, handle) = server(StatusCode::OK, "{\"content\":\"too big\"}").await;
    let config = WebFetchConfig {
        max_response_bytes: 8,
        ..Default::default()
    };
    assert!(fetch(request(), &provider, &config)
        .await
        .unwrap_err()
        .to_string()
        .contains("max_response_bytes"));
    handle.abort();
}

#[tokio::test]
async fn fetch_provider_selection_rejects_unknown_or_unsupported_kind() {
    let (mut provider, handle) = server(StatusCode::OK, "{}").await;
    let mut config = super::super::WebConfig::default();
    assert!(
        super::super::fetch::fetch_with_provider(request(), Some("absent"), &config)
            .await
            .is_err()
    );
    provider.kind = super::super::WebProviderKind::Brave;
    config.providers.insert("brave".into(), provider);
    assert!(
        super::super::fetch::fetch_with_provider(request(), Some("brave"), &config)
            .await
            .is_err()
    );
    handle.abort();
}
