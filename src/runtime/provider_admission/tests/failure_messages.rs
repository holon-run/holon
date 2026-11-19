use super::*;
use crate::runtime_error::{describe_runtime_error, RuntimeError, RuntimeErrorDomain};

fn rejection(denial: Option<LocalDispatchDenial>, disposition: &str) -> anyhow::Error {
    let diagnostic = match denial {
        Some(denial) => anyhow::Error::new(DispatchDeferred(denial, None)),
        None => anyhow::anyhow!("legacy admission diagnostic"),
    };
    diagnostic.context(
        RuntimeError::new(
            RuntimeErrorDomain::Conflict,
            "provider_dispatch_rejected",
            "Local provider dispatch admission rejected this send before IO.",
        )
        .with_safe_context("recovery_disposition", disposition),
    )
}

#[test]
fn revision_dispatch_failure_has_explicit_receipt_and_reentry_exit() {
    use crate::runtime_db::provider_admission::ProviderDispatchRejection;
    for reason in [
        ProviderDispatchRejection::WorkItemRevisionMismatch,
        ProviderDispatchRejection::WorkItemRevisionDrift,
    ] {
        let error = rejection(
            Some(LocalDispatchDenial::Dispatch(
                ProviderRecoveryDispatchDenial::StaleExecution(reason),
            )),
            "execution_reentry",
        );
        let before = serde_json::to_value(describe_runtime_error(&error)).unwrap();
        let summary = RuntimeHandle::concise_runtime_failure_text(&operator(), &error);
        assert!(summary.contains("input was received"));
        assert!(summary.contains("local source revision fault"));
        assert!(summary.contains("re-enters the queue"));
        assert!(summary.contains("pause it until the blocker is cleared"));
        assert!(!summary.contains("Automatic recovery has stopped"));
        assert_eq!(
            serde_json::to_value(describe_runtime_error(&error)).unwrap(),
            before
        );
    }
}

#[test]
fn local_dispatch_failure_messages_use_typed_reasons_without_changing_diagnostics() {
    let deadline = Utc::now() + Duration::minutes(10);
    for (denial, reason) in [
        (
            LocalDispatchDenial::ScopeCooling {
                not_before: deadline,
            },
            "The model channel is cooling down. This send was blocked.",
        ),
        (
            LocalDispatchDenial::EpisodeChanged,
            "A local safety check blocked this model send.",
        ),
        (
            LocalDispatchDenial::Dispatch(ProviderRecoveryDispatchDenial::StaleAdmission),
            "Model admission could not be verified. This send was blocked.",
        ),
        (
            LocalDispatchDenial::Reservation(ProviderRecoveryDispatchDenial::BudgetExhausted),
            "A local safety check blocked this model send.",
        ),
    ] {
        let error =
            rejection(Some(denial), "execution_reentry").context("private diagnostic detail");
        let before = serde_json::to_value(describe_runtime_error(&error)).unwrap();
        let mut message = operator();
        message.kind = MessageKind::TaskResult;
        let summary = RuntimeHandle::concise_runtime_failure_text(&message, &error);
        assert_eq!(
            summary,
            format!(
                "{reason} The WorkItem re-enters the queue automatically after this pre-IO stop; persistent conflicts pause it until the blocker is cleared."
            )
        );
        assert!(!summary.contains("task_result"));
        assert!(!summary.contains("before IO"));
        assert!(!summary.contains("private diagnostic detail"));
        let after = serde_json::to_value(describe_runtime_error(&error)).unwrap();
        assert_eq!(after, before);
        assert!(after["source_chain"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry
                .as_str()
                .unwrap()
                .contains("private diagnostic detail")));
    }
}

#[test]
fn local_dispatch_failure_messages_do_not_promise_uncommitted_recovery() {
    for disposition in ["pause_requested", "", "unknown"] {
        let error = rejection(
            Some(LocalDispatchDenial::ScopeCooling {
                not_before: Utc::now() + Duration::minutes(10),
            }),
            disposition,
        );
        assert_eq!(
            RuntimeHandle::concise_runtime_failure_text(&operator(), &error),
            "The model channel is cooling down. This send was blocked. Check the diagnostics for recovery status."
        );
    }
}

