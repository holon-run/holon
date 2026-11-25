use holon_domain::{
    continuation::{resolve_continuation, ContinuationTrigger},
    AuthorityClass, ClosureDecision, ClosureOutcome, ContinuationClass, ContinuationTriggerKind,
    MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority, RuntimePosture,
    TaskResultOutcome, WaitingReason,
};

fn waiting(reason: WaitingReason) -> ClosureDecision {
    ClosureDecision {
        outcome: ClosureOutcome::Waiting,
        waiting_reason: Some(reason),
        work_signal: None,
        runtime_posture: RuntimePosture::Awake,
        evidence: vec![],
    }
}

#[test]
fn closure_task_label_without_exact_wait_requests_result_reentry() {
    let resolution = resolve_continuation(
        &waiting(WaitingReason::AwaitingTaskResult),
        &ContinuationTrigger {
            kind: ContinuationTriggerKind::TaskResult,
            contentful: true,
            task_result_outcome: Some(TaskResultOutcome::Succeeded),
            wake_hint_source: None,
            task_work_item_id: None,
            exact_task_wait: false,
            exact_wait_recheck: false,
        },
    );
    assert_eq!(resolution.class, ContinuationClass::TaskResultReentry);
    assert!(resolution.model_reentry);
    assert!(!resolution.matched_waiting_reason);
}

#[test]
fn non_terminal_task_result_does_not_resume_expected_wait() {
    let resolution = resolve_continuation(
        &waiting(WaitingReason::AwaitingTaskResult),
        &ContinuationTrigger {
            kind: ContinuationTriggerKind::TaskResult,
            contentful: true,
            task_result_outcome: None,
            wake_hint_source: None,
            task_work_item_id: None,
            exact_task_wait: false,
            exact_wait_recheck: false,
        },
    );

    assert_eq!(resolution.class, ContinuationClass::LivenessOnly);
    assert!(!resolution.model_reentry);
    assert!(!resolution.matched_waiting_reason);
    assert!(resolution
        .evidence
        .iter()
        .any(|entry| entry == "does_not_satisfy_waiting_reason"));
}

#[test]
fn terminal_task_result_for_captured_owner_does_not_satisfy_closure_wait() {
    let resolution = resolve_continuation(
        &waiting(WaitingReason::AwaitingTaskResult),
        &ContinuationTrigger {
            kind: ContinuationTriggerKind::TaskResult,
            contentful: true,
            task_result_outcome: Some(TaskResultOutcome::Succeeded),
            wake_hint_source: None,
            task_work_item_id: Some("other-work".into()),
            exact_task_wait: false,
            exact_wait_recheck: false,
        },
    );

    assert_eq!(resolution.class, ContinuationClass::TaskResultReentry);
    assert!(resolution.model_reentry);
    assert!(!resolution.matched_waiting_reason);
}

#[test]
fn wake_hint_system_tick_is_liveness_only() {
    let resolution = resolve_continuation(
        &waiting(WaitingReason::AwaitingExternalChange),
        &ContinuationTrigger {
            kind: ContinuationTriggerKind::SystemTick,
            contentful: false,
            task_result_outcome: None,
            wake_hint_source: Some("callback".into()),
            task_work_item_id: None,
            exact_task_wait: false,
            exact_wait_recheck: false,
        },
    );
    assert_eq!(resolution.class, ContinuationClass::LivenessOnly);
    assert!(!resolution.model_reentry);
}

#[test]
fn contentful_system_tick_resumes_external_wait_recheck() {
    let resolution = resolve_continuation(
        &waiting(WaitingReason::AwaitingExternalChange),
        &ContinuationTrigger {
            kind: ContinuationTriggerKind::SystemTick,
            contentful: true,
            task_result_outcome: None,
            wake_hint_source: None,
            task_work_item_id: None,
            exact_task_wait: false,
            exact_wait_recheck: false,
        },
    );

    assert_eq!(resolution.class, ContinuationClass::ResumeExpectedWait);
    assert!(resolution.model_reentry);
    assert!(resolution.matched_waiting_reason);
}

