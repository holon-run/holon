use anyhow::{anyhow, Result};
use chrono::Utc;
use reqwest::{Client, StatusCode};
use serde::Serialize;
use serde_json::json;
use url::{form_urlencoded, Url};

use crate::{
    tool::ToolError,
    web::{policy::timeout, WebConfig, WebFetchConfig, WebProviderConfig, WebProviderKind},
};

const SEARCH_RESPONSE_BYTES: usize = 1_000_000;

#[derive(Debug, Clone)]
pub struct WebSearchRequest {
    pub query: String,
    pub max_results: Option<usize>,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebSearchResponse {
    pub query: String,
    pub provider: String,
    pub results: Vec<WebSearchResult>,
    pub citations: Vec<WebCitation>,
    pub fetched_at: String,
    pub summary_text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
    pub source: String,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebCitation {
    pub title: String,
    pub url: String,
}

pub async fn search(request: WebSearchRequest, config: &WebConfig) -> Result<WebSearchResponse> {
    if !config.search.enabled {
        return Err(search_error(
            "provider_unavailable",
            "WebSearch is disabled by configuration",
            "disabled",
            "enable web.search.enabled or use WebFetch with a known URL",
        ));
    }

    let max_results = normalize_max_results(request.max_results, config.search.max_results)?;
    let provider = request
        .provider
        .as_deref()
        .unwrap_or(config.search.provider.as_str());

    let results = match provider {
        "auto" => search_auto(&request.query, max_results, config).await?,
        "duckduckgo" => duckduckgo_search(&request.query, max_results, &config.fetch).await?,
        provider_id => {
            let provider_config = config.providers.get(provider_id).ok_or_else(|| {
                search_error(
                    "provider_unavailable",
                    format!("WebSearch provider `{provider_id}` is not configured"),
                    provider_id,
                    "configure web.providers or use provider=duckduckgo",
                )
            })?;
            match provider_config.kind {
                WebProviderKind::Searxng => {
                    searxng_search(
                        &request.query,
                        max_results,
                        provider_id,
                        provider_config,
                        &config.fetch,
                    )
                    .await?
                }
                WebProviderKind::DuckDuckGo => {
                    duckduckgo_search(&request.query, max_results, &config.fetch).await?
                }
                kind => {
                    return Err(search_error(
                        "provider_unavailable",
                        format!("WebSearch provider kind `{kind:?}` is reserved for API-backed or native provider support"),
                        provider_id,
                        "configure a duckduckgo or searxng provider for this Holon version",
                    ));
                }
            }
        }
    };

    let citations = results
        .iter()
        .map(|result| WebCitation {
            title: result.title.clone(),
            url: result.url.clone(),
        })
        .collect::<Vec<_>>();
    Ok(WebSearchResponse {
        query: request.query,
        provider: results
            .first()
            .map(|result| result.source.clone())
            .unwrap_or_else(|| provider.to_string()),
        summary_text: format!("{} web results", results.len()),
        results,
        citations,
        fetched_at: Utc::now().to_rfc3339(),
    })
}

async fn search_auto(
    query: &str,
    max_results: usize,
    config: &WebConfig,
) -> Result<Vec<WebSearchResult>> {
    if let Some((provider_id, provider_config)) = config
        .providers
        .iter()
        .find(|(_, provider)| provider.kind == WebProviderKind::Searxng)
    {
        return searxng_search(
            query,
            max_results,
            provider_id,
            provider_config,
            &config.fetch,
        )
        .await;
    }
    duckduckgo_search(query, max_results, &config.fetch).await
}

async fn duckduckgo_search(
    query: &str,
    max_results: usize,
    fetch_config: &crate::web::WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let encoded = form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>();
    let url = format!("https://lite.duckduckgo.com/lite/?q={encoded}");
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let html = send_search_text(client.get(&url), "duckduckgo").await?;
    let results = parse_duckduckgo_lite_results(&html, max_results);
    if results.is_empty() {
        return Err(search_error(
            "parse_failed",
            "DuckDuckGo returned no parseable search results",
            "duckduckgo",
            "configure SearXNG or an API-backed provider if DuckDuckGo HTML is unavailable",
        ));
    }
    Ok(results)
}

async fn searxng_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let base_url = provider.base_url.as_deref().ok_or_else(|| {
        search_error(
            "provider_unavailable",
            "SearXNG provider requires base_url",
            provider_id,
            "set web.providers.<id>.base_url to a SearXNG instance",
        )
    })?;
    let mut url = searxng_search_url(base_url)
        .map_err(|error| anyhow!("invalid SearXNG base_url for {provider_id}: {error}"))?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("format", "json")
        .append_pair("language", "auto");
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let body = send_search_text(client.get(url), provider_id).await?;
    let payload: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        search_error(
            "parse_failed",
            format!("SearXNG returned invalid JSON: {error}"),
            provider_id,
            "check the configured SearXNG instance or use provider=duckduckgo",
        )
    })?;
    let results = payload
        .get("results")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let title = entry.get("title")?.as_str()?.trim().to_string();
            let url = entry.get("url")?.as_str()?.trim().to_string();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            Some(WebSearchResult {
                title,
                url,
                snippet: entry
                    .get("content")
                    .and_then(|value| value.as_str())
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                source: provider_id.to_string(),
                published_at: None,
            })
        })
        .take(max_results)
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Err(search_error(
            "parse_failed",
            "SearXNG returned no parseable search results",
            provider_id,
            "check the SearXNG instance or use provider=duckduckgo",
        ));
    }
    Ok(results)
}

