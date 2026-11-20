use crate::diagnostics::resource_io::admit_cloned_value;
use crate::types::{
    AdmissionContext, AuthorityClass, MessageDeliverySurface, MessageEnvelope, MessageKind,
    MessageOrigin, WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource,
};
use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WakeMatch<'a> {
    pub source: &'static str,
    pub subject_ref: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WaitTriggerSelection<'a> {
    Match {
        condition: &'a WaitConditionRecord,
        wake: WakeMatch<'a>,
    },
    StaleExact {
        wait_id: String,
    },
    NoMatch,
}

pub(crate) fn is_agent_message(message: &MessageEnvelope) -> bool {
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

pub(crate) use holon_domain::agent_message_reply_reference;

pub(crate) fn has_operator_queue_authority(message: &MessageEnvelope) -> bool {
    // A pre-derivation agent envelope can retain its caller's operator label.
    // Its recorded priority still applies, but its label cannot move it ahead.
    message.authority_class == AuthorityClass::OperatorInstruction && !is_agent_message(message)
}

pub(crate) fn select_wait_to_trigger<'a>(
    message: &'a MessageEnvelope,
    conditions: &'a [WaitConditionRecord],
) -> Result<WaitTriggerSelection<'a>> {
    let mut candidates = wait_trigger_candidates(message, conditions);
    if let Some(wait_id) = authoritative_wait_correlation(message) {
        return Ok(
            if let Some((condition, wake)) =
                candidates.find(|(condition, _)| condition.id == wait_id)
            {
                WaitTriggerSelection::Match { condition, wake }
            } else {
                admit_cloned_value(&wait_id)?;
                WaitTriggerSelection::StaleExact {
                    wait_id: wait_id.to_string(),
                }
            },
        );
    }
    let Some(first) = candidates.next() else {
        return Ok(WaitTriggerSelection::NoMatch);
    };
    let Some(second) = candidates.next() else {
        return Ok(WaitTriggerSelection::Match {
            condition: first.0,
            wake: first.1,
        });
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
    Ok(WaitTriggerSelection::Match { condition, wake })
}