#[test]
fn operator_input_overrides_waiting_task_result() {
    let resolution = resolve_continuation(
        &waiting(WaitingReason::AwaitingTaskResult),
        &ContinuationTrigger {
            kind: ContinuationTriggerKind::OperatorInput,
            contentful: true,
            task_result_outcome: None,
            wake_hint_source: None,
            task_work_item_id: None,
            exact_task_wait: false,
            exact_wait_recheck: false,
        },
    );
    assert_eq!(resolution.class, ContinuationClass::ResumeOverride);
    assert!(resolution.model_reentry);
}

#[test]
fn empty_external_event_without_wait_is_liveness_only() {
    let resolution = resolve_continuation(
        &ClosureDecision {
            outcome: ClosureOutcome::Completed,
            waiting_reason: None,
            work_signal: None,
            runtime_posture: RuntimePosture::Awake,
            evidence: vec![],
        },
        &ContinuationTrigger {
            kind: ContinuationTriggerKind::ExternalEvent,
            contentful: false,
            task_result_outcome: None,
            wake_hint_source: None,
            task_work_item_id: None,
            exact_task_wait: false,
            exact_wait_recheck: false,
        },
    );
    assert_eq!(resolution.class, ContinuationClass::LivenessOnly);
    assert!(!resolution.model_reentry);
}

#[test]
fn unbound_terminal_task_results_reenter_without_prior_wait() {
    for outcome in [
        TaskResultOutcome::Succeeded,
        TaskResultOutcome::Failed,
        TaskResultOutcome::Cancelled,
        TaskResultOutcome::Interrupted,
    ] {
        let resolution = resolve_continuation(
            &ClosureDecision {
                outcome: ClosureOutcome::Completed,
                waiting_reason: None,
                work_signal: None,
                runtime_posture: RuntimePosture::Awake,
                evidence: vec![],
            },
            &ContinuationTrigger {
                kind: ContinuationTriggerKind::TaskResult,
                contentful: true,
                task_result_outcome: Some(outcome),
                wake_hint_source: None,
                task_work_item_id: None,
                exact_task_wait: false,
                exact_wait_recheck: false,
            },
        );
        assert_eq!(resolution.class, ContinuationClass::TaskResultReentry);
        assert!(resolution.model_reentry);
    }
}

#[test]
fn unbound_terminal_task_result_reenters_from_sleeping_posture() {
    let resolution = resolve_continuation(
        &ClosureDecision {
            outcome: ClosureOutcome::Completed,
            waiting_reason: None,
            work_signal: None,
            runtime_posture: RuntimePosture::Sleeping,
            evidence: vec![],
        },
        &ContinuationTrigger {
            kind: ContinuationTriggerKind::TaskResult,
            contentful: true,
            task_result_outcome: Some(TaskResultOutcome::Succeeded),
            wake_hint_source: None,
            task_work_item_id: None,
            exact_task_wait: false,
            exact_wait_recheck: false,
        },
    );
    assert_eq!(resolution.class, ContinuationClass::TaskResultReentry);
    assert!(resolution.model_reentry);
}

#[test]
fn terminal_task_result_reentry_does_not_satisfy_external_wait() {
    let resolution = resolve_continuation(
        &waiting(WaitingReason::AwaitingExternalChange),
        &ContinuationTrigger {
            kind: ContinuationTriggerKind::TaskResult,
            contentful: true,
            task_result_outcome: Some(TaskResultOutcome::Succeeded),
            wake_hint_source: None,
            task_work_item_id: None,
            exact_task_wait: false,
            exact_wait_recheck: false,
        },
    );
    assert_eq!(resolution.class, ContinuationClass::TaskResultReentry);
    assert!(resolution.model_reentry);
    assert!(!resolution.matched_waiting_reason);
}

#[test]
fn terminal_task_result_reentry_does_not_satisfy_operator_wait() {
    let resolution = resolve_continuation(
        &waiting(WaitingReason::AwaitingOperatorInput),
        &ContinuationTrigger {
            kind: ContinuationTriggerKind::TaskResult,
            contentful: true,
            task_result_outcome: Some(TaskResultOutcome::Succeeded),
            wake_hint_source: None,
            task_work_item_id: None,
            exact_task_wait: false,
            exact_wait_recheck: false,
        },
    );
    assert_eq!(resolution.class, ContinuationClass::TaskResultReentry);
    assert!(resolution.model_reentry);
    assert!(!resolution.matched_waiting_reason);
}

