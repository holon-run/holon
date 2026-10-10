use std::error::Error;

use reqwest::StatusCode;
use serde::Serialize;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::time::Duration;

use super::{
    http_trace::ProviderHttpTraceRequest, ProviderFallbackDisposition,
    ProviderTransportDiagnostics, ReqwestTransportDiagnostics,
};
use crate::types::TokenUsage;

pub(crate) const PROVIDER_MAX_RETRIES: usize = 2;
pub(crate) const PROVIDER_RATE_LIMIT_MAX_RETRIES: usize = 3;
const PROVIDER_RETRY_BASE_BACKOFF_MS: u64 = 200;
const PROVIDER_SERVER_ERROR_RETRY_BASE_BACKOFF_MS: u64 = 2_000;
const PROVIDER_RETRY_JITTER_MAX_PERCENT: u64 = 25;
pub(crate) const PROVIDER_RETRY_SERVER_HINT_CAP_MS: u64 = 30_000;
pub(crate) const PROVIDER_RECOVERY_BASE_BACKOFF_MS: u64 = 2_000;
pub(crate) const PROVIDER_RECOVERY_MAX_BACKOFF_MS: u64 = 30_000;
pub(crate) const PROVIDER_RECOVERY_MAX_FALLBACKS: usize = 2;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProviderFailureKind {
    Timeout,
    Connection,
    CredentialRefreshBusy,
    RateLimited,
    ServerError,
    EmptyResponse,
    AuthError,
    ContractError,
    InvalidResponse,
    UnsupportedTransport,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RetryDisposition {
    Retryable,
    FailFast,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct ProviderFailureClassification {
    pub kind: ProviderFailureKind,
    pub disposition: RetryDisposition,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub(crate) struct ProviderTransportError {
    pub classification: ProviderFailureClassification,
    pub code: Option<String>,
    pub status: Option<u16>,
    pub retry_after: Option<Duration>,
    pub diagnostics: Option<ProviderTransportDiagnostics>,
    pub token_usage: Option<TokenUsage>,
    message: String,
}

pub(crate) fn set_provider_transport_streaming(
    mut error: anyhow::Error,
    streaming: bool,
) -> anyhow::Error {
    if let Some(transport_error) = error.downcast_mut::<ProviderTransportError>() {
        if let Some(diagnostics) = transport_error.diagnostics.as_mut() {
            diagnostics.streaming = Some(streaming);
        }
    }
    error
}

pub(crate) fn set_provider_transport_quota_identity(
    mut error: anyhow::Error,
    identity: super::ProviderQuotaIdentity,
) -> anyhow::Error {
    if let Some(transport_error) = error.downcast_mut::<ProviderTransportError>() {
        if let Some(diagnostics) = transport_error.diagnostics.as_mut() {
            diagnostics.quota_identity = Some(identity);
        }
    }
    error
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderRetryDelaySource {
    ServerRetryAfter,
    ServerErrorExponentialBackoff,
    ComputedBackoff,
}

impl ProviderRetryDelaySource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ServerRetryAfter => "server_retry_after",
            Self::ServerErrorExponentialBackoff => "server_error_exponential_backoff",
            Self::ComputedBackoff => "computed_backoff",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderRetryDelay {
    Wait {
        backoff: Duration,
        source: ProviderRetryDelaySource,
    },
    SkipToFallback,
}

impl ProviderFailureKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connection => "connection",
            Self::CredentialRefreshBusy => "credential_refresh_busy",
            Self::RateLimited => "rate_limited",
            Self::ServerError => "server_error",
            Self::EmptyResponse => "empty_response",
            Self::AuthError => "auth_error",
            Self::ContractError => "contract_error",
            Self::InvalidResponse => "invalid_response",
            Self::UnsupportedTransport => "unsupported_transport",
            Self::Unknown => "unknown",
        }
    }
}

impl RetryDisposition {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Retryable => "retryable",
            Self::FailFast => "fail_fast",
        }
    }
}

pub(crate) fn provider_fallback_disposition(
    kind: ProviderFailureKind,
) -> ProviderFallbackDisposition {
    match kind {
        ProviderFailureKind::Timeout | ProviderFailureKind::Connection => {
            ProviderFallbackDisposition::Deferred
        }
        _ => ProviderFallbackDisposition::Immediate,
    }
}

pub(crate) fn provider_retry_policy_json() -> Value {
    json!({
        "max_retries_per_provider": PROVIDER_MAX_RETRIES,
        "max_attempts_per_provider": provider_max_attempts(),
        "max_rate_limit_retries": PROVIDER_RATE_LIMIT_MAX_RETRIES,
        "base_backoff_ms": PROVIDER_RETRY_BASE_BACKOFF_MS,
        "server_error_base_backoff_ms": PROVIDER_SERVER_ERROR_RETRY_BASE_BACKOFF_MS,
        "server_error_jitter_max_percent": PROVIDER_RETRY_JITTER_MAX_PERCENT,
        "server_error_backoff_cap_ms": PROVIDER_RETRY_SERVER_HINT_CAP_MS,
        "server_hint_cap_ms": PROVIDER_RETRY_SERVER_HINT_CAP_MS,
        "server_hint_semantics": "429 Retry-After is capped and retried in the current provider turn; 503 hints above the cap skip remaining retries and defer to fallback",
        "retryable_failure_kinds": [
            ProviderFailureKind::Timeout.as_str(),
            ProviderFailureKind::Connection.as_str(),
            ProviderFailureKind::CredentialRefreshBusy.as_str(),
            ProviderFailureKind::RateLimited.as_str(),
            ProviderFailureKind::ServerError.as_str(),
            ProviderFailureKind::EmptyResponse.as_str(),
        ],
        "fail_fast_failure_kinds": [
            ProviderFailureKind::AuthError.as_str(),
            ProviderFailureKind::ContractError.as_str(),
            ProviderFailureKind::InvalidResponse.as_str(),
            ProviderFailureKind::UnsupportedTransport.as_str(),
            ProviderFailureKind::Unknown.as_str(),
        ],
        "fallback": {
            "max_lineage_fallbacks": PROVIDER_RECOVERY_MAX_FALLBACKS,
            "deferred_base_backoff_ms": PROVIDER_RECOVERY_BASE_BACKOFF_MS,
            "deferred_max_backoff_ms": PROVIDER_RECOVERY_MAX_BACKOFF_MS,
            "deferred_failure_kinds": [
                ProviderFailureKind::Timeout.as_str(),
                ProviderFailureKind::Connection.as_str(),
            ],
        },
    })
}