/// Deterministic single-waiter order for a wake that matches several waits
/// (#3914). It mirrors the scheduler's canonical selection so the planned
/// owner and the triggered wait row agree.
pub(crate) fn trigger_wait_order(
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

pub(crate) fn authoritative_wait_correlation(message: &MessageEnvelope) -> Option<&str> {
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

pub(crate) fn matching_wake_source<'a>(
    message: &'a MessageEnvelope,
    condition: &'a WaitConditionRecord,
) -> Option<WakeMatch<'a>> {
    // A task result message is addressed to the task's owner, but any
    // WorkItem (or agent lifecycle) inside the same agent may hold the
    // dependency wait (#3124): match task waits by task identity instead of
    // the message's WorkItem binding.
    let exact_reply = is_agent_message(message)
        && condition.kind == crate::types::WaitConditionKind::Task
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
            if exact_wait_recheck_source(message, condition, subsystem) {
                return Some(matched("wait_recheck", Some(condition.id.as_str())));
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

fn exact_wait_recheck_source(
    message: &MessageEnvelope,
    condition: &WaitConditionRecord,
    subsystem: &str,
) -> bool {
    if subsystem != "wait_condition_recheck"
        || message.authority_class != AuthorityClass::RuntimeInstruction
        || message.admission_context != Some(AdmissionContext::RuntimeOwned)
        || message.delivery_surface != Some(MessageDeliverySurface::RuntimeSystem)
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

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MessageBody, Priority, WaitConditionKind};
    use chrono::Utc;

    mod selector_budget_tests;

    fn task_wait(id: &str, work_item_id: Option<&str>, task_id: &str) -> WaitConditionRecord {
        let now = Utc::now();
        WaitConditionRecord {
            id: id.into(),
            agent_id: "agent-a".into(),
            work_item_id: work_item_id.map(ToString::to_string),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Task,
            source: None,
            subject_ref: Some(task_id.into()),
            waiting_for: format!("waiting for {task_id}"),
            wake_sources: vec![WakeSource::TaskResult {
                task_id: task_id.into(),
            }],
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: Some("turn-wait".into()),
            trigger_message_id: None,
            triggered_at: None,
        }
    }

    fn task_result(task_id: &str, work_item_id: Option<&str>) -> MessageEnvelope {
        let mut message = MessageEnvelope::new(
            "agent-a",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: task_id.into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "completed".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        message.work_item_id = work_item_id.map(ToString::to_string);
        message.turn_id = Some("turn-trigger".into());
        message
    }

    #[test]
    fn selection_matches_cross_owner_task_wait() {
        // #3124: a task result message is addressed to the task's owner,
        // but any WorkItem in the same agent may hold the dependency wait.
        let message = task_result("task-a", Some("work-b"));
        let wait = task_wait("wait-a", Some("work-a"), "task-a");
        match select_wait_to_trigger(&message, &[wait]).unwrap() {
            WaitTriggerSelection::Match { condition, wake } => {
                assert_eq!(condition.id, "wait-a");
                assert_eq!(wake.source, "task_result");
            }
            other => panic!("expected cross-owner task match, got {other:?}"),
        }
    }

    #[test]
    fn selection_picks_owner_wait_deterministically_among_task_waiters() {
        // One result message admits one consumed trigger marker per agent;
        // the task owner's own wait wins over older foreign waiters.
        let mut message = task_result("task-a", Some("work-b"));
        message.task_id = Some("task-a".into());
        let owner_wait = task_wait("wait-owner", Some("work-b"), "task-a");
        let mut other_wait = task_wait("wait-other", Some("work-a"), "task-a");
        other_wait.created_at -= chrono::Duration::milliseconds(5);
        match select_wait_to_trigger(&message, &[other_wait, owner_wait]).unwrap() {
            WaitTriggerSelection::Match { condition, .. } => {
                assert_eq!(condition.id, "wait-owner");
            }
            other => panic!("expected deterministic task wait pick, got {other:?}"),
        }
    }

    #[test]
    fn selection_picks_one_wait_deterministically_instead_of_quarantining() {
        // #3914: a wake matching several waits is not ambiguous. The runtime
        // picks one waiter: an exact task waiter, then WorkItem scope over
        // agent lifecycle, then registration order, then the stable id.
        let message = task_result("task-a", Some("work-a"));
        let mut lifecycle = task_wait("wait-0-lifecycle", None, "task-a");
        lifecycle.created_at -= chrono::Duration::days(1);
        let same_time = task_wait("wait-a", Some("work-a"), "task-a");
        let mut later = task_wait("wait-b", Some("work-a"), "task-a");
        later.created_at = same_time.created_at + chrono::Duration::seconds(1);
        let selected = |waits: &[WaitConditionRecord]| match select_wait_to_trigger(&message, waits)
            .unwrap()
        {
            WaitTriggerSelection::Match { condition, .. } => condition.id.clone(),
            other => panic!("expected a single deterministic match, got {other:?}"),
        };
        // WorkItem scope wins over an older agent-lifecycle wait.
        assert_eq!(
            selected(&[lifecycle, same_time.clone(), later.clone()]),
            "wait-a"
        );
        // Equal registration time falls back to the stable id.
        let mut same_time_late_id = task_wait("wait-c", Some("work-a"), "task-a");
        same_time_late_id.created_at = same_time.created_at;
        assert_eq!(selected(&[same_time.clone(), same_time_late_id]), "wait-a");
        // Registration order decides otherwise.
        assert_eq!(selected(&[later, same_time]), "wait-a");
    }

    #[test]
    fn exact_correlation_does_not_fall_back_to_another_wait() {
        let mut message = task_result("task-a", Some("work-a"));
        message
            .source_refs
            .insert("wait_id".into(), "wait-old".into());
        assert_eq!(
            select_wait_to_trigger(
                &message,
                &[task_wait("wait-current", Some("work-a"), "task-a")]
            )
            .unwrap(),
            WaitTriggerSelection::StaleExact {
                wait_id: "wait-old".into(),
            }
        );
    }

    #[test]
    fn same_turn_message_cannot_trigger_a_new_wait() {
        let mut message = task_result("task-a", Some("work-a"));
        message.turn_id = Some("turn-wait".into());
        assert_eq!(
            select_wait_to_trigger(&message, &[task_wait("wait-a", Some("work-a"), "task-a")])
                .unwrap(),
            WaitTriggerSelection::NoMatch
        );
    }
}
