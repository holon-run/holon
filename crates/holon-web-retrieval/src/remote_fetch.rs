use anyhow::Result;
use chrono::Utc;
use reqwest::{header::HeaderValue, redirect::Policy, Client};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use url::Url;

use holon_domain::ToolError;
use holon_tools::helpers::truncate_text;

use super::{
    deepseek_tool_url,
    fetch::{external_content_wrapper, ExtractMode, WebFetchRequest, WebFetchResponse},
    firecrawl_scrape_url,
    policy::{policy_error, timeout, validate_fetch_url},
    tavily_extract_url, WebFetchConfig, WebProviderConfig, WebProviderKind,
};

#[derive(Deserialize)]
struct FetchPage {
    content: String,
}

/// Whether the local fetch policy keeps remote adapters from ever passing
/// the shared preflight: the service does not expose its redirect chain,
/// so custom host rules or a redirect budget cannot be enforced remotely.
pub(super) fn remote_fetch_policy_conflict(config: &WebFetchConfig) -> bool {
    !config.allowed_hosts.is_empty()
        || !config.denied_hosts.is_empty()
        || config.max_redirects != WebFetchConfig::default().max_redirects
}

/// Shared preflight for remote fetch adapters: enforce the site policy,
/// URL shape, and credential rules before any provider request leaves the
/// process. Mode and API-key gating stay with each adapter's caller.
pub(super) async fn preflight_remote_url(
    service: &str,
    request: &WebFetchRequest,
    config: &WebFetchConfig,
) -> Result<Url> {
    if !config.enabled {
        return Err(policy_error(
            "web_fetch_disabled",
            "WebFetch is disabled by configuration",
            json!({}),
            "enable web.fetch.enabled",
        ));
    }
    // The service does not expose its redirect chain. Never claim to enforce
    // custom host rules or redirect budgets on a remotely fetched page.
    if remote_fetch_policy_conflict(config) {
        return Err(policy_error(
            "network_denied",
            format!("{service} cannot enforce custom host rules or redirect limits"),
            json!({}),
            "omit provider to enforce local web.fetch policy",
        ));
    }
    let url = Url::parse(request.url.trim()).map_err(|_| {
        policy_error(
            "invalid_url",
            format!("{service} requires an absolute HTTP(S) URL"),
            json!({}),
            "provide a public HTTP(S) URL without credentials",
        )
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(policy_error(
            "invalid_url",
            format!("{service} does not accept URLs containing credentials"),
            json!({}),
            "provide a public URL without credentials",
        ));
    }
    if url.as_str().chars().count() > 8192 {
        return Err(policy_error(
            "invalid_url",
            format!("{service} URLs must contain at most 8192 characters"),
            json!({}),
            "provide a shorter URL",
        ));
    }
    validate_fetch_url(&url, config).await?;
    Ok(url)
}

/// Shared API-key gate for remote fetch adapters.
pub(super) fn require_remote_api_key(service: &str, provider: &WebProviderConfig) -> Result<()> {
    if provider.api_key.trim().is_empty() {
        return Err(policy_error(
            "provider_unavailable",
            format!("{service} requires an API key"),
            json!({}),
            "set credential_profile on the web provider to an API key profile",
        ));
    }
    Ok(())
}

pub(super) async fn fetch(
    request: WebFetchRequest,
    provider_id: &str,
    provider: &WebProviderConfig,
    config: &WebFetchConfig,
) -> Result<WebFetchResponse> {
    match provider.kind {
        WebProviderKind::DeepSeek => deepseek_fetch(request, provider_id, provider, config).await,
        WebProviderKind::Tavily => {
            tavily_extract_fetch(request, provider_id, provider, config).await
        }
        WebProviderKind::Exa => exa_contents_fetch(request, provider_id, provider, config).await,
        WebProviderKind::Firecrawl => {
            firecrawl_scrape_fetch(request, provider_id, provider, config).await
        }
        kind => Err(policy_error(
            "provider_unavailable",
            format!(
                "WebFetch remote extraction is not available for provider kind {}",
                kind.as_str()
            ),
            json!({ "provider": provider_id, "kind": kind.as_str() }),
            "select a provider from ListWebFetchProviders or omit provider for local HTTP",
        )),
    }
}

async fn deepseek_fetch(
    request: WebFetchRequest,
    provider_id: &str,
    provider: &WebProviderConfig,
    config: &WebFetchConfig,
) -> Result<WebFetchResponse> {
    let url = preflight_remote_url("DeepSeek Fetch", &request, config).await?;
    require_remote_api_key("DeepSeek Fetch", provider)?;
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
        provider_id: Some(provider_id.to_string()),
        status_source: "provider_api",
        text: external_content_wrapper(&url, &text),
        summary_text: format!("DeepSeek Fetch {} {url}", status.as_u16()),
    })
}

