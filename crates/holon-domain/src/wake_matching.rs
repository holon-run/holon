//! Wait matching for atomic wake writes and scheduler activation candidates.

use crate::{
    AdmissionContext, AuthorityClass, MessageDeliverySurface, MessageEnvelope, MessageKind,
    MessageOrigin, WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakeMatch<'a> {
    pub source: &'static str,
    pub subject_ref: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitTriggerSelection<'a, StaleWaitId = &'a str> {
    Match {
        condition: &'a WaitConditionRecord,
        wake: WakeMatch<'a>,
    },
    StaleExact {
        wait_id: StaleWaitId,
    },
    NoMatch,
}

pub fn is_agent_message(message: &MessageEnvelope) -> bool {
    message.kind == MessageKind::InternalFollowup
        && message.delivery_surface == Some(MessageDeliverySurface::RuntimeSystem)
        && message.admission_context == Some(AdmissionContext::RuntimeOwned)
        && message.routing_context.is_some()
        && message
            .metadata
            .as_ref()
            .and_then(|m| m.get("agent_message_delivery"))
            .is_some()
}

pub fn has_operator_queue_authority(message: &MessageEnvelope) -> bool {
    // A pre-derivation agent envelope can retain its caller's operator label.
    // Its recorded priority still applies, but its label cannot move it ahead.
    message.authority_class == AuthorityClass::OperatorInstruction && !is_agent_message(message)
}

/// Select the writer's exact wait using the supplied records and borrow their evidence.
pub fn select_wait_to_trigger<'a>(
    message: &'a MessageEnvelope,
    conditions: &'a [WaitConditionRecord],
) -> WaitTriggerSelection<'a> {
    let mut candidates = wait_trigger_candidates(message, conditions);
    if let Some(wait_id) = authoritative_wait_correlation(message) {
        return match candidates.find(|(condition, _)| condition.id == wait_id) {
            Some((condition, wake)) => WaitTriggerSelection::Match { condition, wake },
            None => WaitTriggerSelection::StaleExact { wait_id },
        };
    }
    let Some(first) = candidates.next() else {
        return WaitTriggerSelection::NoMatch;
    };
    let Some(second) = candidates.next() else {
        return WaitTriggerSelection::Match {
            condition: first.0,
            wake: first.1,
        };
    };
    // One wake consumes exactly one durable trigger marker. Selecting the
    // waiter is the runtime's decision, never a reason to quarantine the wake
    // (#3914): an exact task waiter for this result wins, then a WorkItem wait
    // over agent-lifecycle scope, then the earliest registration, then the
    // stable wait id. The remaining matches keep their own durable state.
    let (condition, wake) = std::iter::once(first)
        .chain(std::iter::once(second))
        .chain(candidates)
        .min_by(|(left, _), (right, _)| trigger_wait_order(message, left, right))
        .expect("two candidates have a deterministic selection");
    WaitTriggerSelection::Match { condition, wake }
}

/// Deterministic single-waiter order for a wake that matches several waits
/// (#3914). It mirrors the scheduler's canonical selection so the planned
/// owner and the triggered wait row agree.
pub fn trigger_wait_order(
    message: &MessageEnvelope,
    left: &WaitConditionRecord,
    right: &WaitConditionRecord,
) -> std::cmp::Ordering {
    let exact_task_waiter = |condition: &WaitConditionRecord| {
        condition.kind == WaitConditionKind::Task
            && condition.work_item_id.as_deref() == message.work_item_id.as_deref()
            && message.task_id.as_deref().is_some_and(|task_id| {
                condition.wake_sources.iter().any(|source| {
                    matches!(source, WakeSource::TaskResult { task_id: id } if id == task_id)
                })
            })
    };
    exact_task_waiter(right)
        .cmp(&exact_task_waiter(left))
        .then_with(|| {
            right
                .work_item_id
                .is_some()
                .cmp(&left.work_item_id.is_some())
        })
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.id.cmp(&right.id))
}

fn wait_trigger_candidates<'a>(
    message: &'a MessageEnvelope,
    conditions: &'a [WaitConditionRecord],
) -> impl Iterator<Item = (&'a WaitConditionRecord, WakeMatch<'a>)> {
    conditions
        .iter()
        .filter(|condition| {
            condition.status == WaitConditionStatus::Active
                || (condition.status == WaitConditionStatus::Triggered
                    && condition.trigger_message_id() == Some(message.id.as_str()))
        })
        .filter(|condition| {
            message
                .turn_id
                .as_deref()
                .zip(condition.turn_id.as_deref())
                .is_none_or(|(message_turn, wait_turn)| message_turn != wait_turn)
        })
        .filter_map(|condition| {
            matching_wake_source(message, condition).map(|wake| (condition, wake))
        })
}

