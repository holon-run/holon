use super::BuiltinToolDefinition;
use crate::tool::spec::typed_spec;
use crate::types::AuthorityClass;
use crate::types::ToolCapabilityFamily;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;

pub const NAME: &str = crate::tool::names::ADVISORY_DECISION;

pub const SCHEMA: &str = "holon.advisory_decision";

pub const SCHEMA_VERSION: &str = "1";

const MAX_QUESTION_CHARS: usize = 512;

const MAX_STATE_CHARS: usize = 4_000;

const MAX_OPTIONS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdvisoryDecisionArgs {
    #[schemars(length(min = 1, max = 512))]
    pub question: String,
    #[schemars(length(min = 2, max = 8))]
    pub options: Vec<String>,
    #[serde(default)]
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdvisoryDecisionResult {
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
}

pub fn definition() -> Result<BuiltinToolDefinition> {
    Ok(BuiltinToolDefinition {
        family: ToolCapabilityFamily::CoreAgent,
        spec: typed_spec::<AdvisoryDecisionArgs>(
            NAME,
            include_str!("../tool_descriptions/advisory_decision.md"),
        )?,
    })
}

pub fn ensure_authority(authority_class: &AuthorityClass) -> Result<()> {
    if matches!(authority_class, AuthorityClass::ExternalEvidence) {
        return Err(crate::tool::ToolError::new(
            "authority_denied",
            "advisory decision tool is unavailable for external evidence",
        )
        .into());
    }
    Ok(())
}

pub fn validate_args(args: &AdvisoryDecisionArgs) -> Result<()> {
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

pub fn sanitize_state(state: &str) -> String {
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

pub fn fingerprint(question: &str, options: &[String], state: &str) -> String {
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

pub fn redact_text(value: &str, max_chars: usize) -> String {
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

pub fn contains_sensitive_state_marker(value: &str) -> bool {
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

pub fn abstain_result(
    reason: &str,
    message: &str,
    provider: &str,
    question_fingerprint: String,
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
    )
}

#[allow(clippy::too_many_arguments)]
pub fn abstain_result_with_metadata(
    reason: &str,
    message: &str,
    provider: String,
    model: String,
    confidence: Option<f32>,
    latency_ms: Option<u64>,
    evidence: Vec<Value>,
    question_fingerprint: String,
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
        );

        assert_eq!(result.question_fingerprint, "0123456789abcdef");
        let serialized = serde_json::to_value(&result).unwrap();
        assert!(serialized.get("options_summary").is_none());
    }
}
