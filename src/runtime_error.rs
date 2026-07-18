use std::collections::{BTreeMap, BTreeSet};

use anyhow::Error as AnyhowError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    provider::{provider_attempt_timeline, provider_transport_diagnostics, ProviderTransportError},
    runtime_db::{RuntimeDbRetryableError, RuntimeStateTransitionConflict},
    tool::ToolError,
};

const SOURCE_CHAIN_MAX_ENTRIES: usize = 8;
const ERROR_TEXT_MAX_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeErrorDomain {
    Runtime,
    Storage,
    Policy,
    Io,
    Conflict,
    NotFound,
    Validation,
    Provider,
    Tool,
    Task,
    Http,
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeErrorContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<String>,
}

impl RuntimeErrorContext {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeErrorDescriptor {
    pub domain: RuntimeErrorDomain,
    pub code: String,
    pub retryable: bool,
    pub operator_message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_hint: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub safe_context: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_chain: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeError {
    descriptor: RuntimeErrorDescriptor,
}

impl RuntimeError {
    pub fn new(
        domain: RuntimeErrorDomain,
        code: impl Into<String>,
        operator_message: impl Into<String>,
    ) -> Self {
        Self {
            descriptor: RuntimeErrorDescriptor {
                domain,
                code: code.into(),
                retryable: false,
                operator_message: sanitize_runtime_error_text(&operator_message.into()),
                recovery_hint: None,
                safe_context: BTreeMap::new(),
                source_chain: Vec::new(),
            },
        }
    }

    pub fn not_found(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(RuntimeErrorDomain::NotFound, code, message)
    }

    pub fn validation(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(RuntimeErrorDomain::Validation, code, message)
    }

    pub fn policy(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(RuntimeErrorDomain::Policy, code, message)
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.descriptor.retryable = retryable;
        self
    }

    pub fn with_recovery_hint(mut self, hint: impl Into<String>) -> Self {
        self.descriptor.recovery_hint = Some(sanitize_runtime_error_text(&hint.into()));
        self
    }

    pub fn with_safe_context(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let key = key.into();
        if is_safe_context_key(&key) {
            self.descriptor
                .safe_context
                .insert(key, sanitize_runtime_error_text(&value.into()));
        }
        self
    }

    pub fn descriptor(&self) -> &RuntimeErrorDescriptor {
        &self.descriptor
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.descriptor.operator_message)
    }
}

impl std::error::Error for RuntimeError {}

pub fn describe_runtime_error(error: &AnyhowError) -> RuntimeErrorDescriptor {
    let source_chain = collect_runtime_error_source_chain(error);

    if let Some(runtime_error) = error
        .chain()
        .find_map(|source| source.downcast_ref::<RuntimeError>())
    {
        let mut descriptor = runtime_error.descriptor().clone();
        descriptor.source_chain = merge_source_chains(descriptor.source_chain, source_chain);
        return descriptor;
    }

    if let Some(tool_error) = error
        .chain()
        .find_map(|source| source.downcast_ref::<ToolError>())
    {
        return descriptor_from_tool_error(tool_error, source_chain);
    }

    if let Some(provider_error) = error
        .chain()
        .find_map(|source| source.downcast_ref::<ProviderTransportError>())
    {
        let diagnostics = provider_transport_diagnostics(error);
        let provider_chain = diagnostics
            .map(|diagnostics| diagnostics.source_chain.clone())
            .unwrap_or_default();
        let kind = provider_error.classification.kind.as_str();
        let code = provider_error.code.as_deref().unwrap_or(kind);
        let retryable = provider_error.classification.disposition.as_str() == "retryable";
        let mut safe_context = BTreeMap::new();
        if let Some(status) = provider_error.status {
            safe_context.insert("status".into(), status.to_string());
        }
        if let Some(diagnostics) = diagnostics {
            insert_optional_context(
                &mut safe_context,
                "provider",
                diagnostics.provider.as_deref(),
            );
            insert_optional_context(
                &mut safe_context,
                "model_ref",
                diagnostics.model_ref.as_deref(),
            );
            insert_optional_context(&mut safe_context, "stage", Some(&diagnostics.stage));
        }
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Provider,
            code: code.to_string(),
            retryable,
            operator_message: format!("provider request failed: {kind}"),
            recovery_hint: provider_recovery_hint(kind),
            safe_context,
            source_chain: merge_source_chains(provider_chain, source_chain),
        };
    }