#[test]
fn empty_external_event_waiting_for_external_change_is_liveness_only() {
    let resolution = resolve_continuation(
        &waiting(WaitingReason::AwaitingExternalChange),
        &ContinuationTrigger {
            kind: ContinuationTriggerKind::ExternalEvent,
            contentful: false,
            task_result_outcome: None,
            wake_hint_source: None,
            task_work_item_id: None,
            exact_task_wait: false,
            exact_wait_recheck: false,
        },
    );
    assert_eq!(resolution.class, ContinuationClass::LivenessOnly);
    assert!(!resolution.model_reentry);
    assert!(resolution.matched_waiting_reason);
}

#[test]
fn mismatched_timer_trigger_stays_liveness_only() {
    let resolution = resolve_continuation(
        &waiting(WaitingReason::AwaitingTaskResult),
        &ContinuationTrigger {
            kind: ContinuationTriggerKind::TimerFire,
            contentful: true,
            task_result_outcome: None,
            wake_hint_source: None,
            task_work_item_id: None,
            exact_task_wait: false,
            exact_wait_recheck: false,
        },
    );

    assert_eq!(resolution.class, ContinuationClass::LivenessOnly);
    assert!(!resolution.model_reentry);
    assert!(!resolution.matched_waiting_reason);
    assert!(resolution
        .evidence
        .iter()
        .any(|entry| entry == "does_not_satisfy_waiting_reason"));
}

fn wake_hint_system_tick(reason: &str, body: Option<serde_json::Value>) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "wake_hint".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: format!("wake hint: {reason}"),
        },
    );
    message.metadata = Some(serde_json::json!({
        "wake_hint": {
            "reason": reason,
            "body": body,
        }
    }));
    message
}

#[test]
fn wake_hint_with_reason_only_is_contentful() {
    let message = wake_hint_system_tick("scheduler drill system wake", None);
    assert!(
        ContinuationTrigger::from_message(&message, None)
            .unwrap()
            .contentful
    );
}

#[test]
fn wake_hint_with_empty_reason_and_no_body_is_not_contentful() {
    let message = wake_hint_system_tick("", None);
    assert!(
        !ContinuationTrigger::from_message(&message, None)
            .unwrap()
            .contentful
    );
}

#[test]
fn exact_task_wait_overrides_polluted_closure_for_terminal_task_result() {
    let trigger = ContinuationTrigger {
        kind: ContinuationTriggerKind::TaskResult,
        contentful: true,
        task_result_outcome: Some(TaskResultOutcome::Succeeded),
        wake_hint_source: None,
        task_work_item_id: None,
        exact_task_wait: true,
        exact_wait_recheck: false,
    };
    let resolution = resolve_continuation(&waiting(WaitingReason::AwaitingOperatorInput), &trigger);
    assert!(resolution.model_reentry);
    assert_eq!(resolution.class, ContinuationClass::ResumeExpectedWait);
    assert!(resolution
        .evidence
        .iter()
        .any(|entry| entry == "exact_task_wait"));
}

#[test]
fn polluted_closure_without_exact_task_wait_requests_result_reentry_only() {
    let trigger = ContinuationTrigger {
        kind: ContinuationTriggerKind::TaskResult,
        contentful: true,
        task_result_outcome: Some(TaskResultOutcome::Succeeded),
        wake_hint_source: None,
        task_work_item_id: None,
        exact_task_wait: false,
        exact_wait_recheck: false,
    };
    let resolution = resolve_continuation(&waiting(WaitingReason::AwaitingOperatorInput), &trigger);
    assert!(resolution.model_reentry);
    assert_eq!(resolution.class, ContinuationClass::TaskResultReentry);
    assert!(resolution
        .evidence
        .iter()
        .any(|entry| entry == "does_not_satisfy_waiting_reason"));
}
