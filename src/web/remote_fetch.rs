use anyhow::Result;
use chrono::Utc;
use reqwest::{redirect::Policy, Client};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use url::Url;

use crate::tool::{helpers::truncate_text, ToolError};

use super::{
    deepseek_tool_url,
    fetch::{ExtractMode, WebFetchRequest, WebFetchResponse},
    policy::{policy_error, timeout, validate_fetch_url},
    WebFetchConfig, WebProviderConfig,
};

#[derive(Deserialize)]
struct FetchPage {
    content: String,
}

pub(super) async fn fetch(
    request: WebFetchRequest,
    provider: &WebProviderConfig,
    config: &WebFetchConfig,
) -> Result<WebFetchResponse> {
    if !config.enabled {
        return Err(policy_error(
            "web_fetch_disabled",
            "WebFetch is disabled by configuration",
            json!({}),
            "enable web.fetch.enabled",
        ));
    }
    if !matches!(request.extract_mode, ExtractMode::Auto) {
        return Err(policy_error(
            "invalid_argument",
            "DeepSeek Fetch returns Markdown and only supports extract_mode=auto",
            json!({}),
            "omit provider to use local text/raw extraction",
        ));
    }
    // The service does not expose its redirect chain. Never claim to enforce
    // custom host rules or redirect budgets on a remotely fetched page.
    if !config.allowed_hosts.is_empty()
        || !config.denied_hosts.is_empty()
        || config.max_redirects != WebFetchConfig::default().max_redirects
    {
        return Err(policy_error(
            "network_denied",
            "DeepSeek Fetch cannot enforce custom host rules or redirect limits",
            json!({}),
            "omit provider to enforce local web.fetch policy",
        ));
    }
    let url = Url::parse(request.url.trim()).map_err(|_| {
        policy_error(
            "invalid_url",
            "DeepSeek Fetch requires an absolute HTTP(S) URL",
            json!({}),
            "provide a public HTTP(S) URL without credentials",
        )
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(policy_error(
            "invalid_url",
            "DeepSeek Fetch does not accept URLs containing credentials",
            json!({}),
            "provide a public URL without credentials",
        ));
    }
    if url.as_str().chars().count() > 8192 {
        return Err(policy_error(
            "invalid_url",
            "DeepSeek Fetch URLs must contain at most 8192 characters",
            json!({}),
            "provide a shorter URL",
        ));
    }
    validate_fetch_url(&url, config).await?;
    if provider.api_key.trim().is_empty() {
        return Err(policy_error(
            "provider_unavailable",
            "DeepSeek Fetch requires an API key",
            json!({}),
            "set credential_profile on the web provider to an API key profile",
        ));
    }
    let client = Client::builder()
        .timeout(timeout(config))
        .redirect(Policy::none())
        .build()?;
    let mut response = client
        .post(deepseek_tool_url(provider, "fetch")?)
        .bearer_auth(&provider.api_key)
        .json(&json!({ "url": url.as_str() }))
        .send()
        .await
        .map_err(|_| remote_error("DeepSeek Fetch request failed", true))?;
    let status = response.status();
    if !status.is_success() {
        return Err(remote_error(
            &format!("DeepSeek Fetch returned HTTP {}", status.as_u16()),
            status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error(),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| remote_error("DeepSeek Fetch response read failed", true))?
    {
        if chunk.len() > config.max_response_bytes.saturating_sub(bytes.len()) {
            return Err(remote_error(
                "DeepSeek Fetch response exceeds web.fetch.max_response_bytes",
                false,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let page: FetchPage = serde_json::from_slice(&bytes)
        .map_err(|_| remote_error("DeepSeek Fetch returned invalid page JSON", false))?;
    if page.content.trim().is_empty() {
        return Err(remote_error(
            "DeepSeek Fetch returned no page content",
            false,
        ));
    }
    let max_chars = request
        .max_chars
        .unwrap_or(config.max_chars)
        .min(config.max_chars);
    let truncated = page.content.chars().count() > max_chars;
    let text = truncate_text(&page.content, max_chars);
    Ok(WebFetchResponse {
        url: url.to_string(),
        final_url: None,
        status: status.as_u16(),
        content_type: Some("text/markdown".into()),
        bytes_read: page.content.len(),
        truncated,
        sha256: format!("{:x}", Sha256::digest(page.content.as_bytes())),
        fetched_at: Utc::now().to_rfc3339(),
        source: "deepseek_remote",
        text: format!("<external_content source=\"{url}\">\n{text}\n</external_content>"),
        summary_text: format!("DeepSeek Fetch {} {url}", status.as_u16()),
    })
}

fn remote_error(message: &str, retryable: bool) -> anyhow::Error {
    ToolError::new("provider_error", message)
        .with_retryable(retryable)
        .with_recovery_hint("check the DeepSeek credential, balance, or service availability")
        .into()
}

#[cfg(test)]
mod tests {
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
}
