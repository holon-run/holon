use std::{
    collections::{BTreeMap, HashSet},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{config::XSearchRuntimeConfig, provider::OpenAiBearerAuth};

#[derive(Debug, Clone)]
pub struct XSearchRequest {
    pub query: String,
    pub allowed_x_handles: Vec<String>,
    pub excluded_x_handles: Vec<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct XSearchResponse {
    pub text: String,
    pub citations: Vec<XSearchCitation>,
    pub provider: String,
    pub backend: String,
    pub model: String,
    pub diagnostics: XSearchDiagnostics,
    pub summary_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XSearchCitation {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_index: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_index: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct XSearchDiagnostics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<String>,
    pub latency_ms: u64,
    pub hosted_item_types: BTreeMap<String, usize>,
}

pub async fn search(
    request: XSearchRequest,
    config: &XSearchRuntimeConfig,
) -> Result<XSearchResponse> {
    let body = build_request_body(&request, &config.model);
    let client = Client::builder()
        .timeout(config.timeout)
        .build()
        .context("failed to build x_search HTTP client")?;
    let auth = OpenAiBearerAuth::from_runtime_config(&config.provider, client.clone())?;
    let mut authorization = auth
        .resolve_authorization_header()
        .await?
        .ok_or_else(|| anyhow!("x_search_unavailable: xAI credential is not available"))?;
    let started = Instant::now();
    let endpoint = format!(
        "{}/responses",
        config.provider.base_url.trim_end_matches('/')
    );
    let mut response = send_request(&client, &endpoint, &authorization, &body).await?;
    if response.status().as_u16() == 401 {
        if let Some(refreshed) = auth.refresh_authorization_header().await? {
            authorization = refreshed;
            response = send_request(&client, &endpoint, &authorization, &body).await?;
        }
    }
    let provider_request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let status = response.status();
    let response_body: Value = response
        .json()
        .await
        .context("x_search response was not valid JSON")?;
    if !status.is_success() {
        return Err(anyhow!(
            "x_search provider returned HTTP {}: {}",
            status.as_u16(),
            provider_error_message(&response_body)
        ));
    }

    let (text, citations, hosted_item_types) = parse_response(&response_body)?;
    Ok(XSearchResponse {
        summary_text: format!(
            "xAI X search returned {} citation{}",
            citations.len(),
            if citations.len() == 1 { "" } else { "s" }
        ),
        text,
        citations,
        provider: "xai".into(),
        backend: "x_search".into(),
        model: config.model.clone(),
        diagnostics: XSearchDiagnostics {
            provider_request_id,
            latency_ms: duration_millis(started.elapsed()),
            hosted_item_types,
        },
    })
}

async fn send_request(
    client: &Client,
    endpoint: &str,
    authorization: &str,
    body: &Value,
) -> Result<reqwest::Response> {
    client
        .post(endpoint)
        .header("authorization", authorization)
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await
        .context("x_search request failed")
}

pub(crate) fn build_request_body(request: &XSearchRequest, model: &str) -> Value {
    let mut tool = json!({ "type": "x_search" });
    if !request.allowed_x_handles.is_empty() {
        tool["allowed_x_handles"] = json!(request.allowed_x_handles);
    }
    if !request.excluded_x_handles.is_empty() {
        tool["excluded_x_handles"] = json!(request.excluded_x_handles);
    }
    if let Some(from_date) = request.from_date.as_deref() {
        tool["from_date"] = json!(from_date);
    }
    if let Some(to_date) = request.to_date.as_deref() {
        tool["to_date"] = json!(to_date);
    }
    json!({
        "model": model,
        "input": request.query,
        "tools": [tool],
        "tool_choice": "auto",
        "store": false,
    })
}

pub(crate) fn parse_response(
    response: &Value,
) -> Result<(String, Vec<XSearchCitation>, BTreeMap<String, usize>)> {
    let mut text = String::new();
    let mut citations = Vec::new();
    let mut seen_urls = HashSet::new();
    let mut hosted_item_types = BTreeMap::new();
    for item in response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let item_type = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if item_type != "message" {
            *hosted_item_types.entry(item_type.to_string()).or_default() += 1;
            continue;
        }
        for content in item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if content.get("type").and_then(Value::as_str) != Some("output_text") {
                continue;
            }
            if let Some(value) = content.get("text").and_then(Value::as_str) {
                text.push_str(value);
            }
            for annotation in content
                .get("annotations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if annotation.get("type").and_then(Value::as_str) != Some("url_citation") {
                    continue;
                }
                let Some(url) = annotation.get("url").and_then(Value::as_str) else {
                    continue;
                };
                if seen_urls.insert(url.to_string()) {
                    citations.push(XSearchCitation {
                        url: url.to_string(),
                        title: annotation
                            .get("title")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                        start_index: annotation.get("start_index").and_then(Value::as_u64),
                        end_index: annotation.get("end_index").and_then(Value::as_u64),
                    });
                }
            }
        }
    }
    if text.trim().is_empty() {
        return Err(anyhow!(
            "x_search response did not contain final output text"
        ));
    }
    Ok((text, citations, hosted_item_types))
}

