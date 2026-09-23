use crate::types::{
    AdmissionContext, AuthorityClass, MessageDeliverySurface, MessageEnvelope, MessageKind,
    MessageOrigin, WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WakeMatch {
    pub source: String,
    pub subject_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WaitTriggerSelection<'a> {
    Match {
        condition: &'a WaitConditionRecord,
        wake: WakeMatch,
    },
    StaleExact {
        wait_id: String,
    },
    NoMatch,
    Ambiguous {
        wait_ids: Vec<String>,
    },
}

pub(crate) fn select_wait_to_trigger<'a>(
    message: &MessageEnvelope,
    conditions: &'a [WaitConditionRecord],
) -> WaitTriggerSelection<'a> {
    let candidates = conditions
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
        .collect::<Vec<_>>();

    if let Some(wait_id) = authoritative_wait_correlation(message) {
        return candidates
            .into_iter()
            .find(|(condition, _)| condition.id == wait_id)
            .map_or_else(
                || WaitTriggerSelection::StaleExact {
                    wait_id: wait_id.to_string(),
                },
                |(condition, wake)| WaitTriggerSelection::Match { condition, wake },
            );
    }

    match candidates.as_slice() {
        [] => WaitTriggerSelection::NoMatch,
        [(condition, wake)] => WaitTriggerSelection::Match {
            condition,
            wake: wake.clone(),
        },
        _ => {
            // One task result message admits only one consumed trigger
            // marker per agent (wait_conditions UNIQUE(agent_id,
            // trigger_message_id)). When several same-agent task waits
            // match the result, the task owner's own wait wins first and
            // then the earliest wait deterministically; other waiters keep
            // their unsatisfied waits as durable evidence (#3124).
            if message.kind == MessageKind::TaskResult
                && candidates.iter().all(|(condition, _)| {
                    condition.kind == WaitConditionKind::Task
                        && condition.wake_sources.iter().any(|source| {
                            matches!(
                                source,
                                WakeSource::TaskResult { task_id }
                                    if Some(task_id.as_str()) == message.task_id.as_deref()
                            )
                        })
                })
            {
                let mut selection = candidates;
                selection.sort_by(|(left, _), (right, _)| {
                    let left_owner =
                        left.work_item_id.as_deref() == message.work_item_id.as_deref();
                    let right_owner =
                        right.work_item_id.as_deref() == message.work_item_id.as_deref();
                    right_owner
                        .cmp(&left_owner)
                        .then_with(|| left.created_at.cmp(&right.created_at))
                        .then_with(|| left.id.cmp(&right.id))
                });
                let (condition, wake) = selection
                    .into_iter()
                    .next()
                    .expect("non-empty candidate slice has a deterministic task wait");
                return WaitTriggerSelection::Match { condition, wake };
            }
            WaitTriggerSelection::Ambiguous {
                wait_ids: candidates
                    .iter()
                    .map(|(condition, _)| condition.id.clone())
                    .collect(),
            }
        }
    }
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

pub(crate) fn matching_wake_source(
    message: &MessageEnvelope,
    condition: &WaitConditionRecord,
) -> Option<WakeMatch> {
    // A task result message is addressed to the task's owner, but any
    // WorkItem (or agent lifecycle) inside the same agent may hold the
    // dependency wait (#3124): match task waits by task identity instead of
    // the message's WorkItem binding.
    let task_result_message = matches!(
        (&message.kind, &message.origin),
        (MessageKind::TaskResult, MessageOrigin::Task { .. })
    );
    if message.agent_id != condition.agent_id
        || (!task_result_message
            && message.work_item_id.as_deref() != condition.work_item_id.as_deref())
        || (condition.status == WaitConditionStatus::Triggered
            && condition.trigger_message_id() != Some(message.id.as_str()))
    {
        return None;
    }
    let matched = |source: &str, subject_ref: Option<String>| WakeMatch {
        source: source.to_string(),
        subject_ref,
    };
    match (&message.kind, &message.origin) {
        (MessageKind::TaskResult, MessageOrigin::Task { task_id }) => condition
            .wake_sources
            .iter()
            .any(|source| matches!(source, WakeSource::TaskResult { task_id: id } if id == task_id))
            .then(|| matched("task_result", Some(task_id.clone()))),
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
                .then(|| matched("external_ingress", external_trigger_id.cloned()))
        }
        (MessageKind::TimerTick, MessageOrigin::Timer { timer_id }) => {
            (condition.subject_ref.as_deref() == Some(timer_id.as_str())
                && condition
                    .wake_sources
                    .iter()
                    .any(|source| matches!(source, WakeSource::Timer { .. })))
            .then(|| matched("timer", Some(timer_id.clone())))
        }
        (MessageKind::OperatorPrompt, MessageOrigin::Operator { actor_id, .. }) => condition
            .wake_sources
            .iter()
            .any(|source| matches!(source, WakeSource::OperatorInput))
            .then(|| matched("operator_input", actor_id.clone())),
        (MessageKind::SystemTick, MessageOrigin::System { subsystem }) => {
            if exact_wait_recheck_source(message, condition, subsystem) {
                return Some(matched("wait_recheck", Some(condition.id.clone())));
            }
            if let Some(external) = matching_wake_hint_external_source(message, condition) {
                return Some(external);
            }
            condition
                .wake_sources
                .iter()
                .any(|source| matches!(source, WakeSource::SystemTick))
                .then(|| matched("system_tick", Some(subsystem.clone())))
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

fn matching_wake_hint_external_source(
    message: &MessageEnvelope,
    condition: &WaitConditionRecord,
) -> Option<WakeMatch> {
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
            source: "external_ingress".to_string(),
            subject_ref: external_trigger_id.map(ToString::to_string),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MessageBody, Priority, WaitConditionKind};
    use chrono::Utc;

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
        match select_wait_to_trigger(&message, &[wait]) {
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
        match select_wait_to_trigger(&message, &[other_wait, owner_wait]) {
            WaitTriggerSelection::Match { condition, .. } => {
                assert_eq!(condition.id, "wait-owner");
            }
            other => panic!("expected deterministic task wait pick, got {other:?}"),
        }
    }

    #[test]
    fn selection_rejects_ambiguous_uncorrelated_waits() {
        let message = task_result("task-a", Some("work-a"));
        assert_eq!(
            select_wait_to_trigger(
                &message,
                &[
                    task_wait("wait-a", Some("work-a"), "task-a"),
                    task_wait("wait-b", Some("work-a"), "task-a"),
                ],
            ),
            WaitTriggerSelection::Ambiguous {
                wait_ids: vec!["wait-a".into(), "wait-b".into()],
            }
        );
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
            ),
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
            select_wait_to_trigger(&message, &[task_wait("wait-a", Some("work-a"), "task-a")]),
            WaitTriggerSelection::NoMatch
        );
    }
}
