use std::collections::{BTreeMap, BTreeSet};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::Utc;
use reqwest::{Client, Response, StatusCode};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{io::AsyncReadExt, process::Command, time};
use url::{form_urlencoded, Url};

use holon_domain::ToolError;

use crate::{
    deepseek_tool_url, policy::timeout, WebCommandOutputConfig, WebCommandResultMapping, WebConfig,
    WebFetchConfig, WebProviderCapabilityMetadata, WebProviderConfig, WebProviderKind,
    WebProviderSupportStatus, WebSearchMode,
};

const DUCKDUCKGO_PROVIDER_ID: &str = "duckduckgo";
const FIRECRAWL_PROVIDER_ID: &str = "firecrawl";
const KEENABLE_PROVIDER_ID: &str = "keenable";

/// Zero-configuration managed fallback chain, most preferred first.
///
/// Every entry works without credentials: Firecrawl's search endpoint and
/// Keenable's public endpoint allow keyless requests (a configured API key
/// only raises limits), and DuckDuckGo remains the final HTML fallback.
/// Explicitly configured providers always sort ahead of this chain.
const BUILTIN_KEYLESS_SEARCH_PROVIDERS: [(&str, WebProviderKind); 3] = [
    (FIRECRAWL_PROVIDER_ID, WebProviderKind::Firecrawl),
    (KEENABLE_PROVIDER_ID, WebProviderKind::Keenable),
    (DUCKDUCKGO_PROVIDER_ID, WebProviderKind::DuckDuckGo),
];

const SEARCH_RESPONSE_BYTES: usize = 1_000_000;
const COMMAND_QUERY_TEMPLATE: &str = "{{query}}";
const COMMAND_MAX_RESULTS_TEMPLATE: &str = "{{max_results}}";

/// Error surfaced by a native search dispatcher with a WebSearch error kind.
#[derive(Debug, Clone)]
pub struct WebSearchNativeDispatchError {
    pub kind: &'static str,
    pub message: String,
    pub recovery_hint: Option<&'static str>,
}

