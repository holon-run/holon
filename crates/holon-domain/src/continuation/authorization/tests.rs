use super::*;

fn decision(
    outcome: ClosureOutcome,
    reason: Option<WaitingReason>,
    kind: ContinuationTriggerKind,
    contentful: bool,
    task_result_outcome: Option<TaskResultOutcome>,
    exact_task_wait: bool,
) -> ResumeDecision {
    resolve_resume_authorization(
        outcome,
        reason,
        kind,
        contentful,
        task_result_outcome,
        exact_task_wait,
    )
}

#[test]
fn classifies_the_four_authorization_sources() {
    assert_eq!(
        decision(
            ClosureOutcome::Waiting,
            Some(WaitingReason::AwaitingTimer),
            ContinuationTriggerKind::TimerFire,
            false,
            None,
            false,
        )
        .authorization,
        ResumeAuthorization::ExpectedWait
    );
    assert_eq!(
        decision(
            ClosureOutcome::Waiting,
            Some(WaitingReason::AwaitingTimer),
            ContinuationTriggerKind::TaskResult,
            true,
            Some(TaskResultOutcome::Succeeded),
            true,
        )
        .authorization,
        ResumeAuthorization::ExpectedWait
    );
    assert_eq!(
        decision(
            ClosureOutcome::Waiting,
            Some(WaitingReason::AwaitingTimer),
            ContinuationTriggerKind::OperatorInput,
            true,
            None,
            false,
        )
        .authorization,
        ResumeAuthorization::Override
    );
    assert_eq!(
        decision(
            ClosureOutcome::Continuable,
            None,
            ContinuationTriggerKind::ExternalEvent,
            false,
            None,
            false,
        )
        .authorization,
        ResumeAuthorization::LivenessOnly
    );
}

#[test]
fn separates_terminal_reentry_from_exact_wait_correlation() {
    let unmatched_terminal = decision(
        ClosureOutcome::Waiting,
        Some(WaitingReason::AwaitingTimer),
        ContinuationTriggerKind::TaskResult,
        true,
        Some(TaskResultOutcome::Succeeded),
        false,
    );
    assert_eq!(
        unmatched_terminal.authorization,
        ResumeAuthorization::RuntimeEventReentry
    );
    assert!(unmatched_terminal.model_reentry);
    assert!(!unmatched_terminal.matched_waiting_reason);

    let active = decision(
        ClosureOutcome::Waiting,
        Some(WaitingReason::AwaitingTaskResult),
        ContinuationTriggerKind::TaskResult,
        true,
        None,
        true,
    );
    assert_eq!(active.authorization, ResumeAuthorization::LivenessOnly);
    assert!(active.matched_waiting_reason);
}

#[test]
fn waiting_reason_matching_matrix_is_fail_closed() {
    let matching = [
        (
            WaitingReason::AwaitingOperatorInput,
            ContinuationTriggerKind::OperatorInput,
            false,
            None,
            false,
        ),
        (
            WaitingReason::AwaitingTaskResult,
            ContinuationTriggerKind::TaskResult,
            true,
            Some(TaskResultOutcome::Succeeded),
            true,
        ),
        (
            WaitingReason::AwaitingExternalChange,
            ContinuationTriggerKind::ExternalEvent,
            true,
            None,
            false,
        ),
        (
            WaitingReason::AwaitingExternalChange,
            ContinuationTriggerKind::SystemTick,
            true,
            None,
            false,
        ),
        (
            WaitingReason::AwaitingTimer,
            ContinuationTriggerKind::TimerFire,
            false,
            None,
            false,
        ),
    ];

    for (reason, kind, contentful, task_result_outcome, exact_task_wait) in matching {
        let matched = decision(
            ClosureOutcome::Waiting,
            Some(reason),
            kind,
            contentful,
            task_result_outcome,
            exact_task_wait,
        );
        assert_eq!(matched.authorization, ResumeAuthorization::ExpectedWait);
        assert!(matched.model_reentry);
        assert!(matched.matched_waiting_reason);
    }

    for (reason, kind) in [
        (
            WaitingReason::AwaitingOperatorInput,
            ContinuationTriggerKind::TimerFire,
        ),
        (
            WaitingReason::AwaitingTaskResult,
            ContinuationTriggerKind::ExternalEvent,
        ),
        (
            WaitingReason::AwaitingExternalChange,
            ContinuationTriggerKind::TimerFire,
        ),
        (
            WaitingReason::AwaitingTimer,
            ContinuationTriggerKind::SystemTick,
        ),
    ] {
        let unmatched = decision(
            ClosureOutcome::Waiting,
            Some(reason),
            kind,
            true,
            None,
            false,
        );
        assert_eq!(unmatched.authorization, ResumeAuthorization::LivenessOnly);
        assert!(!unmatched.model_reentry);
        assert!(!unmatched.matched_waiting_reason);
    }
}
