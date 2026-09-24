use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;

use decision_core::{DecisionOutcome, DecisionRequest};

use crate::{
    decision_telemetry::{stable_decision_id, DecisionAdvisoryCompletedEvent},
    runtime::RuntimeHandle,
    tool::{
        helpers::parse_tool_args,
        spec::{typed_spec, ToolExecutionContext},
    },
    types::{AuthorityClass, ToolCapabilityFamily},
};

use super::{serialize_success, BuiltinToolDefinition};

pub(crate) const NAME: &str = crate::tool::names::ADVISORY_DECISION;
const SCHEMA: &str = "holon.advisory_decision";
const SCHEMA_VERSION: &str = "1";
const MAX_QUESTION_CHARS: usize = 512;
const MAX_STATE_CHARS: usize = 4_000;
const MAX_OPTIONS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdvisoryDecisionArgs {
    #[schemars(length(min = 1, max = 512))]
    pub question: String,
    #[schemars(length(min = 2, max = 8))]
    pub options: Vec<String>,
    #[serde(default)]
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AdvisoryDecisionResult {
    pub outcome: String,
    pub choice: Option<String>,
    pub confidence: Option<f32>,
    pub abstain: bool,
    pub reason: Option<String>,
    pub provider: String,
    pub model: String,
    pub latency_ms: Option<u64>,
    pub evidence: Vec<Value>,
    pub question_fingerprint: String,
    pub options_summary: Vec<String>,
}

pub(crate) fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<AdvisoryDecisionArgs>(
            NAME,
            include_str!("../tool_descriptions/advisory_decision.md"),
        )?,
    })
}

