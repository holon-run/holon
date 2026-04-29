use std::error::Error;

use reqwest::StatusCode;
use serde::Serialize;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::time::Duration;

use super::{ProviderTransportDiagnostics, ReqwestTransportDiagnostics};

pub(crate) const PROVIDER_MAX_RETRIES: usize = 2;
const PROVIDER_RETRY_BASE_BACKOFF_MS: u64 = 200;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProviderFailureKind {
    Timeout,
    Connection,
    RateLimited,
    ServerError,
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
    pub status: Option<u16>,
    pub diagnostics: Option<ProviderTransportDiagnostics>,
    message: String,
}

impl ProviderFailureKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connection => "connection",
            Self::RateLimited => "rate_limited",
            Self::ServerError => "server_error",
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
        "retryable_failure_kinds": [
            ProviderFailureKind::Timeout.as_str(),
            ProviderFailureKind::Connection.as_str(),
            ProviderFailureKind::RateLimited.as_str(),
            ProviderFailureKind::ServerError.as_str(),
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

pub(crate) fn classify_provider_error(error: &anyhow::Error) -> ProviderFailureClassification {
    error
        .downcast_ref::<ProviderTransportError>()
        .map(|error| error.classification)
        .unwrap_or(ProviderFailureClassification {
            kind: ProviderFailureKind::Unknown,
            disposition: RetryDisposition::FailFast,
        })
}

pub(crate) fn provider_transport_error(
    classification: ProviderFailureClassification,
    status: Option<u16>,
    diagnostics: Option<ProviderTransportDiagnostics>,
    message: impl Into<String>,
) -> anyhow::Error {
    ProviderTransportError {
        classification,
        status,
        diagnostics,
        message: message.into(),
    }
    .into()
}

pub(crate) fn classify_reqwest_transport_error(
    context: &str,
    stage: &str,
    provider: &str,
    model_ref: Option<&str>,
    url: Option<&str>,
    error: reqwest::Error,
) -> anyhow::Error {
    let status = error.status().map(|status| status.as_u16());
    let classification = if error.is_timeout() {
        ProviderFailureClassification {
            kind: ProviderFailureKind::Timeout,
            disposition: RetryDisposition::Retryable,
        }
    } else if error.is_connect() {
        ProviderFailureClassification {
            kind: ProviderFailureKind::Connection,
            disposition: RetryDisposition::Retryable,
        }
    } else {
        ProviderFailureClassification {
            kind: ProviderFailureKind::Unknown,
            disposition: RetryDisposition::FailFast,
        }
    };
    provider_transport_error(
        classification,
        status,
        Some(reqwest_transport_diagnostics(
            stage, provider, model_ref, url, &error,
        )),
        format!("{context}: {error}"),
    )
}

pub(crate) fn classify_status_error(
    context: &str,
    status: StatusCode,
    body: String,
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
    provider_transport_error(
        classification,
        Some(status.as_u16()),
        None,
        format!("{context} with status {status}: {body}"),
    )
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

fn reqwest_transport_diagnostics(
    stage: &str,
    provider: &str,
    model_ref: Option<&str>,
    url: Option<&str>,
    error: &reqwest::Error,
) -> ProviderTransportDiagnostics {
    ProviderTransportDiagnostics {
        stage: stage.to_string(),
        provider: Some(provider.to_string()),
        model_ref: model_ref.map(ToString::to_string),
        url: url
            .map(ToString::to_string)
            .or_else(|| error.url().map(|url| url.to_string())),
        status: error.status().map(|status| status.as_u16()),
        reqwest: Some(ReqwestTransportDiagnostics {
            is_timeout: error.is_timeout(),
            is_connect: error.is_connect(),
            is_request: error.is_request(),
            is_body: error.is_body(),
            is_decode: error.is_decode(),
            is_redirect: error.is_redirect(),
            status: error.status().map(|status| status.as_u16()),
        }),
        source_chain: error_chain_messages(error),
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