pub(crate) fn provider_max_attempts() -> usize {
    PROVIDER_MAX_RETRIES + 1
}

pub(crate) fn provider_retry_backoff(attempt: usize) -> Duration {
    Duration::from_millis(PROVIDER_RETRY_BASE_BACKOFF_MS * attempt as u64)
}

pub(crate) fn provider_retry_jitter_seed(provider_name: &str, model_ref: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in provider_name.bytes().chain([0]).chain(model_ref.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn provider_server_error_retry_backoff(attempt: usize, jitter_seed: u64) -> Duration {
    let exponent = attempt.saturating_sub(1).min(63);
    let exponential = PROVIDER_SERVER_ERROR_RETRY_BASE_BACKOFF_MS
        .saturating_mul(1u64 << exponent)
        .min(PROVIDER_RETRY_SERVER_HINT_CAP_MS);
    let jitter_cap = exponential
        .saturating_mul(PROVIDER_RETRY_JITTER_MAX_PERCENT)
        .checked_div(100)
        .unwrap_or_default()
        .min(PROVIDER_RETRY_SERVER_HINT_CAP_MS.saturating_sub(exponential));
    let jitter = if jitter_cap == 0 {
        0
    } else {
        let mut mixed = jitter_seed ^ (attempt as u64).wrapping_mul(0x9e3779b97f4a7c15);
        mixed ^= mixed >> 30;
        mixed = mixed.wrapping_mul(0xbf58476d1ce4e5b9);
        mixed ^= mixed >> 27;
        mixed = mixed.wrapping_mul(0x94d049bb133111eb);
        mixed ^= mixed >> 31;
        mixed % (jitter_cap + 1)
    };
    Duration::from_millis(exponential.saturating_add(jitter))
}

pub(crate) fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers
        .get(&reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if raw.is_empty() {
        return None;
    }
    // RFC 9110 delta-seconds: a non-negative integer; zero means no wait is
    // required, so fall back to the computed backoff.
    if let Ok(seconds) = raw.parse::<u64>() {
        let duration = Duration::from_secs(seconds);
        return (!duration.is_zero()).then_some(duration);
    }
    let date = chrono::DateTime::parse_from_rfc2822(raw).ok()?;
    date.with_timezone(&chrono::Utc)
        .signed_duration_since(chrono::Utc::now())
        .to_std()
        .ok()
        .filter(|duration| !duration.is_zero())
}

pub(crate) fn provider_retry_delay(
    attempt: usize,
    kind: ProviderFailureKind,
    retry_after: Option<Duration>,
    jitter_seed: u64,
) -> ProviderRetryDelay {
    let (computed, computed_source) = if kind == ProviderFailureKind::ServerError {
        (
            provider_server_error_retry_backoff(attempt, jitter_seed),
            ProviderRetryDelaySource::ServerErrorExponentialBackoff,
        )
    } else if kind == ProviderFailureKind::RateLimited {
        (
            provider_retry_backoff(attempt)
                .min(Duration::from_millis(PROVIDER_RETRY_SERVER_HINT_CAP_MS)),
            ProviderRetryDelaySource::ComputedBackoff,
        )
    } else {
        (
            provider_retry_backoff(attempt),
            ProviderRetryDelaySource::ComputedBackoff,
        )
    };
    // Retry-After is a server-side throttle hint; only the kinds that carry
    // that semantic (429 rate limits and 5xx server errors) may extend the wait.
    let server_hint = match kind {
        ProviderFailureKind::RateLimited | ProviderFailureKind::ServerError => retry_after,
        _ => None,
    };
    let Some(server_hint) = server_hint else {
        return ProviderRetryDelay::Wait {
            backoff: computed,
            source: computed_source,
        };
    };
    if server_hint > Duration::from_millis(PROVIDER_RETRY_SERVER_HINT_CAP_MS) {
        if kind == ProviderFailureKind::RateLimited {
            return ProviderRetryDelay::Wait {
                backoff: Duration::from_millis(PROVIDER_RETRY_SERVER_HINT_CAP_MS),
                source: ProviderRetryDelaySource::ServerRetryAfter,
            };
        }
        return ProviderRetryDelay::SkipToFallback;
    }
    ProviderRetryDelay::Wait {
        backoff: server_hint.max(computed),
        source: ProviderRetryDelaySource::ServerRetryAfter,
    }
}

pub(crate) fn classify_provider_error(error: &anyhow::Error) -> ProviderFailureClassification {
    error
        .downcast_ref::<ProviderTransportError>()
        .map(|error| error.classification)
        .unwrap_or(ProviderFailureClassification {
            kind: ProviderFailureKind::Unknown,
            disposition: RetryDisposition::FailFast,
        })
}

pub(crate) fn provider_error_retry_after(error: &anyhow::Error) -> Option<Duration> {
    error
        .downcast_ref::<ProviderTransportError>()
        .and_then(|error| error.retry_after)
}

pub(crate) fn provider_transport_error(
    classification: ProviderFailureClassification,
    status: Option<u16>,
    diagnostics: Option<ProviderTransportDiagnostics>,
    message: impl Into<String>,
) -> anyhow::Error {
    provider_transport_error_with_code(classification, None, status, diagnostics, message)
}

fn provider_transport_error_with_evidence(
    classification: ProviderFailureClassification,
    code: Option<&str>,
    status: Option<u16>,
    diagnostics: Option<ProviderTransportDiagnostics>,
    token_usage: Option<TokenUsage>,
    retry_after: Option<Duration>,
    message: impl Into<String>,
) -> anyhow::Error {
    ProviderTransportError {
        classification,
        code: code.map(ToString::to_string),
        status,
        diagnostics,
        token_usage,
        retry_after,
        message: message.into(),
    }
    .into()
}

pub(crate) fn provider_transport_error_with_code(
    classification: ProviderFailureClassification,
    code: Option<&str>,
    status: Option<u16>,
    diagnostics: Option<ProviderTransportDiagnostics>,
    message: impl Into<String>,
) -> anyhow::Error {
    provider_transport_error_with_evidence(
        classification,
        code,
        status,
        diagnostics,
        None,
        None,
        message,
    )
}

pub(crate) fn provider_transport_error_with_code_and_retry_after(
    classification: ProviderFailureClassification,
    code: Option<&str>,
    status: Option<u16>,
    diagnostics: Option<ProviderTransportDiagnostics>,
    retry_after: Option<Duration>,
    message: impl Into<String>,
) -> anyhow::Error {
    provider_transport_error_with_evidence(
        classification,
        code,
        status,
        diagnostics,
        None,
        retry_after,
        message,
    )
}

pub(crate) fn classify_reqwest_transport_error_with_trace(
    context: &str,
    stage: &str,
    provider: &str,
    model_ref: Option<&str>,
    url: Option<&str>,
    error: reqwest::Error,
    trace: Option<&ProviderHttpTraceRequest>,
) -> anyhow::Error {
    let status = error.status().map(|status| status.as_u16());
    let source_chain = error_chain_messages(&error);
    let classification = classify_reqwest_transport_failure(stage, &error, &source_chain);
    let message = format_reqwest_transport_error_message(
        context,
        stage,
        classification.kind == ProviderFailureKind::Timeout,
        error.to_string(),
    );
    provider_transport_error(
        classification,
        status,
        Some(reqwest_transport_diagnostics(
            stage,
            provider,
            model_ref,
            url,
            &error,
            source_chain,
            trace,
        )),
        message,
    )
}

fn format_reqwest_transport_error_message(
    context: &str,
    stage: &str,
    timed_out: bool,
    raw_error: String,
) -> String {
    if timed_out {
        let body_label = match stage {
            "streaming_response_body" => Some("the streaming response body"),
            "response_body" => Some("the response body"),
            _ => None,
        };
        if let Some(body_label) = body_label {
            return format!("{context}: timed out while reading {body_label}");
        }
    }
    format!("{context}: {raw_error}")
}

fn classify_reqwest_transport_failure(
    stage: &str,
    error: &reqwest::Error,
    source_chain: &[String],
) -> ProviderFailureClassification {
    if error.is_timeout() {
        ProviderFailureClassification {
            kind: ProviderFailureKind::Timeout,
            disposition: RetryDisposition::Retryable,
        }
    } else if error.is_connect() {
        ProviderFailureClassification {
            kind: ProviderFailureKind::Connection,
            disposition: RetryDisposition::Retryable,
        }
    } else if is_retryable_request_send_transport_failure(stage, source_chain)
        || is_retryable_response_body_read_interruption(stage, error, source_chain)
    {
        ProviderFailureClassification {
            kind: ProviderFailureKind::Connection,
            disposition: RetryDisposition::Retryable,
        }
    } else {
        ProviderFailureClassification {
            kind: ProviderFailureKind::Unknown,
            disposition: RetryDisposition::FailFast,
        }
    }
}

fn is_retryable_request_send_transport_failure(stage: &str, source_chain: &[String]) -> bool {
    if !matches!(stage, "request_send" | "streaming_request_send") {
        return false;
    }

    source_chain.iter().any(|message| {
        let message = message.to_ascii_lowercase();
        message.contains("connection error")
            || message.contains("connection closed")
            || message.contains("connection reset")
            || message.contains("connection aborted")
            || message.contains("tls close_notify")
            || message.contains("broken pipe")
    })
}

fn is_retryable_response_body_read_interruption(
    stage: &str,
    error: &reqwest::Error,
    source_chain: &[String],
) -> bool {
    if !matches!(stage, "response_body" | "streaming_response_body") {
        return false;
    }
    if !(error.is_body() || error.is_decode()) {
        return false;
    }

    source_chain.iter().any(|message| {
        let message = message.to_ascii_lowercase();
        message.contains("unexpected eof")
            || message.contains("end of file")
            || message.contains("connection reset")
            || message.contains("connection closed")
            || message.contains("connection aborted")
            || message.contains("broken pipe")
            || message.contains("incomplete message")
            || message.contains("error reading a body from connection")
            || message.contains("chunk size")
            || message.contains("request or response body error")
    })
}

pub(crate) fn classify_status_error_with_trace(
    context: &str,
    stage: &str,
    provider: Option<&str>,
    model_ref: Option<&str>,
    url: Option<&str>,
    status: StatusCode,
    body: String,
    trace: Option<&ProviderHttpTraceRequest>,
    retry_after: Option<Duration>,
) -> anyhow::Error {
    let detail = extract_upstream_error_detail(&body);
    let deterministic_error =
        is_known_deterministic_provider_error(detail.as_ref(), Some(body.as_str()));
    let classification = match status {
        StatusCode::TOO_MANY_REQUESTS => ProviderFailureClassification {
            kind: ProviderFailureKind::RateLimited,
            disposition: RetryDisposition::Retryable,
        },
        _ if deterministic_error => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderFailureClassification {
            kind: ProviderFailureKind::AuthError,
            disposition: RetryDisposition::FailFast,
        },
        _ if status.is_server_error() => ProviderFailureClassification {
            kind: ProviderFailureKind::ServerError,
            disposition: RetryDisposition::Retryable,
        },
        _ if status.is_client_error() => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        _ => ProviderFailureClassification {
            kind: ProviderFailureKind::Unknown,
            disposition: RetryDisposition::FailFast,
        },
    };
    let code = detail
        .as_ref()
        .and_then(|detail| detail.code.as_deref())
        .or_else(|| status_error_code(&body));
    let detail_message = detail
        .as_ref()
        .map(format_upstream_error_detail)
        .filter(|detail| !detail.is_empty())
        .map(|detail| format!(": {detail}"))
        .unwrap_or_default();
    let tool_protocol_hint = (status == StatusCode::BAD_REQUEST
        && is_explicit_tool_protocol_rejection(detail.as_ref()))
        .then_some(
            ". This endpoint explicitly rejected the requested tool protocol; update the provider/model/endpoint or builtin_web_search configuration. Holon will not remove the tool or retry with a degraded request.",
        )
        .unwrap_or_default();
    provider_transport_error_with_code_and_retry_after(
        classification,
        code,
        Some(status.as_u16()),
        Some(ProviderTransportDiagnostics {
            stage: stage.to_string(),
            streaming: None,
            provider: provider.map(ToString::to_string),
            model_ref: model_ref.map(ToString::to_string),
            url: url.map(sanitize_transport_url),
            status: Some(status.as_u16()),
            reqwest: None,
            context_budget: None,
            http_trace: trace.and_then(|trace| trace.diagnostics(Some(status.as_u16()))),
            quota_identity: None,
            source_chain: status_error_source_chain(provider, status),
        }),
        retry_after,
        format!("{context} with status {status}{detail_message}{tool_protocol_hint}"),
    )
}

fn status_error_code(body: &str) -> Option<&'static str> {
    body.contains("Items are not persisted when `store` is set to false")
        .then_some("non_persisted_item_id")
}

const MAX_UPSTREAM_ERROR_BODY_BYTES: usize = 16 * 1024;
const MAX_UPSTREAM_ERROR_FIELD_CHARS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpstreamErrorDetail {
    pub error_type: Option<String>,
    pub code: Option<String>,
    pub message: Option<String>,
}

pub(crate) fn is_known_deterministic_provider_error(
    detail: Option<&UpstreamErrorDetail>,
    raw_text: Option<&str>,
) -> bool {
    raw_text.is_some_and(is_known_deterministic_provider_error_text)
        || detail.is_some_and(|detail| {
            detail
                .error_type
                .as_deref()
                .is_some_and(is_known_deterministic_provider_error_text)
                || detail
                    .code
                    .as_deref()
                    .is_some_and(is_known_deterministic_provider_error_text)
                || detail
                    .message
                    .as_deref()
                    .is_some_and(is_known_deterministic_provider_error_text)
        })
}

pub(crate) fn is_known_deterministic_provider_error_text(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "no user query found in messages",
        "context_length_exceeded",
        "context length exceeded",
        "maximum context length",
        "context window exceeded",
        "context window is too small",
        "prompt is too long",
        "input is too long",
        "too many tokens",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

pub(crate) fn is_explicit_tool_protocol_rejection(detail: Option<&UpstreamErrorDetail>) -> bool {
    detail.is_some_and(|detail| {
        detail
            .code
            .as_deref()
            .is_some_and(|code| matches!(code, "unsupported_tool" | "tool_not_supported"))
    })
}

pub(crate) fn extract_upstream_error_detail(body: &str) -> Option<UpstreamErrorDetail> {
    if body.len() > MAX_UPSTREAM_ERROR_BODY_BYTES {
        return None;
    }
    let value = serde_json::from_str::<Value>(body).ok()?;
    extract_upstream_error_detail_from_value(&value)
}

pub(crate) fn extract_upstream_error_detail_from_value(
    value: &Value,
) -> Option<UpstreamErrorDetail> {
    let object = if let Some(error) = value.get("error") {
        error.as_object()?
    } else {
        value.as_object()?
    };

    let detail = UpstreamErrorDetail {
        error_type: object
            .get("type")
            .and_then(Value::as_str)
            .and_then(sanitize_upstream_error_text),
        code: object
            .get("code")
            .and_then(Value::as_str)
            .and_then(sanitize_upstream_error_text),
        message: object
            .get("message")
            .and_then(Value::as_str)
            .and_then(sanitize_upstream_error_text),
    };
    (detail.error_type.is_some() || detail.code.is_some() || detail.message.is_some())
        .then_some(detail)
}

pub(crate) fn format_upstream_error_detail(detail: &UpstreamErrorDetail) -> String {
    let mut fields = Vec::new();
    if let Some(error_type) = detail.error_type.as_deref() {
        fields.push(format!("type={error_type}"));
    }
    if let Some(code) = detail.code.as_deref() {
        fields.push(format!("code={code}"));
    }
    if let Some(message) = detail.message.as_deref() {
        fields.push(format!("message={message}"));
    }
    fields.join(", ")
}

fn sanitize_upstream_error_text(raw: &str) -> Option<String> {
    let mut sanitized = String::new();
    let mut redact_tokens = 0usize;
    for token in raw.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        let token = if redact_tokens > 0 {
            redact_tokens -= 1;
            if lower == "bearer" {
                redact_tokens = 1;
                token.to_string()
            } else {
                "[REDACTED]".to_string()
            }
        } else if lower == "bearer" {
            redact_tokens = 1;
            token.to_string()
        } else if lower == "authorization:" || lower == "authorization" {
            redact_tokens = 2;
            token.to_string()
        } else if let Some((key, _)) = token.split_once('=') {
            if matches!(
                key.to_ascii_lowercase().as_str(),
                "api_key"
                    | "apikey"
                    | "access_token"
                    | "refresh_token"
                    | "token"
                    | "authorization"
                    | "cookie"
            ) {
                format!("{key}=[REDACTED]")
            } else {
                token.to_string()
            }
        } else if let Some(query_start) = token.find('?') {
            format!("{}?[REDACTED]", &token[..query_start])
        } else {
            token.to_string()
        };
        if !sanitized.is_empty() {
            sanitized.push(' ');
        }
        sanitized.push_str(&token);
    }
    let sanitized = sanitized.trim();
    if sanitized.is_empty() {
        return None;
    }
    let mut bounded = sanitized
        .chars()
        .take(MAX_UPSTREAM_ERROR_FIELD_CHARS)
        .collect::<String>();
    if sanitized.chars().count() > MAX_UPSTREAM_ERROR_FIELD_CHARS {
        bounded.push('…');
    }
    Some(bounded)
}

fn status_error_source_chain(provider: Option<&str>, status: StatusCode) -> Vec<String> {
    if provider == Some("openai-codex")
        && matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
    {
        return vec![
            "OpenAI Codex uses Codex CLI authentication.".into(),
            "The Codex CLI access token may be expired or revoked.".into(),
            "Run `codex login`, then retry.".into(),
        ];
    }
    Vec::new()
}

pub(crate) fn invalid_response_error(
    context: &str,
    error: impl std::fmt::Display,
) -> anyhow::Error {
    provider_transport_error(
        ProviderFailureClassification {
            kind: ProviderFailureKind::InvalidResponse,
            disposition: RetryDisposition::FailFast,
        },
        None,
        None,
        format!("{context}: {error}"),
    )
}

pub(crate) fn invalid_response_error_with_trace(
    context: &str,
    stage: &str,
    provider: &str,
    model_ref: Option<&str>,
    url: Option<&str>,
    error: impl std::fmt::Display,
    trace: Option<&ProviderHttpTraceRequest>,
) -> anyhow::Error {
    let error = error.to_string();
    provider_transport_error(
        ProviderFailureClassification {
            kind: ProviderFailureKind::InvalidResponse,
            disposition: RetryDisposition::FailFast,
        },
        None,
        Some(ProviderTransportDiagnostics {
            stage: stage.to_string(),
            streaming: None,
            provider: Some(provider.to_string()),
            model_ref: model_ref.map(ToString::to_string),
            url: url.map(sanitize_transport_url),
            status: None,
            reqwest: None,
            context_budget: None,
            http_trace: trace.and_then(|trace| trace.diagnostics(None)),
            quota_identity: None,
            source_chain: vec![error.clone()],
        }),
        format!("{context}: {error}"),
    )
}

/// #2902: provider returned a structurally invalid but plausibly transient
/// response (e.g. `stop_reason=tool_use` with zero tool-call blocks). Unlike
/// `invalid_response_error_with_trace`, the same provider chain is retried
/// before the failure surfaces.
pub(crate) fn retryable_invalid_response_error_with_trace(
    context: &str,
    stage: &str,
    provider: &str,
    model_ref: Option<&str>,
    url: Option<&str>,
    error: impl std::fmt::Display,
    trace: Option<&ProviderHttpTraceRequest>,
    token_usage: TokenUsage,
) -> anyhow::Error {
    let error = error.to_string();
    provider_transport_error_with_evidence(
        ProviderFailureClassification {
            kind: ProviderFailureKind::InvalidResponse,
            disposition: RetryDisposition::Retryable,
        },
        None,
        None,
        Some(ProviderTransportDiagnostics {
            stage: stage.to_string(),
            streaming: None,
            provider: Some(provider.to_string()),
            model_ref: model_ref.map(ToString::to_string),
            url: url.map(sanitize_transport_url),
            status: None,
            reqwest: None,
            context_budget: None,
            http_trace: trace.and_then(|trace| trace.diagnostics(None)),
            quota_identity: None,
            source_chain: vec![error.clone()],
        }),
        Some(token_usage),
        None,
        format!("{context}: {error}"),
    )
}

pub(crate) fn empty_response_error(
    context: &str,
    error: impl std::fmt::Display,
    token_usage: TokenUsage,
) -> anyhow::Error {
    provider_transport_error_with_evidence(
        ProviderFailureClassification {
            kind: ProviderFailureKind::EmptyResponse,
            disposition: RetryDisposition::Retryable,
        },
        None,
        None,
        None,
        Some(token_usage),
        None,
        format!("{context}: {error}"),
    )
}

pub(crate) fn empty_response_error_with_trace(
    context: &str,
    stage: &str,
    provider: &str,
    model_ref: Option<&str>,
    url: Option<&str>,
    error: impl std::fmt::Display,
    trace: Option<&ProviderHttpTraceRequest>,
    token_usage: TokenUsage,
) -> anyhow::Error {
    let error = error.to_string();
    provider_transport_error_with_evidence(
        ProviderFailureClassification {
            kind: ProviderFailureKind::EmptyResponse,
            disposition: RetryDisposition::Retryable,
        },
        None,
        None,
        Some(ProviderTransportDiagnostics {
            stage: stage.to_string(),
            streaming: None,
            provider: Some(provider.to_string()),
            model_ref: model_ref.map(ToString::to_string),
            url: url.map(sanitize_transport_url),
            status: None,
            reqwest: None,
            context_budget: None,
            http_trace: trace.and_then(|trace| trace.diagnostics(None)),
            quota_identity: None,
            source_chain: vec![error.clone()],
        }),
        Some(token_usage),
        None,
        format!("{context}: {error}"),
    )
}

pub(crate) fn timeout_transport_error_with_trace(
    context: &str,
    stage: &str,
    provider: &str,
    model_ref: Option<&str>,
    url: Option<&str>,
    reason: impl Into<String>,
    trace: Option<&ProviderHttpTraceRequest>,
) -> anyhow::Error {
    provider_transport_error(
        ProviderFailureClassification {
            kind: ProviderFailureKind::Timeout,
            disposition: RetryDisposition::Retryable,
        },
        None,
        Some(ProviderTransportDiagnostics {
            stage: stage.to_string(),
            streaming: None,
            provider: Some(provider.to_string()),
            model_ref: model_ref.map(ToString::to_string),
            url: url.map(sanitize_transport_url),
            status: None,
            reqwest: None,
            context_budget: None,
            http_trace: trace.and_then(|trace| trace.diagnostics(None)),
            quota_identity: None,
            source_chain: vec![reason.into()],
        }),
        context.to_string(),
    )
}

pub(crate) fn sanitize_transport_url(raw: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(raw) else {
        return raw.to_string();
    };

    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);

    url.to_string()
}