/// Exact correlation accepted by the wait writer, including runtime-owned timer delivery.
pub fn authoritative_wait_correlation(message: &MessageEnvelope) -> Option<&str> {
    let trusted = matches!(
        (message.delivery_surface, message.admission_context),
        (
            Some(MessageDeliverySurface::RuntimeSystem),
            Some(AdmissionContext::RuntimeOwned)
        ) | (
            Some(MessageDeliverySurface::TaskRejoin),
            Some(AdmissionContext::RuntimeOwned)
        ) | (
            Some(MessageDeliverySurface::TimerScheduler),
            Some(AdmissionContext::RuntimeOwned)
        ) | (
            Some(MessageDeliverySurface::HttpCallbackWake),
            Some(AdmissionContext::ExternalTriggerCapability)
        )
    ) && matches!(
        message.authority_class,
        AuthorityClass::RuntimeInstruction | AuthorityClass::IntegrationSignal
    );
    trusted
        .then(|| message.source_refs.get("wait_id").map(String::as_str))
        .flatten()
}

pub fn matching_wake_source<'a>(
    message: &'a MessageEnvelope,
    condition: &'a WaitConditionRecord,
) -> Option<WakeMatch<'a>> {
    // A task result message is addressed to the task's owner, but any
    // WorkItem (or agent lifecycle) inside the same agent may hold the
    // dependency wait (#3124): match task waits by task identity instead of
    // the message's WorkItem binding.
    let exact_reply = is_agent_message(message)
        && condition.kind == WaitConditionKind::Task
        && condition.trigger_message_id() == Some(message.id.as_str());
    let task_result_message = exact_reply
        || matches!(
            (&message.kind, &message.origin),
            (MessageKind::TaskResult, MessageOrigin::Task { .. })
        );
    // A Timer fire carries no WorkItem binding when several waits hold the
    // same Timer (#3914). Both scopes stay candidates; the deterministic
    // selection decides which one owns the wake and the rest keep their own
    // durable state.
    let unbound_timer_fire = message.work_item_id.is_none()
        && matches!(
            (&message.kind, &message.origin),
            (MessageKind::TimerTick, MessageOrigin::Timer { .. })
        );
    if message.agent_id != condition.agent_id
        || (!task_result_message
            && !unbound_timer_fire
            && message.work_item_id.as_deref() != condition.work_item_id.as_deref())
        || (condition.status == WaitConditionStatus::Triggered
            && condition.trigger_message_id() != Some(message.id.as_str()))
    {
        return None;
    }
    let matched = |source: &'static str, subject_ref| WakeMatch {
        source,
        subject_ref,
    };
    if exact_reply {
        return Some(matched("task_result", condition.subject_ref.as_deref()));
    }
    match (&message.kind, &message.origin) {
        (MessageKind::TaskResult, MessageOrigin::Task { task_id }) => condition
            .wake_sources
            .iter()
            .any(|source| matches!(source, WakeSource::TaskResult { task_id: id } if id == task_id))
            .then(|| matched("task_result", Some(task_id.as_str()))),
        (MessageKind::CallbackEvent | MessageKind::WebhookEvent | MessageKind::ChannelEvent, _) => {
            let external_trigger_id = message.source_refs.get("external_trigger_id");
            condition
                .wake_sources
                .iter()
                .any(|source| match source {
                    WakeSource::ExternalIngress {
                        external_trigger_id: expected,
                    } => expected
                        .as_ref()
                        .is_none_or(|expected| external_trigger_id == Some(expected)),
                    _ => false,
                })
                .then(|| matched("external_ingress", external_trigger_id.map(String::as_str)))
        }
        (MessageKind::TimerTick, MessageOrigin::Timer { timer_id }) => {
            (condition.subject_ref.as_deref() == Some(timer_id.as_str())
                && condition
                    .wake_sources
                    .iter()
                    .any(|source| matches!(source, WakeSource::Timer { .. })))
            .then(|| matched("timer", Some(timer_id.as_str())))
        }
        (MessageKind::OperatorPrompt, MessageOrigin::Operator { actor_id, .. }) => condition
            .wake_sources
            .iter()
            .any(|source| matches!(source, WakeSource::OperatorInput))
            .then(|| matched("operator_input", actor_id.as_deref())),
        (MessageKind::SystemTick, MessageOrigin::System { subsystem }) => {
            if subsystem == "wait_condition_recheck" {
                return exact_wait_recheck_source(message, condition)
                    .then(|| matched("wait_recheck", Some(condition.id.as_str())));
            }
            if let Some(external) = matching_wake_hint_external_source(message, condition) {
                return Some(external);
            }
            condition
                .wake_sources
                .iter()
                .any(|source| matches!(source, WakeSource::SystemTick))
                .then(|| matched("system_tick", Some(subsystem.as_str())))
        }
        _ => None,
    }
}

fn exact_wait_recheck_source(message: &MessageEnvelope, condition: &WaitConditionRecord) -> bool {
    if message.authority_class != AuthorityClass::RuntimeInstruction
        || message.admission_context != Some(AdmissionContext::RuntimeOwned)
        || message.delivery_surface != Some(MessageDeliverySurface::RuntimeSystem)
        || message.work_item_id != condition.work_item_id
        || message.source_refs.get("wait_id") != Some(&condition.id)
    {
        return false;
    }
    let Some(recheck_at) = condition.recheck_at() else {
        return false;
    };
    let recheck = message
        .metadata
        .as_ref()
        .and_then(|value| value.get("wait_condition_recheck"));
    recheck
        .and_then(|value| value.get("wait_id"))
        .and_then(serde_json::Value::as_str)
        == Some(condition.id.as_str())
        && recheck
            .and_then(|value| value.get("recheck_at"))
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.parse().ok())
            == Some(recheck_at)
}

