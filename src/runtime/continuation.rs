use crate::types::{
    admission_trigger_kind_for_message_kind, ClosureDecision, ClosureOutcome, ContinuationClass,
    ContinuationResolution, ContinuationTriggerKind, MessageBody, MessageEnvelope, MessageKind,
    TaskRecord, TaskStatus, WaitingReason,
};

#[derive(Debug, Clone)]
pub(super) struct ContinuationTrigger {
    pub(super) kind: ContinuationTriggerKind,
    pub(super) contentful: bool,
    pub(super) task_terminal: bool,
    pub(super) wake_hint_source: Option<String>,
    pub(super) task_work_item_id: Option<String>,
}

impl ContinuationTrigger {
    pub(super) fn from_message(
        message: &MessageEnvelope,
        task: Option<&TaskRecord>,
    ) -> Option<Self> {
        match message.kind {
            MessageKind::OperatorPrompt => Some(Self {
                kind: admission_trigger_kind_for_message_kind(&message.kind),
                contentful: body_is_contentful(&message.body),
                task_terminal: false,
                wake_hint_source: None,
                task_work_item_id: None,
            }),
            MessageKind::WebhookEvent | MessageKind::CallbackEvent | MessageKind::ChannelEvent => {
                Some(Self {
                    kind: admission_trigger_kind_for_message_kind(&message.kind),
                    contentful: body_is_contentful(&message.body),
                    task_terminal: false,
                    wake_hint_source: None,
                    task_work_item_id: None,
                })
            }
            MessageKind::TimerTick => Some(Self {
                kind: admission_trigger_kind_for_message_kind(&message.kind),
                contentful: body_is_contentful(&message.body),
                task_terminal: false,
                wake_hint_source: None,
                task_work_item_id: None,
            }),
            MessageKind::InternalFollowup => Some(Self {
                kind: admission_trigger_kind_for_message_kind(&message.kind),
                contentful: body_is_contentful(&message.body),
                task_terminal: false,
                task_work_item_id: None,
                wake_hint_source: None,
            }),
            MessageKind::SystemTick => Some(Self {
                kind: admission_trigger_kind_for_message_kind(&message.kind),
                contentful: system_tick_is_contentful(message),
                task_terminal: false,
                wake_hint_source: message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("wake_hint"))
                    .and_then(|value| value.get("source"))
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string),
                task_work_item_id: None,
            }),
            MessageKind::TaskResult => Some(Self {
                kind: admission_trigger_kind_for_message_kind(&message.kind),
                contentful: body_is_contentful(&message.body),
                task_terminal: task
                    .map(|task| {
                        matches!(
                            task.status,
                            TaskStatus::Completed
                                | TaskStatus::Failed
                                | TaskStatus::Cancelled
                                | TaskStatus::Interrupted
                        )
                    })
                    .unwrap_or(false),
                wake_hint_source: None,
                task_work_item_id: task
                    .and_then(|t| t.effective_work_item_id().map(ToString::to_string)),
            }),
            MessageKind::TaskStatus
            | MessageKind::Control
            | MessageKind::BriefAck
            | MessageKind::BriefResult => None,
        }
    }
}

pub(super) fn resolve_continuation(
    prior: &ClosureDecision,
    trigger: &ContinuationTrigger,
    agent_work_item_id: Option<&str>,
) -> ContinuationResolution {
    let same_work_item = match (trigger.task_work_item_id.as_deref(), agent_work_item_id) {
        (Some(_), None) | (None, Some(_)) => false,
        (None, None) => true,
        (Some(t), Some(a)) => t == a,
    };
    let mut evidence = Vec::new();
    evidence.push(format!("trigger_kind={}", enum_label(trigger.kind)));
    if trigger.contentful {
        evidence.push("contentful".to_string());
    } else {
        evidence.push("not_contentful".to_string());
    }
    if trigger.task_terminal {
        evidence.push("task_terminal".to_string());
    }
    if let Some(source) = trigger.wake_hint_source.as_ref() {
        evidence.push(format!("wake_hint_source={source}"));
    }

    let prior_waiting_reason = prior.waiting_reason;
    let prior_closure_outcome = prior.outcome;

    if prior.outcome == ClosureOutcome::Waiting {
        return resolve_waiting(
            prior_waiting_reason,
            prior_closure_outcome,
            trigger,
            same_work_item,
            evidence,
        );
    }

    let terminal_task_result = trigger.kind == ContinuationTriggerKind::TaskResult
        && trigger.task_terminal
        && same_work_item;
    let model_reentry = terminal_task_result
        || matches!(
            trigger.kind,
            ContinuationTriggerKind::OperatorInput
                | ContinuationTriggerKind::TimerFire
                | ContinuationTriggerKind::InternalFollowup
        )
        || ((trigger.kind == ContinuationTriggerKind::ExternalEvent
            || trigger.kind == ContinuationTriggerKind::SystemTick)
            && trigger.contentful);
    let class = if model_reentry {
        ContinuationClass::LocalContinuation
    } else {
        ContinuationClass::LivenessOnly
    };
    ContinuationResolution {
        trigger_kind: trigger.kind,
        class,
        model_reentry,
        prior_closure_outcome,
        prior_waiting_reason,
        matched_waiting_reason: false,
        evidence,
    }
}