fn reqwest_transport_diagnostics(
    stage: &str,
    provider: &str,
    model_ref: Option<&str>,
    url: Option<&str>,
    error: &reqwest::Error,
    source_chain: Vec<String>,
    trace: Option<&ProviderHttpTraceRequest>,
) -> ProviderTransportDiagnostics {
    let status = error.status().map(|status| status.as_u16());
    ProviderTransportDiagnostics {
        stage: stage.to_string(),
        streaming: None,
        provider: Some(provider.to_string()),
        model_ref: model_ref.map(ToString::to_string),
        url: url
            .or_else(|| error.url().map(reqwest::Url::as_str))
            .map(sanitize_transport_url),
        status,
        reqwest: Some(ReqwestTransportDiagnostics {
            is_timeout: error.is_timeout(),
            is_connect: error.is_connect(),
            is_request: error.is_request(),
            is_body: error.is_body(),
            is_decode: error.is_decode(),
            is_redirect: error.is_redirect(),
            status,
        }),
        context_budget: None,
        http_trace: trace.and_then(|trace| trace.diagnostics(status)),
        quota_identity: None,
        source_chain,
    }
}

fn error_chain_messages(error: &reqwest::Error) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = error.source();
    while let Some(source) = current {
        let message = source.to_string();
        if !message.trim().is_empty() {
            chain.push(message);
        }
        current = source.source();
    }
    chain
}

