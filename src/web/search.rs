use std::collections::BTreeSet;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::Utc;
use reqwest::{Client, Response, StatusCode};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{io::AsyncReadExt, process::Command, time};
use url::{form_urlencoded, Url};

use crate::{
    tool::ToolError,
    web::{
        policy::timeout, WebCommandOutputConfig, WebCommandResultMapping, WebConfig,
        WebFetchConfig, WebProviderCapabilityMetadata, WebProviderConfig, WebProviderKind,
        WebProviderSupportStatus, WebSearchMode,
    },
};

const DUCKDUCKGO_PROVIDER_ID: &str = "duckduckgo";
const SEARCH_RESPONSE_BYTES: usize = 1_000_000;
const COMMAND_QUERY_TEMPLATE: &str = "{{query}}";
const COMMAND_MAX_RESULTS_TEMPLATE: &str = "{{max_results}}";

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
    let mode = if request.provider.is_some() && provider != "auto" {
        WebSearchMode::Single
    } else {
        config.search.mode
    };
    let provider_order = provider_order(provider, config);
    let routed = match mode {
        WebSearchMode::Single => {
            let provider_id = provider_order
                .first()
                .cloned()
                .unwrap_or_else(|| provider.to_string());
            let outcome =
                search_one_provider(&request.query, max_results, &provider_id, config).await;
            let kind = provider_kind(&provider_id, config);
            routed_single(provider_id, kind, outcome)?
        }
        WebSearchMode::Fallback => {
            search_fallback(&request.query, max_results, provider_order, config).await?
        }
        WebSearchMode::Aggregate => {
            search_aggregate(&request.query, max_results, provider_order, config).await?
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
            .unwrap_or_else(|| provider.to_string()),
        mode,
        provider_attempts: routed.provider_attempts,
        winning_provider: routed.winning_provider,
        summary_text: format!("{} web results", routed.results.len()),
        results: routed.results,
        citations,
        fetched_at: Utc::now().to_rfc3339(),
    })
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

fn provider_order(provider: &str, config: &WebConfig) -> Vec<String> {
    if provider != "auto" {
        let mut providers = vec![provider.trim().to_string()];
        providers.extend(default_provider_order(config));
        providers = dedupe_provider_order(providers);
        providers.truncate(config.search.max_provider_attempts.max(1));
        return providers;
    }
    let configured = dedupe_provider_order(&config.search.providers);
    let mut providers = if configured.is_empty() {
        default_provider_order(config)
    } else {
        configured
    };
    if providers.is_empty() {
        providers.push(DUCKDUCKGO_PROVIDER_ID.to_string());
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
        .chain(std::iter::once((
            DUCKDUCKGO_PROVIDER_ID.to_string(),
            WebProviderKind::DuckDuckGo.capabilities().default_priority,
            WebProviderKind::DuckDuckGo.as_str().to_string(),
        )))
        .collect::<Vec<_>>();
    providers.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.0.cmp(&right.0))
    });
    dedupe_provider_order(providers.into_iter().map(|(id, _, _)| id))
}

fn provider_kind(provider: &str, config: &WebConfig) -> Option<WebProviderKind> {
    (provider == DUCKDUCKGO_PROVIDER_ID)
        .then_some(WebProviderKind::DuckDuckGo)
        .or_else(|| config.providers.get(provider).map(|provider| provider.kind))
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
        Err(error) => Err(single_provider_error(&provider_id, kind, error)),
    }
}

async fn search_fallback(
    query: &str,
    max_results: usize,
    provider_order: Vec<String>,
    config: &WebConfig,
) -> Result<RoutedSearchOutcome> {
    let mut attempts = Vec::new();
    for provider_id in provider_order {
        let kind = provider_kind(&provider_id, config);
        match search_one_provider(query, max_results, &provider_id, config).await {
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
            Err(error) => attempts.push(failed_attempt(&provider_id, kind, &error)),
        }
    }
    Err(routing_error(attempts, None))
}