fn normalize_max_results(requested: Option<usize>, configured: usize) -> Result<usize> {
    if requested == Some(0) {
        return Err(search_error(
            "invalid_tool_input",
            "WebSearch max_results must be greater than zero",
            "web_search",
            "omit max_results or provide a positive integer",
        ));
    }
    Ok(requested
        .unwrap_or(configured.max(1))
        .min(configured.max(1))
        .max(1))
}

async fn send_search_text(request: reqwest::RequestBuilder, provider: &str) -> Result<String> {
    let response = request.send().await.map_err(|error| {
        search_error(
            "network_failed",
            format!("WebSearch provider `{provider}` request failed: {error}"),
            provider,
            "retry later or configure another WebSearch provider",
        )
    })?;
    let status = response.status();
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(search_error(
            "rate_limited",
            format!("WebSearch provider `{provider}` rate limited the request"),
            provider,
            "retry later or configure another WebSearch provider",
        ));
    }
    if !status.is_success() {
        return Err(search_error(
            "network_failed",
            format!("WebSearch provider `{provider}` returned HTTP {status}"),
            provider,
            "retry later or configure another WebSearch provider",
        ));
    }

    let mut bytes = Vec::new();
    let mut response = response;
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        search_error(
            "network_failed",
            format!("WebSearch provider `{provider}` response failed: {error}"),
            provider,
            "retry later or configure another WebSearch provider",
        )
    })? {
        if bytes.len() + chunk.len() > SEARCH_RESPONSE_BYTES {
            return Err(search_error(
                "response_too_large",
                format!("WebSearch provider `{provider}` response exceeded the byte limit"),
                provider,
                "narrow the query or configure another WebSearch provider",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|error| {
        search_error(
            "parse_failed",
            format!("WebSearch provider `{provider}` returned non-UTF-8 text: {error}"),
            provider,
            "configure another WebSearch provider",
        )
    })
}

fn searxng_search_url(base_url: &str) -> Result<Url> {
    let mut base = Url::parse(base_url)?;
    if !base.path().ends_with('/') {
        let mut path = base.path().to_string();
        path.push('/');
        base.set_path(&path);
    }
    Ok(base.join("search")?)
}

fn parse_duckduckgo_lite_results(html: &str, max_results: usize) -> Vec<WebSearchResult> {
    let mut results = Vec::new();
    let marker = "<a rel=\"nofollow\" href=\"";
    let mut rest = html;
    while let Some(start) = rest.find(marker) {
        let after_marker = &rest[start + marker.len()..];
        let Some(href_end) = after_marker.find('"') else {
            break;
        };
        let href = decode_html_entities(&after_marker[..href_end]);
        let after_href = &after_marker[href_end..];
        let Some(text_start) = after_href.find('>') else {
            break;
        };
        let after_text_start = &after_href[text_start + 1..];
        let Some(text_end) = after_text_start.find("</a>") else {
            break;
        };
        let title = decode_html_entities(&strip_tags(&after_text_start[..text_end]));
        if let Some(url) = normalize_duckduckgo_url(&href) {
            if !title.trim().is_empty() {
                results.push(WebSearchResult {
                    title: title.trim().to_string(),
                    url,
                    snippet: None,
                    source: "duckduckgo".into(),
                    published_at: None,
                });
            }
        }
        if results.len() >= max_results {
            break;
        }
        rest = &after_text_start[text_end + "</a>".len()..];
    }
    results
}

fn normalize_duckduckgo_url(value: &str) -> Option<String> {
    if let Ok(url) = Url::parse(value) {
        if let Some(target) = url
            .query_pairs()
            .find(|(key, _)| key == "uddg")
            .map(|(_, value)| value.into_owned())
        {
            return Some(target);
        }
        return Some(url.to_string());
    }
    None
}

fn strip_tags(value: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for ch in value.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    output
}

fn decode_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x2F;", "/")
        .replace("&#39;", "'")
}

fn search_error(
    kind: &'static str,
    message: impl Into<String>,
    provider: impl Into<String>,
    recovery_hint: impl Into<String>,
) -> anyhow::Error {
    let provider = provider.into();
    anyhow::Error::from(
        ToolError::new(kind, message)
            .with_details(json!({ "provider": provider }))
            .with_recovery_hint(recovery_hint)
            .with_retryable(matches!(kind, "rate_limited" | "network_failed")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duckduckgo_lite_links() {
        let html = r#"
            <a rel="nofollow" href="https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdocs&amp;rut=x">Example &amp; Docs</a>
        "#;
        let results = parse_duckduckgo_lite_results(html, 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example & Docs");
        assert_eq!(results[0].url, "https://example.com/docs");
    }

    #[test]
    fn searxng_search_url_preserves_path_prefix() {
        assert_eq!(
            searxng_search_url("https://example.com/searxng/")
                .unwrap()
                .as_str(),
            "https://example.com/searxng/search"
        );
        assert_eq!(
            searxng_search_url("https://example.com/searxng")
                .unwrap()
                .as_str(),
            "https://example.com/searxng/search"
        );
    }

    #[test]
    fn max_results_zero_is_invalid() {
        let error = normalize_max_results(Some(0), 5).unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "invalid_tool_input");
    }
}