pub(crate) async fn execute(
    runtime: &RuntimeHandle,
    agent_id: &str,
    authority_class: &AuthorityClass,
    input: &Value,
    context: &ToolExecutionContext,
) -> Result<crate::tool::ToolResult> {
    ensure_authority(authority_class)?;
    let mut args: AdvisoryDecisionArgs = parse_tool_args(NAME, input)?;
    validate_args(&args)?;
    args.question = args.question.trim().to_owned();
    args.options = args
        .options
        .into_iter()
        .map(|option| option.trim().to_owned())
        .collect();
    let state = sanitize_state(&args.state);
    let fingerprint = fingerprint(&args.question, &args.options, &state);
    let (enabled, max_calls, timeout_ms, min_confidence) = runtime.advisory_decision_tool_config();
    if !enabled {
        return finish_result(
            runtime,
            agent_id,
            context,
            &args,
            &fingerprint,
            abstain_result(
                "disabled",
                "advisory decision tool is disabled",
                "",
                fingerprint.clone(),
                &args.options,
            ),
        );
    }
    if let Some(max_calls) = max_calls {
        let call_index = context
            .decision_tool_calls
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if call_index >= max_calls {
            return finish_result(
                runtime,
                agent_id,
                context,
                &args,
                &fingerprint,
                abstain_result(
                    "rate_limited",
                    "per-turn advisory decision limit reached",
                    "",
                    fingerprint.clone(),
                    &args.options,
                ),
            );
        }
    }

    let request = DecisionRequest {
        request_id: format!("advisory-{fingerprint}"),
        input: json!({
            "question": args.question,
            "state": state,
        }),
        candidates: args.options.iter().map(|option| json!(option)).collect(),
        schema: SCHEMA.into(),
        schema_version: SCHEMA_VERSION.into(),
        metadata: [("integration".into(), "holon-agent-tool".into())]
            .into_iter()
            .collect(),
        deadline_ms: Some(timeout_ms),
    };
    let started = std::time::Instant::now();
    let response = match tokio::time::timeout(
        Duration::from_millis(timeout_ms.max(1)),
        runtime.execute_advisory_decision(request),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            return finish_result(
                runtime,
                agent_id,
                context,
                &args,
                &fingerprint,
                abstain_result(
                    "provider_failure",
                    &redact_text(&error.to_string(), 240),
                    "",
                    fingerprint.clone(),
                    &args.options,
                ),
            );
        }
        Err(_) => {
            return finish_result(
                runtime,
                agent_id,
                context,
                &args,
                &fingerprint,
                abstain_result(
                    "timeout",
                    "advisory decision provider timed out",
                    "",
                    fingerprint.clone(),
                    &args.options,
                ),
            );
        }
    };
    let (response, provider, model) = response;
    let latency_ms = response
        .elapsed_ms
        .or_else(|| Some(started.elapsed().as_millis() as u64));
    if let Err(error) = response.validate(SCHEMA_VERSION) {
        return finish_result(
            runtime,
            agent_id,
            context,
            &args,
            &fingerprint,
            abstain_result_with_metadata(
                "invalid_response",
                &redact_text(&error.to_string(), 240),
                provider,
                model,
                response.confidence,
                latency_ms,
                Vec::new(),
                fingerprint.clone(),
                args.options
                    .iter()
                    .map(|option| redact_text(option, 120))
                    .collect(),
            ),
        );
    }
    let confidence = response.confidence;
    let evidence = response
        .evidence
        .into_iter()
        .map(|item| {
            json!({
                "kind": item.kind,
                "summary": redact_text(&item.summary, 240),
            })
        })
        .collect();
    let options_summary = args
        .options
        .iter()
        .map(|option| redact_text(option, 120))
        .collect();
    let result = match response.outcome {
        DecisionOutcome::Select { value } => {
            let choice = value.as_str().map(str::to_owned);
            if confidence.is_none_or(|value| value < min_confidence) {
                abstain_result_with_metadata(
                    "low_confidence",
                    "provider confidence is missing or below the configured threshold",
                    provider,
                    model,
                    confidence,
                    latency_ms,
                    evidence,
                    fingerprint.clone(),
                    options_summary,
                )
            } else if choice
                .as_deref()
                .is_some_and(|value| args.options.iter().any(|option| option == value))
            {
                AdvisoryDecisionResult {
                    outcome: "select".into(),
                    choice,
                    confidence,
                    abstain: false,
                    reason: None,
                    provider,
                    model,
                    latency_ms,
                    evidence,
                    question_fingerprint: fingerprint.clone(),
                    options_summary,
                }
            } else {
                abstain_result_with_metadata(
                    "invalid_choice",
                    "provider returned an option outside the request",
                    provider,
                    model,
                    confidence,
                    latency_ms,
                    evidence,
                    fingerprint.clone(),
                    options_summary,
                )
            }
        }
        DecisionOutcome::Fallback { value } => {
            let choice = value.as_str().map(str::to_owned);
            if confidence.is_none_or(|value| value < min_confidence) {
                abstain_result_with_metadata(
                    "low_confidence",
                    "provider confidence is missing or below the configured threshold",
                    provider,
                    model,
                    confidence,
                    latency_ms,
                    evidence,
                    fingerprint.clone(),
                    options_summary,
                )
            } else if choice
                .as_deref()
                .is_some_and(|value| args.options.iter().any(|option| option == value))
            {
                AdvisoryDecisionResult {
                    outcome: "fallback".into(),
                    choice,
                    confidence,
                    abstain: false,
                    reason: None,
                    provider,
                    model,
                    latency_ms,
                    evidence,
                    question_fingerprint: fingerprint.clone(),
                    options_summary,
                }
            } else {
                abstain_result_with_metadata(
                    "invalid_choice",
                    "provider returned an option outside the request",
                    provider,
                    model,
                    confidence,
                    latency_ms,
                    evidence,
                    fingerprint.clone(),
                    options_summary,
                )
            }
        }
        DecisionOutcome::Abstain { reason } => abstain_result_with_metadata(
            "provider_abstain",
            &redact_text(&reason, 240),
            provider,
            model,
            confidence,
            latency_ms,
            evidence,
            fingerprint.clone(),
            options_summary,
        ),
        DecisionOutcome::Rank { .. } => abstain_result_with_metadata(
            "unsupported_outcome",
            "provider returned a ranking instead of a single advisory choice",
            provider,
            model,
            confidence,
            latency_ms,
            evidence,
            fingerprint.clone(),
            options_summary,
        ),
    };
    finish_result(runtime, agent_id, context, &args, &fingerprint, result)
}

fn ensure_authority(authority_class: &AuthorityClass) -> Result<()> {
    if matches!(authority_class, AuthorityClass::ExternalEvidence) {
        return Err(crate::tool::ToolError::new(
            "authority_denied",
            "advisory decision tool is unavailable for external evidence",
        )
        .into());
    }
    Ok(())
}