fn enum_label<T: serde::Serialize + std::fmt::Debug>(value: T) -> String {
    serde_json::to_value(&value)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_else(|| format!("{value:?}").to_lowercase())
}

fn resolve_waiting(
    prior_waiting_reason: Option<WaitingReason>,
    prior_closure_outcome: ClosureOutcome,
    trigger: &ContinuationTrigger,
    same_work_item: bool,
    mut evidence: Vec<String>,
) -> ContinuationResolution {
    let reason = prior_waiting_reason;
    let expected = matches!(
        (reason, trigger.kind),
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
    );
    let override_allowed = trigger.kind == ContinuationTriggerKind::OperatorInput;
    if expected {
        let model_reentry = match trigger.kind {
            ContinuationTriggerKind::TaskResult => trigger.task_terminal && same_work_item,
            ContinuationTriggerKind::ExternalEvent => trigger.contentful,
            ContinuationTriggerKind::SystemTick => trigger.contentful,
            _ => true,
        };
        evidence.push("matches_waiting_reason".to_string());
        return ContinuationResolution {
            trigger_kind: trigger.kind,
            class: if model_reentry {
                ContinuationClass::ResumeExpectedWait
            } else {
                ContinuationClass::LivenessOnly
            },
            model_reentry,
            prior_closure_outcome,
            prior_waiting_reason,
            matched_waiting_reason: true,
            evidence,
        };
    }

    if override_allowed {
        evidence.push("override_waiting_reason".to_string());
        return ContinuationResolution {
            trigger_kind: trigger.kind,
            class: ContinuationClass::ResumeOverride,
            model_reentry: true,
            prior_closure_outcome,
            prior_waiting_reason,
            matched_waiting_reason: false,
            evidence,
        };
    }

    if trigger.kind == ContinuationTriggerKind::SystemTick && trigger.contentful {
        evidence.push("contentful_system_tick_expected_external_recheck".to_string());
        let matched_waiting_reason = reason == Some(WaitingReason::AwaitingExternalChange);
        return ContinuationResolution {
            trigger_kind: trigger.kind,
            class: if matched_waiting_reason {
                ContinuationClass::ResumeExpectedWait
            } else {
                ContinuationClass::LivenessOnly
            },
            model_reentry: matched_waiting_reason,
            prior_closure_outcome,
            prior_waiting_reason,
            matched_waiting_reason,
            evidence,
        };
    }

    if trigger.kind == ContinuationTriggerKind::TaskResult
        && trigger.task_terminal
        && same_work_item
    {
        // Terminal task state is persisted before TaskResult enqueue, so
        // resuming here cannot reopen the stale active-task wait that just ended.
        evidence.push("terminal_task_result".to_string());
        return ContinuationResolution {
            trigger_kind: trigger.kind,
            class: ContinuationClass::ResumeOverride,
            model_reentry: true,
            prior_closure_outcome,
            prior_waiting_reason,
            matched_waiting_reason: false,
            evidence,
        };
    }

    evidence.push("does_not_satisfy_waiting_reason".to_string());
    ContinuationResolution {
        trigger_kind: trigger.kind,
        class: ContinuationClass::LivenessOnly,
        model_reentry: false,
        prior_closure_outcome,
        prior_waiting_reason,
        matched_waiting_reason: false,
        evidence,
    }
}

fn body_is_contentful(body: &MessageBody) -> bool {
    match body {
        MessageBody::Text { text } => !text.trim().is_empty(),
        MessageBody::Json { .. } => true,
        MessageBody::Brief { text, .. } => !text.trim().is_empty(),
    }
}

fn wake_hint_body_is_contentful(message: &MessageEnvelope) -> bool {
    message
        .metadata
        .as_ref()
        .and_then(|value| value.get("wake_hint"))
        .and_then(|value| value.get("body"))
        .cloned()
        .and_then(|value| serde_json::from_value::<MessageBody>(value).ok())
        .is_some_and(|body| body_is_contentful(&body))
}

fn system_tick_is_contentful(message: &MessageEnvelope) -> bool {
    if message
        .metadata
        .as_ref()
        .and_then(|value| value.get("wake_hint"))
        .is_some()
    {
        return wake_hint_body_is_contentful(message);
    }
    body_is_contentful(&message.body)
}

#[cfg(test)]
mod tests {
    use crate::types::{ClosureDecision, RuntimePosture};