fn remote_error(message: &str, retryable: bool) -> anyhow::Error {
    ToolError::new("provider_error", message)
        .with_retryable(retryable)
        .with_recovery_hint("check the DeepSeek credential, balance, or service availability")
        .into()
}

#[derive(Deserialize)]
struct TavilyExtractEntry {
    url: Option<String>,
    raw_content: Option<String>,
}

#[derive(Deserialize)]
struct TavilyExtractResponse {
    #[serde(default)]
    results: Vec<TavilyExtractEntry>,
    #[serde(default)]
    failed_results: Vec<TavilyExtractEntry>,
}

/// Tavily Extract maps `auto`/`text` to the documented body `format` values
/// and sends exactly one URL per request; `raw` never leaves the process.
async fn tavily_extract_fetch(
    request: WebFetchRequest,
    provider_id: &str,
    provider: &WebProviderConfig,
    config: &WebFetchConfig,
) -> Result<WebFetchResponse> {
    let url = preflight_remote_url("Tavily Extract", &request, config).await?;
    require_remote_api_key("Tavily Extract", provider)?;
    let (format, content_type) = match request.extract_mode {
        ExtractMode::Auto => ("markdown", "text/markdown"),
        ExtractMode::Text => ("text", "text/plain"),
        ExtractMode::Raw => {
            return Err(policy_error(
                "invalid_argument",
                "Tavily Extract does not send raw page requests",
                json!({}),
                "use extract_mode auto or text, or omit provider for local HTTP",
            ));
        }
    };
    let endpoint = tavily_extract_url(provider)?;
    let client = Client::builder()
        .timeout(timeout(config))
        .redirect(Policy::none())
        .build()?;
    let mut response = client
        .post(endpoint)
        .bearer_auth(&provider.api_key)
        .json(&json!({ "urls": [url.as_str()], "format": format }))
        .send()
        .await
        .map_err(|_| tavily_error("Tavily Extract request failed", true))?;
    let status = response.status();
    if !status.is_success() {
        return Err(tavily_error(
            &format!("Tavily Extract returned HTTP {}", status.as_u16()),
            status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error(),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| tavily_error("Tavily Extract response read failed", true))?
    {
        if chunk.len() > config.max_response_bytes.saturating_sub(bytes.len()) {
            return Err(tavily_error(
                "Tavily Extract response exceeds web.fetch.max_response_bytes",
                false,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let payload: TavilyExtractResponse = serde_json::from_slice(&bytes)
        .map_err(|_| tavily_error("Tavily Extract returned invalid JSON", false))?;
    // HTTP 200 does not mean the URL was extracted: check failed_results
    // first; an entry matching the requested URL fails the whole request.
    let requested = url.as_str();
    if payload
        .failed_results
        .iter()
        .any(|entry| entry.url.as_deref() == Some(requested))
    {
        return Err(tavily_error(
            "Tavily Extract reported the requested URL as failed",
            false,
        ));
    }
    let Some(entry) = payload.results.iter().find(|entry| {
        entry.url.as_deref() == Some(requested)
            && entry
                .raw_content
                .as_deref()
                .is_some_and(|content| !content.trim().is_empty())
    }) else {
        return Err(tavily_error(
            "Tavily Extract returned no non-empty content for the requested URL",
            false,
        ));
    };
    let content = entry.raw_content.as_deref().unwrap_or_default();
    let max_chars = request
        .max_chars
        .unwrap_or(config.max_chars)
        .min(config.max_chars);
    let truncated = content.chars().count() > max_chars;
    let text = truncate_text(content, max_chars);
    Ok(WebFetchResponse {
        url: url.to_string(),
        final_url: None,
        status: status.as_u16(),
        content_type: Some(content_type.into()),
        bytes_read: content.len(),
        truncated,
        sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
        fetched_at: Utc::now().to_rfc3339(),
        source: "tavily_remote",
        provider_id: Some(provider_id.to_string()),
        status_source: "provider_api",
        text: external_content_wrapper(&url, &text),
        summary_text: format!("Tavily Extract {} {url}", status.as_u16()),
    })
}

fn tavily_error(message: &str, retryable: bool) -> anyhow::Error {
    ToolError::new("provider_error", message)
        .with_retryable(retryable)
        .with_recovery_hint("check the Tavily API key, subscription, or service availability")
        .into()
}

pub(super) fn exa_contents_url(provider: &WebProviderConfig) -> Result<Url> {
    let invalid_base = || {
        policy_error(
            "provider_unavailable",
            "Exa Contents requires an HTTP(S) base URL without credentials, query, or fragment",
            json!({}),
            "check the configured Exa base URL",
        )
    };
    let base = provider.base_url.as_deref().unwrap_or("https://api.exa.ai");
    let mut url = Url::parse(base).map_err(|_| invalid_base())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid_base());
    }
    url.set_path(&format!("{}/contents", url.path().trim_end_matches('/')));
    Ok(url)
}

pub(super) fn exa_api_key(provider: &WebProviderConfig) -> Result<HeaderValue> {
    require_remote_api_key("Exa Contents", provider)?;
    let mut key = HeaderValue::from_str(&provider.api_key).map_err(|_| {
        policy_error(
            "provider_unavailable",
            "Exa Contents API key is not a valid HTTP header value",
            json!({}),
            "check the configured Exa credential profile",
        )
    })?;
    key.set_sensitive(true);
    Ok(key)
}

#[derive(Deserialize)]
struct ExaContentsStatus {
    id: String,
    status: String,
}

#[derive(Deserialize)]
struct ExaContentsEntry {
    id: String,
    url: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize)]
struct ExaContentsResponse {
    #[serde(default)]
    results: Vec<ExaContentsEntry>,
    #[serde(default)]
    statuses: Vec<ExaContentsStatus>,
}

async fn exa_contents_fetch(
    request: WebFetchRequest,
    provider_id: &str,
    provider: &WebProviderConfig,
    config: &WebFetchConfig,
) -> Result<WebFetchResponse> {
    let url = preflight_remote_url("Exa Contents", &request, config).await?;
    let key = exa_api_key(provider)?;
    let endpoint = exa_contents_url(provider)?;
    let client = Client::builder()
        .timeout(timeout(config))
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .build()?;
    let mut response = client
        .post(endpoint)
        .header("x-api-key", key)
        .json(&json!({ "ids": [url.as_str()], "text": true }))
        .send()
        .await
        .map_err(|_| exa_error("Exa Contents request failed", true))?;
    let status = response.status();
    if !status.is_success() {
        return Err(exa_error(
            &format!("Exa Contents returned HTTP {}", status.as_u16()),
            status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error(),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| exa_error("Exa Contents response read failed", true))?
    {
        if chunk.len() > config.max_response_bytes.saturating_sub(bytes.len()) {
            return Err(exa_error(
                "Exa Contents response exceeds web.fetch.max_response_bytes",
                false,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }

    let payload: ExaContentsResponse = serde_json::from_slice(&bytes)
        .map_err(|_| exa_error("Exa Contents returned invalid JSON", false))?;
    let requested = url.as_str();
    // HTTP success does not establish extraction success for the requested ID.
    if !matches!(payload.statuses.as_slice(), [entry] if entry.id == requested && entry.status == "success")
    {
        return Err(exa_error(
            "Exa Contents did not report a unique successful status for the requested URL",
            false,
        ));
    }
    // Exa may report a different page URL. A matching result ID still binds
    // the text to this request, but never establishes a final redirect target.
    let [entry] = payload.results.as_slice() else {
        return Err(exa_error(
            "Exa Contents did not return exactly one result",
            false,
        ));
    };
    if entry.id != requested && entry.url.as_deref() != Some(requested) {
        return Err(exa_error(
            "Exa Contents returned an unexpected result",
            false,
        ));
    }
    let content = entry
        .text
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| exa_error("Exa Contents returned no non-empty text", false))?;
    let max_chars = request
        .max_chars
        .unwrap_or(config.max_chars)
        .min(config.max_chars);
    let truncated = content.chars().count() > max_chars;
    let text = truncate_text(content, max_chars);
    Ok(WebFetchResponse {
        url: url.to_string(),
        final_url: None,
        status: status.as_u16(),
        content_type: Some("text/plain".into()),
        bytes_read: content.len(),
        truncated,
        sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
        fetched_at: Utc::now().to_rfc3339(),
        source: "exa_remote",
        provider_id: Some(provider_id.to_string()),
        status_source: "provider_api",
        text: external_content_wrapper(&url, &text),
        summary_text: format!("Exa Contents {} {url}", status.as_u16()),
    })
}

fn exa_error(message: &str, retryable: bool) -> anyhow::Error {
    ToolError::new("provider_error", message)
        .with_retryable(retryable)
        .with_recovery_hint("check the Exa API key, subscription, or service availability")
        .into()
}

#[derive(Deserialize)]
struct FirecrawlScrapeMetadata {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct FirecrawlScrapeData {
    #[serde(default)]
    markdown: Option<String>,
    #[serde(default)]
    metadata: Option<FirecrawlScrapeMetadata>,
}

#[derive(Deserialize)]
struct FirecrawlScrapeResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    data: Option<FirecrawlScrapeData>,
}

/// Firecrawl Scrape v2 sends exactly one `POST /v2/scrape` per WebFetch call
/// with `formats: ["markdown"]` and `onlyMainContent: true`; the first version
/// exposes only `auto` extraction reading `data.markdown`.
async fn firecrawl_scrape_fetch(
    request: WebFetchRequest,
    provider_id: &str,
    provider: &WebProviderConfig,
    config: &WebFetchConfig,
) -> Result<WebFetchResponse> {
    let url = preflight_remote_url("Firecrawl Scrape", &request, config).await?;
    require_remote_api_key("Firecrawl Scrape", provider)?;
    let endpoint = firecrawl_scrape_url(provider)?;
    let client = Client::builder()
        .timeout(timeout(config))
        .redirect(Policy::none())
        .build()?;
    let mut response = client
        .post(endpoint)
        .bearer_auth(&provider.api_key)
        .json(&json!({
            "url": url.as_str(),
            "formats": ["markdown"],
            "onlyMainContent": true,
        }))
        .send()
        .await
        .map_err(|_| firecrawl_error("Firecrawl Scrape request failed", true))?;
    let status = response.status();
    if !status.is_success() {
        return Err(firecrawl_error(
            &format!("Firecrawl Scrape returned HTTP {}", status.as_u16()),
            status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error(),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| firecrawl_error("Firecrawl Scrape response read failed", true))?
    {
        if chunk.len() > config.max_response_bytes.saturating_sub(bytes.len()) {
            return Err(firecrawl_error(
                "Firecrawl Scrape response exceeds web.fetch.max_response_bytes",
                false,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let payload: FirecrawlScrapeResponse = serde_json::from_slice(&bytes)
        .map_err(|_| firecrawl_error("Firecrawl Scrape returned invalid JSON", false))?;
    // HTTP 200 does not mean the page was scraped: `success=false` and a
    // page-level `metadata.error` are provider failures regardless of the
    // API status.
    if !payload.success {
        return Err(firecrawl_error(
            "Firecrawl Scrape returned success=false",
            false,
        ));
    }
    let data = payload
        .data
        .as_ref()
        .ok_or_else(|| firecrawl_error("Firecrawl Scrape returned no scrape data", false))?;
    if let Some(error) = data
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.error.as_deref())
    {
        if !error.trim().is_empty() {
            return Err(firecrawl_error(
                "Firecrawl Scrape reported the page as failed",
                false,
            ));
        }
    }
    let Some(content) = data
        .markdown
        .as_deref()
        .filter(|content| !content.trim().is_empty())
    else {
        return Err(firecrawl_error(
            "Firecrawl Scrape returned no non-empty markdown for the requested URL",
            false,
        ));
    };
    // metadata.url is the provider-observed final URL after redirects; keep
    // final_url null when it is missing or not a parseable absolute URL.
    let final_url = data
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.url.as_deref())
        .and_then(|value| Url::parse(value.trim()).ok())
        .map(|value| value.to_string());
    let max_chars = request
        .max_chars
        .unwrap_or(config.max_chars)
        .min(config.max_chars);
    let truncated = content.chars().count() > max_chars;
    let text = truncate_text(content, max_chars);
    Ok(WebFetchResponse {
        url: url.to_string(),
        final_url,
        status: status.as_u16(),
        content_type: Some("text/markdown".into()),
        bytes_read: content.len(),
        truncated,
        sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
        fetched_at: Utc::now().to_rfc3339(),
        source: "firecrawl_remote",
        provider_id: Some(provider_id.to_string()),
        status_source: "provider_api",
        text: external_content_wrapper(&url, &text),
        summary_text: format!("Firecrawl Scrape {} {url}", status.as_u16()),
    })
}

fn firecrawl_error(message: &str, retryable: bool) -> anyhow::Error {
    ToolError::new("provider_error", message)
        .with_retryable(retryable)
        .with_recovery_hint("check the Firecrawl API key, plan credits, or service availability")
        .into()
}

#[cfg(test)]
mod tests;