    if let Some(conflict) = error
        .chain()
        .find_map(|source| source.downcast_ref::<RuntimeStateTransitionConflict>())
    {
        let mut safe_context = BTreeMap::from([
            ("record_type".into(), conflict.domain().to_string()),
            ("record_id".into(), conflict.record_id().to_string()),
            (
                "existing_status".into(),
                conflict.existing_status().to_string(),
            ),
            (
                "incoming_status".into(),
                conflict.incoming_status().to_string(),
            ),
        ]);
        if let Some(revision) = conflict.expected_revision() {
            safe_context.insert("expected_revision".into(), revision.to_string());
        }
        if let Some(revision) = conflict.actual_revision() {
            safe_context.insert("actual_revision".into(), revision.to_string());
        }
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Conflict,
            code: conflict.code().to_string(),
            retryable: conflict.retryable(),
            operator_message: sanitize_runtime_error_text(&conflict.to_string()),
            recovery_hint: conflict
                .retryable()
                .then(|| "retry with fresh state".into()),
            safe_context,
            source_chain,
        };
    }

    if let Some(db_error) = error
        .chain()
        .find_map(|source| source.downcast_ref::<RuntimeDbRetryableError>())
    {
        let source_chain = vec![format!(
            "runtime database operation {} is temporarily unavailable",
            db_error.operation()
        )];
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Storage,
            code: "runtime_db_busy".into(),
            retryable: true,
            operator_message: format!(
                "runtime database operation {} is temporarily unavailable",
                db_error.operation()
            ),
            recovery_hint: Some("retry the operation".into()),
            safe_context: BTreeMap::from([("operation".into(), db_error.operation().to_string())]),
            source_chain,
        };
    }

    if let Some(io_error) = error
        .chain()
        .find_map(|source| source.downcast_ref::<std::io::Error>())
    {
        let mut safe_context = BTreeMap::new();
        if let Some(code) = io_error.raw_os_error() {
            safe_context.insert("os_error".into(), code.to_string());
        }
        safe_context.insert("io_kind".into(), format!("{:?}", io_error.kind()));
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Io,
            code: "io_error".into(),
            retryable: matches!(
                io_error.kind(),
                std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
            ),
            operator_message: "I/O operation failed".into(),
            recovery_hint: None,
            safe_context,
            source_chain,
        };
    }

    if let Some(attempt) =
        provider_attempt_timeline(error).and_then(|timeline| timeline.attempts.last())
    {
        let kind = attempt.failure_kind.as_deref().unwrap_or("unknown");
        let retryable = attempt.disposition.as_deref() == Some("retryable");
        let mut safe_context = BTreeMap::from([
            ("provider".into(), attempt.provider.clone()),
            ("model_ref".into(), attempt.model_ref.clone()),
        ]);
        if let Some(diagnostics) = attempt.transport_diagnostics.as_ref() {
            safe_context.insert("stage".into(), diagnostics.stage.clone());
            if let Some(status) = diagnostics.status {
                safe_context.insert("status".into(), status.to_string());
            }
        }
        let provider_chain = attempt
            .transport_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.source_chain.clone())
            .unwrap_or_default();
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Provider,
            code: kind.to_string(),
            retryable,
            operator_message: format!("provider request failed: {kind}"),
            recovery_hint: provider_recovery_hint(kind),
            safe_context,
            source_chain: merge_source_chains(provider_chain, source_chain),
        };
    }

    RuntimeErrorDescriptor {
        domain: RuntimeErrorDomain::Unknown,
        code: "runtime_error".into(),
        retryable: false,
        operator_message: sanitize_runtime_error_text(&error.to_string()),
        recovery_hint: None,
        safe_context: BTreeMap::new(),
        source_chain,
    }
}