    use super::*;

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
    fn terminal_task_result_resumes_expected_wait() {
        let resolution = resolve_continuation(
            &waiting(WaitingReason::AwaitingTaskResult),
            &ContinuationTrigger {
                kind: ContinuationTriggerKind::TaskResult,
                contentful: true,
                task_terminal: true,
                wake_hint_source: None,
                task_work_item_id: None,
            },
            None,
        );
        assert_eq!(resolution.class, ContinuationClass::ResumeExpectedWait);
        assert!(resolution.model_reentry);
    }

    #[test]
    fn non_terminal_task_result_does_not_resume_expected_wait() {
        let resolution = resolve_continuation(
            &waiting(WaitingReason::AwaitingTaskResult),
            &ContinuationTrigger {
                kind: ContinuationTriggerKind::TaskResult,
                contentful: true,
                task_terminal: false,
                wake_hint_source: None,
                task_work_item_id: None,
            },
            None,
        );

        assert_eq!(resolution.class, ContinuationClass::LivenessOnly);
        assert!(!resolution.model_reentry);
        assert!(resolution.matched_waiting_reason);
        assert!(resolution
            .evidence
            .iter()
            .any(|entry| entry == "matches_waiting_reason"));
    }

    #[test]
    fn terminal_task_result_for_other_work_item_does_not_resume_expected_wait() {
        let resolution = resolve_continuation(
            &waiting(WaitingReason::AwaitingTaskResult),
            &ContinuationTrigger {
                kind: ContinuationTriggerKind::TaskResult,
                contentful: true,
                task_terminal: true,
                wake_hint_source: None,
                task_work_item_id: Some("other-work".into()),
            },
            Some("active-work"),
        );

        assert_eq!(resolution.class, ContinuationClass::LivenessOnly);
        assert!(!resolution.model_reentry);
        assert!(resolution.matched_waiting_reason);
    }

    #[test]
    fn wake_hint_system_tick_is_liveness_only() {
        let resolution = resolve_continuation(
            &waiting(WaitingReason::AwaitingExternalChange),
            &ContinuationTrigger {
                kind: ContinuationTriggerKind::SystemTick,
                contentful: false,
                task_terminal: false,
                wake_hint_source: Some("callback".into()),
                task_work_item_id: None,
            },
            None,
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
                task_terminal: false,
                wake_hint_source: None,
                task_work_item_id: None,
            },
            None,
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
                task_terminal: false,
                wake_hint_source: None,
                task_work_item_id: None,
            },
            None,
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
                task_terminal: false,
                wake_hint_source: None,
                task_work_item_id: None,
            },
            None,
        );
        assert_eq!(resolution.class, ContinuationClass::LivenessOnly);
        assert!(!resolution.model_reentry);
    }

    #[test]
    fn terminal_task_result_resumes_without_prior_wait() {
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
                task_terminal: true,
                wake_hint_source: None,
                task_work_item_id: None,
            },
            None,
        );
        assert_eq!(resolution.class, ContinuationClass::LocalContinuation);
        assert!(resolution.model_reentry);
    }

    #[test]
    fn terminal_task_result_resumes_from_sleeping_posture() {
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
                task_terminal: true,
                wake_hint_source: None,
                task_work_item_id: None,
            },
            None,
        );
        assert_eq!(resolution.class, ContinuationClass::LocalContinuation);
        assert!(resolution.model_reentry);
    }

    #[test]
    fn terminal_task_result_overrides_mismatched_wait() {
        let resolution = resolve_continuation(
            &waiting(WaitingReason::AwaitingExternalChange),
            &ContinuationTrigger {
                kind: ContinuationTriggerKind::TaskResult,
                contentful: true,
                task_terminal: true,
                wake_hint_source: None,
                task_work_item_id: None,
            },
            None,
        );
        assert_eq!(resolution.class, ContinuationClass::ResumeOverride);
        assert!(resolution.model_reentry);
    }

    #[test]
    fn empty_external_event_waiting_for_external_change_is_liveness_only() {
        let resolution = resolve_continuation(
            &waiting(WaitingReason::AwaitingExternalChange),
            &ContinuationTrigger {
                kind: ContinuationTriggerKind::ExternalEvent,
                contentful: false,
                task_terminal: false,
                wake_hint_source: None,
                task_work_item_id: None,
            },
            None,
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
                task_terminal: false,
                wake_hint_source: None,
                task_work_item_id: None,
            },
            None,
        );

        assert_eq!(resolution.class, ContinuationClass::LivenessOnly);
        assert!(!resolution.model_reentry);
        assert!(!resolution.matched_waiting_reason);
        assert!(resolution
            .evidence
            .iter()
            .any(|entry| entry == "does_not_satisfy_waiting_reason"));
    }
}
