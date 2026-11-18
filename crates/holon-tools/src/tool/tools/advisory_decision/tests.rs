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