/// Sends one standalone search request through the model route bound to a
/// native search provider. Only the query and the search parameters cross
/// this boundary; the implementation must deduplicate by canonical URL and
/// truncate the results to `max_results`.
#[async_trait::async_trait]
pub trait WebSearchNativeDispatcher: Send + Sync {
    async fn dispatch(
        &self,
        provider_id: &str,
        model_route: &str,
        query: &str,
        max_results: usize,
    ) -> std::result::Result<Vec<WebSearchResult>, WebSearchNativeDispatchError>;
}

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
    pub mode: WebSearchMode,
    pub provider_attempts: Vec<WebSearchProviderAttempt>,
    pub winning_provider: Option<String>,
    pub results: Vec<WebSearchResult>,
    pub citations: Vec<WebCitation>,
    pub fetched_at: String,
    pub summary_text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebSearchProviderAttempt {
    pub provider: String,
    pub status: WebSearchProviderAttemptStatus,
    pub result_count: usize,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
    pub provider_kind: Option<WebProviderKind>,
    pub capabilities: Option<WebProviderCapabilityMetadata>,
    pub command: Option<WebSearchCommandAttempt>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct WebSearchCommandAttempt {
    pub argv_template: Vec<String>,
    pub exit_status: Option<i32>,
    pub exit_status_text: Option<String>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchProviderAttemptStatus {
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
    pub source: String,
    pub published_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebCitation {
    pub title: String,
    pub url: String,
}

/// Search using resolved configuration and the caller's runtime error mapping.
pub async fn search(
    map_error: fn(&anyhow::Error) -> ToolError,
    request: WebSearchRequest,
    config: &WebConfig,
) -> Result<WebSearchResponse> {
    search_with_native(map_error, request, config, None).await
}

/// Search with an optional native search dispatcher attached. Native search
/// providers run only for an explicit provider ID argument; omission, `auto`,
/// fallback, and aggregate paths never dispatch native search requests.
pub async fn search_with_native(
    map_error: fn(&anyhow::Error) -> ToolError,
    request: WebSearchRequest,
    config: &WebConfig,
    native: Option<&dyn WebSearchNativeDispatcher>,
) -> Result<WebSearchResponse> {
    // Interpret aliases only on the call boundary, not in configured provider IDs.
    let (selector, order_selector, mode) = match request.provider.as_deref() {
        Some(alias) if is_route_selector(alias) && config.providers.contains_key(alias) => {
            return Err(search_error(
                "invalid_tool_input",
                format!("WebSearch provider `{alias}` conflicts with a configured provider ID"),
                alias,
                "rename the configured provider or omit the provider argument",
            ));
        }
        None => {
            let configured = config.search.provider.as_str();
            if is_native_provider(configured, config) {
                return Err(search_error(
                    "provider_unavailable",
                    format!("WebSearch default provider `{configured}` is a native search provider; native search requires an explicit provider argument"),
                    configured,
                    "call WebSearch with provider=<native provider id> or point web.search.provider at a managed provider",
                ));
            }
            (configured, configured, config.search.mode)
        }
        Some("auto") => ("auto", "auto", config.search.mode),
        Some("aggregate") => ("aggregate", "auto", WebSearchMode::Aggregate),
        Some(id) => (id, id, WebSearchMode::Single),
    };

    if !config.search.enabled {
        return Err(search_error(
            "provider_unavailable",
            "WebSearch is disabled by configuration",
            "disabled",
            "enable web.search.enabled or use WebFetch with a known URL",
        ));
    }

    let max_results = normalize_max_results(request.max_results, config.search.max_results)?;
    let provider_order = provider_order(order_selector, config);
    let routed = match mode {
        WebSearchMode::Single => {
            let provider_id = provider_order
                .first()
                .cloned()
                .unwrap_or_else(|| order_selector.to_string());
            let outcome = search_one_provider(
                map_error,
                &request.query,
                max_results,
                &provider_id,
                config,
                native,
            )
            .await;
            let kind = provider_kind(&provider_id, config);
            routed_single(map_error, provider_id, kind, outcome)?
        }
        WebSearchMode::Fallback => {
            search_fallback(
                map_error,
                &request.query,
                max_results,
                provider_order,
                config,
                native,
            )
            .await?
        }
        WebSearchMode::Aggregate => {
            search_aggregate(
                map_error,
                &request.query,
                max_results,
                provider_order,
                config,
                native,
            )
            .await?
        }
    };

    let citations = routed
        .results
        .iter()
        .map(|result| WebCitation {
            title: result.title.clone(),
            url: result.url.clone(),
        })
        .collect::<Vec<_>>();
    Ok(WebSearchResponse {
        query: request.query,
        provider: routed
            .winning_provider
            .clone()
            .unwrap_or_else(|| selector.to_string()),
        mode,
        provider_attempts: routed.provider_attempts,
        winning_provider: routed.winning_provider,
        summary_text: format!("{} web results", routed.results.len()),
        results: routed.results,
        citations,
        fetched_at: Utc::now().to_rfc3339(),
    })
}

/// Reserved selector words that must never name a concrete provider route.
pub(crate) fn is_route_selector(provider: &str) -> bool {
    matches!(provider, "auto" | "aggregate")
}

/// Enumerate the locally configured routes reachable by naming a provider.
/// This neither constructs a client nor starts an adapter.
pub fn list_search_providers(config: &WebConfig) -> Vec<String> {
    if !config.search.enabled {
        return Vec::new();
    }
    // The built-in route takes precedence over any registry entry with this ID.
    let mut ids = BTreeSet::from([DUCKDUCKGO_PROVIDER_ID.to_string()]);
    for (id, provider) in &config.providers {
        if id.is_empty() || id.trim() != id || is_route_selector(id) {
            continue;
        }
        if configured_search_ready(id, provider) {
            ids.insert(id.clone());
        }
    }
    ids.into_iter().collect()
}

fn configured_search_ready(id: &str, provider: &WebProviderConfig) -> bool {
    if provider.kind.is_native_search() {
        // Standalone native search is explicit-only and callable once a model
        // route is bound; reserved native kinds stay undiscoverable.
        return provider.kind == WebProviderKind::OpenAiNative
            && provider
                .model_route
                .as_deref()
                .is_some_and(|route| !route.trim().is_empty());
    }
    if provider.kind.capabilities().status != WebProviderSupportStatus::Supported {
        return false;
    }
    // Match the local inputs consumed by the adapters. In particular,
    // Keenable has a public route without an API key.
    match provider.kind {
        WebProviderKind::DuckDuckGo => true,
        WebProviderKind::Searxng => searxng_endpoint(id, provider).is_ok_and(|url| is_http(&url)),
        WebProviderKind::DeepSeek => {
            search_api_key(id, provider).is_ok() && deepseek_endpoint(id, provider).is_ok()
        }
        WebProviderKind::Command => command_configuration(id, provider).is_ok_and(|(argv, _)| {
            expand_command_argv(argv, "", 1, id).is_ok()
                && argv.iter().all(|arg| !arg.contains('\0'))
        }),
        _ => {
            search_api_key(id, provider).is_ok()
                && provider
                    .base_url
                    .as_deref()
                    .is_none_or(|url| Url::parse(url).is_ok_and(|url| is_http(&url)))
        }
    }
}

fn is_http(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
}

fn search_api_key<'a>(id: &str, provider: &'a WebProviderConfig) -> Result<&'a str> {
    let key = provider.api_key.as_str();
    let service = match provider.kind {
        WebProviderKind::Brave => "Brave Search",
        WebProviderKind::TencentCloudWsa => "Tencent Cloud WSA SearchPro",
        WebProviderKind::Bocha => "Bocha AI Web Search",
        WebProviderKind::DeepSeek => "DeepSeek",
        WebProviderKind::Tavily => "Tavily",
        WebProviderKind::Exa => "Exa",
        WebProviderKind::Perplexity => "Perplexity",
        _ => return Ok(key),
    };
    let empty = if provider.kind == WebProviderKind::DeepSeek {
        key.trim().is_empty()
    } else {
        key.is_empty()
    };
    if empty {
        return Err(search_error(
            "provider_unavailable",
            format!("{service} requires an API key (set credential_profile on the provider)"),
            id,
            "add a credential_profile with an api_key in the credential store",
        ));
    }
    Ok(key)
}

struct RoutedSearchOutcome {
    results: Vec<WebSearchResult>,
    provider_attempts: Vec<WebSearchProviderAttempt>,
    winning_provider: Option<String>,
}

#[derive(Debug)]
struct ProviderSearchOutput {
    results: Vec<WebSearchResult>,
    diagnostics: Option<ProviderSearchDiagnostics>,
}

#[derive(Debug, Clone, Default)]
struct ProviderRawSearchResult {
    title: String,
    url: String,
    snippet: Option<String>,
    published_at: Option<String>,
    language: Option<String>,
    region: Option<String>,
    metadata: BTreeMap<String, Value>,
}

#[derive(Debug)]
enum ProviderSearchDiagnostics {
    Command(WebSearchCommandAttempt),
}

impl ProviderSearchOutput {
    fn new(results: Vec<WebSearchResult>) -> Self {
        Self {
            results,
            diagnostics: None,
        }
    }

    fn command(results: Vec<WebSearchResult>, command: WebSearchCommandAttempt) -> Self {
        Self {
            results,
            diagnostics: Some(ProviderSearchDiagnostics::Command(command)),
        }
    }
}

fn normalize_search_result(
    provider_id: &str,
    rank: usize,
    raw: ProviderRawSearchResult,
) -> Option<WebSearchResult> {
    let title = raw.title.trim().to_string();
    let url = raw.url.trim().to_string();
    if title.is_empty() || url.is_empty() {
        return None;
    }
    let parsed_url = Url::parse(&url).ok()?;
    let canonical_url = canonicalize_search_url(parsed_url.clone());
    let display_url = display_search_url(&parsed_url);
    let snippet = raw.snippet.and_then(normalize_optional_string);
    let published_at = raw.published_at.and_then(normalize_optional_string);
    let language = raw.language.and_then(normalize_optional_string);
    let region = raw.region.and_then(normalize_optional_string);
    let metadata = (!raw.metadata.is_empty()).then_some(raw.metadata);

    Some(WebSearchResult {
        title,
        url,
        snippet,
        source: provider_id.to_string(),
        published_at,
        canonical_url: Some(canonical_url),
        display_url: Some(display_url),
        rank: Some(rank),
        language,
        region,
        metadata,
    })
}

fn normalize_optional_string(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn canonicalize_search_url(mut url: Url) -> String {
    url.set_fragment(None);
    url.to_string()
}

/// Canonical dedupe key for search URLs: the fragment is stripped so one
/// page cited with different fragments occupies a single result slot.
/// Unparseable URLs dedupe on their exact text.
pub fn canonical_search_url_key(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(url) => canonicalize_search_url(url),
        Err(_) => raw.to_string(),
    }
}

fn display_search_url(url: &Url) -> String {
    let mut display = String::new();
    if let Some(host) = url.host_str() {
        display.push_str(host);
    }
    let path = url.path();
    if path != "/" {
        display.push_str(path);
    }
    if let Some(query) = url.query() {
        display.push('?');
        display.push_str(query);
    }
    if display.is_empty() {
        url.as_str().to_string()
    } else {
        display
    }
}

fn search_result_dedupe_key(result: &WebSearchResult) -> String {
    result
        .canonical_url
        .as_deref()
        .unwrap_or(&result.url)
        .to_string()
}

fn provider_order(provider: &str, config: &WebConfig) -> Vec<String> {
    if provider != "auto" {
        let mut providers = vec![provider.trim().to_string()];
        providers.extend(default_provider_order(config));
        providers = dedupe_provider_order(providers);
        providers.truncate(config.search.max_provider_attempts.max(1));
        return providers;
    }
    let configured = dedupe_provider_order(&config.search.providers);
    let configured = configured
        .into_iter()
        .filter(|id| !is_native_provider(id, config))
        .collect::<Vec<_>>();
    let mut providers = if configured.is_empty() {
        default_provider_order(config)
    } else {
        configured
    };
    if providers.is_empty() {
        providers.extend(
            BUILTIN_KEYLESS_SEARCH_PROVIDERS
                .iter()
                .map(|(id, _)| id.to_string()),
        );
    }
    providers.truncate(config.search.max_provider_attempts.max(1));
    providers
}

fn default_provider_order(config: &WebConfig) -> Vec<String> {
    let mut providers = config
        .providers
        .iter()
        .filter_map(|(id, provider)| {
            let capabilities = provider.kind.capabilities();
            (capabilities.status == WebProviderSupportStatus::Supported).then(|| {
                (
                    id.clone(),
                    capabilities.default_priority,
                    provider.kind.as_str().to_string(),
                )
            })
        })
        .collect::<Vec<_>>();
    providers.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.0.cmp(&right.0))
    });
    let mut order = providers
        .into_iter()
        .map(|(id, _, _)| id)
        .collect::<Vec<_>>();
    for (builtin_id, _) in BUILTIN_KEYLESS_SEARCH_PROVIDERS {
        if !config.providers.contains_key(builtin_id) {
            order.push(builtin_id.to_string());
        }
    }
    dedupe_provider_order(order)
}

fn provider_kind(provider: &str, config: &WebConfig) -> Option<WebProviderKind> {
    config
        .providers
        .get(provider)
        .map(|provider| provider.kind)
        .or_else(|| builtin_keyless_provider_kind(provider))
}

/// Kind of a builtin keyless provider id (see `BUILTIN_KEYLESS_SEARCH_PROVIDERS`).
fn builtin_keyless_provider_kind(provider_id: &str) -> Option<WebProviderKind> {
    BUILTIN_KEYLESS_SEARCH_PROVIDERS
        .iter()
        .find(|(id, _)| *id == provider_id)
        .map(|(_, kind)| *kind)
}

/// Config used when a builtin keyless provider id participates in the fallback
/// chain without an explicit `web.providers` entry.
fn builtin_keyless_provider_config(provider_id: &str) -> Option<WebProviderConfig> {
    Some(WebProviderConfig {
        kind: builtin_keyless_provider_kind(provider_id)?,
        base_url: None,
        api_key: String::new(),
        model_route: None,
        command: None,
        output: None,
        limits: Default::default(),
    })
}

