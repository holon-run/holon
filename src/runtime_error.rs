use std::collections::{BTreeMap, BTreeSet};

use anyhow::Error as AnyhowError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    provider::{provider_attempt_timeline, provider_transport_diagnostics, ProviderTransportError},
    runtime_db::{
        PersistenceCertainty, PersistenceFailure, RuntimeDbProtectionError,
        RuntimeDbRetryableError, RuntimeStateTransitionConflict,
    },
    tool::ToolError,
};

const SOURCE_CHAIN_MAX_ENTRIES: usize = 8;
const ERROR_TEXT_MAX_CHARS: usize = 512;

mod idle_input_recovery;
pub(crate) use idle_input_recovery::safe_recovery_id as safe_runtime_evidence_id;
pub(crate) use idle_input_recovery::{IdleInputRecoveryPredicate, IdleInputRecoveryRejection};

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
    if let Some(rejection) = error.downcast_ref::<IdleInputRecoveryRejection>() {
        // Do not copy outer error contexts that may contain rejected evidence.
        return rejection.descriptor();
    }
    if let Some(crate::context::ContextPlanningError::PinnedMinimumOverBudget {
        budget_estimated_tokens,
        pinned_minimum_estimated_tokens,
    }) = error.downcast_ref::<crate::context::ContextPlanningError>()
    {
        // Only typed numeric evidence is safe here; outer contexts may contain input text.
        let mut safe_context = BTreeMap::from([
            ("phase".into(), "context_build".into()),
            (
                "recovery_disposition".into(),
                "stop_until_input_change".into(),
            ),
            (
                "effective_budget_estimated_tokens".into(),
                budget_estimated_tokens.to_string(),
            ),
            (
                "pinned_minimum_estimated_tokens".into(),
                pinned_minimum_estimated_tokens.to_string(),
            ),
            (
                "deficit_estimated_tokens".into(),
                pinned_minimum_estimated_tokens
                    .saturating_sub(*budget_estimated_tokens)
                    .to_string(),
            ),
        ]);
        if let Some(budget) = error.downcast_ref::<crate::prompt::ContextBuildBudget>() {
            safe_context.insert(
                "configured_budget_estimated_tokens".into(),
                budget.configured_estimated_tokens.to_string(),
            );
            safe_context.insert(
                "runtime_time_reserved_tokens".into(),
                budget.runtime_time_reserved_tokens.to_string(),
            );
        }
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Validation,
            code: "context_pinned_minimum_over_budget".into(),
            retryable: false,
            operator_message: format!(
                "context pinned minimum exceeds budget: budget={budget_estimated_tokens}, pinned_minimum={pinned_minimum_estimated_tokens}"
            ),
            recovery_hint: Some(
                "Split new input or adjust the configured context budget within the model window; retry only after the input or budget changes. Provider fallback cannot fix this local planning failure."
                    .into(),
            ),
            safe_context,
            source_chain: Vec::new(),
        };
    }
    let source_chain = collect_runtime_error_source_chain(error);

    // Transaction certainty wins over retryable domain errors retained underneath it.
    if let Some(failure) = error
        .downcast_ref::<PersistenceFailure>()
        .filter(|failure| failure.certainty() == PersistenceCertainty::Indeterminate)
    {
        let phase = failure.phase().as_str();
        let mut safe_context = BTreeMap::from([
            ("certainty".into(), "indeterminate".into()),
            ("phase".into(), phase.into()),
        ]);
        safe_context.extend(runtime_db_failure_context(error));
        if let Some(db_error) = error.downcast_ref::<RuntimeDbRetryableError>() {
            safe_context
                .entry("operation".into())
                .or_insert_with(|| db_error.operation().to_string());
        }
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Storage,
            code: "runtime_db_outcome_unknown".into(),
            retryable: false,
            operator_message: "runtime database operation outcome is unknown".into(),
            recovery_hint: Some(
                "reconcile the original operation's durable outcome before submitting it again"
                    .into(),
            ),
            safe_context,
            source_chain,
        };
    }

    if let Some(runtime_error) = error.downcast_ref::<RuntimeError>().or_else(|| {
        error
            .chain()
            .find_map(|source| source.downcast_ref::<RuntimeError>())
    }) {
        let mut descriptor = runtime_error.descriptor().clone();
        descriptor.source_chain = merge_source_chains(descriptor.source_chain, source_chain);
        return descriptor;
    }

    if let Some(tool_error) = error.downcast_ref::<ToolError>().or_else(|| {
        error
            .chain()
            .find_map(|source| source.downcast_ref::<ToolError>())
    }) {
        return descriptor_from_tool_error(tool_error, source_chain);
    }

    if let Some(archive_error) =
        error.downcast_ref::<crate::skills::RemoteSkillArchiveFetchFailed>()
    {
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Io,
            code: "remote_skill_archive_fetch_failed".into(),
            retryable: archive_error.retryable(),
            operator_message: archive_error.to_string(),
            recovery_hint: Some(archive_error.recovery_hint().into()),
            safe_context: BTreeMap::from([("package".into(), archive_error.package.clone())]),
            source_chain,
        };
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
        if let Some(attempt) =
            provider_attempt_timeline(error).and_then(|timeline| timeline.attempts.last())
        {
            safe_context
                .entry("provider".into())
                .or_insert_with(|| sanitize_runtime_error_text(&attempt.provider));
            safe_context
                .entry("model_ref".into())
                .or_insert_with(|| sanitize_runtime_error_text(&attempt.model_ref));
        }
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Provider,
            code: code.to_string(),
            retryable,
            operator_message: provider_operator_message(kind, diagnostics, &safe_context),
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
        .find_map(|source| source.downcast_ref::<RuntimeDbProtectionError>())
    {
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Storage,
            code: "runtime_db_quarantined".into(),
            retryable: false,
            operator_message: sanitize_runtime_error_text(&db_error.to_string()),
            recovery_hint: Some(
                "preserve the runtime database sidecars and open-FD evidence, then restart or perform offline recovery"
                    .into(),
            ),
            safe_context: BTreeMap::from([(
                "protection_state".into(),
                "quarantined".into(),
            )]),
            source_chain: vec![sanitize_runtime_error_text(db_error.evidence())],
        };
    }

    if let Some(db_error) = error.downcast_ref::<RuntimeDbRetryableError>().or_else(|| {
        error
            .chain()
            .find_map(|source| source.downcast_ref::<RuntimeDbRetryableError>())
    }) {
        let source_chain = vec![format!(
            "runtime database operation {} is temporarily unavailable",
            db_error.operation()
        )];
        let mut safe_context = runtime_db_failure_context(error);
        safe_context
            .entry("operation".into())
            .or_insert_with(|| db_error.operation().to_string());
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Storage,
            code: "runtime_db_busy".into(),
            retryable: true,
            operator_message: format!(
                "runtime database operation {} is temporarily unavailable",
                db_error.operation()
            ),
            recovery_hint: Some("retry the operation".into()),
            safe_context,
            source_chain,
        };
    }

    if error.chain().any(|source| {
        source
            .downcast_ref::<rusqlite::Error>()
            .is_some_and(crate::runtime_db::connection::is_sqlite_locked)
    }) {
        let retryable = crate::runtime_db::persistence_certainty(error)
            == Some(PersistenceCertainty::DefinitelyNotCommitted);
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Storage,
            code: if retryable {
                "runtime_db_busy"
            } else {
                "runtime_db_locked"
            }
            .into(),
            retryable,
            operator_message: "runtime database is locked".into(),
            recovery_hint: Some(
                if retryable {
                    "retry the operation after the confirmed rollback"
                } else {
                    "inspect the original operation's transaction outcome before retrying"
                }
                .into(),
            ),
            safe_context: runtime_db_failure_context(error),
            source_chain,
        };
    }

    if error
        .chain()
        .any(|source| is_wait_owner_uniqueness_violation(source))
    {
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Storage,
            code: "wait_condition_owner_conflict".into(),
            retryable: true,
            operator_message:
                "wait condition registration conflicted with an unresolved wait for the same owner"
                    .into(),
            recovery_hint: Some(
                "bounded restart; the next wait registration converges the stale unresolved row"
                    .into(),
            ),
            safe_context: BTreeMap::new(),
            source_chain,
        };
    }

    let resource_conflict = error
        .chain()
        .find_map(|source| {
            source.downcast_ref::<crate::runtime::resource_observation::ResourceObservationError>()
        })
        .filter(|error| error.is_preparation_conflict())
        .map(ToString::to_string)
        .or_else(|| {
            error
                .downcast_ref::<crate::host::context_resources::ContextResourceWritersActive>()
                .map(ToString::to_string)
        });
    if let Some(conflict) = resource_conflict {
        return RuntimeErrorDescriptor {
            domain: RuntimeErrorDomain::Conflict,
            code: "context_resource_changed".into(),
            retryable: true,
            operator_message: conflict,
            recovery_hint: Some("prepare Context again from current resources; the host may rebuild a failed runtime loop".into()),
            safe_context: BTreeMap::new(),
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
            operator_message: provider_operator_message(
                kind,
                attempt.transport_diagnostics.as_ref(),
                &safe_context,
            ),
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

fn runtime_db_failure_context(error: &AnyhowError) -> BTreeMap<String, String> {
    let mut context = BTreeMap::new();
    if let Some(failure) = error.downcast_ref::<PersistenceFailure>() {
        context.insert("phase".into(), failure.phase().as_str().into());
        context.insert(
            "certainty".into(),
            match failure.certainty() {
                PersistenceCertainty::DefinitelyNotCommitted => "definitely_not_committed",
                PersistenceCertainty::Indeterminate => "indeterminate",
            }
            .into(),
        );
        if let Some(operation) = failure.operation() {
            context.insert("operation".into(), operation.into());
        }
        if let Some(table) = failure.table() {
            context.insert("table".into(), table.into());
        }
    }
    if let Some(code) = crate::runtime_db::persistence::sqlite_extended_code(error) {
        context.insert("sqlite_code".into(), (code & 0xff).to_string());
        context.insert("sqlite_extended_code".into(), code.to_string());
    }
    context
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
        domain: tool_error.domain.unwrap_or(RuntimeErrorDomain::Tool),
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

fn provider_operator_message(
    kind: &str,
    diagnostics: Option<&crate::provider::ProviderTransportDiagnostics>,
    context: &BTreeMap<String, String>,
) -> String {
    let Some(diagnostics) = diagnostics.filter(|diagnostics| diagnostics.stage == "oauth_refresh")
    else {
        return format!("provider request failed: {kind}");
    };
    let provider = context
        .get("model_ref")
        .or_else(|| context.get("provider"))
        .map(String::as_str)
        .unwrap_or("unknown");
    let reason = diagnostics
        .source_chain
        .first()
        .map(|message| sanitize_runtime_error_text(message))
        .unwrap_or_else(|| "OAuth refresh failed (refresh_failed)".into());
    format!("provider request failed for {provider}: {kind}: {reason}")
}

fn provider_recovery_hint(kind: &str) -> Option<String> {
    match kind {
        "auth_error" => Some("check provider authentication and credentials; sign in again if OAuth credentials were rejected".into()),
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
            | "repair_admitted"
            | "existing_status"
            | "incoming_status"
            | "expected_revision"
            | "actual_revision"
            | "operation"
            | "predicate"
            | "recovery_disposition"
            | "activation_id"
            | "predecessor_activation_id"
            | "source_turn_id"
            | "batch_activation_id"
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

/// Wait-owner uniqueness violations (migration 44 partial unique indexes) are
/// transient convergence races, not permanent state damage: the next wait
/// registration cancels the stale unresolved row. Classify them as retryable so
/// the host rebuilds the runtime loop instead of leaving the agent stopped.
/// Match only the two migration 44 owner-index messages: the migration 43
/// trigger-message index shares the `agent_id` prefix, but a duplicate trigger
/// is not repaired by owner convergence, so it must stay non-retryable.
fn is_wait_owner_uniqueness_message(message: &str) -> bool {
    matches!(
        message,
        "UNIQUE constraint failed: wait_conditions.agent_id"
            | "UNIQUE constraint failed: wait_conditions.agent_id, wait_conditions.work_item_id"
    )
}

fn is_wait_owner_uniqueness_violation(source: &(dyn std::error::Error + 'static)) -> bool {
    if let Some(db_error) = source.downcast_ref::<rusqlite::Error>() {
        return matches!(
            db_error,
            rusqlite::Error::SqliteFailure(inner, Some(message))
                if inner.code == rusqlite::ErrorCode::ConstraintViolation
                    && is_wait_owner_uniqueness_message(message)
        );
    }
    is_wait_owner_uniqueness_message(&source.to_string())
}

#[cfg(holon_lib_test_shard = "d")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_db::PersistenceFailurePhase;

    #[test]
    fn oauth_refresh_brief_preserves_safe_reason_and_active_model_with_or_without_source() {
        use crate::provider::{
            provider_transport_error_with_code, provider_turn_error, ProviderFailureClassification,
            ProviderFailureKind, ProviderTransportDiagnostics, RetryDisposition,
        };

        let diagnostics: ProviderTransportDiagnostics = serde_json::from_value(serde_json::json!({
            "stage": "oauth_refresh",
            "provider": "xai",
            "status": 400,
            "source_chain": ["OAuth refresh rejected with HTTP 400 (refresh_failed)"]
        }))
        .unwrap();
        let timeline: crate::provider::ProviderAttemptTimeline =
            serde_json::from_value(serde_json::json!({
                "requested_model_ref": "openai@default/requested",
                "attempts": [{
                    "provider": "xai",
                    "model_ref": "xai@default/grok-test",
                    "attempt": 1,
                    "max_attempts": 3,
                    "failure_kind": "auth_error",
                    "disposition": "fail_fast",
                    "outcome": "fail_fast_aborted",
                    "advanced_to_fallback": false,
                    "transport_diagnostics": diagnostics
                }]
            }))
            .unwrap();
        for with_source in [true, false] {
            let source = if with_source {
                provider_transport_error_with_code(
                    ProviderFailureClassification {
                        kind: ProviderFailureKind::AuthError,
                        disposition: RetryDisposition::FailFast,
                    },
                    None,
                    Some(400),
                    Some(diagnostics.clone()),
                    "OAuth refresh failed: private-upstream-payload",
                )
            } else {
                AnyhowError::msg("provider source no longer available")
            };
            let error = provider_turn_error("provider turn failed", timeline.clone(), source);
            let descriptor = describe_runtime_error(&error);
            assert_eq!(descriptor.domain, RuntimeErrorDomain::Provider);
            assert_eq!(descriptor.code, "auth_error");
            assert!(!descriptor.retryable);
            assert!(descriptor
                .operator_message
                .contains("xai@default/grok-test"));
            assert!(descriptor.operator_message.contains("refresh_failed"));
            assert!(descriptor.operator_message.contains("HTTP 400"));
            assert!(!descriptor
                .operator_message
                .contains("private-upstream-payload"));
            assert!(descriptor.recovery_hint.unwrap().contains("sign in again"));
        }
    }

    #[test]
    fn context_resource_recovery_retries_conflicts_but_not_identity_or_closed_scopes() {
        use crate::runtime::resource_observation::{
            ResourceFreshness, ResourceObservationError as Error,
        };
        for error in [
            Error::SourceChanged,
            Error::ObsoleteRefresh,
            Error::InventorySnapshotChanged,
            Error::NotReady(ResourceFreshness::Dirty(
                crate::runtime::resource_observation::ResourceInvalidation::KnownWrite,
            )),
        ] {
            let error = AnyhowError::new(error).context("context acquisition");
            let descriptor = describe_runtime_error(&error);
            assert_eq!(descriptor.domain, RuntimeErrorDomain::Conflict);
            assert_eq!(descriptor.code, "context_resource_changed");
            assert!(descriptor.retryable);
        }
        for error in [
            Error::ScopeChanged,
            Error::NotReady(ResourceFreshness::Closed),
            Error::GenerationExhausted,
        ] {
            assert!(!describe_runtime_error(&AnyhowError::new(error)).retryable);
        }
        let writer = AnyhowError::new(crate::host::context_resources::ContextResourceWritersActive);
        assert!(describe_runtime_error(&writer).retryable);
    }

    #[test]
    fn sqlite_lock_classification_requires_a_known_transaction_outcome() {
        use crate::runtime_db::persistence::mark_persistence_failure;
        fn locked() -> AnyhowError {
            AnyhowError::new(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY_SNAPSHOT),
                Some("database is locked".into()),
            ))
        }
        let unknown = describe_runtime_error(&locked());
        assert_eq!(unknown.domain, RuntimeErrorDomain::Storage);
        assert_eq!(unknown.code, "runtime_db_locked");
        assert!(!unknown.retryable);
        assert_eq!(unknown.safe_context["sqlite_code"], "5");
        assert_eq!(unknown.safe_context["sqlite_extended_code"], "517");
        let rolled_back = mark_persistence_failure(
            locked(),
            PersistenceCertainty::DefinitelyNotCommitted,
            PersistenceFailurePhase::Callback,
            None,
        );
        let known = describe_runtime_error(&rolled_back);
        assert_eq!(known.code, "runtime_db_busy");
        assert!(known.retryable);
        let ambiguous = mark_persistence_failure(
            locked(),
            PersistenceCertainty::Indeterminate,
            PersistenceFailurePhase::Commit,
            None,
        );
        let unknown = describe_runtime_error(&ambiguous);
        assert_eq!(unknown.code, "runtime_db_outcome_unknown");
        assert!(!unknown.retryable);
        assert_eq!(unknown.safe_context["phase"], "commit");
        assert_eq!(unknown.safe_context["sqlite_extended_code"], "517");
    }

    #[test]
    fn context_budget_descriptor_preserves_typed_numeric_evidence_without_input() {
        let error = AnyhowError::from(
            crate::context::ContextPlanningError::PinnedMinimumOverBudget {
                budget_estimated_tokens: 3968,
                pinned_minimum_estimated_tokens: 6382,
            },
        )
        .context(crate::prompt::ContextBuildBudget {
            configured_estimated_tokens: 4096,
            effective_estimated_tokens: 3968,
            runtime_time_reserved_tokens: 128,
        })
        .context("INPUT_SECRET /private/input https://example.com/?token=SECRET");

        assert!(error.is::<crate::context::ContextPlanningError>());
        let descriptor = describe_runtime_error(&error);
        assert_eq!(descriptor.domain, RuntimeErrorDomain::Validation);
        assert_eq!(descriptor.code, "context_pinned_minimum_over_budget");
        assert!(!descriptor.retryable);
        assert_eq!(descriptor.safe_context["phase"], "context_build");
        assert_eq!(
            descriptor.safe_context["recovery_disposition"],
            "stop_until_input_change"
        );
        assert_eq!(
            descriptor.safe_context["configured_budget_estimated_tokens"],
            "4096"
        );
        assert_eq!(
            descriptor.safe_context["effective_budget_estimated_tokens"],
            "3968"
        );
        assert_eq!(
            descriptor.safe_context["runtime_time_reserved_tokens"],
            "128"
        );
        assert_eq!(
            descriptor.safe_context["pinned_minimum_estimated_tokens"],
            "6382"
        );
        assert_eq!(descriptor.safe_context["deficit_estimated_tokens"], "2414");
        assert!(descriptor.source_chain.is_empty());
        let serialized = serde_json::to_string(&descriptor).unwrap();
        for secret in ["INPUT_SECRET", "/private/input", "example.com", "SECRET"] {
            assert!(!serialized.contains(secret), "{serialized}");
        }
    }

    #[test]
    fn context_budget_classification_requires_typed_planning_error() {
        let descriptor = describe_runtime_error(&AnyhowError::msg(
            "pinned_minimum_over_budget: budget=3968, pinned_minimum=6382",
        ));
        assert_eq!(descriptor.domain, RuntimeErrorDomain::Unknown);
        assert_eq!(descriptor.code, "runtime_error");

        let error = AnyhowError::from(
            crate::context::ContextPlanningError::PinnedMinimumOverBudget {
                budget_estimated_tokens: 8,
                pinned_minimum_estimated_tokens: 9,
            },
        );
        let descriptor = describe_runtime_error(&error);
        assert_eq!(descriptor.code, "context_pinned_minimum_over_budget");
        assert_eq!(
            descriptor.safe_context["effective_budget_estimated_tokens"],
            "8"
        );
        assert!(!descriptor
            .safe_context
            .contains_key("configured_budget_estimated_tokens"));
    }

    #[test]
    fn archive_fetch_descriptor_keeps_safe_cause_with_long_package() {
        let cause = AnyhowError::msg(
            "wrapper\nfailed https://user:secret@example.com/archive?token=secret#secret",
        );
        let context = crate::skills::RemoteSkillArchiveFetchFailed::new(&"包".repeat(1000), &cause);
        let error = cause
            .context(context)
            .context("template skill installation failed");
        let descriptor = describe_runtime_error(&error);
        assert_eq!(descriptor.domain, RuntimeErrorDomain::Io);
        assert_eq!(descriptor.code, "remote_skill_archive_fetch_failed");
        assert!(!descriptor.retryable);
        assert!(descriptor.operator_message.contains("example.com/archive"));
        assert!(descriptor.operator_message.chars().count() <= ERROR_TEXT_MAX_CHARS);
        assert!(descriptor.safe_context["package"].chars().count() <= 96);
        let json = serde_json::to_string(&descriptor).unwrap();
        assert!(!json.contains("secret"), "{json}");
        assert!(!json.contains("user:"), "{json}");
        assert!(descriptor
            .source_chain
            .iter()
            .any(|cause| cause.contains("archive fetch")));
        assert!(descriptor.recovery_hint.unwrap().contains("archive"));
    }

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
    fn indeterminate_persistence_overrides_retryable_errors_and_outer_runtime_context() {
        use crate::runtime_db::persistence::mark_persistence_failure;
        use std::path::Path;

        let cases = [
            (
                AnyhowError::new(RuntimeDbRetryableError::new(
                    "committing transaction",
                    Path::new("/private/agent/runtime.sqlite"),
                    "database is locked",
                )),
                PersistenceFailurePhase::Commit,
                "commit",
            ),
            (
                AnyhowError::new(RuntimeStateTransitionConflict::revision(
                    "agent-1",
                    "agent_state_revision_conflict",
                    Some(1),
                    Some(2),
                    true,
                )),
                PersistenceFailurePhase::Rollback,
                "rollback",
            ),
        ];
        for (cause, phase, expected_phase) in cases {
            let error =
                mark_persistence_failure(cause, PersistenceCertainty::Indeterminate, phase, None)
                    .context(RuntimeError::new(
                        RuntimeErrorDomain::Runtime,
                        "outer_command_failed",
                        "outer command failed",
                    ));

            let descriptor = describe_runtime_error(&error);

            assert_eq!(descriptor.domain, RuntimeErrorDomain::Storage);
            assert_eq!(descriptor.code, "runtime_db_outcome_unknown");
            assert!(!descriptor.retryable);
            assert_eq!(descriptor.safe_context["certainty"], "indeterminate");
            assert_eq!(descriptor.safe_context["phase"], expected_phase);
            assert!(descriptor
                .recovery_hint
                .unwrap()
                .contains("original operation"));
            assert!(error.downcast_ref::<PersistenceFailure>().is_some());
            if phase == PersistenceFailurePhase::Commit {
                assert!(error.downcast_ref::<RuntimeDbRetryableError>().is_some());
                assert_eq!(
                    descriptor.safe_context["operation"],
                    "committing transaction"
                );
                assert!(!descriptor.source_chain.join(" ").contains("/private/agent"));
            } else {
                assert!(error
                    .downcast_ref::<RuntimeStateTransitionConflict>()
                    .is_some());
            }
        }
    }

    #[test]
    fn definitely_uncommitted_persistence_keeps_existing_retry_classification() {
        use crate::runtime_db::persistence::mark_persistence_failure;
        use std::path::Path;

        let cases = [
            (
                AnyhowError::new(RuntimeDbRetryableError::new(
                    "beginning transaction",
                    Path::new("runtime.sqlite"),
                    "database is locked",
                )),
                "runtime_db_busy",
            ),
            (
                AnyhowError::new(RuntimeStateTransitionConflict::revision(
                    "agent-1",
                    "agent_state_revision_conflict",
                    Some(1),
                    Some(2),
                    true,
                )),
                "agent_state_revision_conflict",
            ),
        ];
        for (cause, expected_code) in cases {
            let error = mark_persistence_failure(
                cause,
                PersistenceCertainty::DefinitelyNotCommitted,
                PersistenceFailurePhase::Callback,
                None,
            );
            let descriptor = describe_runtime_error(&error);

            assert_eq!(descriptor.code, expected_code);
            assert!(descriptor.retryable);
        }
    }

    #[test]
    fn wait_owner_uniqueness_violation_is_retryable() {
        let error = AnyhowError::from(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(
                "UNIQUE constraint failed: wait_conditions.agent_id, wait_conditions.work_item_id"
                    .into(),
            ),
        ))
        .context("runtime db transaction rolled back");

        let descriptor = describe_runtime_error(&error);

        assert_eq!(descriptor.domain, RuntimeErrorDomain::Storage);
        assert_eq!(descriptor.code, "wait_condition_owner_conflict");
        assert!(descriptor.retryable);
    }

    #[test]
    fn wait_owner_uniqueness_violation_matches_rendered_message_fallback() {
        let error = AnyhowError::msg("UNIQUE constraint failed: wait_conditions.agent_id");

        let descriptor = describe_runtime_error(&error);

        assert_eq!(descriptor.code, "wait_condition_owner_conflict");
        assert!(descriptor.retryable);
    }

    #[test]
    fn trigger_message_uniqueness_violation_is_not_owner_conflict() {
        let error = AnyhowError::from(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(
                "UNIQUE constraint failed: wait_conditions.agent_id, wait_conditions.trigger_message_id"
                    .into(),
            ),
        ))
        .context("runtime db transaction rolled back");

        let descriptor = describe_runtime_error(&error);

        assert_ne!(descriptor.code, "wait_condition_owner_conflict");
        assert!(!descriptor.retryable);
    }

    #[test]
    fn trigger_message_uniqueness_violation_is_not_owner_conflict_rendered() {
        let error = AnyhowError::msg(
            "UNIQUE constraint failed: wait_conditions.agent_id, wait_conditions.trigger_message_id",
        );

        let descriptor = describe_runtime_error(&error);

        assert_ne!(descriptor.code, "wait_condition_owner_conflict");
        assert!(!descriptor.retryable);
    }

    #[test]
    fn typed_runtime_error_is_detected_when_used_as_anyhow_context() {
        let error = AnyhowError::msg("template selector is invalid").context(
            RuntimeError::new(
                RuntimeErrorDomain::Task,
                "agent_invocation_failed",
                "failed to invoke agent",
            )
            .with_safe_context("task_id", "task_123"),
        );

        let descriptor = describe_runtime_error(&error);

        assert_eq!(descriptor.domain, RuntimeErrorDomain::Task);
        assert_eq!(descriptor.code, "agent_invocation_failed");
        assert_eq!(descriptor.safe_context["task_id"], "task_123");
        assert!(descriptor
            .source_chain
            .iter()
            .any(|message| message == "template selector is invalid"));
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
            sanitize_runtime_error_text(r#"provider failed: {"access_token":"secret"}"#),
            "<redacted-sensitive-error-context>"
        );
        assert_eq!(
            sanitize_runtime_error_text("provider failed: x-api-key: short-secret"),
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
