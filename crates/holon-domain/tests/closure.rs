use chrono::{Duration, Utc};
use holon_domain::{
    closure::{self, runtime_error_active, ClosureFacts},
    AuditEvent, BriefKind, BriefRecord, ClosureDecision, ClosureOutcome, RuntimePosture,
    TurnTerminalKind, WaitingReason, WorkItemState, WorkReactivationMode, WorkReactivationSignal,
};

fn derive_closure_decision(facts: ClosureFacts) -> anyhow::Result<ClosureDecision> {
    closure::derive_closure_decision(facts, |evidence, value| {
        evidence.push(value.to_string());
        Ok(())
    })
}

fn facts() -> ClosureFacts {
    ClosureFacts::default()
}

#[test]
fn runtime_error_overrides_every_waiting_condition() {
    let decision = derive_closure_decision(ClosureFacts {
        runtime_error: true,
        awaiting_operator_input: true,
        active_blocking_tasks: 1,
        active_waiting_intents: 1,
        active_timers: 1,
        turn_in_progress: true,
        runtime_posture: Some(RuntimePosture::Sleeping),
        ..facts()
    })
    .unwrap();

    assert_eq!(decision.outcome, ClosureOutcome::Failed);
    assert_eq!(decision.waiting_reason, None);
    assert_eq!(decision.runtime_posture, RuntimePosture::Sleeping);
}

#[test]
fn awaiting_operator_input_wins_over_other_waiting_conditions() {
    let decision = derive_closure_decision(ClosureFacts {
        awaiting_operator_input: true,
        active_blocking_tasks: 1,
        active_waiting_intents: 1,
        active_timers: 1,
        ..facts()
    })
    .unwrap();

    assert_eq!(decision.outcome, ClosureOutcome::Waiting);
    assert_eq!(
        decision.waiting_reason,
        Some(WaitingReason::AwaitingOperatorInput)
    );
}

#[test]
fn blocking_tasks_are_metadata_not_waiting_reason() {
    let decision = derive_closure_decision(ClosureFacts {
        active_blocking_tasks: 2,
        ..facts()
    })
    .unwrap();

    assert_eq!(decision.outcome, ClosureOutcome::Completed);
    assert_eq!(decision.waiting_reason, None);
    assert!(decision
        .evidence
        .iter()
        .any(|item| item == "active_blocking_tasks=2"));
}

#[test]
fn waiting_intents_map_to_awaiting_external_change() {
    let decision = derive_closure_decision(ClosureFacts {
        active_waiting_intents: 1,
        active_agent_waiting_intents: 1,
        ..facts()
    })
    .unwrap();

    assert_eq!(decision.outcome, ClosureOutcome::Waiting);
    assert_eq!(
        decision.waiting_reason,
        Some(WaitingReason::AwaitingExternalChange)
    );
}

#[test]
fn timers_map_to_awaiting_timer() {
    let decision = derive_closure_decision(ClosureFacts {
        active_timers: 1,
        ..facts()
    })
    .unwrap();

    assert_eq!(decision.outcome, ClosureOutcome::Waiting);
    assert_eq!(decision.waiting_reason, Some(WaitingReason::AwaitingTimer));
}

#[test]
fn sleeping_posture_is_preserved_independently_from_outcome() {
    let waiting = derive_closure_decision(ClosureFacts {
        active_timers: 1,
        runtime_posture: Some(RuntimePosture::Sleeping),
        ..facts()
    })
    .unwrap();
    let completed = derive_closure_decision(ClosureFacts {
        turn_terminal_kind: Some(TurnTerminalKind::Completed),
        runtime_posture: Some(RuntimePosture::Sleeping),
        ..facts()
    })
    .unwrap();

    assert_eq!(waiting.runtime_posture, RuntimePosture::Sleeping);
    assert_eq!(completed.runtime_posture, RuntimePosture::Sleeping);
}

#[test]
fn aborted_turn_maps_to_failed_closure() {
    let decision = derive_closure_decision(ClosureFacts {
        turn_terminal_kind: Some(TurnTerminalKind::Aborted),
        ..facts()
    })
    .unwrap();

    assert_eq!(decision.outcome, ClosureOutcome::Failed);
    assert_eq!(decision.waiting_reason, None);
}