fn is_native_provider(provider_id: &str, config: &WebConfig) -> bool {
    provider_kind(provider_id, config).is_some_and(|kind| kind.is_native_search())
}

fn dedupe_provider_order<I, S>(providers: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = BTreeSet::new();
    providers
        .into_iter()
        .filter_map(|provider| {
            let provider = provider.as_ref().trim().to_string();
            (!provider.is_empty() && seen.insert(provider.clone())).then_some(provider)
        })
        .collect()
}

fn routed_single(
    map_error: fn(&anyhow::Error) -> ToolError,
    provider_id: String,
    kind: Option<WebProviderKind>,
    outcome: Result<ProviderSearchOutput>,
) -> Result<RoutedSearchOutcome> {
    match outcome {
        Ok(output) => Ok(RoutedSearchOutcome {
            provider_attempts: vec![successful_attempt(
                &provider_id,
                kind,
                output.results.len(),
                output.diagnostics,
            )],
            winning_provider: Some(provider_id),
            results: output.results,
        }),
        Err(error) => Err(single_provider_error(map_error, &provider_id, kind, error)),
    }
}

async fn search_fallback(
    map_error: fn(&anyhow::Error) -> ToolError,
    query: &str,
    max_results: usize,
    provider_order: Vec<String>,
    config: &WebConfig,
    native: Option<&dyn WebSearchNativeDispatcher>,
) -> Result<RoutedSearchOutcome> {
    let mut attempts = Vec::new();
    for provider_id in provider_order {
        let kind = provider_kind(&provider_id, config);
        match search_one_provider(map_error, query, max_results, &provider_id, config, native).await
        {
            Ok(output) => {
                attempts.push(successful_attempt(
                    &provider_id,
                    kind,
                    output.results.len(),
                    output.diagnostics,
                ));
                return Ok(RoutedSearchOutcome {
                    results: output.results,
                    provider_attempts: attempts,
                    winning_provider: Some(provider_id),
                });
            }
            Err(error) => attempts.push(failed_attempt(map_error, &provider_id, kind, &error)),
        }
    }
    Err(routing_error(attempts, None))
}

async fn search_aggregate(
    map_error: fn(&anyhow::Error) -> ToolError,
    query: &str,
    max_results: usize,
    provider_order: Vec<String>,
    config: &WebConfig,
    native: Option<&dyn WebSearchNativeDispatcher>,
) -> Result<RoutedSearchOutcome> {
    let mut attempts = Vec::new();
    let mut seen_urls = BTreeSet::new();
    let mut results = Vec::new();

    for provider_id in provider_order {
        let kind = provider_kind(&provider_id, config);
        match search_one_provider(map_error, query, max_results, &provider_id, config, native).await
        {
            Ok(output) => {
                attempts.push(successful_attempt(
                    &provider_id,
                    kind,
                    output.results.len(),
                    output.diagnostics,
                ));
                for result in output.results {
                    if seen_urls.insert(search_result_dedupe_key(&result)) {
                        results.push(result);
                    }
                    if results.len() >= max_results {
                        break;
                    }
                }
            }
            Err(error) => attempts.push(failed_attempt(map_error, &provider_id, kind, &error)),
        }
        if results.len() >= max_results {
            break;
        }
    }

    if results.is_empty() {
        return Err(routing_error(attempts, None));
    }

    Ok(RoutedSearchOutcome {
        results,
        provider_attempts: attempts,
        winning_provider: None,
    })
}

fn successful_attempt(
    provider: &str,
    kind: Option<WebProviderKind>,
    result_count: usize,
    diagnostics: Option<ProviderSearchDiagnostics>,
) -> WebSearchProviderAttempt {
    WebSearchProviderAttempt {
        provider: provider.to_string(),
        status: WebSearchProviderAttemptStatus::Success,
        result_count,
        error_kind: None,
        error_message: None,
        provider_kind: kind,
        capabilities: kind.map(|kind| kind.capabilities()),
        command: command_attempt_from_diagnostics(diagnostics),
    }
}

fn failed_attempt(
    map_error: fn(&anyhow::Error) -> ToolError,
    provider: &str,
    kind: Option<WebProviderKind>,
    error: &anyhow::Error,
) -> WebSearchProviderAttempt {
    let tool_error = map_error(error);
    WebSearchProviderAttempt {
        provider: provider.to_string(),
        status: WebSearchProviderAttemptStatus::Error,
        result_count: 0,
        error_kind: Some(tool_error.kind),
        error_message: Some(tool_error.message),
        provider_kind: kind,
        capabilities: kind.map(|kind| kind.capabilities()),
        command: command_attempt_from_details(tool_error.details.as_ref()),
    }
}

fn command_attempt_from_diagnostics(
    diagnostics: Option<ProviderSearchDiagnostics>,
) -> Option<WebSearchCommandAttempt> {
    match diagnostics {
        Some(ProviderSearchDiagnostics::Command(command)) => Some(command),
        None => None,
    }
}

fn command_attempt_from_details(details: Option<&Value>) -> Option<WebSearchCommandAttempt> {
    let command = details?.get("command")?;
    serde_json::from_value(command.clone()).ok()
}

fn single_provider_error(
    map_error: fn(&anyhow::Error) -> ToolError,
    provider: &str,
    kind: Option<WebProviderKind>,
    error: anyhow::Error,
) -> anyhow::Error {
    let attempt = failed_attempt(map_error, provider, kind, &error);
    let original = map_error(&error);
    let mut tool_error = ToolError::new(original.kind, original.message)
        .with_details(single_provider_error_details(original.details, attempt))
        .with_retryable(original.retryable);
    if let Some(recovery_hint) = original.recovery_hint {
        tool_error = tool_error.with_recovery_hint(recovery_hint);
    }
    anyhow::Error::from(tool_error)
}

fn single_provider_error_details(
    details: Option<Value>,
    attempt: WebSearchProviderAttempt,
) -> Value {
    let attempted_providers = vec![attempt.provider.clone()];
    let provider_attempts = vec![attempt];
    match details {
        Some(Value::Object(mut object)) => {
            object.insert(
                "attempted_providers".to_string(),
                json!(attempted_providers),
            );
            object.insert("winning_provider".to_string(), Value::Null);
            object.insert("provider_attempts".to_string(), json!(provider_attempts));
            Value::Object(object)
        }
        Some(details) => json!({
            "provider_error_details": details,
            "attempted_providers": attempted_providers,
            "winning_provider": null,
            "provider_attempts": provider_attempts,
        }),
        None => json!({
            "attempted_providers": attempted_providers,
            "winning_provider": null,
            "provider_attempts": provider_attempts,
        }),
    }
}

fn routing_error(
    provider_attempts: Vec<WebSearchProviderAttempt>,
    winning_provider: Option<String>,
) -> anyhow::Error {
    let attempted_providers = provider_attempts
        .iter()
        .map(|attempt| attempt.provider.clone())
        .collect::<Vec<_>>();
    let retryable = provider_attempts.iter().any(|attempt| {
        attempt
            .error_kind
            .as_deref()
            .is_some_and(|kind| matches!(kind, "rate_limited" | "network_failed"))
    });
    anyhow::Error::from(
        ToolError::new(
            "provider_unavailable",
            "WebSearch routing exhausted all configured providers",
        )
        .with_details(json!({
            "attempted_providers": attempted_providers,
            "winning_provider": winning_provider,
            "provider_attempts": provider_attempts,
        }))
        .with_recovery_hint(
            "configure web.search.providers or use provider=<id> for single-provider debugging",
        )
        .with_retryable(retryable),
    )
}