fn validate_args(args: &AdvisoryDecisionArgs) -> Result<()> {
    anyhow::ensure!(
        !args.question.trim().is_empty() && args.question.chars().count() <= MAX_QUESTION_CHARS,
        "question must be non-empty and at most {MAX_QUESTION_CHARS} characters"
    );
    anyhow::ensure!(
        (2..=MAX_OPTIONS).contains(&args.options.len()),
        "options must contain between 2 and {MAX_OPTIONS} mutually exclusive values"
    );
    anyhow::ensure!(
        args.options
            .iter()
            .all(|option| !option.trim().is_empty() && option.chars().count() <= 256),
        "each option must be non-empty and at most 256 characters"
    );
    anyhow::ensure!(
        {
            let mut unique = std::collections::HashSet::new();
            args.options
                .iter()
                .map(|option| option.trim())
                .all(|option| unique.insert(option))
        },
        "options must be unique"
    );
    Ok(())
}

fn sanitize_state(state: &str) -> String {
    let state = state.trim();
    let sanitized = serde_json::from_str::<Value>(state)
        .map(|value| redact_state_value(&value))
        .ok()
        .and_then(|value| serde_json::to_string(&value).ok())
        .unwrap_or_else(|| {
            if contains_sensitive_state_marker(state) {
                "[REDACTED-SENSITIVE-STATE]".into()
            } else {
                state.into()
            }
        });
    sanitized.chars().take(MAX_STATE_CHARS).collect()
}

fn fingerprint(question: &str, options: &[String], state: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(question.as_bytes());
    for option in options {
        hasher.update([0]);
        hasher.update(option.as_bytes());
    }
    hasher.update([0]);
    hasher.update(state.as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_owned()
}

fn redact_text(value: &str, max_chars: usize) -> String {
    let value = sanitize_state(value);
    value.chars().take(max_chars).collect()
}

fn redact_state_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let redacted = if is_sensitive_state_key(key) {
                        Value::String("[REDACTED]".into())
                    } else {
                        redact_state_value(value)
                    };
                    (key.clone(), redacted)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_state_value).collect()),
        Value::String(value) if contains_sensitive_state_marker(value) => {
            Value::String("[REDACTED]".into())
        }
        other => other.clone(),
    }
}

fn is_sensitive_state_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['-', ' ', '.'], "_");
    [
        "secret",
        "token",
        "api_key",
        "apikey",
        "authorization",
        "password",
        "credential",
        "capability",
    ]
    .iter()
    .any(|sensitive| normalized.contains(sensitive))
}

fn contains_sensitive_state_marker(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "authorization:",
        "bearer ",
        "api_key",
        "api-key",
        "access_token",
        "access-token",
        "x-api-key",
        "capability_secret",
        "capability://",
        "/api/callbacks/wake/",
        "/api/callbacks/enqueue/",
        "/callbacks/wake/",
        "/callbacks/enqueue/",
        "token=",
        "secret=",
        "password=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn finish_result(
    runtime: &RuntimeHandle,
    agent_id: &str,
    context: &ToolExecutionContext,
    args: &AdvisoryDecisionArgs,
    question_fingerprint: &str,
    result: AdvisoryDecisionResult,
) -> Result<crate::tool::ToolResult> {
    let decision_id = stable_decision_id(
        agent_id,
        context.turn_id.as_deref(),
        context.message_id.as_deref(),
        context.effective_work_item_id.as_deref(),
        context.tool_call_id.as_deref(),
        question_fingerprint,
    );
    let error_class = result
        .reason
        .as_deref()
        .and_then(|reason| reason.split_once(':').map(|(class, _)| class.to_string()));
    let event = DecisionAdvisoryCompletedEvent {
        decision_id,
        agent_id: agent_id.to_string(),
        turn_id: context.turn_id.clone(),
        message_id: context.message_id.clone(),
        work_item_id: context.effective_work_item_id.clone(),
        request_fingerprint: question_fingerprint.to_string(),
        provider: result.provider.clone(),
        model: result.model.clone(),
        latency_ms: result.latency_ms,
        token_count: None,
        cost_usd: None,
        outcome: result.outcome.clone(),
        choice: result.choice.clone(),
        confidence: result.confidence,
        abstain: result.abstain,
        fallback: result.outcome == "fallback",
        timeout: error_class.as_deref() == Some("timeout"),
        error_class,
        reason: result.reason.as_deref().and_then(|reason| {
            crate::decision_telemetry::safe_value(Some(&Value::String(reason.to_string())), 512)
        }),
        evidence: result.evidence.clone(),
        recorded_at: chrono::Utc::now(),
    };
    if let Err(error) = runtime.append_decision_advisory_event(&event) {
        tracing::warn!(error = %error, "failed to append decision advisory telemetry");
    }
    let audit = json!({
        "agent_id": agent_id,
        "work_item_id": context.effective_work_item_id,
        "question_fingerprint": question_fingerprint,
        "options_summary": result.options_summary,
        "provider": result.provider,
        "model": result.model,
        "latency_ms": result.latency_ms,
        "outcome": result.outcome,
        "abstain": result.abstain,
        "reason": result.reason,
        "confidence": result.confidence,
        "state_policy": "truncated_redacted_not_recorded",
        "question_chars": args.question.chars().count(),
    });
    if let Err(error) = runtime.append_audit_event("advisory_decision_tool", audit) {
        tracing::warn!(error = %error, "failed to append advisory decision audit event");
    }
    Ok(serialize_success(NAME, &result)?)
}