#[test]
fn unknown_dispatch_failure_promises_only_work_item_reentry() {
    for denial in [LocalDispatchDenial::UnknownDispatch] {
        let error = rejection(Some(denial), "execution_reentry");
        let summary = RuntimeHandle::concise_runtime_failure_text(&operator(), &error);
        assert!(summary.contains("unknown outcome"));
        assert!(summary.contains("re-enters the queue"));
        assert!(summary.contains("blocker is cleared"));
        assert!(!summary.contains("Automatic recovery has stopped"));
    }
}

#[test]
fn local_dispatch_failure_messages_without_typed_evidence_are_conservative() {
    for disposition in ["execution_reentry", "pause_requested", "", "unknown"] {
        let error = rejection(None, disposition);
        let expected = if disposition == "execution_reentry" {
            "The model request could not continue. The WorkItem re-enters the queue automatically after this pre-IO stop; persistent conflicts pause it until the blocker is cleared."
        } else {
            "The model request could not continue. Check the diagnostics for recovery status."
        };
        assert_eq!(
            RuntimeHandle::concise_runtime_failure_text(&operator(), &error),
            expected
        );
    }
}

#[test]
fn provider_io_failure_message_is_not_reclassified_from_error_text() {
    let error = anyhow::anyhow!(
        "ScopeCooling: Local provider dispatch admission rejected this send before IO."
    );
    assert_eq!(
        RuntimeHandle::concise_runtime_failure_text(&operator(), &error),
        // The generic summary keeps the real IO failure text and only applies
        // its existing enum-token normalization ("scopecooling"); it must not
        // adopt the local rejection wording from error text alone.
        "Turn failed while processing operator_prompt: scopecooling: Local provider dispatch admission rejected this send before IO."
    );
}

fn scope_already_attempted_rejection(
    safe_context: &[(&str, &str)],
    disposition: &str,
) -> anyhow::Error {
    let diagnostic = anyhow::Error::new(DispatchDeferred(
        LocalDispatchDenial::Dispatch(ProviderRecoveryDispatchDenial::ScopeAlreadyAttempted),
        None,
    ));
    let mut runtime_error = RuntimeError::new(
        RuntimeErrorDomain::Conflict,
        "provider_dispatch_rejected",
        "Local provider dispatch admission rejected this send before IO.",
    )
    .with_safe_context("recovery_disposition", disposition);
    for (key, value) in safe_context {
        runtime_error = runtime_error.with_safe_context(*key, *value);
    }
    diagnostic.context(runtime_error)
}

#[test]
fn scope_already_attempted_summary_explains_provider_credential_failure() {
    let error = scope_already_attempted_rejection(
        &[
            ("cause_code", "auth_error"),
            ("model_ref", "openai-codex@default/gpt-6.2-sol"),
            ("next_retry_at", "2026-11-19T07:46:00Z"),
            ("status", "401"),
        ],
        "execution_reentry",
    );
    let summary = RuntimeHandle::concise_runtime_failure_text(&operator(), &error);
    assert!(!summary.contains("local safety check"));
    assert!(summary.contains("rejected the credential"));
    assert!(summary.contains("openai-codex@default/gpt-6.2-sol"));
    assert!(summary.contains("2026-11-19T07:46:00Z"));
    assert!(summary.contains("HTTP 401"));
    // The misleading local-blocker direction must be gone too.
    assert!(!summary.contains("blocker is cleared"));
}

#[test]
fn scope_already_attempted_summary_reports_generic_cause_without_credential_claim() {
    let error = scope_already_attempted_rejection(
        &[
            ("cause_code", "rate_limited"),
            ("model_ref", "route-a"),
            ("next_retry_at", "2026-11-19T09:00:00Z"),
        ],
        "execution_reentry",
    );
    let summary = RuntimeHandle::concise_runtime_failure_text(&operator(), &error);
    assert!(!summary.contains("local safety check"));
    assert!(!summary.contains("credential"));
    assert!(summary.contains("cause: rate_limited"));
    assert!(summary.contains("route-a"));
    assert!(summary.contains("2026-11-19T09:00:00Z"));
}

#[test]
fn scope_already_attempted_summary_degrades_without_inventing_retry_time() {
    let error = scope_already_attempted_rejection(&[], "execution_reentry");
    let summary = RuntimeHandle::concise_runtime_failure_text(&operator(), &error);
    assert!(!summary.contains("local safety check"));
    assert!(summary.contains("already failed in this recovery episode"));
    // No retry time was supplied, so none may be fabricated.
    assert!(!summary.contains("retry after"));
    assert!(!summary.contains("blocker is cleared"));
}