async fn search_one_provider(
    map_error: fn(&anyhow::Error) -> ToolError,
    query: &str,
    max_results: usize,
    provider_id: &str,
    config: &WebConfig,
    native: Option<&dyn WebSearchNativeDispatcher>,
) -> Result<ProviderSearchOutput> {
    match provider_id {
        DUCKDUCKGO_PROVIDER_ID => duckduckgo_search(query, max_results, &config.fetch)
            .await
            .map(ProviderSearchOutput::new),
        provider_id => {
            let provider_config = config
                .providers
                .get(provider_id)
                .cloned()
                .or_else(|| builtin_keyless_provider_config(provider_id))
                .ok_or_else(|| {
                    search_error(
                        "provider_unavailable",
                        format!("WebSearch provider `{provider_id}` is not configured"),
                        provider_id,
                        "configure web.providers or use provider=duckduckgo",
                    )
                })?;
            search_configured_provider(
                map_error,
                query,
                max_results,
                provider_id,
                &provider_config,
                &config.fetch,
                native,
            )
            .await
        }
    }
}

async fn search_configured_provider(
    map_error: fn(&anyhow::Error) -> ToolError,
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider_config: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
    native: Option<&dyn WebSearchNativeDispatcher>,
) -> Result<ProviderSearchOutput> {
    let results = match provider_config.kind {
        WebProviderKind::Searxng => {
            searxng_search(query, max_results, provider_id, provider_config, fetch_config).await
        }
        WebProviderKind::DuckDuckGo => duckduckgo_search(query, max_results, fetch_config).await,
        WebProviderKind::Brave => {
            brave_search(query, max_results, provider_id, provider_config, fetch_config).await
        }
        WebProviderKind::TencentCloudWsa => {
            tencent_cloud_wsa_search(query, max_results, provider_id, provider_config, fetch_config)
                .await
        }
        WebProviderKind::Bocha => {
            bocha_search(query, max_results, provider_id, provider_config, fetch_config).await
        }
        WebProviderKind::Tavily => {
            tavily_search(query, max_results, provider_id, provider_config, fetch_config).await
        }
        WebProviderKind::DeepSeek => {
            deepseek_search(query, max_results, provider_id, provider_config, fetch_config).await
        }
        WebProviderKind::Keenable => {
            keenable_search(query, max_results, provider_id, provider_config, fetch_config).await
        }
        WebProviderKind::Exa => {
            exa_search(query, max_results, provider_id, provider_config, fetch_config).await
        }
        WebProviderKind::Perplexity => {
            perplexity_search(query, max_results, provider_id, provider_config, fetch_config).await
        }
        WebProviderKind::Firecrawl => {
            firecrawl_search(query, max_results, provider_id, provider_config, fetch_config).await
        }
        WebProviderKind::Command => {
            return command_search(map_error, query, max_results, provider_id, provider_config).await;
        }
        WebProviderKind::OpenAiNative => {
            return native_openai_search(query, max_results, provider_id, provider_config, native)
                .await
                .map(ProviderSearchOutput::new);
        }
        kind => Err(search_error(
            "provider_unavailable",
            format!("WebSearch provider kind `{kind:?}` is reserved for future provider support"),
            provider_id,
            "configure a duckduckgo, searxng, brave, tencent_cloud_wsa, bocha, tavily, keenable, deepseek, exa, perplexity, or firecrawl provider for this Holon version",
        )),
    }?;
    Ok(ProviderSearchOutput::new(results))
}

async fn native_openai_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider_config: &WebProviderConfig,
    native: Option<&dyn WebSearchNativeDispatcher>,
) -> Result<Vec<WebSearchResult>> {
    if query.trim().is_empty() {
        return Err(search_error(
            "invalid_request",
            "WebSearch native search query must not be empty",
            provider_id,
            "provide a non-empty query",
        ));
    }
    let model_route = provider_config
        .model_route
        .as_deref()
        .map(str::trim)
        .filter(|route| !route.is_empty())
        .ok_or_else(|| {
            search_error(
                "provider_unavailable",
                format!("WebSearch native provider `{provider_id}` has no model_route bound"),
                provider_id,
                "bind web.providers.<id>.model_route to a configured model route",
            )
        })?;
    let dispatcher = native.ok_or_else(|| {
        search_error(
            "provider_unavailable",
            format!(
                "WebSearch native provider `{provider_id}` has no native search runtime attached"
            ),
            provider_id,
            "call WebSearch from the runtime host or use a managed provider",
        )
    })?;
    dispatcher
        .dispatch(provider_id, model_route, query, max_results)
        .await
        .map_err(|error| {
            search_error(
                error.kind,
                error.message,
                provider_id,
                error
                    .recovery_hint
                    .unwrap_or("retry later or check the provider configuration"),
            )
        })
}

fn command_configuration<'a>(
    provider_id: &str,
    provider: &'a WebProviderConfig,
) -> Result<(&'a [String], &'a WebCommandOutputConfig)> {
    let command = provider.command.as_ref().ok_or_else(|| {
        search_error(
            "provider_unavailable",
            "WebSearch command provider requires command.argv",
            provider_id,
            "configure web.providers.<id>.command.argv",
        )
    })?;
    let output = provider.output.as_ref().ok_or_else(|| {
        search_error(
            "provider_unavailable",
            "WebSearch command provider requires output.mapping",
            provider_id,
            "configure web.providers.<id>.output.mapping",
        )
    })?;
    Ok((&command.argv, output))
}

async fn command_search(
    map_error: fn(&anyhow::Error) -> ToolError,
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
) -> Result<ProviderSearchOutput> {
    let (argv, output) = command_configuration(provider_id, provider)?;
    let (binary, args) = expand_command_argv(argv, query, max_results, provider_id)?;
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            search_error(
                "provider_unavailable",
                format!("WebSearch command provider failed to start: {error}"),
                provider_id,
                "check that the configured command binary is installed and executable",
            )
        })?;
    let mut stdout = child.stdout.take().ok_or_else(|| {
        search_error(
            "provider_unavailable",
            "WebSearch command provider stdout was unavailable",
            provider_id,
            "check the configured command provider",
        )
    })?;
    let mut stderr = child.stderr.take().ok_or_else(|| {
        search_error(
            "provider_unavailable",
            "WebSearch command provider stderr was unavailable",
            provider_id,
            "check the configured command provider",
        )
    })?;
    let stdout_limit = provider.limits.max_output_bytes;
    let stderr_limit = provider.limits.max_output_bytes.min(64 * 1024);
    let stdout_task =
        tokio::spawn(async move { read_limited_output(&mut stdout, stdout_limit).await });
    let stderr_task =
        tokio::spawn(async move { read_limited_output(&mut stderr, stderr_limit).await });
    let wait = time::timeout(
        Duration::from_millis(provider.limits.timeout_ms),
        child.wait(),
    )
    .await;
    let status = match wait {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            abort_output_readers(stdout_task, stderr_task).await;
            return Err(search_error(
                "provider_unavailable",
                format!("WebSearch command provider failed while waiting: {error}"),
                provider_id,
                "check the configured command provider",
            ));
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = time::timeout(Duration::from_millis(100), child.wait()).await;
            abort_output_readers(stdout_task, stderr_task).await;
            return Err(search_error(
                "network_failed",
                format!(
                    "WebSearch command provider timed out after {} ms",
                    provider.limits.timeout_ms
                ),
                provider_id,
                "increase web.providers.<id>.limits.timeout_ms or use another provider",
            ));
        }
    };
    let stdout = stdout_task.await.map_err(|error| {
        search_error(
            "network_failed",
            format!("WebSearch command stdout reader failed: {error}"),
            provider_id,
            "retry later or check the configured command provider",
        )
    })??;
    let stderr = stderr_task.await.map_err(|error| {
        search_error(
            "network_failed",
            format!("WebSearch command stderr reader failed: {error}"),
            provider_id,
            "retry later or check the configured command provider",
        )
    })??;
    let command_attempt = command_attempt(argv, &status, stdout.truncated, stderr.truncated);
    if stdout.truncated || stderr.truncated {
        return Err(command_search_error(
            "response_too_large",
            format!(
                "WebSearch command provider output exceeded limits (stdout limit: {stdout_limit} bytes; stderr limit: {stderr_limit} bytes)"
            ),
            provider_id,
            "narrow the query, reduce stderr output, or increase web.providers.<id>.limits.max_output_bytes",
            command_attempt,
        ));
    }
    if !status.success() {
        return Err(command_search_error(
            "provider_unavailable",
            format!(
                "WebSearch command provider exited with status {}; stderr: {}",
                status,
                String::from_utf8_lossy(&stderr.bytes).trim()
            ),
            provider_id,
            "check the configured command provider and its arguments",
            command_attempt,
        ));
    }
    let stdout = String::from_utf8(stdout.bytes).map_err(|error| {
        command_search_error(
            "parse_failed",
            format!("WebSearch command provider returned non-UTF-8 stdout: {error}"),
            provider_id,
            "configure the command provider to emit UTF-8 JSON",
            command_attempt.clone(),
        )
    })?;
    parse_command_results(&stdout, output, provider_id, max_results)
        .map_err(|error| {
            command_parse_error_with_attempt(map_error, error, command_attempt.clone())
        })
        .map(|results| ProviderSearchOutput::command(results, command_attempt))
}

