use crate::{ClosureOutcome, ContinuationTriggerKind, TaskResultOutcome, WaitingReason};

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
mod tests;
