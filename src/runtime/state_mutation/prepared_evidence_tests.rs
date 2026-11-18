use super::*;
use crate::runtime_error::describe_runtime_error;

#[test]
fn shutdown_attribution_preserves_both_settlement_states_and_replay_policy() {
    for not_committed in [false, true] {
        for retiring in [false, true] {
            let cause = if retiring {
                crate::runtime::execution_admission::closed_error()
            } else {
                anyhow::anyhow!("storage unavailable")
            };
            let cause = cause.context("original evidence write");
            let cause = if not_committed {
                cause.context(ToolResultReceiptAbsent)
            } else {
                cause
            };
            let error =
                tool_result_settlement_error(cause, "exec-original".into(), "turn-original".into());
            let report = describe_runtime_error(&error);
            assert_eq!(report.domain, RuntimeErrorDomain::Storage);
            assert_eq!(report.code, TOOL_RESULT_SETTLEMENT_PENDING);
            assert!(!report.retryable);
            assert_eq!(report.safe_context["tool_execution_id"], "exec-original");
            assert_eq!(report.safe_context["turn_id"], "turn-original");
            assert_eq!(
                report.safe_context["status"],
                if not_committed {
                    "not_persisted"
                } else {
                    "outcome_unknown"
                }
            );
            assert_eq!(
                report.safe_context.get("cause_code").map(String::as_str),
                retiring.then_some("runtime_instance_retiring")
            );
            let original_message = if not_committed {
                "Tool returned, but its result was not persisted"
            } else {
                "Tool returned, but its result settlement is unconfirmed"
            };
            assert_eq!(
                report.operator_message,
                if retiring {
                    format!("Turn interrupted by runtime shutdown/restart; {original_message}")
                } else {
                    original_message.to_owned()
                }
            );
            assert_eq!(report.recovery_hint.as_deref(),
                Some("Inspect the original tool execution and external state before submitting it again"));
            if retiring {
                assert!(error.chain().any(|source| source
                    .downcast_ref::<RuntimeError>()
                    .is_some_and(
                        |inner| inner.descriptor().code == "runtime_instance_retiring"
                            && inner.descriptor().retryable
                    )));
            }
        }
    }
}

#[test]
fn shutdown_attribution_requires_a_typed_runtime_admission_cause() {
    for cause in [
        anyhow::anyhow!("runtime_instance_retiring: runtime instance admission has closed"),
        RuntimeError::new(
            RuntimeErrorDomain::Storage,
            "runtime_instance_retiring",
            "storage failure is not runtime admission",
        )
        .into(),
    ] {
        let report = describe_runtime_error(&tool_result_settlement_error(
            cause,
            "exec-original".into(),
            "turn-original".into(),
        ));
        assert!(!report.safe_context.contains_key("cause_code"));
        assert_eq!(report.safe_context["status"], "outcome_unknown");
        assert_eq!(
            report.operator_message,
            "Tool returned, but its result settlement is unconfirmed"
        );
        assert!(!report.retryable);
    }
}