pub(crate) fn format_provider_failure(
    model_ref: &str,
    attempts: usize,
    error: &anyhow::Error,
) -> String {
    let classification = classify_provider_error(error);
    let transport_error = error.downcast_ref::<ProviderTransportError>();
    let status = error
        .downcast_ref::<ProviderTransportError>()
        .and_then(|error| error.status)
        .map(|status| format!(", status={status}"))
        .unwrap_or_default();
    let shared_account_rate_limited = classification.kind == ProviderFailureKind::RateLimited
        && (transport_error.and_then(|error| error.code.as_deref())
            == Some("shared_account_rate_limited")
            || transport_error
                .and_then(|error| error.diagnostics.as_ref())
                .and_then(|diagnostics| diagnostics.quota_identity.as_ref())
                .is_some());
    let kind = if shared_account_rate_limited {
        "shared_account_rate_limited"
    } else {
        classification.kind.as_str()
    };
    match classification.disposition {
        RetryDisposition::Retryable => format!(
            "{model_ref}: retries_exhausted after {attempts} attempts ({kind}{status}): {error}",
        ),
        RetryDisposition::FailFast => format!("{model_ref}: fail_fast ({kind}{status}): {error}",),
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use std::time::Duration;

    use super::{
        classify_status_error_with_trace, provider_fallback_disposition, provider_retry_delay,
        set_provider_transport_quota_identity, ProviderFailureKind, ProviderRetryDelay,
        ProviderRetryDelaySource, ProviderTransportError,
    };
    use crate::provider::{ProviderFallbackDisposition, ProviderQuotaIdentity};

    #[test]
    fn network_failures_defer_fallback_but_other_failures_remain_immediate() {
        assert_eq!(
            provider_fallback_disposition(ProviderFailureKind::Timeout),
            ProviderFallbackDisposition::Deferred
        );
        assert_eq!(
            provider_fallback_disposition(ProviderFailureKind::Connection),
            ProviderFallbackDisposition::Deferred
        );
        assert_eq!(
            provider_fallback_disposition(ProviderFailureKind::ServerError),
            ProviderFallbackDisposition::Immediate
        );
        assert_eq!(
            provider_fallback_disposition(ProviderFailureKind::RateLimited),
            ProviderFallbackDisposition::Immediate
        );
    }

    fn retry_after_headers(value: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_str(value).expect("valid header value"),
        );
        headers
    }

    #[test]
    fn parse_retry_after_reads_delta_seconds() {
        assert_eq!(
            super::parse_retry_after(&retry_after_headers("13")),
            Some(Duration::from_secs(13))
        );
    }

    #[test]
    fn parse_retry_after_reads_future_http_date() {
        let date = (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc2822();
        let parsed =
            super::parse_retry_after(&retry_after_headers(&date)).expect("future date parses");
        assert!(parsed > Duration::from_secs(20));
        assert!(parsed <= Duration::from_secs(30));
    }

    #[test]
    fn parse_retry_after_ignores_past_http_date() {
        let date = (chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc2822();
        assert_eq!(super::parse_retry_after(&retry_after_headers(&date)), None);
    }

    #[test]
    fn parse_retry_after_ignores_missing_malformed_and_zero_values() {
        assert_eq!(
            super::parse_retry_after(&reqwest::header::HeaderMap::new()),
            None
        );
        assert_eq!(super::parse_retry_after(&retry_after_headers("soon")), None);
        assert_eq!(super::parse_retry_after(&retry_after_headers("0")), None);
        assert_eq!(super::parse_retry_after(&retry_after_headers(" ")), None);
    }

    #[test]
    fn parse_retry_after_header_lookup_is_case_insensitive() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::HeaderName::from_static("retry-after"),
            reqwest::header::HeaderValue::from_static("5"),
        );
        assert_eq!(
            super::parse_retry_after(&headers),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn retry_delay_uses_server_hint_within_cap() {
        assert_eq!(
            provider_retry_delay(
                1,
                ProviderFailureKind::RateLimited,
                Some(Duration::from_secs(5)),
                0
            ),
            ProviderRetryDelay::Wait {
                backoff: Duration::from_secs(5),
                source: ProviderRetryDelaySource::ServerRetryAfter
            }
        );
    }

    #[test]
    fn retry_delay_keeps_computed_floor_when_hint_is_smaller() {
        assert_eq!(
            provider_retry_delay(
                2,
                ProviderFailureKind::RateLimited,
                Some(Duration::from_millis(50)),
                0
            ),
            ProviderRetryDelay::Wait {
                backoff: Duration::from_millis(400),
                source: ProviderRetryDelaySource::ServerRetryAfter
            }
        );
    }

    #[test]
    fn retry_delay_caps_hint_before_retrying() {
        assert_eq!(
            provider_retry_delay(
                1,
                ProviderFailureKind::RateLimited,
                Some(Duration::from_secs(45)),
                0
            ),
            ProviderRetryDelay::Wait {
                backoff: Duration::from_secs(30),
                source: ProviderRetryDelaySource::ServerRetryAfter
            }
        );
    }

    #[test]
    fn retry_delay_without_hint_uses_computed_backoff() {
        assert_eq!(
            provider_retry_delay(1, ProviderFailureKind::RateLimited, None, 0),
            ProviderRetryDelay::Wait {
                backoff: Duration::from_millis(200),
                source: ProviderRetryDelaySource::ComputedBackoff
            }
        );
    }

    #[test]
    fn server_error_without_hint_uses_stable_exponential_backoff_with_jitter() {
        let seed = super::provider_retry_jitter_seed("openai", "gpt-5.4");
        let first = provider_retry_delay(1, ProviderFailureKind::ServerError, None, seed);
        let second = provider_retry_delay(2, ProviderFailureKind::ServerError, None, seed);
        let backoff = |delay| match delay {
            ProviderRetryDelay::Wait { backoff, .. } => backoff,
            ProviderRetryDelay::SkipToFallback => panic!("expected retry wait"),
        };

        assert_eq!(
            first,
            provider_retry_delay(1, ProviderFailureKind::ServerError, None, seed)
        );
        assert_eq!(
            first,
            ProviderRetryDelay::Wait {
                backoff: backoff(first),
                source: ProviderRetryDelaySource::ServerErrorExponentialBackoff
            }
        );
        assert_eq!(
            second,
            ProviderRetryDelay::Wait {
                backoff: backoff(second),
                source: ProviderRetryDelaySource::ServerErrorExponentialBackoff
            }
        );
        assert!((Duration::from_secs(2)..=Duration::from_millis(2_500)).contains(&backoff(first)));
        assert!((Duration::from_secs(4)..=Duration::from_millis(5_000)).contains(&backoff(second)));
    }

    #[test]
    fn server_error_hint_still_extends_computed_backoff() {
        let seed = super::provider_retry_jitter_seed("openai", "gpt-5.4");
        assert_eq!(
            provider_retry_delay(
                1,
                ProviderFailureKind::ServerError,
                Some(Duration::from_secs(5)),
                seed
            ),
            ProviderRetryDelay::Wait {
                backoff: Duration::from_secs(5),
                source: ProviderRetryDelaySource::ServerRetryAfter
            }
        );
    }

    #[test]
    fn server_error_backoff_is_capped_at_retry_hint_limit() {
        let delay = provider_retry_delay(
            usize::MAX,
            ProviderFailureKind::ServerError,
            None,
            super::provider_retry_jitter_seed("openai", "gpt-5.4"),
        );

        assert_eq!(
            delay,
            ProviderRetryDelay::Wait {
                backoff: Duration::from_millis(super::PROVIDER_RETRY_SERVER_HINT_CAP_MS),
                source: ProviderRetryDelaySource::ServerErrorExponentialBackoff
            }
        );
    }

    #[test]
    fn retry_delay_ignores_hint_for_non_throttle_kinds() {
        assert_eq!(
            provider_retry_delay(
                1,
                ProviderFailureKind::Timeout,
                Some(Duration::from_secs(5)),
                0
            ),
            ProviderRetryDelay::Wait {
                backoff: Duration::from_millis(200),
                source: ProviderRetryDelaySource::ComputedBackoff
            }
        );
    }

    #[test]
    fn status_error_carries_retry_after_hint() {
        let error = classify_status_error_with_trace(
            "OpenAI request failed",
            "response_status",
            Some("openai-codex"),
            Some("openai-codex/gpt-5.3-codex-spark"),
            Some("https://chatgpt.com/backend-api/codex/responses"),
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"rate limited"}}"#.into(),
            None,
            Some(Duration::from_secs(5)),
        );
        let transport = error
            .downcast_ref::<ProviderTransportError>()
            .expect("transport error");
        assert_eq!(
            transport.classification.kind,
            ProviderFailureKind::RateLimited
        );
        assert_eq!(transport.retry_after, Some(Duration::from_secs(5)));
    }

    #[test]
    fn explicit_tool_protocol_rejection_is_fail_fast_with_configuration_guidance() {
        let error = classify_status_error_with_trace(
            "OpenAI request failed",
            "response_status",
            Some("openai"),
            Some("openai/gpt-5"),
            Some("https://example.test/v1/responses"),
            StatusCode::BAD_REQUEST,
            r#"{"error":{"code":"unsupported_tool","message":"tool protocol rejected"}}"#.into(),
            None,
            None,
        );
        let transport = error.downcast_ref::<ProviderTransportError>().unwrap();
        assert_eq!(
            transport.classification.kind,
            ProviderFailureKind::ContractError
        );
        assert_eq!(
            transport.classification.disposition,
            super::RetryDisposition::FailFast
        );
        assert!(error
            .to_string()
            .contains("builtin_web_search configuration"));
    }

    #[test]
    fn ordinary_bad_request_has_no_tool_configuration_guidance() {
        let error = classify_status_error_with_trace(
            "OpenAI request failed",
            "response_status",
            Some("openai"),
            Some("openai/gpt-5"),
            Some("https://example.test/v1/responses"),
            StatusCode::BAD_REQUEST,
            r#"{"error":{"code":"invalid_request","message":"bad input"}}"#.into(),
            None,
            None,
        );
        assert!(!error
            .to_string()
            .contains("builtin_web_search configuration"));
    }

    #[test]
    fn rate_limit_diagnostics_carry_redacted_quota_identity_and_round_trip() {
        let identity = ProviderQuotaIdentity::exact("codex-account", "account-secret")
            .expect("non-empty account should produce an identity");
        let error = set_provider_transport_quota_identity(
            classify_status_error_with_trace(
                "OpenAI request failed",
                "response_status",
                Some("openai-codex"),
                Some("openai-codex/gpt-5.3-codex-spark"),
                Some("https://chatgpt.com/backend-api/codex/responses"),
                StatusCode::TOO_MANY_REQUESTS,
                r#"{"error":{"message":"rate limited"}}"#.into(),
                None,
                Some(Duration::from_secs(5)),
            ),
            identity.clone(),
        );
        let transport = error
            .downcast_ref::<ProviderTransportError>()
            .expect("transport error");
        assert_eq!(
            transport.status,
            Some(StatusCode::TOO_MANY_REQUESTS.as_u16())
        );
        assert_eq!(transport.retry_after, Some(Duration::from_secs(5)));

        let diagnostics = transport
            .diagnostics
            .as_ref()
            .expect("rate limit should include transport diagnostics");
        assert_eq!(diagnostics.provider.as_deref(), Some("openai-codex"));
        assert_eq!(
            diagnostics.status,
            Some(StatusCode::TOO_MANY_REQUESTS.as_u16())
        );
        assert_eq!(diagnostics.quota_identity.as_ref(), Some(&identity));

        let encoded = serde_json::to_value(diagnostics).expect("diagnostics should serialize");
        assert_eq!(
            encoded["quota_identity"],
            serde_json::to_value(&identity).expect("identity should serialize")
        );
        assert!(!encoded.to_string().contains("account-secret"));

        let decoded: crate::provider::ProviderTransportDiagnostics =
            serde_json::from_value(encoded).expect("diagnostics should deserialize");
        assert_eq!(decoded, *diagnostics);
    }

    #[test]
    fn rate_limit_status_wins_over_deterministic_marker() {
        let error = classify_status_error_with_trace(
            "Provider request failed",
            "response_status",
            Some("openai"),
            Some("openai/gpt-5.4"),
            Some("https://example.com/v1/chat/completions"),
            StatusCode::TOO_MANY_REQUESTS,
            "too many tokens per minute".into(),
            None,
            Some(Duration::from_secs(5)),
        );
        let transport = error
            .downcast_ref::<ProviderTransportError>()
            .expect("transport error");
        assert_eq!(
            transport.classification.kind,
            ProviderFailureKind::RateLimited
        );
        assert_eq!(
            transport.classification.disposition,
            super::RetryDisposition::Retryable
        );
        assert_eq!(transport.retry_after, Some(Duration::from_secs(5)));
    }

    #[test]
    fn deterministic_context_overflow_on_server_error_is_fail_fast() {
        let error = classify_status_error_with_trace(
            "Ollama request failed",
            "response_status",
            Some("ollama"),
            Some("ollama/qwen3.8:latest"),
            Some("http://localhost:11434/v1/chat/completions"),
            StatusCode::INTERNAL_SERVER_ERROR,
            "context length exceeded: prompt is too long".into(),
            None,
            None,
        );
        let transport = error
            .downcast_ref::<ProviderTransportError>()
            .expect("transport error");
        assert_eq!(
            transport.classification.kind,
            ProviderFailureKind::ContractError
        );
        assert_eq!(
            transport.classification.disposition,
            super::RetryDisposition::FailFast
        );
    }

    #[test]
    fn ollama_missing_user_query_on_server_error_is_fail_fast() {
        let error = classify_status_error_with_trace(
            "Ollama request failed",
            "response_status",
            Some("ollama"),
            Some("ollama/qwen3.8:latest"),
            Some("http://localhost:11434/v1/chat/completions"),
            StatusCode::INTERNAL_SERVER_ERROR,
            "no user query found in messages".into(),
            None,
            None,
        );
        let transport = error
            .downcast_ref::<ProviderTransportError>()
            .expect("transport error");
        assert_eq!(
            transport.classification.kind,
            ProviderFailureKind::ContractError
        );
        assert_eq!(
            transport.classification.disposition,
            super::RetryDisposition::FailFast
        );
    }

    #[test]
    fn generic_server_error_remains_retryable() {
        let error = classify_status_error_with_trace(
            "Provider request failed",
            "response_status",
            Some("ollama"),
            Some("ollama/qwen3.8:latest"),
            Some("http://localhost:11434/v1/chat/completions"),
            StatusCode::INTERNAL_SERVER_ERROR,
            "temporary upstream failure".into(),
            None,
            None,
        );
        let transport = error
            .downcast_ref::<ProviderTransportError>()
            .expect("transport error");
        assert_eq!(
            transport.classification.kind,
            ProviderFailureKind::ServerError
        );
        assert_eq!(
            transport.classification.disposition,
            super::RetryDisposition::Retryable
        );
    }

    #[test]
    fn transport_url_sanitizer_removes_credentials_query_and_fragment() {
        assert_eq!(
            super::sanitize_transport_url(
                "https://user:secret@example.com/v1/responses?api_key=token#frag"
            ),
            "https://example.com/v1/responses"
        );
    }

    #[test]
    fn status_error_display_preserves_safe_upstream_detail_without_secrets() {
        let error = classify_status_error_with_trace(
            "OpenAI compact request failed",
            "response_status",
            Some("openai"),
            Some("openai/gpt-5.4"),
            Some("https://api.openai.com/v1/responses/compact"),
            StatusCode::NOT_FOUND,
            r#"{"error":{"message":"Items are not persisted when `store` is set to false","access_token":"short-secret"}}"#.into(),
            None,
            None,
        );

        assert_eq!(
            error.to_string(),
            "OpenAI compact request failed with status 404 Not Found: message=Items are not persisted when `store` is set to false"
        );
        assert!(!error.to_string().contains("short-secret"));
        assert_eq!(
            error
                .downcast_ref::<ProviderTransportError>()
                .and_then(|error| error.code.as_deref()),
            Some("non_persisted_item_id")
        );
    }

    #[test]
    fn gateway_entitlement_error_preserves_structured_type_and_message() {
        let error = classify_status_error_with_trace(
            "Vercel AI Gateway request failed",
            "response_status",
            Some("openai"),
            Some("openai/gpt-5.4"),
            Some("https://gateway.example/v1/chat/completions"),
            StatusCode::FORBIDDEN,
            r#"{"error":{"type":"no_providers_available","message":"Free tier users do not have access to this model","token":"must-not-leak"}}"#.into(),
            None,
            None,
        );

        assert_eq!(
            error.to_string(),
            "Vercel AI Gateway request failed with status 403 Forbidden: type=no_providers_available, message=Free tier users do not have access to this model"
        );
        assert!(!error.to_string().contains("must-not-leak"));
    }

    #[test]
    fn timeout_transport_summary_does_not_present_decode_wording_as_root_cause() {
        assert_eq!(
            super::format_reqwest_transport_error_message(
                "Anthropic streaming response body failed",
                "streaming_response_body",
                true,
                "error decoding response body".into(),
            ),
            "Anthropic streaming response body failed: timed out while reading the streaming response body"
        );
        assert_eq!(
            super::format_reqwest_transport_error_message(
                "Anthropic streaming response body failed",
                "streaming_response_parse",
                true,
                "error decoding response body".into(),
            ),
            "Anthropic streaming response body failed: error decoding response body"
        );
    }

    #[test]
    fn upstream_error_detail_requires_error_object_when_envelope_is_present() {
        let value = serde_json::json!({
            "error": "not an object",
            "message": "should not be used"
        });
        assert_eq!(
            super::extract_upstream_error_detail_from_value(&value),
            None
        );
    }

    #[test]
    fn streaming_request_send_connection_source_chain_is_retryable() {
        let source_chain = vec![
            "client error (SendRequest)".to_string(),
            "connection error".to_string(),
            "peer closed connection without sending TLS close_notify".to_string(),
        ];

        assert!(super::is_retryable_request_send_transport_failure(
            "streaming_request_send",
            &source_chain
        ));
    }

    #[test]
    fn request_send_connection_closed_source_chain_is_retryable() {
        let source_chain = vec![
            "client error (SendRequest)".to_string(),
            "connection closed before message completed".to_string(),
        ];

        assert!(super::is_retryable_request_send_transport_failure(
            "request_send",
            &source_chain
        ));
    }

    #[test]
    fn request_send_connection_source_chain_is_stage_limited() {
        let source_chain = vec!["connection error".to_string()];

        assert!(!super::is_retryable_request_send_transport_failure(
            "response_status",
            &source_chain
        ));
    }

    #[test]
    fn request_send_non_transport_source_chain_is_not_retryable() {
        let source_chain = vec![
            "builder error".to_string(),
            "invalid header value".to_string(),
        ];

        assert!(!super::is_retryable_request_send_transport_failure(
            "request_send",
            &source_chain
        ));
    }
}
