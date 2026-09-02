use super::wake_matching::{resolve_resume_authorization, ResumeAuthorization};
use crate::types::{
    admission_trigger_kind_for_message_kind, ClosureDecision, ClosureOutcome, ContinuationClass,
    ContinuationResolution, ContinuationTriggerKind, MessageBody, MessageEnvelope, MessageKind,
    TaskRecord, TaskResultOutcome, TaskStatus, WaitingReason,
};

#[derive(Debug, Clone)]
pub(super) struct ContinuationTrigger {
    pub(super) kind: ContinuationTriggerKind,
    pub(super) contentful: bool,
    pub(super) task_result_outcome: Option<TaskResultOutcome>,
    pub(super) wake_hint_source: Option<String>,
    pub(super) task_work_item_id: Option<String>,
    /// Durable evidence that an explicit same-scope WaitFor targeted the
    /// exact task whose result this message carries.  It authorizes terminal
    /// task result reentry even when the prior closure waiting_reason was
    /// polluted by unrelated waits.
    pub(super) exact_task_wait: bool,
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
                task_result_outcome: None,
                wake_hint_source: None,
                task_work_item_id: None,
                exact_task_wait: false,
            }),
            MessageKind::WebhookEvent | MessageKind::CallbackEvent | MessageKind::ChannelEvent => {
                Some(Self {
                    kind: admission_trigger_kind_for_message_kind(&message.kind),
                    contentful: body_is_contentful(&message.body),
                    task_result_outcome: None,
                    wake_hint_source: None,
                    task_work_item_id: None,
                    exact_task_wait: false,
                })
            }
            MessageKind::TimerTick => Some(Self {
                kind: admission_trigger_kind_for_message_kind(&message.kind),
                contentful: body_is_contentful(&message.body),
                task_result_outcome: None,
                wake_hint_source: None,
                task_work_item_id: None,
                exact_task_wait: false,
            }),
            MessageKind::InternalFollowup => Some(Self {
                kind: admission_trigger_kind_for_message_kind(&message.kind),
                contentful: body_is_contentful(&message.body),
                task_result_outcome: None,
                task_work_item_id: None,
                wake_hint_source: None,
                exact_task_wait: false,
            }),
            MessageKind::SystemTick => Some(Self {
                kind: admission_trigger_kind_for_message_kind(&message.kind),
                contentful: system_tick_is_contentful(message),
                task_result_outcome: None,
                wake_hint_source: message
                    .metadata
                    .as_ref()
                    .and_then(|value| value.get("wake_hint"))
                    .and_then(|value| value.get("source"))
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string),
                task_work_item_id: None,
                exact_task_wait: false,
            }),
            MessageKind::TaskResult => Some(Self {
                kind: admission_trigger_kind_for_message_kind(&message.kind),
                contentful: body_is_contentful(&message.body),
                task_result_outcome: task.and_then(|task| match task.status {
                    TaskStatus::Completed => Some(TaskResultOutcome::Succeeded),
                    TaskStatus::Failed => Some(TaskResultOutcome::Failed),
                    TaskStatus::Cancelled => Some(TaskResultOutcome::Cancelled),
                    TaskStatus::Interrupted => Some(TaskResultOutcome::Interrupted),
                    TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling => None,
                }),
                wake_hint_source: None,
                task_work_item_id: task
                    .and_then(|t| t.effective_work_item_id().map(ToString::to_string)),
                // Filled by the dispatch plan builder from durable wait
                // evidence; from_message alone cannot see wait conditions.
                exact_task_wait: false,
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
    let prior_waiting_reason = prior.waiting_reason;
    let same_work_item = match (trigger.task_work_item_id.as_deref(), agent_work_item_id) {
        (Some(_), None) | (None, Some(_)) => false,
        (None, None) => {
            trigger.kind == ContinuationTriggerKind::TaskResult
                && trigger.task_result_outcome.is_some()
                && (trigger.exact_task_wait
                    || matches!(
                        prior_waiting_reason,
                        None | Some(WaitingReason::AwaitingTaskResult)
                    ))
        }
        (Some(t), Some(a)) => t == a,
    };
    let authorization = resolve_resume_authorization(
        prior.outcome,
        prior_waiting_reason,
        trigger.kind,
        trigger.contentful,
        trigger.task_result_outcome,
        same_work_item,
    );
    let mut evidence = Vec::new();
    evidence.push(format!("trigger_kind={}", enum_label(trigger.kind)));
    if trigger.contentful {
        evidence.push("contentful".to_string());
    } else {
        evidence.push("not_contentful".to_string());
    }
    if let Some(outcome) = trigger.task_result_outcome {
        evidence.push("task_terminal".to_string());
        evidence.push(format!("task_result_outcome={}", enum_label(outcome)));
    }
    if trigger.exact_task_wait {
        evidence.push("exact_task_wait".to_string());
    }
    if authorization.matched_waiting_reason {
        evidence.push("matches_waiting_reason".to_string());
    } else {
        evidence.push("does_not_satisfy_waiting_reason".to_string());
    }
    if let Some(source) = trigger.wake_hint_source.as_ref() {
        evidence.push(format!("wake_hint_source={source}"));
    }
    match authorization.authorization {
        ResumeAuthorization::ExpectedWait => {
            evidence.push("resume_authorization=expected_wait".into())
        }
        ResumeAuthorization::RuntimeEventReentry => {
            evidence.push("resume_authorization=runtime_event_reentry".into())
        }
        ResumeAuthorization::Override => evidence.push("resume_authorization=override".into()),
        ResumeAuthorization::LocalContinuation => {
            evidence.push("resume_authorization=local_continuation".into())
        }
        ResumeAuthorization::LivenessOnly => {
            evidence.push("resume_authorization=liveness_only".into())
        }
    }
    let class = match authorization.authorization {
        ResumeAuthorization::ExpectedWait => ContinuationClass::ResumeExpectedWait,
        ResumeAuthorization::RuntimeEventReentry => {
            if prior.outcome == ClosureOutcome::Waiting {
                ContinuationClass::ResumeOverride
            } else {
                ContinuationClass::TaskResultReentry
            }
        }
        ResumeAuthorization::Override => ContinuationClass::ResumeOverride,
        ResumeAuthorization::LocalContinuation => ContinuationClass::LocalContinuation,
        ResumeAuthorization::LivenessOnly => ContinuationClass::LivenessOnly,
    };
    ContinuationResolution {
        trigger_kind: trigger.kind,
        class,
        model_reentry: authorization.model_reentry,
        prior_closure_outcome: prior.outcome,
        prior_waiting_reason,
        matched_waiting_reason: authorization.matched_waiting_reason,
        evidence,
    }
}

fn enum_label<T: serde::Serialize + std::fmt::Debug>(value: T) -> String {
    serde_json::to_value(&value)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_else(|| format!("{value:?}").to_lowercase())
}

fn body_is_contentful(body: &MessageBody) -> bool {
    match body {
        MessageBody::Text { text } => !text.trim().is_empty(),
        MessageBody::Json { .. } => true,
        MessageBody::Brief { text, .. } => !text.trim().is_empty(),
    }
}

fn wake_hint_body_is_contentful(message: &MessageEnvelope) -> bool {
    let wake_hint = message
        .metadata
        .as_ref()
        .and_then(|value| value.get("wake_hint"));
    let explicit_body = wake_hint
        .and_then(|value| value.get("body"))
        .cloned()
        .and_then(|value| serde_json::from_value::<MessageBody>(value).ok())
        .is_some_and(|body| body_is_contentful(&body));
    if explicit_body {
        return true;
    }
    // A wake hint carrying a non-empty reason is deliberate operator/external
    // signal content and warrants model reentry even without an explicit body.
    wake_hint
        .and_then(|value| value.get("reason"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|reason| !reason.trim().is_empty())
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

    use crate::types::{AuthorityClass, MessageOrigin, Priority};

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
    fn unbound_terminal_task_result_resumes_expected_wait() {
        let resolution = resolve_continuation(
            &waiting(WaitingReason::AwaitingTaskResult),
            &ContinuationTrigger {
                kind: ContinuationTriggerKind::TaskResult,
                contentful: true,
                task_result_outcome: Some(TaskResultOutcome::Succeeded),
                wake_hint_source: None,
                task_work_item_id: None,
                exact_task_wait: false,
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
                task_result_outcome: None,
                wake_hint_source: None,
                task_work_item_id: None,
                exact_task_wait: false,
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
                task_result_outcome: Some(TaskResultOutcome::Succeeded),
                wake_hint_source: None,
                task_work_item_id: Some("other-work".into()),
                exact_task_wait: false,
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
                task_result_outcome: None,
                wake_hint_source: Some("callback".into()),
                task_work_item_id: None,
                exact_task_wait: false,
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
                task_result_outcome: None,
                wake_hint_source: None,
                task_work_item_id: None,
                exact_task_wait: false,
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
                task_result_outcome: None,
                wake_hint_source: None,
                task_work_item_id: None,
                exact_task_wait: false,
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
                task_result_outcome: None,
                wake_hint_source: None,
                task_work_item_id: None,
                exact_task_wait: false,
            },
            None,
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
                },
                None,
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
            },
            None,
        );
        assert_eq!(resolution.class, ContinuationClass::TaskResultReentry);
        assert!(resolution.model_reentry);
    }

    #[test]
    fn unbound_terminal_task_result_does_not_override_mismatched_wait() {
        let resolution = resolve_continuation(
            &waiting(WaitingReason::AwaitingExternalChange),
            &ContinuationTrigger {
                kind: ContinuationTriggerKind::TaskResult,
                contentful: true,
                task_result_outcome: Some(TaskResultOutcome::Succeeded),
                wake_hint_source: None,
                task_work_item_id: None,
                exact_task_wait: false,
            },
            None,
        );
        assert_eq!(resolution.class, ContinuationClass::LivenessOnly);
        assert!(!resolution.model_reentry);
    }

    #[test]
    fn unbound_terminal_task_result_does_not_override_operator_input_wait() {
        let resolution = resolve_continuation(
            &waiting(WaitingReason::AwaitingOperatorInput),
            &ContinuationTrigger {
                kind: ContinuationTriggerKind::TaskResult,
                contentful: true,
                task_result_outcome: Some(TaskResultOutcome::Succeeded),
                wake_hint_source: None,
                task_work_item_id: None,
                exact_task_wait: false,
            },
            None,
        );
        assert_eq!(resolution.class, ContinuationClass::LivenessOnly);
        assert!(!resolution.model_reentry);
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
                task_result_outcome: None,
                wake_hint_source: None,
                task_work_item_id: None,
                exact_task_wait: false,
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
        assert!(system_tick_is_contentful(&message));
    }

    #[test]
    fn wake_hint_with_empty_reason_and_no_body_is_not_contentful() {
        let message = wake_hint_system_tick("", None);
        assert!(!system_tick_is_contentful(&message));
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
        };
        let resolution = resolve_continuation(
            &waiting(WaitingReason::AwaitingOperatorInput),
            &trigger,
            None,
        );
        assert!(resolution.model_reentry);
        assert_eq!(resolution.class, ContinuationClass::ResumeOverride);
        assert!(resolution
            .evidence
            .iter()
            .any(|entry| entry == "exact_task_wait"));
    }

    #[test]
    fn polluted_closure_without_exact_task_wait_stays_liveness_only() {
        let trigger = ContinuationTrigger {
            kind: ContinuationTriggerKind::TaskResult,
            contentful: true,
            task_result_outcome: Some(TaskResultOutcome::Succeeded),
            wake_hint_source: None,
            task_work_item_id: None,
            exact_task_wait: false,
        };
        let resolution = resolve_continuation(
            &waiting(WaitingReason::AwaitingOperatorInput),
            &trigger,
            None,
        );
        assert!(!resolution.model_reentry);
        assert_eq!(resolution.class, ContinuationClass::LivenessOnly);
        assert!(resolution
            .evidence
            .iter()
            .any(|entry| entry == "does_not_satisfy_waiting_reason"));
    }
}