fn command_attempt(
    argv_template: &[String],
    status: &ExitStatus,
    stdout_truncated: bool,
    stderr_truncated: bool,
) -> WebSearchCommandAttempt {
    WebSearchCommandAttempt {
        argv_template: argv_template.to_vec(),
        exit_status: status.code(),
        exit_status_text: Some(status.to_string()),
        stdout_truncated,
        stderr_truncated,
    }
}

fn expand_command_argv(
    argv: &[String],
    query: &str,
    max_results: usize,
    provider_id: &str,
) -> Result<(String, Vec<String>)> {
    let binary = argv
        .first()
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .ok_or_else(|| {
            search_error(
                "provider_unavailable",
                "WebSearch command provider command.argv must not be empty",
                provider_id,
                "configure web.providers.<id>.command.argv with a fixed binary",
            )
        })?;
    let args = argv
        .iter()
        .skip(1)
        .map(|arg| expand_command_arg(arg, query, max_results))
        .collect::<Vec<_>>();
    Ok((binary.to_string(), args))
}

fn expand_command_arg(arg: &str, query: &str, max_results: usize) -> String {
    arg.replace(COMMAND_MAX_RESULTS_TEMPLATE, &max_results.to_string())
        .replace(COMMAND_QUERY_TEMPLATE, query)
}

struct LimitedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_limited_output<R>(reader: &mut R, max_bytes: usize) -> Result<LimitedOutput>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let remaining = max_bytes.saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..read.min(remaining)]);
        truncated |= read > remaining;
    }
    Ok(LimitedOutput { bytes, truncated })
}

async fn abort_output_readers(
    stdout_task: tokio::task::JoinHandle<Result<LimitedOutput>>,
    stderr_task: tokio::task::JoinHandle<Result<LimitedOutput>>,
) {
    let _ = tokio::join!(
        abort_output_reader(stdout_task),
        abort_output_reader(stderr_task)
    );
}

async fn abort_output_reader(task: tokio::task::JoinHandle<Result<LimitedOutput>>) {
    task.abort();
    let _ = time::timeout(Duration::from_millis(100), task).await;
}

async fn duckduckgo_search(
    query: &str,
    max_results: usize,
    fetch_config: &crate::WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let encoded = form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>();
    let url = format!("https://lite.duckduckgo.com/lite/?q={encoded}");
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let html = send_search_text(client.get(&url), DUCKDUCKGO_PROVIDER_ID, None).await?;
    let results = parse_duckduckgo_lite_results(&html, max_results);
    if results.is_empty() {
        return Err(search_error(
            "parse_failed",
            "DuckDuckGo returned no parseable search results",
            DUCKDUCKGO_PROVIDER_ID,
            "configure SearXNG or an API-backed provider if DuckDuckGo HTML is unavailable",
        ));
    }
    Ok(results)
}

fn searxng_endpoint(provider_id: &str, provider: &WebProviderConfig) -> Result<Url> {
    let base_url = provider.base_url.as_deref().ok_or_else(|| {
        search_error(
            "provider_unavailable",
            "SearXNG provider requires base_url",
            provider_id,
            "set web.providers.<id>.base_url to a SearXNG instance",
        )
    })?;
    searxng_search_url(base_url)
        .map_err(|error| anyhow!("invalid SearXNG base_url for {provider_id}: {error}"))
}

async fn searxng_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let mut url = searxng_endpoint(provider_id, provider)?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("format", "json")
        .append_pair("language", "auto");
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let body = send_search_text(
        client.get(url),
        provider_id,
        Some(
            "check whether `search.formats` in the SearXNG instance's `settings.yml` includes `json`; \
             disabled JSON output is one possible cause of HTTP 403",
        ),
    )
    .await?;
    let payload: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        search_error(
            "parse_failed",
            format!("SearXNG returned invalid JSON: {error}"),
            provider_id,
            "check the configured SearXNG instance or use provider=duckduckgo",
        )
    })?;
    let no_parseable_results = || {
        search_error(
            "parse_failed",
            "SearXNG returned no parseable search results",
            provider_id,
            "check the SearXNG instance or use provider=duckduckgo",
        )
    };
    let entries = payload
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(no_parseable_results)?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let results = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let title = entry.get("title")?.as_str()?.trim().to_string();
            let url = entry.get("url")?.as_str()?.trim().to_string();
            normalize_search_result(
                provider_id,
                index + 1,
                ProviderRawSearchResult {
                    title,
                    url,
                    snippet: entry
                        .get("content")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    published_at: None,
                    ..ProviderRawSearchResult::default()
                },
            )
        })
        .take(max_results)
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Err(no_parseable_results());
    }
    Ok(results)
}

async fn brave_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let api_key = search_api_key(provider_id, provider)?;
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let base_url = provider
        .base_url
        .as_deref()
        .unwrap_or("https://api.search.brave.com");
    let url = format!(
        "{}/res/v1/web/search?q={}&count={}",
        base_url.trim_end_matches('/'),
        form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>(),
        max_results.min(20),
    );
    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .header("X-Subscription-Token", api_key)
        .send()
        .await
        .map_err(|error| {
            search_error(
                "network_failed",
                format!("Brave Search request failed: {error}"),
                provider_id,
                "retry later or check the API key",
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let kind = if status == reqwest::StatusCode::UNAUTHORIZED {
            "provider_unavailable"
        } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            "rate_limited"
        } else {
            "network_failed"
        };
        return Err(search_error(
            kind,
            format!("Brave Search returned HTTP {status}"),
            provider_id,
            "check the API key or retry later",
        ));
    }
    let body = read_search_response(response, provider_id).await?;
    let payload: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        search_error(
            "parse_failed",
            format!("Brave Search returned invalid JSON: {error}"),
            provider_id,
            "check the API key or retry later",
        )
    })?;
    let results = payload
        .get("web")
        .and_then(|web| web.get("results"))
        .and_then(|results| results.as_array())
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, entry)| {
            let title = entry.get("title")?.as_str()?.trim().to_string();
            let url = entry.get("url")?.as_str()?.trim().to_string();
            normalize_search_result(
                provider_id,
                index + 1,
                ProviderRawSearchResult {
                    title,
                    url,
                    snippet: entry
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    published_at: None,
                    ..ProviderRawSearchResult::default()
                },
            )
        })
        .take(max_results)
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Err(search_error(
            "parse_failed",
            "Brave Search returned no parseable search results",
            provider_id,
            "try a different query or check the API subscription",
        ));
    }
    Ok(results)
}