async fn search_aggregate(
    query: &str,
    max_results: usize,
    provider_order: Vec<String>,
    config: &WebConfig,
) -> Result<RoutedSearchOutcome> {
    let mut attempts = Vec::new();
    let mut seen_urls = BTreeSet::new();
    let mut results = Vec::new();

    for provider_id in provider_order {
        let kind = provider_kind(&provider_id, config);
        match search_one_provider(query, max_results, &provider_id, config).await {
            Ok(output) => {
                attempts.push(successful_attempt(
                    &provider_id,
                    kind,
                    output.results.len(),
                    output.diagnostics,
                ));
                for result in output.results {
                    if seen_urls.insert(result.url.clone()) {
                        results.push(result);
                    }
                    if results.len() >= max_results {
                        break;
                    }
                }
            }
            Err(error) => attempts.push(failed_attempt(&provider_id, kind, &error)),
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
    provider: &str,
    kind: Option<WebProviderKind>,
    error: &anyhow::Error,
) -> WebSearchProviderAttempt {
    let tool_error = ToolError::from_anyhow(error);
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
    provider: &str,
    kind: Option<WebProviderKind>,
    error: anyhow::Error,
) -> anyhow::Error {
    let attempt = failed_attempt(provider, kind, &error);
    let original = ToolError::from_anyhow(&error);
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
    query: &str,
    max_results: usize,
    provider_id: &str,
    config: &WebConfig,
) -> Result<ProviderSearchOutput> {
    match provider_id {
        DUCKDUCKGO_PROVIDER_ID => duckduckgo_search(query, max_results, &config.fetch)
            .await
            .map(ProviderSearchOutput::new),
        provider_id => {
            let provider_config = config.providers.get(provider_id).ok_or_else(|| {
                search_error(
                    "provider_unavailable",
                    format!("WebSearch provider `{provider_id}` is not configured"),
                    provider_id,
                    "configure web.providers or use provider=duckduckgo",
                )
            })?;
            search_configured_provider(
                query,
                max_results,
                provider_id,
                provider_config,
                &config.fetch,
            )
            .await
        }
    }
}

async fn search_configured_provider(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider_config: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<ProviderSearchOutput> {
    let results = match provider_config.kind {
        WebProviderKind::Searxng => {
            searxng_search(query, max_results, provider_id, provider_config, fetch_config).await
        }
        WebProviderKind::DuckDuckGo => duckduckgo_search(query, max_results, fetch_config).await,
        WebProviderKind::Brave => {
            brave_search(query, max_results, provider_id, provider_config, fetch_config).await
        }
        WebProviderKind::Tavily => {
            tavily_search(query, max_results, provider_id, provider_config, fetch_config).await
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
            return command_search(query, max_results, provider_id, provider_config).await;
        }
        kind => Err(search_error(
            "provider_unavailable",
            format!("WebSearch provider kind `{kind:?}` is reserved for future provider support"),
            provider_id,
            "configure a duckduckgo, searxng, brave, tavily, exa, perplexity, or firecrawl provider for this Holon version",
        )),
    }?;
    Ok(ProviderSearchOutput::new(results))
}

async fn command_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
) -> Result<ProviderSearchOutput> {
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
    let (binary, args) = expand_command_argv(&command.argv, query, max_results, provider_id)?;
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
    let command_attempt =
        command_attempt(&command.argv, &status, stdout.truncated, stderr.truncated);
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
        search_error(
            "parse_failed",
            format!("WebSearch command provider returned non-UTF-8 stdout: {error}"),
            provider_id,
            "configure the command provider to emit UTF-8 JSON",
        )
    })?;
    parse_command_results(&stdout, output, provider_id, max_results)
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
    fetch_config: &crate::web::WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let encoded = form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>();
    let url = format!("https://lite.duckduckgo.com/lite/?q={encoded}");
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let html = send_search_text(client.get(&url), DUCKDUCKGO_PROVIDER_ID).await?;
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

async fn brave_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let api_key = &provider.api_key;
    if api_key.is_empty() {
        return Err(search_error(
            "provider_unavailable",
            "Brave Search requires an API key (set credential_profile on the provider)",
            provider_id,
            "add a credential_profile with an api_key in the credential store",
        ));
    }
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
        .header("X-Subscription-Token", api_key.as_str())
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
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty()),
                source: provider_id.to_string(),
                published_at: None,
            })
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

async fn tavily_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let api_key = &provider.api_key;
    if api_key.is_empty() {
        return Err(search_error(
            "provider_unavailable",
            "Tavily requires an API key (set credential_profile on the provider)",
            provider_id,
            "add a credential_profile with an api_key in the credential store",
        ));
    }
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
                    .and_then(|v| v.as_str())
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty()),
                source: provider_id.to_string(),
                published_at: None,
            })
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

