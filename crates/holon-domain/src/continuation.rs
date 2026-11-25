//! Continuation classification over recorded triggers and closure.

mod authorization;

use crate::{
    admission_trigger_kind_for_message_kind, ClosureDecision, ContinuationClass,
    ContinuationResolution, ContinuationTriggerKind, MessageBody, MessageEnvelope, MessageKind,
    TaskRecord, TaskResultOutcome, TaskStatus,
};
use authorization::{resolve_resume_authorization, ResumeAuthorization, ResumeDecision};

#[derive(Debug, Clone)]
pub struct ContinuationTrigger {
    pub kind: ContinuationTriggerKind,
    pub contentful: bool,
    pub task_result_outcome: Option<TaskResultOutcome>,
    pub wake_hint_source: Option<String>,
    pub task_work_item_id: Option<String>,
    /// Durable evidence that an explicit same-scope WaitFor targeted the
    /// exact task whose result this message carries.  It authorizes terminal
    /// task result reentry even when the prior closure waiting_reason was
    /// polluted by unrelated waits.
    pub exact_task_wait: bool,
    /// Durable evidence that this runtime-owned SystemTick is the exact
    /// recheck event for an agent-scope WaitFor condition.
    pub exact_wait_recheck: bool,
}

impl ContinuationTrigger {
    pub fn from_message(message: &MessageEnvelope, task: Option<&TaskRecord>) -> Option<Self> {
        match message.kind {
            MessageKind::OperatorPrompt => Some(Self {
                kind: admission_trigger_kind_for_message_kind(&message.kind),
                contentful: body_is_contentful(&message.body),
                task_result_outcome: None,
                wake_hint_source: None,
                task_work_item_id: None,
                exact_task_wait: false,
                exact_wait_recheck: false,
            }),
            MessageKind::WebhookEvent | MessageKind::CallbackEvent | MessageKind::ChannelEvent => {
                Some(Self {
                    kind: admission_trigger_kind_for_message_kind(&message.kind),
                    contentful: body_is_contentful(&message.body),
                    task_result_outcome: None,
                    wake_hint_source: None,
                    task_work_item_id: None,
                    exact_task_wait: false,
                    exact_wait_recheck: false,
                })
            }
            MessageKind::TimerTick => Some(Self {
                kind: admission_trigger_kind_for_message_kind(&message.kind),
                contentful: body_is_contentful(&message.body),
                task_result_outcome: None,
                wake_hint_source: None,
                task_work_item_id: None,
                exact_task_wait: false,
                exact_wait_recheck: false,
            }),
            MessageKind::InternalFollowup => Some(Self {
                kind: admission_trigger_kind_for_message_kind(&message.kind),
                contentful: body_is_contentful(&message.body),
                task_result_outcome: None,
                task_work_item_id: None,
                wake_hint_source: None,
                exact_task_wait: false,
                exact_wait_recheck: false,
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
                exact_wait_recheck: false,
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
                exact_wait_recheck: false,
            }),
            MessageKind::TaskStatus
            | MessageKind::Control
            | MessageKind::BriefAck
            | MessageKind::BriefResult => None,
        }
    }
}

/// Classify continuation from the supplied facts. The runtime admits execution.
pub fn resolve_continuation(
    prior: &ClosureDecision,
    trigger: &ContinuationTrigger,
) -> ContinuationResolution {
    let prior_waiting_reason = prior.waiting_reason;
    let authorization = if trigger.exact_wait_recheck && trigger.contentful {
        ResumeDecision {
            authorization: ResumeAuthorization::ExpectedWait,
            model_reentry: true,
            matched_waiting_reason: false,
        }
    } else {
        resolve_resume_authorization(
            prior.outcome,
            prior_waiting_reason,
            trigger.kind,
            trigger.contentful,
            trigger.task_result_outcome,
            trigger.exact_task_wait,
        )
    };
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
    if trigger.exact_wait_recheck {
        evidence.push("exact_wait_recheck".to_string());
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
        ResumeAuthorization::RuntimeEventReentry => ContinuationClass::TaskResultReentry,
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