async fn tencent_cloud_wsa_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let api_key = search_api_key(provider_id, provider)?;
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let body = serde_json::json!({
        "Query": query,
        "Cnt": tencent_cloud_wsa_count(max_results),
    });
    let base_url = provider
        .base_url
        .as_deref()
        .unwrap_or("https://api.wsa.cloud.tencent.com");
    let response = client
        .post(format!("{}/SearchPro", base_url.trim_end_matches('/')))
        .header("Content-Type", "application/json; charset=UTF-8")
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            search_error(
                "network_failed",
                format!("Tencent Cloud WSA SearchPro request failed: {error}"),
                provider_id,
                "retry later or check the API key",
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let kind = if status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN
        {
            "provider_unavailable"
        } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            "rate_limited"
        } else {
            "network_failed"
        };
        return Err(search_error(
            kind,
            format!("Tencent Cloud WSA SearchPro returned HTTP {status}"),
            provider_id,
            "check the API key or retry later",
        ));
    }
    let body = read_search_response(response, provider_id).await?;
    let payload: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        search_error(
            "parse_failed",
            format!("Tencent Cloud WSA SearchPro returned invalid JSON: {error}"),
            provider_id,
            "check the API key or retry later",
        )
    })?;
    if let Some(error) = payload.get("Error") {
        let code = error
            .get("Code")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let kind = if code == "RequestLimitExceeded" {
            "rate_limited"
        } else if matches!(
            code,
            "UnauthorizedOperation" | "ResourceNotFound" | "ResourceUnavailable"
        ) {
            "provider_unavailable"
        } else {
            "network_failed"
        };
        return Err(search_error(
            kind,
            format!("Tencent Cloud WSA SearchPro returned error code `{code}`"),
            provider_id,
            "check the API key or retry later",
        ));
    }
    let results = payload
        .get("Pages")
        .and_then(|pages| pages.as_array())
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, page)| {
            let entry = page
                .as_str()
                .and_then(|page| serde_json::from_str::<Value>(page).ok())?;
            let title = entry.get("title")?.as_str()?.trim().to_string();
            let url = entry.get("url")?.as_str()?.trim().to_string();
            let mut metadata = BTreeMap::new();
            if let Some(site) = entry.get("site").and_then(|value| value.as_str()) {
                metadata.insert("site".to_string(), Value::String(site.to_string()));
            }
            if let Some(score) = entry.get("score").cloned() {
                metadata.insert("score".to_string(), score);
            }
            if let Some(favicon) = entry.get("favicon").and_then(|value| value.as_str()) {
                metadata.insert("favicon".to_string(), Value::String(favicon.to_string()));
            }
            if let Some(images) = entry.get("images").cloned() {
                metadata.insert("images".to_string(), images);
            }
            normalize_search_result(
                provider_id,
                index + 1,
                ProviderRawSearchResult {
                    title,
                    url,
                    snippet: entry
                        .get("passage")
                        .or_else(|| entry.get("content"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    published_at: entry
                        .get("date")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    metadata,
                    ..ProviderRawSearchResult::default()
                },
            )
        })
        .take(max_results)
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Err(search_error(
            "parse_failed",
            "Tencent Cloud WSA SearchPro returned no parseable search results",
            provider_id,
            "try a different query or check the API subscription",
        ));
    }
    Ok(results)
}

fn tencent_cloud_wsa_count(max_results: usize) -> usize {
    // SearchPro accepts result counts in 10-result buckets up to 50.
    match max_results {
        0..=10 => 10,
        11..=20 => 20,
        21..=30 => 30,
        31..=40 => 40,
        _ => 50,
    }
}

async fn bocha_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let api_key = search_api_key(provider_id, provider)?;
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let body = serde_json::json!({
        "query": query,
        // WebSearchRequest has no freshness filter yet, so request unrestricted results.
        "freshness": "noLimit",
        "summary": true,
        "count": max_results.min(50),
    });
    let base_url = provider
        .base_url
        .as_deref()
        .unwrap_or("https://api.bochaai.com");
    let response = client
        .post(bocha_search_url(base_url))
        .header("Content-Type", "application/json")
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            search_error(
                "network_failed",
                format!("Bocha AI Web Search request failed: {error}"),
                provider_id,
                "retry later or check the API key",
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let kind = if status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN
        {
            "provider_unavailable"
        } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            "rate_limited"
        } else {
            "network_failed"
        };
        return Err(search_error(
            kind,
            format!("Bocha AI Web Search returned HTTP {status}"),
            provider_id,
            "check the API key or retry later",
        ));
    }
    let body = read_search_response(response, provider_id).await?;
    let payload: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        search_error(
            "parse_failed",
            format!("Bocha AI Web Search returned invalid JSON: {error}"),
            provider_id,
            "check the API key or retry later",
        )
    })?;
    let entries = payload
        .get("webPages")
        .or_else(|| payload.get("data").and_then(|data| data.get("webPages")))
        .and_then(|web_pages| web_pages.get("value"))
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            search_error(
                "parse_failed",
                "Bocha AI Web Search response did not include webPages.value",
                provider_id,
                "check the API key or retry later",
            )
        })?;
    let results = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let title = entry.get("name")?.as_str()?.trim().to_string();
            let url = entry.get("url")?.as_str()?.trim().to_string();
            let mut metadata = BTreeMap::new();
            if let Some(site_name) = entry.get("siteName").and_then(|value| value.as_str()) {
                metadata.insert(
                    "site_name".to_string(),
                    Value::String(site_name.to_string()),
                );
            }
            if let Some(site_icon) = entry.get("siteIcon").and_then(|value| value.as_str()) {
                metadata.insert(
                    "site_icon".to_string(),
                    Value::String(site_icon.to_string()),
                );
            }
            normalize_search_result(
                provider_id,
                index + 1,
                ProviderRawSearchResult {
                    title,
                    url,
                    snippet: entry
                        .get("summary")
                        .or_else(|| entry.get("snippet"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    published_at: entry
                        .get("datePublished")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    language: entry
                        .get("language")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    metadata,
                    ..ProviderRawSearchResult::default()
                },
            )
        })
        .take(max_results)
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Err(search_error(
            "parse_failed",
            "Bocha AI Web Search returned no parseable search results",
            provider_id,
            "try a different query or check the API subscription",
        ));
    }
    Ok(results)
}

fn bocha_search_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/v1/web-search") {
        base_url.to_string()
    } else if base_url.ends_with("/v1") {
        format!("{base_url}/web-search")
    } else {
        format!("{base_url}/v1/web-search")
    }
}

fn deepseek_endpoint(provider_id: &str, provider: &WebProviderConfig) -> Result<Url> {
    deepseek_tool_url(provider, "search").map_err(|_| {
        search_error(
            "provider_unavailable",
            "DeepSeek base_url is invalid",
            provider_id,
            "configure an HTTP(S) DeepSeek base_url",
        )
    })
}

async fn deepseek_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    if query.trim().is_empty() || !(1..=128).contains(&query.chars().count()) {
        return Err(search_error(
            "invalid_request",
            "DeepSeek search query must contain 1 to 128 Unicode characters",
            provider_id,
            "shorten the query or choose another WebSearch provider",
        ));
    }
    let api_key = search_api_key(provider_id, provider)?;
    let url = deepseek_endpoint(provider_id, provider)?;
    let client = Client::builder()
        .timeout(timeout(fetch_config))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .post(url)
        .bearer_auth(api_key)
        .json(&json!({
            "query": query,
            "count": max_results.clamp(1, 50),
            "need_content": false,
        }))
        .send()
        .await
        .map_err(|_| {
            search_error(
                "network_failed",
                "DeepSeek search request failed",
                provider_id,
                "retry later or check the provider configuration",
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let kind = if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            "provider_unavailable"
        } else if status == StatusCode::TOO_MANY_REQUESTS {
            "rate_limited"
        } else {
            "network_failed"
        };
        return Err(search_error(
            kind,
            format!("DeepSeek search returned HTTP {status}"),
            provider_id,
            "check the API key or retry later",
        ));
    }
    let body = read_search_response(response, provider_id).await?;
    let payload: Value = serde_json::from_str(&body).map_err(|_| {
        search_error(
            "parse_failed",
            "DeepSeek search returned invalid JSON",
            provider_id,
            "retry later or check the provider configuration",
        )
    })?;
    let entries = payload
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            search_error(
                "parse_failed",
                "DeepSeek search response did not include a results array",
                provider_id,
                "retry later or check the provider configuration",
            )
        })?;
    let results = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let title = entry.get("title")?.as_str()?.to_string();
            let mut url = Url::parse(entry.get("url")?.as_str()?.trim()).ok()?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return None;
            }
            let _ = url.set_username("");
            let _ = url.set_password(None);
            normalize_search_result(
                provider_id,
                index + 1,
                ProviderRawSearchResult {
                    title,
                    url: url.to_string(),
                    snippet: entry
                        .get("summary")
                        .and_then(Value::as_str)
                        .filter(|summary| !summary.trim().is_empty())
                        .or_else(|| entry.get("snippet").and_then(Value::as_str))
                        .map(str::to_string),
                    published_at: entry
                        .get("date")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    ..ProviderRawSearchResult::default()
                },
            )
        })
        .take(max_results.min(50))
        .collect::<Vec<_>>();
    if !entries.is_empty() && results.is_empty() && max_results != 0 {
        return Err(search_error(
            "parse_failed",
            "DeepSeek search returned no parseable search results",
            provider_id,
            "try another query or check the provider response format",
        ));
    }
    Ok(results)
}

