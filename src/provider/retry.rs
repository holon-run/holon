use std::error::Error;

use reqwest::StatusCode;
use serde::Serialize;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::time::Duration;

use super::{
    http_trace::ProviderHttpTraceRequest, ProviderTransportDiagnostics, ReqwestTransportDiagnostics,
};
use crate::types::TokenUsage;

pub(crate) const PROVIDER_MAX_RETRIES: usize = 2;
const PROVIDER_RETRY_BASE_BACKOFF_MS: u64 = 200;
pub(crate) const PROVIDER_RETRY_SERVER_HINT_CAP_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProviderFailureKind {
    Timeout,
    Connection,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderRetryDelaySource {
    ServerRetryAfter,
    ComputedBackoff,
}

impl ProviderRetryDelaySource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ServerRetryAfter => "server_retry_after",
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

pub(crate) fn provider_retry_policy_json() -> Value {
    json!({
        "max_retries_per_provider": PROVIDER_MAX_RETRIES,
        "max_attempts_per_provider": provider_max_attempts(),
        "base_backoff_ms": PROVIDER_RETRY_BASE_BACKOFF_MS,
        "server_hint_cap_ms": PROVIDER_RETRY_SERVER_HINT_CAP_MS,
        "server_hint_semantics": "429/503 Retry-After at or below the cap extends the computed backoff; hints above the cap skip remaining retries and defer to fallback",
        "retryable_failure_kinds": [
            ProviderFailureKind::Timeout.as_str(),
            ProviderFailureKind::Connection.as_str(),
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
    })
}

pub(crate) fn provider_max_attempts() -> usize {
    PROVIDER_MAX_RETRIES + 1
}

pub(crate) fn provider_retry_backoff(attempt: usize) -> Duration {
    Duration::from_millis(PROVIDER_RETRY_BASE_BACKOFF_MS * attempt as u64)
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
) -> ProviderRetryDelay {
    let computed = provider_retry_backoff(attempt);
    // Retry-After is a server-side throttle hint; only the kinds that carry
    // that semantic (429 rate limits and 5xx server errors) may extend the wait.
    let server_hint = match kind {
        ProviderFailureKind::RateLimited | ProviderFailureKind::ServerError => retry_after,
        _ => None,
    };
    let Some(server_hint) = server_hint else {
        return ProviderRetryDelay::Wait {
            backoff: computed,
            source: ProviderRetryDelaySource::ComputedBackoff,
        };
    };
    if server_hint > Duration::from_millis(PROVIDER_RETRY_SERVER_HINT_CAP_MS) {
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
        format!("{context}: {error}"),
    )
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
    if !matches!(stage, "streaming_request_send") {
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
    let classification = match status {
        StatusCode::TOO_MANY_REQUESTS => ProviderFailureClassification {
            kind: ProviderFailureKind::RateLimited,
            disposition: RetryDisposition::Retryable,
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
    let code = status_error_code(&body);
    provider_transport_error_with_code_and_retry_after(
        classification,
        code,
        Some(status.as_u16()),
        Some(ProviderTransportDiagnostics {
            stage: stage.to_string(),
            provider: provider.map(ToString::to_string),
            model_ref: model_ref.map(ToString::to_string),
            url: url.map(sanitize_transport_url),
            status: Some(status.as_u16()),
            reqwest: None,
            http_trace: trace.and_then(|trace| trace.diagnostics(Some(status.as_u16()))),
            source_chain: status_error_source_chain(provider, status),
        }),
        retry_after,
        format!("{context} with status {status}"),
    )
}

fn status_error_code(body: &str) -> Option<&'static str> {
    body.contains("Items are not persisted when `store` is set to false")
        .then_some("non_persisted_item_id")
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
            provider: Some(provider.to_string()),
            model_ref: model_ref.map(ToString::to_string),
            url: url.map(sanitize_transport_url),
            status: None,
            reqwest: None,
            http_trace: trace.and_then(|trace| trace.diagnostics(None)),
            source_chain: vec![error.clone()],
        }),
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
            provider: Some(provider.to_string()),
            model_ref: model_ref.map(ToString::to_string),
            url: url.map(sanitize_transport_url),
            status: None,
            reqwest: None,
            http_trace: trace.and_then(|trace| trace.diagnostics(None)),
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
            provider: Some(provider.to_string()),
            model_ref: model_ref.map(ToString::to_string),
            url: url.map(sanitize_transport_url),
            status: None,
            reqwest: None,
            http_trace: trace.and_then(|trace| trace.diagnostics(None)),
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
        http_trace: trace.and_then(|trace| trace.diagnostics(status)),
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
    let status = error
        .downcast_ref::<ProviderTransportError>()
        .and_then(|error| error.status)
        .map(|status| format!(", status={status}"))
        .unwrap_or_default();
    match classification.disposition {
        RetryDisposition::Retryable => format!(
            "{model_ref}: retries_exhausted after {attempts} attempts ({kind}{status}): {error}",
            kind = classification.kind.as_str()
        ),
        RetryDisposition::FailFast => format!(
            "{model_ref}: fail_fast ({kind}{status}): {error}",
            kind = classification.kind.as_str()
        ),
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use std::time::Duration;

    use super::{
        classify_status_error_with_trace, provider_retry_delay, ProviderFailureKind,
        ProviderRetryDelay, ProviderRetryDelaySource, ProviderTransportError,
    };

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
                Some(Duration::from_secs(5))
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
                ProviderFailureKind::ServerError,
                Some(Duration::from_millis(50))
            ),
            ProviderRetryDelay::Wait {
                backoff: Duration::from_millis(400),
                source: ProviderRetryDelaySource::ServerRetryAfter
            }
        );
    }

    #[test]
    fn retry_delay_skips_to_fallback_when_hint_exceeds_cap() {
        assert_eq!(
            provider_retry_delay(
                1,
                ProviderFailureKind::RateLimited,
                Some(Duration::from_secs(45))
            ),
            ProviderRetryDelay::SkipToFallback
        );
    }

    #[test]
    fn retry_delay_without_hint_uses_computed_backoff() {
        assert_eq!(
            provider_retry_delay(1, ProviderFailureKind::RateLimited, None),
            ProviderRetryDelay::Wait {
                backoff: Duration::from_millis(200),
                source: ProviderRetryDelaySource::ComputedBackoff
            }
        );
    }

    #[test]
    fn retry_delay_ignores_hint_for_non_throttle_kinds() {
        assert_eq!(
            provider_retry_delay(
                1,
                ProviderFailureKind::Timeout,
                Some(Duration::from_secs(5))
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
    fn transport_url_sanitizer_removes_credentials_query_and_fragment() {
        assert_eq!(
            super::sanitize_transport_url(
                "https://user:secret@example.com/v1/responses?api_key=token#frag"
            ),
            "https://example.com/v1/responses"
        );
    }

    #[test]
    fn status_error_display_excludes_response_body_but_preserves_typed_code() {
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
            "OpenAI compact request failed with status 404 Not Found"
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
    fn request_send_connection_source_chain_is_stage_limited() {
        let source_chain = vec!["connection error".to_string()];

        assert!(!super::is_retryable_request_send_transport_failure(
            "response_status",
            &source_chain
        ));
    }

    #[test]
    fn streaming_request_send_non_transport_source_chain_is_not_retryable() {
        let source_chain = vec![
            "builder error".to_string(),
            "invalid header value".to_string(),
        ];

        assert!(!super::is_retryable_request_send_transport_failure(
            "streaming_request_send",
            &source_chain
        ));
    }
}