pub fn collect_runtime_error_source_chain(error: &AnyhowError) -> Vec<String> {
    let mut seen = BTreeSet::new();
    error
        .chain()
        .filter_map(|source| {
            if let Some(tool_error) = source.downcast_ref::<ToolError>() {
                return Some(tool_error.message.clone());
            }
            if source.downcast_ref::<ProviderTransportError>().is_some() {
                return None;
            }
            if let Some(runtime_error) = source.downcast_ref::<RuntimeError>() {
                return Some(runtime_error.descriptor.operator_message.clone());
            }
            Some(source.to_string())
        })
        .map(|message| sanitize_runtime_error_text(&message))
        .filter(|message| !message.is_empty())
        .filter(|message| seen.insert(message.clone()))
        .take(SOURCE_CHAIN_MAX_ENTRIES)
        .collect()
}

pub fn sanitize_runtime_error_text(raw: &str) -> String {
    let trimmed = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if trimmed.is_empty() {
        return String::new();
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("authorization:")
        || lower.contains("bearer ")
        || lower.contains("api_key")
        || lower.contains("api-key")
        || lower.contains("access_token")
        || lower.contains("access-token")
        || lower.contains("x-api-key")
        || lower.contains("capability_secret")
        || lower.contains("/api/callbacks/wake/")
        || lower.contains("/api/callbacks/enqueue/")
        || lower.contains("/callbacks/wake/")
        || lower.contains("/callbacks/enqueue/")
    {
        return "<redacted-sensitive-error-context>".into();
    }

    let public_prefix = trimmed
        .find('{')
        .map(|index| {
            let prefix = trimmed[..index].trim_end_matches([' ', ':']);
            if prefix.is_empty() {
                "<redacted-structured-error-context>".to_string()
            } else {
                format!("{prefix}: <redacted-structured-error-context>")
            }
        })
        .unwrap_or_else(|| trimmed.to_string());
    let sanitized = public_prefix
        .split_whitespace()
        .map(sanitize_error_token)
        .collect::<Vec<_>>()
        .join(" ");
    truncate_chars(&sanitized, ERROR_TEXT_MAX_CHARS)
}

fn descriptor_from_tool_error(
    tool_error: &ToolError,
    source_chain: Vec<String>,
) -> RuntimeErrorDescriptor {
    let mut safe_context = BTreeMap::new();
    if let Some(details) = tool_error
        .details
        .as_ref()
        .and_then(serde_json::Value::as_object)
    {
        for (key, value) in details {
            if is_safe_context_key(key) {
                if let Some(value) = value.as_str() {
                    safe_context.insert(key.clone(), sanitize_runtime_error_text(value));
                } else if value.is_number() || value.is_boolean() {
                    safe_context.insert(key.clone(), value.to_string());
                }
            }
        }
    }
    RuntimeErrorDescriptor {
        domain: RuntimeErrorDomain::Tool,
        code: tool_error.kind.clone(),
        retryable: tool_error.retryable,
        operator_message: sanitize_runtime_error_text(&tool_error.message),
        recovery_hint: tool_error
            .recovery_hint
            .as_deref()
            .map(sanitize_runtime_error_text),
        safe_context,
        source_chain,
    }
}

fn merge_source_chains(primary: Vec<String>, secondary: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    primary
        .into_iter()
        .chain(secondary)
        .map(|message| sanitize_runtime_error_text(&message))
        .filter(|message| !message.is_empty())
        .filter(|message| seen.insert(message.clone()))
        .take(SOURCE_CHAIN_MAX_ENTRIES)
        .collect()
}

fn provider_recovery_hint(kind: &str) -> Option<String> {
    match kind {
        "auth_error" => Some("check provider authentication and credentials".into()),
        "rate_limited" => Some("retry after the provider rate limit resets".into()),
        "timeout" | "connection" | "server_error" => Some("retry the provider request".into()),
        _ => None,
    }
}

fn insert_optional_context(context: &mut BTreeMap<String, String>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        context.insert(key.into(), sanitize_runtime_error_text(value));
    }
}

fn is_safe_context_key(key: &str) -> bool {
    matches!(
        key,
        "agent_id"
            | "field"
            | "task_id"
            | "timer_id"
            | "work_item_id"
            | "message_id"
            | "turn_id"
            | "run_id"
            | "tool_execution_id"
            | "provider"
            | "model_ref"
            | "stage"
            | "status"
            | "record_type"
            | "record_id"
            | "existing_status"
            | "incoming_status"
            | "expected_revision"
            | "actual_revision"
            | "operation"
            | "io_kind"
            | "os_error"
    )
}