async fn tavily_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let api_key = search_api_key(provider_id, provider)?;
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let body = serde_json::json!({
        "query": query,
        "api_key": api_key,
        "max_results": max_results.min(20),
        "search_depth": "basic",
    });
    let base_url = provider
        .base_url
        .as_deref()
        .unwrap_or("https://api.tavily.com");
    let response = client
        .post(format!("{}/search", base_url.trim_end_matches('/')))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            search_error(
                "network_failed",
                format!("Tavily request failed: {error}"),
                provider_id,
                "retry later or check the API key",
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let kind = if status == reqwest::StatusCode::UNAUTHORIZED {
            "provider_unavailable"
        } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            "rate_limited"
        } else {
            "network_failed"
        };
        return Err(search_error(
            kind,
            format!("Tavily returned HTTP {status}"),
            provider_id,
            "check the API key or retry later",
        ));
    }
    let body = read_search_response(response, provider_id).await?;
    let payload: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        search_error(
            "parse_failed",
            format!("Tavily returned invalid JSON: {error}"),
            provider_id,
            "check the API key or retry later",
        )
    })?;
    let results = payload
        .get("results")
        .and_then(|results| results.as_array())
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, entry)| {
            let title = entry.get("title")?.as_str()?.trim().to_string();
            let url = entry.get("url")?.as_str()?.trim().to_string();
            normalize_search_result(
                provider_id,
                index + 1,
                ProviderRawSearchResult {
                    title,
                    url,
                    snippet: entry
                        .get("content")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    published_at: None,
                    ..ProviderRawSearchResult::default()
                },
            )
        })
        .take(max_results)
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Err(search_error(
            "parse_failed",
            "Tavily returned no parseable search results",
            provider_id,
            "try a different query or check the API subscription",
        ));
    }
    Ok(results)
}

const KEENABLE_APP_TITLE: &str = "Holon";

fn keenable_request(
    client: &Client,
    base_url: &str,
    api_key: &str,
    query: &str,
    max_results: usize,
) -> reqwest::RequestBuilder {
    let base_url = base_url.trim_end_matches('/');
    let body = serde_json::json!({
        "query": query,
        "max_results": max_results.min(20),
    });
    let request = if api_key.trim().is_empty() {
        // Keenable's public endpoint needs no key; it identifies the app by title instead.
        client
            .post(format!("{base_url}/v1/search/public"))
            .header("X-Keenable-Title", KEENABLE_APP_TITLE)
    } else {
        client
            .post(format!("{base_url}/v1/search"))
            .header("X-API-Key", api_key.trim())
    };
    request.json(&body)
}

async fn keenable_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let base_url = provider
        .base_url
        .as_deref()
        .unwrap_or("https://api.keenable.ai");
    let response = keenable_request(&client, base_url, &provider.api_key, query, max_results)
        .send()
        .await
        .map_err(|error| {
            search_error(
                "network_failed",
                format!("Keenable request failed: {error}"),
                provider_id,
                "retry later or check the API key",
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let kind = if status == reqwest::StatusCode::UNAUTHORIZED {
            "provider_unavailable"
        } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            "rate_limited"
        } else {
            "network_failed"
        };
        return Err(search_error(
            kind,
            format!("Keenable returned HTTP {status}"),
            provider_id,
            "check the API key or retry later",
        ));
    }
    let body = read_search_response(response, provider_id).await?;
    let payload: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        search_error(
            "parse_failed",
            format!("Keenable returned invalid JSON: {error}"),
            provider_id,
            "check the API key or retry later",
        )
    })?;
    let results = payload
        .get("results")
        .and_then(|results| results.as_array())
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, entry)| {
            let url = entry.get("url")?.as_str()?.trim().to_string();
            let title = entry
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let snippet = ["snippet", "description"]
                .iter()
                .filter_map(|key| entry.get(*key).and_then(|v| v.as_str()))
                .find(|text| !text.trim().is_empty())
                .map(str::to_string);
            normalize_search_result(
                provider_id,
                index + 1,
                ProviderRawSearchResult {
                    title,
                    url,
                    snippet,
                    published_at: entry
                        .get("published_at")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    ..ProviderRawSearchResult::default()
                },
            )
        })
        .take(max_results)
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Err(search_error(
            "parse_failed",
            "Keenable returned no parseable search results",
            provider_id,
            "try a different query",
        ));
    }
    Ok(results)
}

async fn exa_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let api_key = search_api_key(provider_id, provider)?;
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let body = serde_json::json!({
        "query": query,
        "numResults": max_results.min(25),
        "type": "auto",
    });
    let base_url = provider.base_url.as_deref().unwrap_or("https://api.exa.ai");
    let response = client
        .post(format!("{}/search", base_url.trim_end_matches('/')))
        .header("Content-Type", "application/json")
        .header("x-api-key", api_key)
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            search_error(
                "network_failed",
                format!("Exa request failed: {error}"),
                provider_id,
                "retry later or check the API key",
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let kind = if status == reqwest::StatusCode::UNAUTHORIZED {
            "provider_unavailable"
        } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            "rate_limited"
        } else {
            "network_failed"
        };
        return Err(search_error(
            kind,
            format!("Exa returned HTTP {status}"),
            provider_id,
            "check the API key or retry later",
        ));
    }
    let body = read_search_response(response, provider_id).await?;
    let payload: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        search_error(
            "parse_failed",
            format!("Exa returned invalid JSON: {error}"),
            provider_id,
            "check the API key or retry later",
        )
    })?;
    let results = payload
        .get("results")
        .and_then(|results| results.as_array())
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, entry)| {
            let title = entry
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let url = entry
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            normalize_search_result(
                provider_id,
                index + 1,
                ProviderRawSearchResult {
                    title,
                    url,
                    snippet: entry
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    published_at: entry
                        .get("publishedDate")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    ..ProviderRawSearchResult::default()
                },
            )
        })
        .take(max_results)
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Err(search_error(
            "parse_failed",
            "Exa returned no parseable search results",
            provider_id,
            "try a different query or check the API subscription",
        ));
    }
    Ok(results)
}

async fn perplexity_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let api_key = search_api_key(provider_id, provider)?;
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let body = serde_json::json!({
        "model": "sonar",
        "messages": [
            {
                "role": "user",
                "content": query,
            }
        ],
        "max_tokens": 1024,
    });
    let base_url = provider
        .base_url
        .as_deref()
        .unwrap_or("https://api.perplexity.ai");
    let response = client
        .post(format!(
            "{}/chat/completions",
            base_url.trim_end_matches('/')
        ))
        .header("Content-Type", "application/json")
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            search_error(
                "network_failed",
                format!("Perplexity request failed: {error}"),
                provider_id,
                "retry later or check the API key",
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let kind = if status == reqwest::StatusCode::UNAUTHORIZED {
            "provider_unavailable"
        } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            "rate_limited"
        } else {
            "network_failed"
        };
        return Err(search_error(
            kind,
            format!("Perplexity returned HTTP {status}"),
            provider_id,
            "check the API key or retry later",
        ));
    }
    let body = read_search_response(response, provider_id).await?;
    let payload: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        search_error(
            "parse_failed",
            format!("Perplexity returned invalid JSON: {error}"),
            provider_id,
            "check the API key or retry later",
        )
    })?;
    let summary = payload
        .get("choices")
        .and_then(|choices| choices.as_array())
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .map(str::trim)
        .filter(|content| !content.is_empty());
    let summary = summary.map(str::to_string);
    let results = payload
        .get("search_results")
        .and_then(|results| results.as_array())
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, entry)| {
            let title = entry
                .get("title")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let url = entry
                .get("url")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            normalize_search_result(
                provider_id,
                index + 1,
                ProviderRawSearchResult {
                    title,
                    url,
                    snippet: summary.clone(),
                    published_at: entry
                        .get("date")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    ..ProviderRawSearchResult::default()
                },
            )
        })
        .take(max_results)
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Err(search_error(
            "parse_failed",
            "Perplexity returned no parseable search results",
            provider_id,
            "try a different query or check the API subscription",
        ));
    }
    Ok(results)
}

