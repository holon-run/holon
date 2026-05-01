use crate::types::{
    AuditEvent, BriefRecord, ClosureDecision, ClosureOutcome, RuntimePosture, TurnTerminalKind,
    WaitingReason, WorkReactivationSignal,
};

#[derive(Debug, Clone, Default)]
pub(super) struct ClosureFacts {
    pub(super) runtime_error: bool,
    pub(super) awaiting_operator_input: bool,
    pub(super) active_blocking_tasks: usize,
    pub(super) active_waiting_intents: usize,
    pub(super) active_timers: usize,
    pub(super) work_signal: Option<WorkReactivationSignal>,
    pub(super) turn_started: bool,
    pub(super) turn_in_progress: bool,
    pub(super) turn_terminal_kind: Option<TurnTerminalKind>,
    pub(super) runtime_posture: Option<RuntimePosture>,
}

pub(super) fn derive_closure_decision(facts: &ClosureFacts) -> ClosureDecision {
    let runtime_posture = facts.runtime_posture.unwrap_or(RuntimePosture::Awake);
    let mut evidence = Vec::new();
    if facts.runtime_error {
        evidence.push("runtime_error".to_string());
    }
    if facts.awaiting_operator_input {
        evidence.push("awaiting_operator_input_signal".to_string());
    }
    if facts.active_blocking_tasks > 0 {
        evidence.push(format!(
            "active_blocking_tasks={}",
            facts.active_blocking_tasks
        ));
    }
    if facts.active_waiting_intents > 0 {
        evidence.push(format!(
            "active_waiting_intents={}",
            facts.active_waiting_intents
        ));
    }
    if facts.active_timers > 0 {
        evidence.push(format!("active_timers={}", facts.active_timers));
    }
    if let Some(work_signal) = facts.work_signal.as_ref() {
        evidence.push(format!(
            "work_reactivation_mode={:?}",
            work_signal.reactivation_mode
        ));
        evidence.push(format!("work_item_state={:?}", work_signal.state));
        evidence.push(format!("work_item_id={}", work_signal.work_item_id));
    }
    if runtime_posture == RuntimePosture::Sleeping {
        evidence.push("runtime_sleeping".to_string());
    }

    if facts.runtime_error {
        return ClosureDecision {
            outcome: ClosureOutcome::Failed,
            waiting_reason: None,
            work_signal: None,
            runtime_posture,
            evidence,
        };
    }

    if facts.awaiting_operator_input {
        return ClosureDecision {
            outcome: ClosureOutcome::Waiting,
            waiting_reason: Some(WaitingReason::AwaitingOperatorInput),
            work_signal: None,
            runtime_posture,
            evidence,
        };
    }

    if facts.active_blocking_tasks > 0 {
        return ClosureDecision {
            outcome: ClosureOutcome::Waiting,
            waiting_reason: Some(WaitingReason::AwaitingTaskResult),
            work_signal: None,
            runtime_posture,
            evidence,
        };
    }

    if facts.active_waiting_intents > 0 {
        return ClosureDecision {
            outcome: ClosureOutcome::Waiting,
            waiting_reason: Some(WaitingReason::AwaitingExternalChange),
            work_signal: None,
            runtime_posture,
            evidence,
        };
    }

    if facts.active_timers > 0 {
        return ClosureDecision {
            outcome: ClosureOutcome::Waiting,
            waiting_reason: Some(WaitingReason::AwaitingTimer),
            work_signal: None,
            runtime_posture,
            evidence,
        };
    }

    if facts.turn_in_progress {
        evidence.push("turn_in_progress".to_string());
        return ClosureDecision {
            outcome: ClosureOutcome::Waiting,
            waiting_reason: Some(WaitingReason::AwaitingExternalChange),
            work_signal: None,
            runtime_posture,
            evidence,
        };
    }

    if let Some(kind) = facts.turn_terminal_kind {
        if kind.is_failure() {
            let marker = match kind {
                TurnTerminalKind::Aborted => "turn_terminal_aborted",
                TurnTerminalKind::BaselineOverBudget => "turn_terminal_baseline_over_budget",
                TurnTerminalKind::Completed => unreachable!("completed is not a failure"),
            };
            evidence.push(marker.to_string());
            return ClosureDecision {
                outcome: ClosureOutcome::Failed,
                waiting_reason: None,
                work_signal: None,
                runtime_posture,
                evidence,
            };
        }
    }

    if facts.turn_started && facts.turn_terminal_kind.is_none() {
        evidence.push("turn_terminal_missing".to_string());
        return ClosureDecision {
            outcome: ClosureOutcome::Waiting,
            waiting_reason: Some(WaitingReason::AwaitingExternalChange),
            work_signal: None,
            runtime_posture,
            evidence,
        };
    }

    if let Some(work_signal) = facts.work_signal.clone() {
        evidence.push("runnable_work_present".to_string());
        return ClosureDecision {
            outcome: ClosureOutcome::Continuable,
            waiting_reason: None,
            work_signal: Some(work_signal),
            runtime_posture,
            evidence,
        };
    }

    if matches!(facts.turn_terminal_kind, Some(TurnTerminalKind::Completed)) {
        evidence.push("turn_terminal_completed".to_string());
    }
    evidence.push("no_waiting_condition".to_string());
    ClosureDecision {
        outcome: ClosureOutcome::Completed,
        waiting_reason: None,
        work_signal: None,
        runtime_posture,
        evidence,
    }
}