async fn exa_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let api_key = &provider.api_key;
    if api_key.is_empty() {
        return Err(search_error(
            "provider_unavailable",
            "Exa requires an API key (set credential_profile on the provider)",
            provider_id,
            "add a credential_profile with an api_key in the credential store",
        ));
    }
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
        .header("x-api-key", api_key.as_str())
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
        .filter_map(|entry| {
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
            if title.is_empty() || url.is_empty() {
                return None;
            }
            Some(WebSearchResult {
                title,
                url,
                snippet: entry
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty()),
                source: provider_id.to_string(),
                published_at: entry
                    .get("publishedDate")
                    .and_then(|v| v.as_str())
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty()),
            })
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
    let api_key = &provider.api_key;
    if api_key.is_empty() {
        return Err(search_error(
            "provider_unavailable",
            "Perplexity requires an API key (set credential_profile on the provider)",
            provider_id,
            "add a credential_profile with an api_key in the credential store",
        ));
    }
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
        .bearer_auth(api_key.as_str())
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
        .filter_map(|entry| {
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
            if title.is_empty() || url.is_empty() {
                return None;
            }
            Some(WebSearchResult {
                title,
                url,
                snippet: summary.clone(),
                source: provider_id.to_string(),
                published_at: entry
                    .get("date")
                    .and_then(|value| value.as_str())
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
            })
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

async fn firecrawl_search(
    query: &str,
    max_results: usize,
    provider_id: &str,
    provider: &WebProviderConfig,
    fetch_config: &WebFetchConfig,
) -> Result<Vec<WebSearchResult>> {
    let api_key = &provider.api_key;
    if api_key.is_empty() {
        return Err(search_error(
            "provider_unavailable",
            "Firecrawl requires an API key (set credential_profile on the provider)",
            provider_id,
            "add a credential_profile with an api_key in the credential store",
        ));
    }
    let client = Client::builder().timeout(timeout(fetch_config)).build()?;
    let body = serde_json::json!({
        "query": query,
        "limit": max_results.min(20),
    });
    let base_url = provider
        .base_url
        .as_deref()
        .unwrap_or("https://api.firecrawl.dev");
    let response = client
        .post(format!("{}/v1/search", base_url.trim_end_matches('/')))
        .header("Content-Type", "application/json")
        .bearer_auth(api_key.as_str())
        .json(&body)
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
            "check the API key or retry later",
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
                    .get("description")
                    .or_else(|| entry.get("markdown"))
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
        .filter_map(|entry| command_result_from_entry(entry, &output.mapping, provider_id))
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
) -> Option<WebSearchResult> {
    let title = mapped_json_string(entry, &mapping.title)?
        .trim()
        .to_string();
    let url = mapped_json_string(entry, &mapping.url)?.trim().to_string();
    if title.is_empty() || url.is_empty() {
        return None;
    }
    Some(WebSearchResult {
        title,
        url,
        snippet: mapping
            .snippet
            .as_deref()
            .and_then(|path| mapped_json_string(entry, path))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        source: provider_id.to_string(),
        published_at: mapping
            .published_at
            .as_deref()
            .and_then(|path| mapped_json_string(entry, path))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
    })
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
            if !title.trim().is_empty() {
                results.push(WebSearchResult {
                    title: title.trim().to_string(),
                    url,
                    snippet: None,
                    source: DUCKDUCKGO_PROVIDER_ID.into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::WebSearchConfig;
    use axum::{
        body::Body,
        http::{
            header::{CONTENT_ENCODING, CONTENT_TYPE},
            HeaderValue,
        },
        response::Response,
    };
    use flate2::{write::GzEncoder, Compression};
    use std::{collections::BTreeMap, io::Write};

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

    #[tokio::test]
    async fn fallback_mode_tries_explicit_order_until_success() {
        let good_url = searxng_mock_base_url(searxng_results_json(&[(
            "Good result",
            "https://example.com/good",
            "ok",
        )]))
        .await;
        let config = test_search_config(
            vec![
                (
                    "bad",
                    WebProviderConfig {
                        kind: WebProviderKind::Searxng,
                        base_url: None,
                        api_key: String::new(),
                        command: None,
                        output: None,
                        limits: Default::default(),
                    },
                ),
                ("good", test_provider(WebProviderKind::Searxng, &good_url)),
            ],
            vec!["bad", "good"],
            WebSearchMode::Fallback,
        );

        let response = search(
            WebSearchRequest {
                query: "test".to_string(),
                max_results: Some(5),
                provider: None,
            },
            &config,
        )
        .await
        .unwrap();

        assert_eq!(response.mode, WebSearchMode::Fallback);
        assert_eq!(response.winning_provider.as_deref(), Some("good"));
        assert_eq!(response.provider_attempts.len(), 2);
        assert_eq!(response.provider_attempts[0].provider, "bad");
        assert_eq!(
            response.provider_attempts[0].status,
            WebSearchProviderAttemptStatus::Error
        );
        assert_eq!(response.provider_attempts[1].provider, "good");
        assert_eq!(
            response.provider_attempts[1].status,
            WebSearchProviderAttemptStatus::Success
        );
        assert_eq!(
            response.provider_attempts[1].provider_kind,
            Some(WebProviderKind::Searxng)
        );
        assert_eq!(
            response.provider_attempts[1].capabilities.unwrap().status,
            crate::web::WebProviderSupportStatus::Supported
        );
        assert_eq!(response.results[0].source, "good");
    }

    #[test]
    fn provider_order_deduplicates_explicit_auto_order() {
        let config = test_search_config(
            vec![(
                "good",
                test_provider(WebProviderKind::Searxng, "https://good.example"),
            )],
            vec![" good ", "bad", "good", "", " bad "],
            WebSearchMode::Fallback,
        );

        assert_eq!(provider_order("auto", &config), vec!["good", "bad"]);
    }

    #[test]
    fn provider_order_defaults_to_configured_providers() {
        let config = test_search_config(
            vec![
                (
                    "zeta",
                    test_provider(WebProviderKind::Searxng, "https://zeta.example"),
                ),
                (
                    "alpha",
                    test_provider(WebProviderKind::Searxng, "https://alpha.example"),
                ),
            ],
            vec![],
            WebSearchMode::Fallback,
        );

        assert_eq!(
            provider_order("auto", &config),
            vec!["alpha", "zeta", DUCKDUCKGO_PROVIDER_ID]
        );
    }

    #[test]
    fn provider_order_defaults_skip_unsupported_configured_providers() {
        let config = test_search_config(
            vec![
                (
                    "future",
                    test_provider(WebProviderKind::GeminiNative, "https://future.example"),
                ),
                (
                    "native",
                    test_provider(WebProviderKind::OpenAiNative, "https://native.example"),
                ),
                (
                    "searx",
                    test_provider(WebProviderKind::Searxng, "https://searx.example"),
                ),
            ],
            vec![],
            WebSearchMode::Fallback,
        );

        assert_eq!(
            provider_order("auto", &config),
            vec!["searx", DUCKDUCKGO_PROVIDER_ID]
        );
    }

    #[test]
    fn provider_order_defaults_to_capability_priority() {
        let config = test_search_config(
            vec![
                (
                    "searx",
                    test_provider(WebProviderKind::Searxng, "https://searx.example"),
                ),
                (
                    "exa",
                    test_provider(WebProviderKind::Exa, "https://exa.example"),
                ),
                (
                    "brave",
                    test_provider(WebProviderKind::Brave, "https://brave.example"),
                ),
                (
                    "tavily",
                    test_provider(WebProviderKind::Tavily, "https://tavily.example"),
                ),
            ],
            vec![],
            WebSearchMode::Fallback,
        );

        assert_eq!(
            provider_order("auto", &config),
            vec!["brave", "tavily", "exa"]
        );
    }

    #[tokio::test]
    async fn single_provider_request_does_not_fallback() {
        let good_url = searxng_mock_base_url(searxng_results_json(&[(
            "Good result",
            "https://example.com/good",
            "ok",
        )]))
        .await;
        let config = test_search_config(
            vec![
                (
                    "bad",
                    WebProviderConfig {
                        kind: WebProviderKind::Searxng,
                        base_url: None,
                        api_key: String::new(),
                        command: None,
                        output: None,
                        limits: Default::default(),
                    },
                ),
                ("good", test_provider(WebProviderKind::Searxng, &good_url)),
            ],
            vec!["bad", "good"],
            WebSearchMode::Fallback,
        );

        let err = search(
            WebSearchRequest {
                query: "test".to_string(),
                max_results: Some(5),
                provider: Some("bad".to_string()),
            },
            &config,
        )
        .await
        .unwrap_err();
        let tool_error = ToolError::from_anyhow(&err);
        assert_eq!(tool_error.kind, "provider_unavailable");
        assert_eq!(tool_error.message, "SearXNG provider requires base_url");
        let details = tool_error.details.as_ref().unwrap();
        assert_eq!(details["provider"], json!("bad"));
        assert_eq!(details["attempted_providers"], json!(["bad"]));
        assert_eq!(details["winning_provider"], serde_json::Value::Null);
        assert_eq!(details["provider_attempts"].as_array().unwrap().len(), 1);
        assert_eq!(
            details["provider_attempts"][0]["provider_kind"],
            json!("searxng")
        );
        assert_eq!(
            details["provider_attempts"][0]["capabilities"]["status"],
            json!("supported")
        );
    }

    #[tokio::test]
    async fn aggregate_mode_deduplicates_urls_and_keeps_provenance() {
        let first_url = searxng_mock_base_url(searxng_results_json(&[
            ("Shared", "https://example.com/shared", "from one"),
            ("One", "https://example.com/one", "only one"),
        ]))
        .await;
        let second_url = searxng_mock_base_url(searxng_results_json(&[
            ("Shared", "https://example.com/shared", "from two"),
            ("Two", "https://example.com/two", "only two"),
        ]))
        .await;
        let config = test_search_config(
            vec![
                ("one", test_provider(WebProviderKind::Searxng, &first_url)),
                ("two", test_provider(WebProviderKind::Searxng, &second_url)),
            ],
            vec!["one", "two"],
            WebSearchMode::Aggregate,
        );

        let response = search(
            WebSearchRequest {
                query: "test".to_string(),
                max_results: Some(5),
                provider: None,
            },
            &config,
        )
        .await
        .unwrap();

        assert_eq!(response.mode, WebSearchMode::Aggregate);
        assert_eq!(response.winning_provider, None);
        assert_eq!(response.provider_attempts.len(), 2);
        assert!(response
            .provider_attempts
            .iter()
            .all(|attempt| attempt.status == WebSearchProviderAttemptStatus::Success));
        assert_eq!(
            response
                .results
                .iter()
                .filter(|result| result.url == "https://example.com/shared")
                .count(),
            1
        );
        assert!(response
            .results
            .iter()
            .any(|result| result.url == "https://example.com/shared" && result.source == "one"));
        assert!(response
            .results
            .iter()
            .any(|result| result.url == "https://example.com/two" && result.source == "two"));
    }

    #[tokio::test]
    async fn command_provider_executes_fixed_argv_and_maps_json_results() {
        let provider = WebProviderConfig {
            kind: WebProviderKind::Command,
            base_url: None,
            api_key: String::new(),
            command: Some(crate::web::WebCommandProviderConfig {
                argv: vec![
                    "printf".into(),
                    r#"[{"title":"{{query}}","url":"https://example.com/{{max_results}}","abstract":"Snippet"}]"#.into(),
                ],
            }),
            output: Some(crate::web::WebCommandOutputConfig {
                format: crate::web::WebCommandOutputFormat::Json,
                mapping: crate::web::WebCommandResultMapping {
                    title: ".title".into(),
                    url: ".url".into(),
                    snippet: Some(".abstract".into()),
                    published_at: None,
                },
            }),
            limits: crate::web::WebProviderLimitsConfig {
                timeout_ms: 5_000,
                max_output_bytes: 10_000,
            },
        };

        let results = command_search("holon", 3, "cmd", &provider).await.unwrap();

        assert_eq!(results.results.len(), 1);
        assert_eq!(results.results[0].title, "holon");
        assert_eq!(results.results[0].url, "https://example.com/3");
        assert_eq!(results.results[0].snippet.as_deref(), Some("Snippet"));
        assert_eq!(results.results[0].source, "cmd");
    }

    #[tokio::test]
    async fn command_provider_exit_failure_is_structured_tool_error() {
        let provider = command_test_provider(vec!["false".into()]);

        let err = command_search("holon", 3, "cmd", &provider)
            .await
            .unwrap_err();
        let tool_error = ToolError::from_anyhow(&err);

        assert_eq!(tool_error.kind, "provider_unavailable");
        assert!(!tool_error.retryable);
        assert_eq!(
            tool_error.details.as_ref().unwrap()["provider"],
            json!("cmd")
        );
    }

    #[test]
    fn command_arg_expansion_does_not_reexpand_query_placeholders() {
        assert_eq!(
            expand_command_arg(
                "q={{query}}&n={{max_results}}",
                "literal {{max_results}}",
                7
            ),
            "q=literal {{max_results}}&n=7"
        );
    }

    #[test]
    fn command_result_mapping_reads_nested_paths() {
        let entry = json!({
            "meta": { "title": "Nested" },
            "url": "https://example.com"
        });

        assert_eq!(
            mapped_json_string(&entry, ".meta.title").as_deref(),
            Some("Nested")
        );
        assert_eq!(
            mapped_json_string(&entry, "url").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(mapped_json_string(&entry, ".missing.title"), None);
    }

    // ---------------------------------------------------------------------------
    // Integration tests against mock HTTP servers for API-backed providers
    // ---------------------------------------------------------------------------

    fn test_provider(kind: WebProviderKind, base_url: &str) -> WebProviderConfig {
        WebProviderConfig {
            kind,
            base_url: Some(base_url.to_string()),
            api_key: "test-key-123".to_string(),
            command: None,
            output: None,
            limits: Default::default(),
        }
    }

    fn command_test_provider(argv: Vec<String>) -> WebProviderConfig {
        WebProviderConfig {
            kind: WebProviderKind::Command,
            base_url: None,
            api_key: String::new(),
            command: Some(crate::web::WebCommandProviderConfig { argv }),
            output: Some(crate::web::WebCommandOutputConfig {
                format: crate::web::WebCommandOutputFormat::Json,
                mapping: crate::web::WebCommandResultMapping {
                    title: ".title".into(),
                    url: ".url".into(),
                    snippet: None,
                    published_at: None,
                },
            }),
            limits: Default::default(),
        }
    }

    fn test_fetch_config() -> WebFetchConfig {
        WebFetchConfig::default()
    }

    fn test_search_config(
        providers: Vec<(&str, WebProviderConfig)>,
        order: Vec<&str>,
        mode: WebSearchMode,
    ) -> WebConfig {
        WebConfig {
            fetch: test_fetch_config(),
            search: WebSearchConfig {
                mode,
                providers: order.into_iter().map(str::to_string).collect(),
                ..WebSearchConfig::default()
            },
            providers: providers
                .into_iter()
                .map(|(id, provider)| (id.to_string(), provider))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn searxng_results_json(entries: &[(&str, &str, &str)]) -> serde_json::Value {
        json!({
            "results": entries
                .iter()
                .map(|(title, url, content)| {
                    json!({
                        "title": title,
                        "url": url,
                        "content": content,
                    })
                })
                .collect::<Vec<_>>()
        })
    }

    async fn searxng_mock_base_url(results: serde_json::Value) -> String {
        let router = axum::Router::new().route(
            "/search",
            axum::routing::get(move || {
                let results = results.clone();
                async move { axum::Json(results) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{}", addr)
    }

    fn brave_results_json() -> serde_json::Value {
        serde_json::json!({
            "web": {
                "results": [
                    {
                        "title": "Brave Search",
                        "url": "https://search.brave.com",
                        "description": "Brave Search engine"
                    },
                    {
                        "title": "Brave Browser",
                        "url": "https://brave.com",
                        "description": "Privacy-focused browser"
                    }
                ]
            }
        })
    }

    fn tavily_results_json() -> serde_json::Value {
        serde_json::json!({
            "results": [
                {
                    "title": "Tavily Search",
                    "url": "https://tavily.com",
                    "content": "AI-powered search API"
                },
                {
                    "title": "Tavily Docs",
                    "url": "https://docs.tavily.com",
                    "content": "Documentation for Tavily API"
                }
            ]
        })
    }

    fn exa_results_json() -> serde_json::Value {
        serde_json::json!({
            "results": [
                {
                    "title": "Exa Search",
                    "url": "https://exa.ai",
                    "text": "Semantic search engine"
                },
                {
                    "title": "Exa Docs",
                    "url": "https://docs.exa.ai",
                    "text": "Exa API documentation"
                }
            ]
        })
    }

    fn perplexity_results_json() -> serde_json::Value {
        serde_json::json!({
            "choices": [
                {
                    "message": {
                        "content": "Perplexity summarized these search results."
                    }
                }
            ],
            "search_results": [
                {
                    "title": "Perplexity Search",
                    "url": "https://www.perplexity.ai",
                    "date": "2026-05-16"
                },
                {
                    "title": "Perplexity Docs",
                    "url": "https://docs.perplexity.ai"
                }
            ]
        })
    }

    fn firecrawl_results_json() -> serde_json::Value {
        serde_json::json!({
            "data": [
                {
                    "title": "Firecrawl Search",
                    "url": "https://www.firecrawl.dev",
                    "description": "Search and scrape API"
                },
                {
                    "title": "Firecrawl Docs",
                    "url": "https://docs.firecrawl.dev",
                    "markdown": "Firecrawl API documentation"
                }
            ]
        })
    }

    fn empty_results_json() -> serde_json::Value {
        serde_json::json!({ "results": [] })
    }

    fn empty_brave_results_json() -> serde_json::Value {
        serde_json::json!({ "web": { "results": [] } })
    }

    #[tokio::test]
    async fn brave_search_integration_success() {
        let router = axum::Router::new().route(
            "/res/v1/web/search",
            axum::routing::get(|| async { axum::Json(brave_results_json()) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Brave, &base_url);
        let results = brave_search("test", 5, "brave_test", &provider, &test_fetch_config())
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Brave Search");
        assert_eq!(results[0].url, "https://search.brave.com");
        assert_eq!(results[0].snippet.as_deref(), Some("Brave Search engine"));
        assert_eq!(results[0].source, "brave_test");
        assert_eq!(results[1].title, "Brave Browser");
    }

    #[tokio::test]
    async fn brave_search_decodes_gzip_json_response() {
        let body = gzip_json(&brave_results_json());
        let router = axum::Router::new().route(
            "/res/v1/web/search",
            axum::routing::get(move || {
                let body = body.clone();
                async move { gzip_response(body, "application/json") }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Brave, &base_url);
        let results = brave_search("test", 5, "brave_test", &provider, &test_fetch_config())
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Brave Search");
    }

    #[tokio::test]
    async fn search_response_limit_applies_after_gzip_decode() {
        let body = gzip_text(&"x".repeat(SEARCH_RESPONSE_BYTES + 1));
        let router = axum::Router::new().route(
            "/search",
            axum::routing::get(move || {
                let body = body.clone();
                async move { gzip_response(body, "text/plain") }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let response = Client::builder()
            .timeout(timeout(&test_fetch_config()))
            .build()
            .unwrap()
            .get(format!("http://{addr}/search"))
            .send()
            .await
            .unwrap();
        let error = read_search_response(response, "gzip_test")
            .await
            .unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "response_too_large");
    }

    #[tokio::test]
    async fn tavily_search_integration_success() {
        let router = axum::Router::new().route(
            "/search",
            axum::routing::post(|| async { axum::Json(tavily_results_json()) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Tavily, &base_url);
        let results = tavily_search("test", 5, "tavily_test", &provider, &test_fetch_config())
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Tavily Search");
        assert_eq!(results[1].title, "Tavily Docs");
    }

    #[tokio::test]
    async fn exa_search_integration_success() {
        let router = axum::Router::new().route(
            "/search",
            axum::routing::post(|| async { axum::Json(exa_results_json()) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Exa, &base_url);
        let results = exa_search("test", 5, "exa_test", &provider, &test_fetch_config())
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Exa Search");
        assert_eq!(results[0].url, "https://exa.ai");
        assert_eq!(
            results[0].snippet.as_deref(),
            Some("Semantic search engine")
        );
        assert_eq!(results[1].snippet.as_deref(), Some("Exa API documentation"));
    }

    #[tokio::test]
    async fn perplexity_search_integration_success() {
        let router = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(|| async { axum::Json(perplexity_results_json()) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Perplexity, &base_url);
        let results = perplexity_search(
            "test",
            5,
            "perplexity_test",
            &provider,
            &test_fetch_config(),
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Perplexity Search");
        assert_eq!(results[0].url, "https://www.perplexity.ai");
        assert_eq!(
            results[0].snippet.as_deref(),
            Some("Perplexity summarized these search results.")
        );
        assert_eq!(results[0].published_at.as_deref(), Some("2026-05-16"));
    }

    #[tokio::test]
    async fn firecrawl_search_integration_success() {
        let router = axum::Router::new().route(
            "/v1/search",
            axum::routing::post(|| async { axum::Json(firecrawl_results_json()) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Firecrawl, &base_url);
        let results =
            firecrawl_search("test", 5, "firecrawl_test", &provider, &test_fetch_config())
                .await
                .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Firecrawl Search");
        assert_eq!(
            results[1].snippet.as_deref(),
            Some("Firecrawl API documentation")
        );
    }

    #[tokio::test]
    async fn perplexity_search_empty_results_is_error() {
        let router = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(|| async { axum::Json(empty_results_json()) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Perplexity, &base_url);
        let err = perplexity_search(
            "test",
            5,
            "perplexity_test",
            &provider,
            &test_fetch_config(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no parseable search results"));
    }

    #[tokio::test]
    async fn perplexity_search_invalid_json_is_error() {
        let router = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(|| async { "not json" }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Perplexity, &base_url);
        let err = perplexity_search(
            "test",
            5,
            "perplexity_test",
            &provider,
            &test_fetch_config(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("invalid JSON"));
    }

    #[tokio::test]
    async fn perplexity_search_http_401_is_error() {
        let router = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(|| async { axum::http::StatusCode::UNAUTHORIZED }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Perplexity, &base_url);
        let err = perplexity_search(
            "test",
            5,
            "perplexity_test",
            &provider,
            &test_fetch_config(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("HTTP 401"));
    }

    #[tokio::test]
    async fn perplexity_search_http_429_is_error() {
        let router = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Perplexity, &base_url);
        let err = perplexity_search(
            "test",
            5,
            "perplexity_test",
            &provider,
            &test_fetch_config(),
        )
        .await
        .unwrap_err();
        let tool_error = ToolError::from_anyhow(&err);
        assert_eq!(tool_error.kind, "rate_limited");
    }

    #[tokio::test]
    async fn perplexity_search_missing_api_key_is_error() {
        let provider = WebProviderConfig {
            kind: WebProviderKind::Perplexity,
            base_url: Some("http://localhost:1".to_string()),
            api_key: String::new(),
            command: None,
            output: None,
            limits: Default::default(),
        };

        let err = perplexity_search(
            "test",
            5,
            "perplexity_test",
            &provider,
            &test_fetch_config(),
        )
        .await
        .unwrap_err();
        let tool_error = ToolError::from_anyhow(&err);
        assert_eq!(tool_error.kind, "provider_unavailable");
        assert!(err.to_string().contains("API key"));
    }

    #[tokio::test]
    async fn firecrawl_search_empty_results_is_error() {
        let router = axum::Router::new().route(
            "/v1/search",
            axum::routing::post(|| async { axum::Json(empty_results_json()) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Firecrawl, &base_url);
        let err = firecrawl_search("test", 5, "firecrawl_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no parseable search results"));
    }

    #[tokio::test]
    async fn firecrawl_search_invalid_json_is_error() {
        let router =
            axum::Router::new().route("/v1/search", axum::routing::post(|| async { "not json" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Firecrawl, &base_url);
        let err = firecrawl_search("test", 5, "firecrawl_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid JSON"));
    }

    #[tokio::test]
    async fn firecrawl_search_http_401_is_error() {
        let router = axum::Router::new().route(
            "/v1/search",
            axum::routing::post(|| async { axum::http::StatusCode::UNAUTHORIZED }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Firecrawl, &base_url);
        let err = firecrawl_search("test", 5, "firecrawl_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("HTTP 401"));
    }

    #[tokio::test]
    async fn firecrawl_search_http_429_is_error() {
        let router = axum::Router::new().route(
            "/v1/search",
            axum::routing::post(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Firecrawl, &base_url);
        let err = firecrawl_search("test", 5, "firecrawl_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        let tool_error = ToolError::from_anyhow(&err);
        assert_eq!(tool_error.kind, "rate_limited");
    }

    #[tokio::test]
    async fn firecrawl_search_missing_api_key_is_error() {
        let provider = WebProviderConfig {
            kind: WebProviderKind::Firecrawl,
            base_url: Some("http://localhost:1".to_string()),
            api_key: String::new(),
            command: None,
            output: None,
            limits: Default::default(),
        };

        let err = firecrawl_search("test", 5, "firecrawl_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        let tool_error = ToolError::from_anyhow(&err);
        assert_eq!(tool_error.kind, "provider_unavailable");
        assert!(err.to_string().contains("API key"));
    }

    #[tokio::test]
    async fn brave_search_empty_results_is_error() {
        let router = axum::Router::new().route(
            "/res/v1/web/search",
            axum::routing::get(|| async { axum::Json(empty_brave_results_json()) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Brave, &base_url);
        let err = brave_search("test", 5, "brave_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        assert!(
            format!("{err}").contains("no parseable search results"),
            "expected empty results error, got: {err}"
        );
    }

    #[tokio::test]
    async fn brave_search_http_401_is_error() {
        let router = axum::Router::new().route(
            "/res/v1/web/search",
            axum::routing::get(|| async { axum::http::StatusCode::UNAUTHORIZED }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Brave, &base_url);
        let err = brave_search("test", 5, "brave_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        assert!(
            format!("{err}").contains("HTTP 401"),
            "expected HTTP 401 error, got: {err}"
        );
    }

    #[tokio::test]
    async fn brave_search_http_429_is_error() {
        let router = axum::Router::new().route(
            "/res/v1/web/search",
            axum::routing::get(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Brave, &base_url);
        let err = brave_search("test", 5, "brave_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        let tool_error = ToolError::from_anyhow(&err);
        assert_eq!(tool_error.kind, "rate_limited");
    }

    #[tokio::test]
    async fn brave_search_missing_api_key_is_error() {
        let provider = WebProviderConfig {
            kind: WebProviderKind::Brave,
            base_url: Some("http://localhost:1".to_string()),
            api_key: String::new(),
            command: None,
            output: None,
            limits: Default::default(),
        };
        let err = brave_search("test", 5, "brave_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        let tool_error = ToolError::from_anyhow(&err);
        assert_eq!(tool_error.kind, "provider_unavailable");
        assert!(
            format!("{err}").contains("API key"),
            "expected API key error, got: {err}"
        );
    }

    #[tokio::test]
    async fn tavily_search_invalid_json_is_error() {
        let router =
            axum::Router::new().route("/search", axum::routing::post(|| async { "not json" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Tavily, &base_url);
        let err = tavily_search("test", 5, "tavily_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        assert!(
            format!("{err}").contains("invalid JSON"),
            "expected invalid JSON error, got: {err}"
        );
    }

    #[tokio::test]
    async fn exa_search_empty_results_is_error() {
        let router = axum::Router::new().route(
            "/search",
            axum::routing::post(|| async { axum::Json(empty_results_json()) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{}", addr);

        let provider = test_provider(WebProviderKind::Exa, &base_url);
        let err = exa_search("test", 5, "exa_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        assert!(
            format!("{err}").contains("no parseable search results"),
            "expected empty results error, got: {err}"
        );
    }

    // ---------------------------------------------------------------------------
    // Real API integration tests (opt-in, requires API keys)
    // ---------------------------------------------------------------------------

    /// Real Brave Search API integration test.
    /// Set BRAVE_API_KEY env var to run: BRAVE_API_KEY=... cargo test brave_search_live -- --ignored
    #[tokio::test]
    #[ignore = "requires BRAVE_API_KEY env var and network access"]
    async fn brave_search_live_integration() {
        let api_key = std::env::var("BRAVE_API_KEY").ok();
        if api_key.is_none() {
            eprintln!("SKIP: BRAVE_API_KEY not set");
            return;
        }
        let api_key = api_key.unwrap();
        assert!(!api_key.is_empty(), "BRAVE_API_KEY is empty");

        let provider = WebProviderConfig {
            kind: WebProviderKind::Brave,
            base_url: None, // use default https://api.search.brave.com
            api_key,
            command: None,
            output: None,
            limits: Default::default(),
        };
        let fetch_config = test_fetch_config();

        let results = brave_search(
            "Rust programming language",
            3,
            "brave_live",
            &provider,
            &fetch_config,
        )
        .await
        .expect("Brave live search should succeed");

        eprintln!("Brave live search returned {} results", results.len());
        for (i, r) in results.iter().enumerate() {
            eprintln!(
                "  [{i}] title={} url={} snippet={:?}",
                r.title, r.url, r.snippet
            );
        }
        assert!(
            !results.is_empty(),
            "Brave live search should return at least 1 result"
        );
        assert!(
            !results[0].title.is_empty(),
            "first result should have a title"
        );
        assert!(!results[0].url.is_empty(), "first result should have a url");
    }

    fn gzip_json(value: &serde_json::Value) -> Vec<u8> {
        gzip_text(&value.to_string())
    }

    fn gzip_text(text: &str) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(text.as_bytes()).unwrap();
        encoder.finish().unwrap()
    }

    fn gzip_response(body: Vec<u8>, content_type: &'static str) -> Response {
        let mut response = Response::new(Body::from(body));
        response
            .headers_mut()
            .insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        response
    }
}