fn firecrawl_request(
    client: &Client,
    base_url: &str,
    api_key: &str,
    query: &str,
    max_results: usize,
) -> reqwest::RequestBuilder {
    let body = serde_json::json!({
        "query": query,
        "limit": max_results.min(20),
    });
    // Firecrawl allows unauthenticated search requests on a free tier; an API
    // key only raises rate limits, so it is sent when present.
    let mut request = client
        .post(format!("{}/v1/search", base_url.trim_end_matches('/')))
        .header("Content-Type", "application/json");
    if !api_key.trim().is_empty() {
        request = request.bearer_auth(api_key.trim());
    }
    request.json(&body)
}

async fn firecrawl_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let api_key = search_api_key(provider_id, provider)?;
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let base_url = provider
        .base_url
        .as_deref()
        .unwrap_or("https://api.firecrawl.dev");
    let response = firecrawl_request(&client, base_url, api_key, query, max_results)
        .send()
        .await
        .map_err(|error| {
            search_error(
                "network_failed",
                format!("Firecrawl request failed: {error}"),
                provider_id,
                "retry later or check the API key",
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let kind = if status == reqwest::StatusCode::UNAUTHORIZED {
            "provider_unavailable"
        } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            "rate_limited"
        } else {
            "network_failed"
        };
        return Err(search_error(
            kind,
            format!("Firecrawl returned HTTP {status}"),
            provider_id,
            "configure an api_key on the provider or retry later",
        ));
    }
    let body = read_search_response(response, provider_id).await?;
    let payload: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        search_error(
            "parse_failed",
            format!("Firecrawl returned invalid JSON: {error}"),
            provider_id,
            "check the API key or retry later",
        )
    })?;
    let results = payload
        .get("data")
        .and_then(|results| results.as_array())
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, entry)| {
            let title = entry.get("title")?.as_str()?.trim().to_string();
            let url = entry.get("url")?.as_str()?.trim().to_string();
            normalize_search_result(
                provider_id,
                index + 1,
                ProviderRawSearchResult {
                    title,
                    url,
                    snippet: entry
                        .get("description")
                        .or_else(|| entry.get("markdown"))
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    published_at: None,
                    ..ProviderRawSearchResult::default()
                },
            )
        })
        .take(max_results)
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Err(search_error(
            "parse_failed",
            "Firecrawl returned no parseable search results",
            provider_id,
            "try a different query or check the API subscription",
        ));
    }
    Ok(results)
}

fn parse_command_results(
    body: &str,
    output: &WebCommandOutputConfig,
    provider_id: &str,
    max_results: usize,
) -> Result<Vec<WebSearchResult>> {
    let payload: Value = serde_json::from_str(body).map_err(|error| {
        search_error(
            "parse_failed",
            format!("WebSearch command provider returned invalid JSON: {error}"),
            provider_id,
            "configure the command provider to emit JSON",
        )
    })?;
    let entries = payload
        .as_array()
        .map(|array| array.iter().collect::<Vec<_>>())
        .or_else(|| {
            payload
                .get("results")
                .and_then(|results| results.as_array())
                .map(|array| array.iter().collect::<Vec<_>>())
        })
        .ok_or_else(|| {
            search_error(
                "parse_failed",
                "WebSearch command provider JSON must be an array or object with a results array",
                provider_id,
                "configure output.mapping for the command provider JSON shape",
            )
        })?;
    let results = entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            command_result_from_entry(entry, &output.mapping, provider_id, index + 1)
        })
        .take(max_results)
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Err(search_error(
            "parse_failed",
            "WebSearch command provider returned no parseable search results",
            provider_id,
            "check output.mapping for title and url fields",
        ));
    }
    Ok(results)
}

fn command_result_from_entry(
    entry: &Value,
    mapping: &WebCommandResultMapping,
    provider_id: &str,
    rank: usize,
) -> Option<WebSearchResult> {
    let title = mapped_json_string(entry, &mapping.title)?
        .trim()
        .to_string();
    let url = mapped_json_string(entry, &mapping.url)?.trim().to_string();
    normalize_search_result(
        provider_id,
        rank,
        ProviderRawSearchResult {
            title,
            url,
            snippet: mapping
                .snippet
                .as_deref()
                .and_then(|path| mapped_json_string(entry, path)),
            published_at: mapping
                .published_at
                .as_deref()
                .and_then(|path| mapped_json_string(entry, path)),
            ..ProviderRawSearchResult::default()
        },
    )
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

async fn send_search_text(
    request: reqwest::RequestBuilder,
    provider: &str,
    forbidden_hint: Option<&str>,
) -> Result<String> {
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
        let default_hint = "retry later or configure another WebSearch provider";
        let hint = if status == StatusCode::FORBIDDEN {
            forbidden_hint.unwrap_or(default_hint)
        } else {
            default_hint
        };
        return Err(search_error(
            "network_failed",
            format!("WebSearch provider `{provider}` returned HTTP {status}"),
            provider,
            hint,
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

/// Read a search response body with a byte limit, using chunked streaming to
/// avoid unbounded memory use when an API endpoint returns an unexpectedly
/// large payload.
async fn read_search_response(response: Response, provider_id: &str) -> Result<String> {
    let mut bytes = Vec::new();
    let mut response = response;
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        search_error(
            "network_failed",
            format!("{provider_id} response failed: {error}"),
            provider_id,
            "retry later or configure another WebSearch provider",
        )
    })? {
        if bytes.len() + chunk.len() > SEARCH_RESPONSE_BYTES {
            return Err(search_error(
                "response_too_large",
                format!("{provider_id} response exceeded the byte limit"),
                provider_id,
                "narrow the query or configure another WebSearch provider",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|error| {
        search_error(
            "parse_failed",
            format!("{provider_id} returned non-UTF-8 text: {error}"),
            provider_id,
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
            if let Some(result) = normalize_search_result(
                DUCKDUCKGO_PROVIDER_ID,
                results.len() + 1,
                ProviderRawSearchResult {
                    title,
                    url,
                    snippet: None,
                    published_at: None,
                    ..ProviderRawSearchResult::default()
                },
            ) {
                results.push(result);
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

fn mapped_json_string(entry: &Value, path: &str) -> Option<String> {
    let mut current = entry;
    for segment in path.trim().trim_start_matches('.').split('.') {
        if segment.is_empty() {
            continue;
        }
        current = current.get(segment)?;
    }
    current
        .as_str()
        .map(str::trim)
        .map(ToOwned::to_owned)
        .filter(|value| !value.is_empty())
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

fn command_search_error(
    kind: &'static str,
    message: impl Into<String>,
    provider: impl Into<String>,
    recovery_hint: impl Into<String>,
    command: WebSearchCommandAttempt,
) -> anyhow::Error {
    let provider = provider.into();
    anyhow::Error::from(
        ToolError::new(kind, message)
            .with_details(json!({
                "provider": provider,
                "command": command,
            }))
            .with_recovery_hint(recovery_hint)
            .with_retryable(matches!(kind, "rate_limited" | "network_failed")),
    )
}

fn command_parse_error_with_attempt(
    map_error: fn(&anyhow::Error) -> ToolError,
    error: anyhow::Error,
    command: WebSearchCommandAttempt,
) -> anyhow::Error {
    let original = map_error(&error);
    let mut details = original
        .details
        .unwrap_or_else(|| json!({}))
        .as_object()
        .cloned()
        .unwrap_or_default();
    details.insert("command".to_string(), json!(command));
    let mut tool_error = ToolError::new(original.kind, original.message)
        .with_details(Value::Object(details))
        .with_retryable(original.retryable);
    if let Some(recovery_hint) = original.recovery_hint {
        tool_error = tool_error.with_recovery_hint(recovery_hint);
    }
    anyhow::Error::from(tool_error)
}

#[cfg(test)]
mod tests;