fn abstain_result(
    reason: &str,
    message: &str,
    provider: &str,
    question_fingerprint: String,
    options: &[String],
) -> AdvisoryDecisionResult {
    abstain_result_with_metadata(
        reason,
        message,
        provider.into(),
        String::new(),
        None,
        None,
        Vec::new(),
        question_fingerprint,
        options
            .iter()
            .map(|option| redact_text(option, 120))
            .collect(),
    )
}

#[allow(clippy::too_many_arguments)]
fn abstain_result_with_metadata(
    reason: &str,
    message: &str,
    provider: String,
    model: String,
    confidence: Option<f32>,
    latency_ms: Option<u64>,
    evidence: Vec<Value>,
    question_fingerprint: String,
    options_summary: Vec<String>,
) -> AdvisoryDecisionResult {
    AdvisoryDecisionResult {
        outcome: "abstain".into(),
        choice: None,
        confidence,
        abstain: true,
        reason: Some(format!("{reason}: {}", redact_text(message, 240))),
        provider,
        model,
        latency_ms,
        evidence,
        question_fingerprint,
        options_summary,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        abstain_result, contains_sensitive_state_marker, ensure_authority, sanitize_state,
        validate_args, AdvisoryDecisionArgs,
    };
    use crate::types::AuthorityClass;

    #[test]
    fn external_evidence_cannot_invoke_advisory_decision() {
        let error = ensure_authority(&AuthorityClass::ExternalEvidence).unwrap_err();
        let tool_error = error.downcast_ref::<crate::tool::ToolError>().unwrap();

        assert_eq!(tool_error.kind, "authority_denied");
        assert!(!tool_error.retryable);
    }

    #[test]
    fn trusted_authority_can_invoke_advisory_decision() {
        ensure_authority(&AuthorityClass::OperatorInstruction).unwrap();
        ensure_authority(&AuthorityClass::RuntimeInstruction).unwrap();
        ensure_authority(&AuthorityClass::IntegrationSignal).unwrap();
    }

    #[test]
    fn state_redaction_removes_structured_credentials_and_capability_urls() {
        let state = r#"{"safe":"keep","token":"real-token","nested":{"url":"/api/callbacks/wake/capability-secret"}}"#;
        let sanitized = sanitize_state(state);

        assert!(sanitized.contains("keep"));
        assert!(!sanitized.contains("real-token"));
        assert!(!sanitized.contains("capability-secret"));
        assert!(sanitized.contains("[REDACTED]"));
    }

    #[test]
    fn state_redaction_drops_inline_sensitive_values_instead_of_preserving_them() {
        let sanitized = sanitize_state("authorization: Bearer real-token token=another-secret");

        assert_eq!(sanitized, "[REDACTED-SENSITIVE-STATE]");
        assert!(!sanitized.contains("real-token"));
        assert!(!sanitized.contains("another-secret"));
        assert!(contains_sensitive_state_marker("token=another-secret"));
    }

    #[test]
    fn advisory_arguments_reject_duplicate_and_oversized_values() {
        let duplicate = AdvisoryDecisionArgs {
            question: "choose".into(),
            options: vec!["yes".into(), "yes".into()],
            state: String::new(),
        };
        assert!(validate_args(&duplicate).is_err());

        let oversized_question = AdvisoryDecisionArgs {
            question: "x".repeat(513),
            options: vec!["yes".into(), "no".into()],
            state: String::new(),
        };
        assert!(validate_args(&oversized_question).is_err());
    }

    #[test]
    fn abstain_result_preserves_question_fingerprint() {
        let result = abstain_result(
            "disabled",
            "advisory decision tool is disabled",
            "",
            "0123456789abcdef".into(),
            &["yes".into(), "no".into()],
        );

        assert_eq!(result.question_fingerprint, "0123456789abcdef");
    }
}