fn matching_wake_hint_external_source<'a>(
    message: &'a MessageEnvelope,
    condition: &WaitConditionRecord,
) -> Option<WakeMatch<'a>> {
    let wake_hint = message.metadata.as_ref()?.get("wake_hint")?;
    let external_trigger_id = wake_hint
        .get("external_trigger_id")
        .and_then(serde_json::Value::as_str);
    let correlated_wait_id = message.source_refs.get("wait_id");
    condition
        .wake_sources
        .iter()
        .any(|source| match source {
            WakeSource::ExternalIngress {
                external_trigger_id: Some(id),
            } => {
                Some(id.as_str()) == external_trigger_id
                    && correlated_wait_id == Some(&condition.id)
            }
            _ => false,
        })
        .then(|| WakeMatch {
            source: "external_ingress",
            subject_ref: external_trigger_id,
        })
}

/// Correlation used when deriving a scheduler activation candidate.
pub fn scheduler_wait_correlation(message: &MessageEnvelope) -> Option<&str> {
    let trusted = matches!(
        (message.delivery_surface, message.admission_context),
        (
            Some(MessageDeliverySurface::RuntimeSystem),
            Some(AdmissionContext::RuntimeOwned)
        ) | (
            Some(MessageDeliverySurface::TaskRejoin),
            Some(AdmissionContext::RuntimeOwned)
        ) | (
            Some(MessageDeliverySurface::HttpCallbackWake),
            Some(AdmissionContext::ExternalTriggerCapability)
        )
    ) && matches!(
        message.authority_class,
        AuthorityClass::RuntimeInstruction | AuthorityClass::IntegrationSignal
    );
    if !trusted {
        return None;
    }
    message.source_refs.get("wait_id").map(String::as_str)
}

/// Match a scheduler candidate; the caller checks whether the wait is current
/// and whether canonical execution admission allows the activation.
pub fn message_matches_wait_condition(
    message: &MessageEnvelope,
    condition: &WaitConditionRecord,
) -> bool {
    if is_agent_message(message) {
        return matching_wake_source(message, condition).is_some();
    }
    if matches!(
        (&message.kind, &message.origin),
        (
            MessageKind::SystemTick,
            MessageOrigin::System { subsystem }
        ) if subsystem == "wait_condition_recheck"
    ) && message.authority_class == AuthorityClass::RuntimeInstruction
        && message.delivery_surface == Some(MessageDeliverySurface::RuntimeSystem)
        && message.admission_context == Some(AdmissionContext::RuntimeOwned)
        && condition.work_item_id == message.work_item_id
        && message.source_refs.get("wait_id") == Some(&condition.id)
    {
        return true;
    }
    match (&message.kind, &message.origin) {
        (MessageKind::TaskResult, MessageOrigin::Task { task_id }) => {
            condition.wake_sources.iter().any(
                |source| matches!(source, WakeSource::TaskResult { task_id: id } if id == task_id),
            )
        }
        (MessageKind::OperatorPrompt, MessageOrigin::Operator { .. }) => condition
            .wake_sources
            .iter()
            .any(|source| matches!(source, WakeSource::OperatorInput)),
        (MessageKind::CallbackEvent | MessageKind::WebhookEvent | MessageKind::ChannelEvent, _) => {
            let external_trigger_id = message.source_refs.get("external_trigger_id");
            condition.wake_sources.iter().any(|source| {
                matches!(
                    source,
                    WakeSource::ExternalIngress {
                        external_trigger_id: expected,
                    } if expected.as_ref().is_none_or(|expected| {
                        external_trigger_id.is_some_and(|actual| actual == expected)
                    })
                )
            })
        }
        (MessageKind::TimerTick, MessageOrigin::Timer { timer_id }) => {
            condition
                .subject_ref
                .as_deref()
                .is_none_or(|subject_ref| subject_ref == timer_id)
                && condition.wake_sources.iter().any(|source| {
                    matches!(source, WakeSource::Timer { .. })
                        && message
                            .source_refs
                            .get("timer_id")
                            .is_none_or(|source_timer_id| source_timer_id == timer_id)
                })
        }
        (MessageKind::SystemTick, MessageOrigin::System { subsystem }) => {
            if subsystem == "work_queue" {
                return false;
            }
            if let Some(external_trigger_id) = message.source_refs.get("external_trigger_id") {
                return condition.wake_sources.iter().any(|source| {
                    matches!(
                        source,
                        WakeSource::ExternalIngress {
                            external_trigger_id: expected,
                        } if expected.as_ref().is_none_or(|expected| {
                            expected == external_trigger_id
                        })
                    )
                });
            }
            condition
                .wake_sources
                .iter()
                .any(|source| matches!(source, WakeSource::SystemTick))
        }
        _ => false,
    }
}
