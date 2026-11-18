use anyhow::Result;
use chrono::Utc;
use reqwest::{redirect::Policy, Client};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use url::Url;

use holon_domain::ToolError;
use holon_tools::helpers::truncate_text;

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
mod tests;
