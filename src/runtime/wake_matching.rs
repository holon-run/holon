use crate::types::{ClosureOutcome, ContinuationTriggerKind, TaskResultOutcome, WaitingReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResumeAuthorization {
    ExpectedWait,
    RuntimeEventReentry,
    Override,
    LocalContinuation,
    LivenessOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ResumeDecision {
    pub(super) authorization: ResumeAuthorization,
    pub(super) model_reentry: bool,
    pub(super) matched_waiting_reason: bool,
}

pub(super) fn resolve_resume_authorization(
    prior_outcome: ClosureOutcome,
    prior_waiting_reason: Option<WaitingReason>,
    trigger_kind: ContinuationTriggerKind,
    contentful: bool,
    task_result_outcome: Option<TaskResultOutcome>,
    exact_task_wait: bool,
) -> ResumeDecision {
    let terminal_task_result =
        trigger_kind == ContinuationTriggerKind::TaskResult && task_result_outcome.is_some();
    let expected = if trigger_kind == ContinuationTriggerKind::TaskResult {
        exact_task_wait
    } else {
        waiting_reason_matches(prior_waiting_reason, trigger_kind)
    };

    if prior_outcome == ClosureOutcome::Waiting {
        if expected {
            let model_reentry = match trigger_kind {
                ContinuationTriggerKind::TaskResult => terminal_task_result,
                ContinuationTriggerKind::ExternalEvent | ContinuationTriggerKind::SystemTick => {
                    contentful
                }
                _ => true,
            };
            return ResumeDecision {
                authorization: if model_reentry {
                    ResumeAuthorization::ExpectedWait
                } else {
                    ResumeAuthorization::LivenessOnly
                },
                model_reentry,
                matched_waiting_reason: true,
            };
        }

        if trigger_kind == ContinuationTriggerKind::OperatorInput {
            return ResumeDecision {
                authorization: ResumeAuthorization::Override,
                model_reentry: true,
                matched_waiting_reason: false,
            };
        }

        if terminal_task_result {
            return ResumeDecision {
                authorization: ResumeAuthorization::RuntimeEventReentry,
                model_reentry: true,
                matched_waiting_reason: false,
            };
        }

        return ResumeDecision {
            authorization: ResumeAuthorization::LivenessOnly,
            model_reentry: false,
            matched_waiting_reason: false,
        };
    }

    let runtime_event_reentry = terminal_task_result;
    let local_continuation = matches!(
        trigger_kind,
        ContinuationTriggerKind::OperatorInput
            | ContinuationTriggerKind::TimerFire
            | ContinuationTriggerKind::InternalFollowup
    ) || matches!(
        trigger_kind,
        ContinuationTriggerKind::ExternalEvent | ContinuationTriggerKind::SystemTick
    ) && contentful;

    let authorization = if runtime_event_reentry {
        ResumeAuthorization::RuntimeEventReentry
    } else if local_continuation {
        ResumeAuthorization::LocalContinuation
    } else {
        ResumeAuthorization::LivenessOnly
    };
    ResumeDecision {
        model_reentry: authorization != ResumeAuthorization::LivenessOnly,
        authorization,
        matched_waiting_reason: false,
    }
}

fn waiting_reason_matches(
    reason: Option<WaitingReason>,
    trigger_kind: ContinuationTriggerKind,
) -> bool {
    matches!(
        (reason, trigger_kind),
        (
            Some(WaitingReason::AwaitingOperatorInput),
            ContinuationTriggerKind::OperatorInput
        ) | (
            Some(WaitingReason::AwaitingTaskResult),
            ContinuationTriggerKind::TaskResult
        ) | (
            Some(WaitingReason::AwaitingExternalChange),
            ContinuationTriggerKind::ExternalEvent
        ) | (
            Some(WaitingReason::AwaitingExternalChange),
            ContinuationTriggerKind::SystemTick
        ) | (
            Some(WaitingReason::AwaitingTimer),
            ContinuationTriggerKind::TimerFire
        )
    )
}

#[cfg(test)]
mod tests {
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
}