pub(super) fn runtime_error_active(events: &[AuditEvent], briefs: &[BriefRecord]) -> bool {
    let latest_runtime_error = events
        .iter()
        .filter(|event| event.kind == "runtime_error")
        .map(|event| event.created_at)
        .max();
    let latest_success_result_brief = briefs
        .iter()
        .filter(|brief| brief.kind.is_success())
        .map(|brief| brief.created_at)
        .max();

    match (latest_runtime_error, latest_success_result_brief) {
        (Some(error_at), Some(result_at)) => error_at > result_at,
        (Some(_), None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{WorkItemState, WorkReactivationMode};
    use chrono::{Duration, Utc};

    fn facts() -> ClosureFacts {
        ClosureFacts::default()
    }

    #[test]
    fn runtime_error_overrides_every_waiting_condition() {
        let decision = derive_closure_decision(&ClosureFacts {
            runtime_error: true,
            awaiting_operator_input: true,
            active_blocking_tasks: 1,
            active_waiting_intents: 1,
            active_timers: 1,
            turn_in_progress: true,
            runtime_posture: Some(RuntimePosture::Sleeping),
            ..facts()
        });

        assert_eq!(decision.outcome, ClosureOutcome::Failed);
        assert_eq!(decision.waiting_reason, None);
        assert_eq!(decision.runtime_posture, RuntimePosture::Sleeping);
    }

    #[test]
    fn awaiting_operator_input_wins_over_other_waiting_conditions() {
        let decision = derive_closure_decision(&ClosureFacts {
            awaiting_operator_input: true,
            active_blocking_tasks: 1,
            active_waiting_intents: 1,
            active_timers: 1,
            ..facts()
        });

        assert_eq!(decision.outcome, ClosureOutcome::Waiting);
        assert_eq!(
            decision.waiting_reason,
            Some(WaitingReason::AwaitingOperatorInput)
        );
    }

    #[test]
    fn blocking_tasks_map_to_awaiting_task_result() {
        let decision = derive_closure_decision(&ClosureFacts {
            active_blocking_tasks: 2,
            ..facts()
        });

        assert_eq!(decision.outcome, ClosureOutcome::Waiting);
        assert_eq!(
            decision.waiting_reason,
            Some(WaitingReason::AwaitingTaskResult)
        );
    }

    #[test]
    fn waiting_intents_map_to_awaiting_external_change() {
        let decision = derive_closure_decision(&ClosureFacts {
            active_waiting_intents: 1,
            ..facts()
        });

        assert_eq!(decision.outcome, ClosureOutcome::Waiting);
        assert_eq!(
            decision.waiting_reason,
            Some(WaitingReason::AwaitingExternalChange)
        );
    }

    #[test]
    fn timers_map_to_awaiting_timer() {
        let decision = derive_closure_decision(&ClosureFacts {
            active_timers: 1,
            ..facts()
        });

        assert_eq!(decision.outcome, ClosureOutcome::Waiting);
        assert_eq!(decision.waiting_reason, Some(WaitingReason::AwaitingTimer));
    }

    #[test]
    fn sleeping_posture_is_preserved_independently_from_outcome() {
        let waiting = derive_closure_decision(&ClosureFacts {
            active_timers: 1,
            runtime_posture: Some(RuntimePosture::Sleeping),
            ..facts()
        });
        let completed = derive_closure_decision(&ClosureFacts {
            turn_terminal_kind: Some(TurnTerminalKind::Completed),
            runtime_posture: Some(RuntimePosture::Sleeping),
            ..facts()
        });

        assert_eq!(waiting.runtime_posture, RuntimePosture::Sleeping);
        assert_eq!(completed.runtime_posture, RuntimePosture::Sleeping);
    }

    #[test]
    fn aborted_turn_maps_to_failed_closure() {
        let decision = derive_closure_decision(&ClosureFacts {
            turn_terminal_kind: Some(TurnTerminalKind::Aborted),
            ..facts()
        });

        assert_eq!(decision.outcome, ClosureOutcome::Failed);
        assert_eq!(decision.waiting_reason, None);
    }

    #[test]
    fn baseline_over_budget_turn_maps_to_failed_closure() {
        let decision = derive_closure_decision(&ClosureFacts {
            turn_terminal_kind: Some(TurnTerminalKind::BaselineOverBudget),
            ..facts()
        });

        assert_eq!(decision.outcome, ClosureOutcome::Failed);
        assert_eq!(decision.waiting_reason, None);
        assert!(decision
            .evidence
            .iter()
            .any(|item| item == "turn_terminal_baseline_over_budget"));
    }

    #[test]
    fn completed_turn_with_no_waiting_conditions_maps_to_completed_closure() {
        let decision = derive_closure_decision(&ClosureFacts {
            turn_terminal_kind: Some(TurnTerminalKind::Completed),
            ..facts()
        });

        assert_eq!(decision.outcome, ClosureOutcome::Completed);
        assert_eq!(decision.waiting_reason, None);
        assert!(decision
            .evidence
            .iter()
            .any(|item| item == "turn_terminal_completed"));
    }

    #[test]
    fn runnable_active_work_maps_to_continuable_closure() {
        let decision = derive_closure_decision(&ClosureFacts {
            work_signal: Some(WorkReactivationSignal {
                work_item_id: "work-1".into(),
                state: WorkItemState::Open,
                reactivation_mode: WorkReactivationMode::ContinueActive,
            }),
            turn_terminal_kind: Some(TurnTerminalKind::Completed),
            ..facts()
        });

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
    fn waiting_conditions_override_runnable_work() {
        let decision = derive_closure_decision(&ClosureFacts {
            active_blocking_tasks: 1,
            work_signal: Some(WorkReactivationSignal {
                work_item_id: "work-1".into(),
                state: WorkItemState::Open,
                reactivation_mode: WorkReactivationMode::ContinueActive,
            }),
            ..facts()
        });

        assert_eq!(decision.outcome, ClosureOutcome::Waiting);
        assert_eq!(
            decision.waiting_reason,
            Some(WaitingReason::AwaitingTaskResult)
        );
        assert_eq!(decision.work_signal, None);
    }

    #[test]
    fn started_turn_without_terminal_record_maps_to_waiting() {
        let decision = derive_closure_decision(&ClosureFacts {
            turn_started: true,
            turn_terminal_kind: None,
            ..facts()
        });

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
            ..AuditEvent::new("runtime_error", serde_json::json!({}))
        };
        let newer_success = BriefRecord {
            created_at: base + Duration::milliseconds(1),
            work_item_id: None,
            ..BriefRecord::new(
                "default",
                crate::types::BriefKind::Result,
                "done",
                None,
                None,
            )
        };

        assert!(!runtime_error_active(&[older_error], &[newer_success]));
    }

    #[test]
    fn runtime_error_active_stays_true_without_later_success_brief() {
        let error = AuditEvent::new("runtime_error", serde_json::json!({}));

        assert!(runtime_error_active(&[error], &[]));
    }
}