fn provider_error_message(body: &Value) -> String {
    body.pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| body.get("message").and_then(Value::as_str))
        .unwrap_or("unknown provider error")
        .to_string()
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AnthropicContextManagementConfig, CredentialKind, CredentialSource, ProviderAuthConfig,
        ProviderEndpointId, ProviderId, ProviderRuntimeConfig, ProviderTransportKind,
    };
    use tokio::{io::AsyncReadExt, io::AsyncWriteExt, net::TcpListener};

    fn xai_oauth_config(base_url: String, credential: String) -> XSearchRuntimeConfig {
        let xai = ProviderId::parse("xai").unwrap();
        XSearchRuntimeConfig {
            provider: ProviderRuntimeConfig {
                id: xai.clone(),
                route_provider: xai,
                route_endpoint: ProviderEndpointId::default_endpoint(),
                transport: ProviderTransportKind::OpenAiResponses,
                base_url,
                auth: ProviderAuthConfig {
                    source: CredentialSource::AuthProfile,
                    kind: CredentialKind::OAuth,
                    env: None,
                    profile: Some("xai".into()),
                    external: None,
                },
                credential: Some(credential),
                credential_store_path: None,
                codex_home: None,
                originator: None,
                reasoning_effort: Some("medium".into()),
                context_management: AnthropicContextManagementConfig::default(),
                builtin_web_search: None,
            },
            model: "grok-test".into(),
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn request_uses_only_x_search_and_disables_storage() {
        let body = build_request_body(
            &XSearchRequest {
                query: "Holon updates".into(),
                allowed_x_handles: vec!["holon_run".into()],
                excluded_x_handles: vec![],
                from_date: Some("2026-07-01".into()),
                to_date: None,
            },
            "grok-4-1-fast",
        );

        assert_eq!(body["model"], "grok-4-1-fast");
        assert_eq!(body["input"], "Holon updates");
        assert_eq!(body["store"], false);
        assert_eq!(body["tools"][0]["type"], "x_search");
        assert_eq!(body["tools"][0]["allowed_x_handles"][0], "holon_run");
        assert_eq!(body["tools"][0]["from_date"], "2026-07-01");
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn response_extracts_text_and_deduplicates_citations() {
        let response = json!({
            "output": [
                {"type": "x_search_call", "id": "search_1", "status": "completed"},
                {"type": "x_keyword_search", "arguments": {"query": "internal"}},
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "Result",
                        "annotations": [
                            {"type": "url_citation", "url": "https://x.com/a/status/1", "title": "A", "start_index": 0, "end_index": 6},
                            {"type": "url_citation", "url": "https://x.com/a/status/1", "title": "A duplicate"}
                        ]
                    }]
                }
            ]
        });

        let (text, citations, diagnostics) = parse_response(&response).unwrap();
        assert_eq!(text, "Result");
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0].url, "https://x.com/a/status/1");
        assert_eq!(diagnostics.get("x_search_call"), Some(&1));
        assert_eq!(diagnostics.get("x_keyword_search"), Some(&1));
    }

    #[test]
    fn response_requires_final_text() {
        let error = parse_response(&json!({
            "output": [{"type": "x_search_call", "status": "completed"}]
        }))
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("did not contain final output text"));
    }

    #[tokio::test]
    async fn search_lowers_xai_oauth_profile_to_access_token() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let size = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer oauth-access-token\r\n"),
                "request did not contain the OAuth access token: {request}"
            );
            assert!(!request.contains("\"access_token\""));

            let body = json!({
                "output": [{
                    "type": "message",
                    "content": [{
                        "type": "output_text",
                        "text": "OAuth search result",
                        "annotations": []
                    }]
                }]
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let credential = json!({
            "tokens": {
                "access_token": "oauth-access-token",
                "refresh_token": "refresh-token"
            }
        })
        .to_string();
        let config = xai_oauth_config(format!("http://{address}"), credential);

        let response = search(
            XSearchRequest {
                query: "Holon OAuth".into(),
                allowed_x_handles: Vec::new(),
                excluded_x_handles: Vec::new(),
                from_date: None,
                to_date: None,
            },
            &config,
        )
        .await
        .unwrap();

        server.await.unwrap();
        assert_eq!(response.text, "OAuth search result");
    }

    #[tokio::test]
    async fn search_does_not_refresh_oauth_profile_on_forbidden() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            stream.read(&mut request).await.unwrap();

            let body = json!({"error": {"message": "insufficient scope"}}).to_string();
            let response = format!(
                "HTTP/1.1 403 Forbidden\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let credential = json!({
            "tokens": {
                "access_token": "oauth-access-token",
                "refresh_token": "refresh-token"
            }
        })
        .to_string();
        let config = xai_oauth_config(format!("http://{address}"), credential);

        let error = search(
            XSearchRequest {
                query: "Holon OAuth".into(),
                allowed_x_handles: Vec::new(),
                excluded_x_handles: Vec::new(),
                from_date: None,
                to_date: None,
            },
            &config,
        )
        .await
        .unwrap_err();

        server.await.unwrap();
        assert_eq!(
            error.to_string(),
            "x_search provider returned HTTP 403: insufficient scope"
        );
    }
}