fn sanitize_error_token(raw: &str) -> String {
    let (prefix, token, suffix) = split_token_punctuation(raw);
    let sanitized = if let Ok(mut url) = url::Url::parse(token) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        url.to_string()
    } else if std::path::Path::new(token).is_absolute() {
        std::path::Path::new(token)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!("<redacted-path>/{name}"))
            .unwrap_or_else(|| "<redacted-path>".into())
    } else {
        token.to_string()
    };
    format!("{prefix}{sanitized}{suffix}")
}

fn split_token_punctuation(raw: &str) -> (&str, &str, &str) {
    let start = raw
        .char_indices()
        .find(|(_, ch)| !matches!(ch, '(' | '[' | '{' | '"' | '\''))
        .map(|(index, _)| index)
        .unwrap_or(raw.len());
    let end = raw
        .char_indices()
        .rev()
        .find(|(_, ch)| !matches!(ch, ')' | ']' | '}' | ',' | ';' | '"' | '\''))
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(start);
    (&raw[..start], &raw[start..end], &raw[end..])
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_runtime_error_survives_anyhow_context() {
        let error = AnyhowError::from(
            RuntimeError::not_found("task_not_found", "task task_123 not found")
                .with_safe_context("task_id", "task_123"),
        )
        .context("failed to read task status");

        let descriptor = describe_runtime_error(&error);

        assert_eq!(descriptor.domain, RuntimeErrorDomain::NotFound);
        assert_eq!(descriptor.code, "task_not_found");
        assert_eq!(descriptor.safe_context["task_id"], "task_123");
        assert!(descriptor
            .source_chain
            .iter()
            .any(|message| message == "failed to read task status"));
    }

    #[test]
    fn source_chain_is_bounded_deduplicated_and_redacted() {
        let error = AnyhowError::msg(
            "failed for /home/operator/private/config.json at https://user:secret@example.test/path?token=secret",
        )
        .context("Authorization: Bearer top-secret")
        .context("outer failure");

        let descriptor = describe_runtime_error(&error);
        let rendered = descriptor.source_chain.join(" ");

        assert!(descriptor.source_chain.len() <= SOURCE_CHAIN_MAX_ENTRIES);
        assert!(!rendered.contains("/home/operator"));
        assert!(!rendered.contains("top-secret"));
        assert!(!rendered.contains("user:secret"));
        assert!(!rendered.contains("?token="));
    }

    #[test]
    fn unknown_errors_fail_closed_for_retryability() {
        let descriptor = describe_runtime_error(&AnyhowError::msg("unexpected failure"));

        assert_eq!(descriptor.domain, RuntimeErrorDomain::Unknown);
        assert_eq!(descriptor.code, "runtime_error");
        assert!(!descriptor.retryable);
    }

    #[test]
    fn tool_error_source_chain_does_not_serialize_details() {
        let error = AnyhowError::from(
            ToolError::new("provider_failed", "provider request failed")
                .with_details(serde_json::json!({"authorization": "secret-value"})),
        )
        .context("tool dispatch failed");

        let descriptor = describe_runtime_error(&error);
        let rendered = descriptor.source_chain.join(" ");

        assert!(rendered.contains("tool dispatch failed"));
        assert!(rendered.contains("provider request failed"));
        assert!(!rendered.contains("secret-value"));
        assert!(!rendered.contains("authorization"));
    }

    #[test]
    fn sanitizer_redacts_json_secrets_and_structured_bodies() {
        assert_eq!(
            sanitize_runtime_error_text(r#"provider failed: {"api_key":"secret"}"#),
            "<redacted-sensitive-error-context>"
        );
        assert_eq!(
            sanitize_runtime_error_text(
                r#"provider failed: {"detail":"raw backend response body"}"#
            ),
            "provider failed: <redacted-structured-error-context>"
        );
        assert_eq!(
            sanitize_runtime_error_text(
                "callback failed for https://example.test/api/callbacks/wake/cb_secret"
            ),
            "<redacted-sensitive-error-context>"
        );
        assert_eq!(
            sanitize_runtime_error_text(
                "callback failed for https://example.test/callbacks/enqueue/cb_secret"
            ),
            "<redacted-sensitive-error-context>"
        );
    }
}