#[test]
fn baseline_over_budget_turn_maps_to_failed_closure() {
    let decision = derive_closure_decision(ClosureFacts {
        turn_terminal_kind: Some(TurnTerminalKind::BaselineOverBudget),
        ..facts()
    })
    .unwrap();

    assert_eq!(decision.outcome, ClosureOutcome::Failed);
    assert_eq!(decision.waiting_reason, None);
    assert!(decision
        .evidence
        .iter()
        .any(|item| item == "turn_terminal_baseline_over_budget"));
}

#[test]
fn completed_turn_with_no_waiting_conditions_maps_to_completed_closure() {
    let decision = derive_closure_decision(ClosureFacts {
        turn_terminal_kind: Some(TurnTerminalKind::Completed),
        ..facts()
    })
    .unwrap();

    assert_eq!(decision.outcome, ClosureOutcome::Completed);
    assert_eq!(decision.waiting_reason, None);
    assert!(decision
        .evidence
        .iter()
        .any(|item| item == "turn_terminal_completed"));
}

#[test]
fn runnable_active_work_maps_to_continuable_closure() {
    let decision = derive_closure_decision(ClosureFacts {
        work_signal: Some(WorkReactivationSignal {
            work_item_id: "work-1".into(),
            state: WorkItemState::Open,
            reactivation_mode: WorkReactivationMode::ContinueActive,
        }),
        turn_terminal_kind: Some(TurnTerminalKind::Completed),
        ..facts()
    })
    .unwrap();

    assert_eq!(decision.outcome, ClosureOutcome::Continuable);
    assert_eq!(decision.waiting_reason, None);
    assert_eq!(
        decision.work_signal,
        Some(WorkReactivationSignal {
            work_item_id: "work-1".into(),
            state: WorkItemState::Open,
            reactivation_mode: WorkReactivationMode::ContinueActive,
        })
    );
}

#[test]
fn runnable_work_overrides_unrelated_agent_waiting_intent() {
    let decision = derive_closure_decision(ClosureFacts {
        active_waiting_intents: 1,
        active_agent_waiting_intents: 1,
        work_signal: Some(WorkReactivationSignal {
            work_item_id: "work-1".into(),
            state: WorkItemState::Open,
            reactivation_mode: WorkReactivationMode::ContinueActive,
        }),
        ..facts()
    })
    .unwrap();

    assert_eq!(decision.outcome, ClosureOutcome::Continuable);
    assert_eq!(decision.waiting_reason, None);
    assert_eq!(
        decision
            .work_signal
            .as_ref()
            .map(|signal| signal.work_item_id.as_str()),
        Some("work-1")
    );
}

#[test]
fn blocking_tasks_do_not_override_runnable_work() {
    let decision = derive_closure_decision(ClosureFacts {
        active_blocking_tasks: 1,
        work_signal: Some(WorkReactivationSignal {
            work_item_id: "work-1".into(),
            state: WorkItemState::Open,
            reactivation_mode: WorkReactivationMode::ContinueActive,
        }),
        ..facts()
    })
    .unwrap();

    assert_eq!(decision.outcome, ClosureOutcome::Continuable);
    assert_eq!(decision.waiting_reason, None);
    assert!(decision
        .evidence
        .iter()
        .any(|item| item == "active_blocking_tasks=1"));
}

#[test]
fn started_turn_without_terminal_record_maps_to_waiting() {
    let decision = derive_closure_decision(ClosureFacts {
        turn_started: true,
        turn_terminal_kind: None,
        ..facts()
    })
    .unwrap();

    assert_eq!(decision.outcome, ClosureOutcome::Waiting);
    assert_eq!(
        decision.waiting_reason,
        Some(WaitingReason::AwaitingExternalChange)
    );
}

#[test]
fn runtime_error_active_clears_when_later_success_brief_exists() {
    let base = Utc::now();
    let older_error = AuditEvent {
        created_at: base,
        ..AuditEvent::legacy("runtime_error", serde_json::json!({}))
    };
    let newer_success = BriefRecord {
        created_at: base + Duration::milliseconds(1),
        work_item_id: None,
        ..BriefRecord::new("default", BriefKind::Result, "done", None, None)
    };

    assert!(!runtime_error_active(&[older_error], &[newer_success]));
}

#[test]
fn runtime_error_active_stays_true_without_later_success_brief() {
    let error = AuditEvent::legacy("runtime_error", serde_json::json!({}));

    assert!(runtime_error_active(&[error], &[]));
}
