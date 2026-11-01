use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{assert_retryable_resource_error, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{WorkItemState, WorkReactivationMode},
};

fn fixture() -> ClosureFacts {
    ClosureFacts {
        active_blocking_tasks: usize::MAX,
        active_waiting_intents: 3,
        active_agent_waiting_intents: 2,
        active_timers: 1,
        waiting_work_item_scheduling_state: Some(WorkItemSchedulingState::WaitingTask),
        work_signal: Some(WorkReactivationSignal {
            work_item_id: "closure 汉🦀 \"{}[],:\\ ".repeat(4),
            state: WorkItemState::Open,
            reactivation_mode: WorkReactivationMode::ContinueActive,
        }),
        ..Default::default()
    }
}

#[test]
fn closure_evidence_admits_exact_bytes_records_and_latches_without_partial_result(
) -> anyhow::Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: "closure-material".into(),
        identity_incarnation: 1,
    };
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, u64::MAX, usize::MAX)?;
    let expected = phase.read(|| derive_closure_decision(fixture()))?;
    let usage = phase.finish()?;
    assert_eq!(expected.outcome, ClosureOutcome::Continuable);
    assert_eq!(
        expected.evidence.last().map(String::as_str),
        Some("runnable_work_present"),
    );
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for shortage in [None, Some(true), Some(false)] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            usage.0 - u64::from(shortage == Some(true)),
            usage.1 - usize::from(shortage == Some(false)),
        )?;
        let result = phase.read(|| derive_closure_decision(fixture()));
        if shortage.is_none() {
            assert_eq!(
                serde_json::to_value(result?)?,
                serde_json::to_value(&expected)?
            );
            assert_eq!(phase.finish()?, usage);
        } else {
            assert_retryable_resource_error(&result.unwrap_err());
            let held = ledger.report()?.observed_bytes;
            assert_retryable_resource_error(
                &phase
                    .read(|| derive_closure_decision(fixture()))
                    .unwrap_err(),
            );
            assert_eq!(ledger.report()?.observed_bytes, held);
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn closure_moves_work_signal_and_keeps_runtime_error_precedence() -> anyhow::Result<()> {
    let facts = fixture();
    let pointer = facts.work_signal.as_ref().unwrap().work_item_id.as_ptr();
    let decision = derive_closure_decision(facts)?;
    assert_eq!(
        decision.work_signal.as_ref().unwrap().work_item_id.as_ptr(),
        pointer
    );
    let decision = derive_closure_decision(ClosureFacts {
        runtime_error: true,
        awaiting_operator_input: true,
        turn_in_progress: true,
        ..fixture()
    })?;
    assert_eq!(decision.outcome, ClosureOutcome::Failed);
    assert_eq!(decision.waiting_reason, None);
    assert_eq!(decision.work_signal, None);
    assert_eq!(
        decision.evidence.first().map(String::as_str),
        Some("runtime_error")
    );
    assert!(!decision
        .evidence
        .iter()
        .any(|value| value == "turn_in_progress"));
    Ok(())
}
