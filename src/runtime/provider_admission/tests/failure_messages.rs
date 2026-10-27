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
            LocalDispatchDenial::ChainCooling {
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
            "A local safety check blocked this model send.",
        ),
        (
            LocalDispatchDenial::Reservation(ProviderRecoveryDispatchDenial::BudgetExhausted),
            "A local safety check blocked this model send.",
        ),
    ] {
        let error = rejection(Some(denial), "needs_repair").context("private diagnostic detail");
        let before = serde_json::to_value(describe_runtime_error(&error)).unwrap();
        let mut message = operator();
        message.kind = MessageKind::TaskResult;
        let summary = RuntimeHandle::concise_runtime_failure_text(&message, &error);
        assert_eq!(
            summary,
            format!("{reason} Recovery needs attention; check the diagnostics before retrying.")
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
fn local_dispatch_failure_messages_without_typed_evidence_are_conservative() {
    for disposition in ["needs_repair", "pause_requested", "", "unknown"] {
        let error = rejection(None, disposition);
        let expected = if disposition == "needs_repair" {
            "The model request could not continue. Recovery needs attention; check the diagnostics before retrying."
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
