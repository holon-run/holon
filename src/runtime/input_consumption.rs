//! Idle-start input snapshots. A batch key never grants execution authority.

use super::message_dispatch::message_text;
use super::*;
use crate::runtime_error::{
    IdleInputRecoveryPredicate as RecoveryPredicate, IdleInputRecoveryRejection,
};
use crate::types::{IdleInputBatchItem, IdleInputBatchRecord};
use rusqlite::OptionalExtension;

pub(crate) const INPUT_BATCH_ITEMS: usize = 8;
pub(crate) const INPUT_BATCH_BYTES: usize = 24 * 1024;
pub(crate) const INPUT_BATCH_SCAN: usize = 64;
const OVERSIZE_PREVIEW_BYTES: usize = 4096;
pub(crate) const BATCH_HEADER: &str =
    "Current input batch (receive order; each item retains its own authority and reply route):\n";

pub(super) fn validate_idle_recovery_predecessor(
    anchor: &MessageEnvelope,
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
    prior: Option<&crate::domain::execution_protocol::ExecutionAttempt>,
    batch: &IdleInputBatchRecord,
) -> std::result::Result<(), IdleInputRecoveryRejection> {
    let missing = || {
        IdleInputRecoveryRejection::new(
            RecoveryPredicate::PredecessorMissing,
            if attempt.recovery_of_attempt_id.is_some() {
                "predecessor"
            } else {
                "attempt.recovery_of_attempt_id"
            },
        )
    };
    let prior = prior.ok_or_else(missing)?;
    let failure =
        if prior.state != crate::domain::execution_protocol::ExecutionAttemptState::Interrupted {
            Some((
                RecoveryPredicate::PredecessorNotInterrupted,
                "predecessor.state",
            ))
        } else if prior.agent_id != anchor.agent_id {
            Some((
                RecoveryPredicate::PredecessorAgentMismatch,
                "predecessor.agent",
            ))
        } else if prior.source_message_id != attempt.source_message_id {
            Some((
                RecoveryPredicate::PredecessorSourceMismatch,
                "predecessor.source_message",
            ))
        } else if prior.binding != attempt.binding {
            Some((
                RecoveryPredicate::PredecessorBindingMismatch,
                "predecessor.binding",
            ))
        } else if batch.activation_id.as_deref() != Some(prior.attempt_id.as_str()) {
            Some((RecoveryPredicate::ActivationMismatch, "batch.activation_id"))
        } else {
            None
        };
    match failure {
        Some((predicate, field)) => Err(IdleInputRecoveryRejection::new(predicate, field)),
        None => Ok(()),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct SenderScope {
    pub(super) sender: String,
    pub(super) principal_kind: crate::types::AgentMessagePrincipalKind,
    pub(super) origin: MessageOrigin,
    pub(super) authority: AuthorityClass,
    pub(super) surface: Option<crate::types::MessageDeliverySurface>,
    pub(super) admission: Option<crate::types::AdmissionContext>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum InputScope {
    ReceiverOwner(String),
    AgentLifecycle(String),
    Sender(SenderScope),
}

fn mixed_result_capacity(
    admitted_results: usize,
    ordinary_anchor: bool,
    candidates: impl IntoIterator<Item = bool>,
) -> usize {
    let candidates: Vec<_> = candidates.into_iter().collect();
    let mut results = candidates
        .iter()
        .filter(|result| **result)
        .count()
        .min(2usize.saturating_sub(admitted_results));
    let ordinary = candidates
        .iter()
        .filter(|result| !**result)
        .count()
        .min(2 - usize::from(ordinary_anchor));
    let mut remaining = INPUT_BATCH_ITEMS
        .saturating_sub(admitted_results + usize::from(ordinary_anchor) + results + ordinary);
    let mut reserved_results = results;
    let mut reserved_ordinary = ordinary;
    for result in candidates {
        let reserved = if result {
            &mut reserved_results
        } else {
            &mut reserved_ordinary
        };
        if *reserved > 0 {
            *reserved -= 1;
            continue;
        }
        if remaining == 0 {
            break;
        }
        results += usize::from(result);
        remaining -= 1;
    }
    admitted_results + results
}

pub(super) fn item_evidence(
    message: &MessageEnvelope,
    limit: usize,
) -> Result<Option<IdleInputBatchItem>> {
    let source_ref = format!("message:{}", message.id);
    let provenance = serde_json::to_string(&serde_json::json!({
        "id": message.id,
        "created_at": message.created_at,
        "origin": message.origin,
        "authority_class": message.authority_class,
        "priority": message.priority,
        "delivery_surface": message.delivery_surface,
        "admission_context": message.admission_context,
        "routing_context": message.routing_context,
        "source_refs": message.source_refs,
        "correlation_id": message.correlation_id,
        "causation_id": message.causation_id,
    }))?;
    let mut prefix = format!(
        "- reply_context_id: {}\n  source_ref: {source_ref}\n  provenance: {provenance}\n  body:\n",
        message.id
    );
    let body = message_text(&message.body);
    let rendered = format!("{prefix}{body}\n");
    if rendered.len() <= limit {
        return Ok(Some(IdleInputBatchItem {
            message_id: message.id.clone(),
            source_ref,
            rendered,
        }));
    }
    let suffix = format!("\n[bounded preview; retrieve original with MemoryGet({source_ref})]\n");
    if prefix.len() + suffix.len() > limit {
        // Oversized metadata shares the body budget; the source retains it all.
        let compact = serde_json::to_string(&serde_json::json!({
            "authority_class": message.authority_class,
            "delivery_surface": message.delivery_surface,
            "admission_context": message.admission_context,
            "full_metadata_source_ref": source_ref,
        }))?;
        prefix = format!(
            "- reply_context_id: {}\n  source_ref: {source_ref}\n  provenance: {compact}\n  body:\n",
            message.id
        );
    }
    let Some(body_budget) = limit.checked_sub(prefix.len() + suffix.len()) else {
        return Ok(None);
    };
    let mut end = body.len().min(body_budget);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    Ok(Some(IdleInputBatchItem {
        message_id: message.id.clone(),
        source_ref,
        rendered: format!("{prefix}{}{suffix}", &body[..end]),
    }))
}

pub(crate) fn render_batch(batch: &IdleInputBatchRecord) -> String {
    let mut text = String::from(BATCH_HEADER);
    for item in &batch.items {
        text.push_str(&item.rendered);
    }
    text
}

pub(crate) fn render_recovered_operator_inputs(
    storage: &AppStorage,
    turn: &TurnRecord,
    seen_messages: &mut std::collections::BTreeSet<String>,
) -> Result<Option<String>> {
    if turn.replay.is_none() {
        return Ok(None);
    }
    render_admitted_operator_inputs(storage, turn, seen_messages)
}

pub(crate) fn render_admitted_operator_inputs(
    storage: &AppStorage,
    turn: &TurnRecord,
    seen_messages: &mut std::collections::BTreeSet<String>,
) -> Result<Option<String>> {
    let batched: std::collections::BTreeSet<_> = turn
        .idle_input_batch
        .iter()
        .flat_map(|batch| &batch.items)
        .chain(
            turn.live_input_batches
                .iter()
                .flat_map(|batch| &batch.items),
        )
        .map(|item| &item.message_id)
        .collect();
    let mut parts = Vec::new();
    for id in &turn.input_message_ids {
        if batched.contains(id)
            || seen_messages.contains(id)
            || turn
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_ref())
                == Some(id)
        {
            continue;
        }
        let Some(message) = storage.read_message_by_id(id)? else {
            continue;
        };
        if message.agent_id != turn.agent_id
            || !scheduler::is_operator_interjection_message(&message)
        {
            continue;
        }
        // Only the recovery transition may restore these independent input IDs.
        // Keep their complete envelope provenance without inventing a live batch.
        let mut provenance = serde_json::to_value(&message)?;
        provenance.as_object_mut().unwrap().remove("body");
        parts.push(format!(
            "Recovered operator interjection (original admission and provenance):\nsource_ref: message:{}\nprovenance: {}\nbody:\n{}\n",
            message.id,
            serde_json::to_string(&provenance)?,
            message_text(&message.body),
        ));
        seen_messages.insert(id.clone());
    }
    Ok((!parts.is_empty()).then(|| parts.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p1_mixed_slots_reserve_two_then_fill_in_receive_order() {
        assert_eq!(
            mixed_result_capacity(0, true, [false; 5].into_iter().chain([true; 8])),
            2
        );
        assert_eq!(
            mixed_result_capacity(1, false, [true; 7].into_iter().chain([false; 6])),
            6
        );
        assert_eq!(mixed_result_capacity(1, false, [true; 7]), 8);
        assert_eq!(mixed_result_capacity(0, true, [false; 7]), 0);
    }

    #[test]
    fn p1_oversized_metadata_uses_the_same_bounded_source_preview() {
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "agent_message".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "更正".repeat(4096),
            },
        );
        message
            .source_refs
            .insert("large_metadata".into(), "x".repeat(INPUT_BATCH_BYTES * 2));
        let item = item_evidence(&message, OVERSIZE_PREVIEW_BYTES)
            .unwrap()
            .unwrap();
        assert!(item.rendered.len() <= OVERSIZE_PREVIEW_BYTES);
        assert!(item.rendered.contains("full_metadata_source_ref"));
        assert!(item.rendered.contains("runtime_instruction"));
        assert!(item
            .rendered
            .contains(&format!("MemoryGet(message:{})", message.id)));
        assert!(!item.rendered.contains("large_metadata"));
    }
}

impl RuntimeHandle {
    pub(super) fn idle_batch_source_turn(
        &self,
        agent_id: &str,
        message_id: &str,
    ) -> Result<Option<TurnRecord>> {
        let connection = self.inner.runtime_db.connection()?;
        let payload: Option<String> = connection
            .query_row(
                "SELECT payload_json FROM turn_records
                 WHERE agent_id = ?1 AND trigger_message_id = ?2
                   AND json_extract(payload_json, '$.idle_input_batch.anchor_message_id') = ?2
                 ORDER BY turn_index DESC LIMIT 1",
                rusqlite::params![agent_id, message_id],
                |row| row.get(0),
            )
            .optional()?;
        payload
            .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
            .transpose()
    }

    pub(super) fn idle_batch_recovery_message(
        &self,
        queue: &crate::queue::RuntimeQueue,
    ) -> Result<Option<String>> {
        let mut result_anchor: Option<String> = None;
        for message in queue.input_snapshot(INPUT_BATCH_SCAN) {
            if message.priority == Priority::Interject
                || message.authority_class == AuthorityClass::OperatorInstruction
            {
                break;
            }
            if message.delivery_surface == Some(MessageDeliverySurface::TaskRejoin) {
                let Some(anchor) = self
                    .inner
                    .runtime_db
                    .task_result_settlements()
                    .interrupted_admitted_result_anchor(
                        &message.agent_id,
                        &message.id,
                        self.now(),
                    )?
                else {
                    break;
                };
                if result_anchor.as_ref().is_some_and(|prior| prior != &anchor) {
                    break;
                }
                if anchor == message.id {
                    return Ok(Some(anchor));
                }
                result_anchor = Some(anchor);
                continue;
            }
            if self
                .inner
                .runtime_db
                .queue_entries()
                .latest(&message.id)?
                .is_some_and(|entry| entry.status == QueueEntryStatus::Interrupted)
                && self
                    .idle_batch_source_turn(&message.agent_id, &message.id)?
                    .is_some()
            {
                if result_anchor
                    .as_ref()
                    .is_some_and(|anchor| anchor != &message.id)
                {
                    break;
                }
                return Ok(Some(message.id));
            }
        }
        Ok(None)
    }

    /// Receiver bindings precede sender fallback; invalid bindings never fall back.
    pub(super) fn idle_input_scope(&self, message: &MessageEnvelope) -> Result<Option<InputScope>> {
        if message.kind == MessageKind::TaskResult {
            let Some(record) = self
                .inner
                .runtime_db
                .task_result_settlements()
                .latest_for_message(&message.id)?
            else {
                return Ok(None);
            };
            if record.agent_id != message.agent_id
                || record.work_item_id != message.work_item_id
                || message.task_id.as_deref() != Some(record.task_id.as_str())
            {
                return Ok(None);
            }
            return match message.work_item_id.as_deref() {
                Some(owner_id) => self.valid_receiver_scope(&message.agent_id, owner_id),
                None => Ok(Some(InputScope::AgentLifecycle(message.agent_id.clone()))),
            };
        }
        if !crate::wake_contract::is_agent_message(message) || message.task_id.is_some() {
            return Ok(None);
        }
        // Reconciliation may still be publishing an accepted reply. Do not
        // mistake that window for an unassociated ordinary message. Read active
        // and completed relations together so the terminal transition cannot
        // fall between two separate lookups.
        let has_reply_relation: bool = self.inner.runtime_db.connection()?.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM tasks WHERE owner_agent_id = ?1 AND kind = 'agent_message_wait'
                  AND (status IN ('queued', 'running', 'cancelling')
                    OR (status = 'completed'
                      AND json_extract(payload_json, '$.detail.message_id') = ?2))
            )",
            rusqlite::params![message.agent_id, message.id],
            |row| row.get(0),
        )?;
        let Some(delivery) = self
            .inner
            .runtime_db
            .agent_message_deliveries()
            .latest_for_message(&message.id)?
        else {
            return Ok(None);
        };
        if delivery.target_agent_id != message.agent_id
            || delivery.message_id.as_deref() != Some(message.id.as_str())
            || delivery.routing_context != message.routing_context
            || delivery.caller.authority_class != message.authority_class
            || delivery.caller.origin != message.origin
            || Some(delivery.caller.delivery_surface) != message.delivery_surface
            || Some(delivery.caller.admission_context) != message.admission_context
            || delivery.outcome != crate::types::AgentMessageDeliveryOutcome::Accepted
            || !matches!(
                delivery.state,
                crate::types::AgentMessageDeliveryState::Queued
                    | crate::types::AgentMessageDeliveryState::Dispatched
            )
            || !matches!(
                delivery.caller.principal_kind,
                crate::types::AgentMessagePrincipalKind::PeerAgent
                    | crate::types::AgentMessagePrincipalKind::SupervisingParent
            )
        {
            return Ok(None);
        }
        let Some(sender) = delivery.caller.caller_agent_id.filter(|id| !id.is_empty()) else {
            return Ok(None);
        };
        if message
            .routing_context
            .as_ref()
            .and_then(|route| route.sender_agent_id.as_ref())
            != Some(&sender)
        {
            return Ok(None);
        }
        if let Some(owner_id) = message.work_item_id.as_deref() {
            return self.valid_receiver_scope(&message.agent_id, owner_id);
        }
        if has_reply_relation {
            return Ok(None);
        }
        Ok(Some(InputScope::Sender(SenderScope {
            sender,
            principal_kind: delivery.caller.principal_kind,
            origin: message.origin.clone(),
            authority: message.authority_class,
            surface: message.delivery_surface,
            admission: message.admission_context,
        })))
    }

    fn valid_receiver_scope(&self, agent_id: &str, owner_id: &str) -> Result<Option<InputScope>> {
        Ok(self
            .inner
            .runtime_db
            .work_items()
            .latest(owner_id)?
            .filter(|owner| owner.agent_id == agent_id && owner.state == WorkItemState::Open)
            .map(|_| InputScope::ReceiverOwner(owner_id.to_owned())))
    }

    pub(super) fn idle_reply_recovery_source_valid(
        &self,
        anchor: &MessageEnvelope,
        binding: &crate::types::WorkItemExecutionBinding,
    ) -> Result<bool> {
        let work_item_id = binding.work_item_id.as_deref();
        let owner = match work_item_id {
            Some(work_item_id) => crate::types::TurnOwner::WorkItem {
                work_item_id: work_item_id.to_owned(),
            },
            None => crate::types::TurnOwner::AgentLifecycle {
                agent_id: anchor.agent_id.clone(),
            },
        };
        if !scheduler::runtime_owned_internal_followup(anchor)
            || anchor.authority_class != AuthorityClass::RuntimeInstruction
            || anchor.work_item_id.as_deref() != work_item_id
            || binding.source_message_id != anchor.id
            || binding.owner.as_ref() != Some(&owner)
            || binding
                .activation_id
                .as_deref()
                .is_none_or(|activation_id| {
                    !matches!(
                        binding.admission_provenance.as_ref(),
                        Some(ExecutionAdmissionProvenance::Canonical {
                            activation_id: provenance_id, ..
                        }) if provenance_id == activation_id
                    )
                })
            || !matches!(
                &anchor.origin,
                MessageOrigin::System { subsystem } if subsystem == "task_result_recovery"
            )
        {
            return Ok(false);
        }
        let Some(source) = self.inner.storage.read_message_by_id(&anchor.id)? else {
            return Ok(false);
        };
        // Replay adds its own refs to the claimed envelope. Recovery identity
        // and provenance must still come from the durable source, not the hint.
        if source.agent_id != anchor.agent_id
            || source.work_item_id != anchor.work_item_id
            || source.created_at != anchor.created_at
            || source.kind != anchor.kind
            || source.task_id != anchor.task_id
            || source.priority != anchor.priority
            || source.origin != anchor.origin
            || source.authority_class != anchor.authority_class
            || source.delivery_surface != anchor.delivery_surface
            || source.admission_context != anchor.admission_context
            || [
                "task_result_message_id",
                "task_result_identity",
                "task_result_owner_key",
                "task_result_rejoin_generation",
                "task_result_parent_turn_id",
                "wait_id",
            ]
            .iter()
            .any(|key| source.source_refs.get(*key) != anchor.source_refs.get(*key))
            || self
                .inner
                .runtime_db
                .queue_entries()
                .latest(&anchor.id)?
                .is_none_or(|entry| {
                    entry.agent_id != anchor.agent_id || entry.status != QueueEntryStatus::Dequeued
                })
        {
            return Ok(false);
        }
        let Some(message_id) = anchor.source_refs.get("task_result_message_id") else {
            return Ok(false);
        };
        let repository = self.inner.runtime_db.task_result_settlements();
        let Some(record) = repository.latest_for_message(message_id)? else {
            return Ok(false);
        };
        let mut consuming_record = record.clone();
        consuming_record.work_item_id = work_item_id.map(str::to_owned);
        if record.agent_id != anchor.agent_id
            || (record.state
                == crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled
                && record.disposition
                    != Some(
                        crate::runtime_db::task_result_settlement::TaskResultSettlementDisposition::ModelDelivered,
                    ))
            || anchor.source_refs.get("task_result_identity") != Some(&record.result_identity)
            || anchor.source_refs.get("task_result_owner_key")
                != Some(&task_result_recovery::recovery_owner_key(&consuming_record))
            || anchor.source_refs.get("task_result_rejoin_generation")
                != Some(&record.rejoin_generation.to_string())
            || anchor.source_refs.get("task_result_parent_turn_id") != Some(&record.parent_turn_id)
        {
            return Ok(false);
        }
        let exact_waiter = repository
            .exact_waiter_recovery_candidate(
                &anchor.agent_id,
                work_item_id,
                binding
                    .activation_id
                    .as_deref()
                    .expect("canonical activation checked above"),
            )?
            .is_some_and(|candidate| candidate.result_identity == record.result_identity);
        if record.work_item_id.as_deref() != work_item_id && !exact_waiter {
            return Ok(false);
        }
        // The persisted exact wait can change the consuming owner, never the
        // task's captured owner or the original result's durable source fence.
        if repository.non_reply_source_matches_owner(&record)? {
            return Ok(true);
        }
        if record.state
            == crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled
        {
            repository.reply_source_is_consumed_for_owner(&record)
        } else {
            Ok(repository.reply_source_is_recoverable(&record)?
                || (exact_waiter && repository.reply_source_is_consumed(&record)?))
        }
    }

    /// Budget a fixed candidate set before the source ledger admits results.
    pub(super) async fn admit_idle_task_results(
        &self,
        anchor: &MessageEnvelope,
        available_bytes: usize,
    ) -> Result<bool> {
        let guard = self.inner.agent.lock().await;
        let state = &guard.state;
        let Some(binding) = state.current_execution_binding.as_ref() else {
            return Ok(false);
        };
        let Some(activation_id) = binding.activation_id.as_deref() else {
            return Ok(false);
        };
        if state.status != AgentStatus::AwakeRunning
            || state.id != anchor.agent_id
            || binding.source_message_id != anchor.id
            || state.current_turn_id.as_deref() != Some(binding.turn_id.as_str())
            || !matches!(
                binding.admission_provenance.as_ref(),
                Some(ExecutionAdmissionProvenance::Canonical {
                    activation_id: provenance_id, ..
                }) if provenance_id == activation_id
            )
            || guard
                .current_run_abort
                .as_ref()
                .is_some_and(|abort| abort.token.is_cancelled())
        {
            return Ok(false);
        }
        let Some(turn) = self.inner.storage.read_turn_by_id(&binding.turn_id)? else {
            return Ok(false);
        };
        let repository = self.inner.runtime_db.task_result_settlements();
        let anchor_replies = if crate::wake_contract::is_agent_message(anchor) {
            repository.reply_message_candidates(
                &anchor.agent_id,
                binding.work_item_id.as_deref(),
                &anchor.id,
                turn.created_at,
                INPUT_BATCH_SCAN,
            )?
        } else {
            Vec::new()
        };
        let is_reply_anchor = !anchor_replies.is_empty();
        let anchor_reply_order: std::collections::BTreeMap<_, _> = anchor_replies
            .iter()
            .enumerate()
            .map(|(index, record)| (record.result_identity.clone(), index))
            .collect();
        let anchor_result_messages: std::collections::BTreeSet<_> = anchor_replies
            .iter()
            .map(|record| record.message_id.clone())
            .collect();
        let result_recovery = self.idle_reply_recovery_source_valid(anchor, binding)?;
        let scope = if is_reply_anchor || result_recovery {
            match binding.work_item_id.as_deref() {
                Some(owner) => self.valid_receiver_scope(&anchor.agent_id, owner)?,
                None => Some(InputScope::AgentLifecycle(anchor.agent_id.clone())),
            }
        } else {
            self.idle_input_scope(anchor)?
        };
        let Some(scope) = scope else {
            return Ok(false);
        };
        match &scope {
            InputScope::ReceiverOwner(owner)
                if binding.work_item_id.as_deref() == Some(owner.as_str()) => {}
            InputScope::AgentLifecycle(agent)
                if agent == &anchor.agent_id && binding.work_item_id.is_none() => {}
            _ => return Ok(false),
        }
        let Some(execution) = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&anchor.agent_id)?
        else {
            return Ok(false);
        };
        let Some(attempt) = execution.attempts.get(activation_id).filter(|attempt| {
            attempt.state == crate::domain::execution_protocol::ExecutionAttemptState::Open
                && attempt.agent_id == anchor.agent_id
                && attempt.source_message_id.as_deref() == Some(anchor.id.as_str())
        }) else {
            return Ok(false);
        };
        let canonical_owner = match &attempt.binding {
            crate::domain::execution_protocol::ExecutionBinding::WorkItem { work_item_id } => {
                crate::types::TurnOwner::WorkItem {
                    work_item_id: work_item_id.clone(),
                }
            }
            crate::domain::execution_protocol::ExecutionBinding::AgentLifecycle { agent_id } => {
                crate::types::TurnOwner::AgentLifecycle {
                    agent_id: agent_id.clone(),
                }
            }
            _ => return Ok(false),
        };
        if turn.agent_id != anchor.agent_id
            || canonical_owner != turn.effective_owner()
            || binding.owner.as_ref() != Some(&turn.effective_owner())
            || turn.terminal.is_some()
            || !turn.input_message_ids.contains(&anchor.id)
            || turn
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                != Some(anchor.id.as_str())
        {
            return Ok(false);
        }
        let mut admitted = repository.admitted_for_activation(&anchor.agent_id, activation_id)?;
        let cutoff = turn.created_at;
        // The frozen input cutoff is not the recovery budget's clock.
        let admission_at = self.now();
        let snapshot = guard.queue.input_snapshot(INPUT_BATCH_SCAN);
        let mut candidates = if result_recovery {
            repository.recovery_snapshot_candidates_for_owner(
                &anchor.agent_id,
                binding.work_item_id.as_deref(),
                activation_id,
                admission_at,
                INPUT_BATCH_SCAN,
            )?
        } else {
            repository.unsettled_for_owner(
                &anchor.agent_id,
                binding.work_item_id.as_deref(),
                INPUT_BATCH_SCAN,
            )?
        };
        let exact_waiter = if result_recovery {
            repository.exact_waiter_recovery_candidate(
                &anchor.agent_id,
                binding.work_item_id.as_deref(),
                activation_id,
            )?
        } else {
            None
        };
        if let Some(record) = exact_waiter.as_ref() {
            candidates.insert(0, record.clone());
        }
        // A shared body is not a shared grant. Only individually eligible
        // canonical waiters may join this original reply's activation.
        candidates.extend(anchor_replies);
        if result_recovery {
            candidates = repository.filter_recovery_snapshot_candidates(
                &anchor.agent_id,
                activation_id,
                &candidates,
                admission_at,
            )?;
        }
        let mut candidate_ids = std::collections::BTreeSet::new();
        candidates.retain(|record| candidate_ids.insert(record.result_identity.clone()));
        let recovering = attempt.recovery_of_attempt_id.as_deref();
        if let Some(prior_id) = recovering {
            let prior = execution.attempts.get(prior_id);
            if !prior.is_some_and(|prior| {
                prior.agent_id == anchor.agent_id
                    && prior.source_message_id == attempt.source_message_id
                    && prior.binding == attempt.binding
                    && prior.state
                        == crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
            }) {
                bail!("idle result recovery does not match canonical predecessor");
            }
            let previous = repository.admitted_for_activation(&anchor.agent_id, prior_id)?;
            let previous_ids: std::collections::BTreeSet<_> = previous
                .iter()
                .map(|record| record.result_identity.as_str())
                .collect();
            if previous_ids.is_empty() && admitted.is_empty() && result_recovery {
                // A crash before the first admission leaves no result snapshot
                // to restore. Revalidate only this canonical source's promise,
                // never refill its empty batch from unrelated owner results.
                candidates.retain(|record| {
                    anchor.source_refs.get("task_result_identity") == Some(&record.result_identity)
                });
            } else {
                candidates.retain(|record| previous_ids.contains(record.result_identity.as_str()));
            }
            candidates.extend(previous.into_iter().filter(|record| {
                !admitted
                    .iter()
                    .any(|prior| prior.result_identity == record.result_identity)
            }));
            let mut identities = std::collections::BTreeSet::new();
            candidates.retain(|record| identities.insert(record.result_identity.clone()));
        }
        // Source selection precedes capacity: unrelated older results must not
        // evict an interrupted recovery's immutable promised identity.
        candidates.sort_by_key(|record| {
            (
                record.state
                    != crate::runtime_db::task_result_settlement::TaskResultSettlementState::CallerAdmitted,
                anchor_reply_order
                    .get(&record.result_identity)
                    .copied()
                    .unwrap_or(usize::MAX),
                record.created_at,
                record.result_identity.clone(),
            )
        });
        candidates.truncate(INPUT_BATCH_SCAN);
        // Include the canonical reply's independently promised identities in
        // the same fixed scan, even if their reducer notifications lag behind.
        let mut scanned = anchor_result_messages;
        for message in &snapshot {
            if scanned.len() == INPUT_BATCH_SCAN {
                break;
            }
            scanned.insert(message.id.clone());
        }
        let mut ordinary_ids = std::collections::BTreeSet::new();
        if recovering.is_none() {
            for message in &snapshot {
                if message.priority != anchor.priority
                    || message.priority == Priority::Interject
                    || message.authority_class == AuthorityClass::OperatorInstruction
                {
                    break;
                }
                if scanned.contains(&message.id)
                    && message.created_at <= cutoff
                    && crate::wake_contract::is_agent_message(message)
                    && self.idle_input_scope(message)?.as_ref() == Some(&scope)
                    && repository
                        .reply_message_candidates(
                            &anchor.agent_id,
                            binding.work_item_id.as_deref(),
                            &message.id,
                            cutoff,
                            INPUT_BATCH_SCAN,
                        )?
                        .is_empty()
                {
                    ordinary_ids.insert(message.id.clone());
                }
            }
        }
        candidates.retain(|record| {
            record.created_at <= cutoff
                && (record.message_id != anchor.id
                    || (recovering.is_some()
                        && record.state
                        == crate::runtime_db::task_result_settlement::TaskResultSettlementState::CallerAdmitted
                        && record.activation_id.as_deref() == recovering))
        });
        let mut eligible_results = Vec::new();
        let mut reply_sources = std::collections::BTreeMap::new();
        for record in candidates {
            if !scanned.contains(&record.message_id) {
                if scanned.len() == INPUT_BATCH_SCAN {
                    break;
                }
                scanned.insert(record.message_id.clone());
            }
            let Some(message) = self.inner.storage.read_message_by_id(&record.message_id)? else {
                continue;
            };
            if let Some(reply_id) = crate::wake_contract::agent_message_reply_reference(&message) {
                if (reply_id == anchor.id
                    && anchor_reply_order.contains_key(&record.result_identity))
                    || repository.reply_source_is_recoverable(&record)?
                    || (exact_waiter
                        .as_ref()
                        .is_some_and(|exact| exact.result_identity == record.result_identity)
                        && repository.reply_source_is_consumed(&record)?)
                {
                    reply_sources.insert(record.result_identity.clone(), reply_id.to_owned());
                    eligible_results.push(record);
                }
            } else if (!result_recovery && message.priority == anchor.priority)
                || (result_recovery && repository.non_reply_source_matches_owner(&record)?)
            {
                eligible_results.push(record);
            }
        }
        let result_message_ids: std::collections::BTreeSet<_> = eligible_results
            .iter()
            .map(|record| record.message_id.as_str())
            .collect();
        let mut order = Vec::new();
        let mut ordered_results = std::collections::BTreeSet::new();
        for message in &snapshot {
            if result_message_ids.contains(message.id.as_str()) {
                order.push(true);
                ordered_results.insert(message.id.as_str());
            } else if ordinary_ids.contains(&message.id) {
                order.push(false);
            }
        }
        order.extend(
            eligible_results
                .iter()
                .filter(|record| !ordered_results.contains(record.message_id.as_str()))
                .map(|_| true),
        );
        let ordinary_anchor = crate::wake_contract::is_agent_message(anchor)
            && (admitted.is_empty() || is_reply_anchor);
        let result_limit = mixed_result_capacity(admitted.len(), ordinary_anchor, order);
        eligible_results.sort_by_key(|record| {
            (
                anchor_reply_order
                    .get(&record.result_identity)
                    .copied()
                    .unwrap_or(usize::MAX),
                snapshot
                    .iter()
                    .position(|message| message.id == record.message_id)
                    .unwrap_or(usize::MAX),
            )
        });
        let anchor_limit = INPUT_BATCH_BYTES
            .min(available_bytes)
            .saturating_sub(BATCH_HEADER.len());
        let anchor_full = item_evidence(anchor, usize::MAX)?.expect("unbounded evidence fits");
        let anchor_limit = if anchor_full.rendered.len() > anchor_limit {
            anchor_limit.min(OVERSIZE_PREVIEW_BYTES)
        } else {
            anchor_limit
        };
        let Some(anchor_item) = item_evidence(anchor, anchor_limit)? else {
            return Ok(false);
        };
        let anchor_bytes = BATCH_HEADER.len() + anchor_item.rendered.len();
        let previous_pending = crate::context::render_pending_task_result_records(
            &self.inner.storage,
            anchor,
            &admitted,
        )?
        .map_or(0, |text| text.len());
        let max_bytes = INPUT_BATCH_BYTES.min(available_bytes.saturating_add(previous_pending));
        let mut reserved_ordinary_bytes = 0usize;
        for message in snapshot
            .iter()
            .filter(|message| ordinary_ids.contains(&message.id))
            .take(2 - usize::from(ordinary_anchor))
        {
            let remaining = max_bytes.saturating_sub(anchor_bytes + reserved_ordinary_bytes);
            let full = item_evidence(message, usize::MAX)?.expect("unbounded evidence fits");
            let item = if full.rendered.len() <= remaining {
                full
            } else if full.rendered.len() > max_bytes.saturating_sub(BATCH_HEADER.len()) {
                let Some(preview) = item_evidence(message, remaining.min(OVERSIZE_PREVIEW_BYTES))?
                else {
                    break;
                };
                preview
            } else {
                break;
            };
            reserved_ordinary_bytes += item.rendered.len();
        }
        let mut selected = std::collections::BTreeSet::new();
        for record in eligible_results {
            if admitted.len() >= result_limit || record.created_at > cutoff {
                break;
            }
            if admitted
                .iter()
                .any(|prior| prior.result_identity == record.result_identity)
            {
                continue;
            }
            admitted.push(record.clone());
            let pending = crate::context::render_pending_task_result_records(
                &self.inner.storage,
                anchor,
                &admitted,
            )?
            .map_or(0, |text| text.len());
            if anchor_bytes
                .saturating_add(reserved_ordinary_bytes)
                .saturating_add(pending)
                > max_bytes
            {
                admitted.pop();
                break;
            }
            selected.insert(record.result_identity);
        }
        if selected.is_empty() && admitted.is_empty() {
            return Ok(false);
        }
        let mut reply_selection =
            std::collections::BTreeMap::<String, std::collections::BTreeSet<String>>::new();
        for identity in &selected {
            if let Some(reply_id) = reply_sources.get(identity) {
                reply_selection
                    .entry(reply_id.clone())
                    .or_default()
                    .insert(identity.clone());
            }
        }
        let ordinary_selection = selected
            .iter()
            .filter(|identity| !reply_sources.contains_key(*identity))
            .cloned()
            .collect();
        if result_recovery && turn.idle_input_batch.is_none() {
            // Preserve the validated source before admission moves its result
            // out of the closed predecessor. A batch alone grants no result.
            let mut recovery_turn = turn.clone();
            recovery_turn.idle_input_batch = Some(IdleInputBatchRecord {
                anchor_message_id: anchor.id.clone(),
                activation_id: Some(activation_id.to_owned()),
                sender_agent_id: String::new(),
                cutoff,
                max_items: 1,
                max_bytes,
                rendered_bytes: anchor_bytes,
                items: vec![anchor_item],
            });
            self.inner.storage.append_turn(&recovery_turn)?;
        }
        if selected.is_empty() {
            return Ok(result_recovery && !admitted.is_empty());
        }
        let mut admitted = repository.admit_unsettled_snapshot(
            &anchor.agent_id,
            binding.work_item_id.as_deref(),
            activation_id,
            (!is_reply_anchor && !result_recovery).then_some(anchor.id.as_str()),
            Some(&ordinary_selection),
            admission_at,
        )?;
        for (reply_id, identities) in reply_selection {
            admitted.extend(repository.admit_reply_message_snapshot(
                &anchor.agent_id,
                binding.work_item_id.as_deref(),
                &reply_id,
                activation_id,
                &identities,
                admission_at,
            )?);
        }
        self.inner.storage.append_event(&AuditEvent::legacy(
            "task_result_settlements_admitted",
            serde_json::json!({
                "agent_id": anchor.agent_id,
                "work_item_id": binding.work_item_id,
                "activation_id": activation_id,
                "cutoff": cutoff,
                "result_identities": admitted.iter().map(|record| &record.result_identity).collect::<Vec<_>>(),
            }),
        ))?;
        Ok(!admitted.is_empty())
    }

    pub(super) async fn recover_provider_result_inputs(
        &self,
        anchor: &MessageEnvelope,
    ) -> Result<bool> {
        let guard = self.inner.agent.lock().await;
        let state = &guard.state;
        let binding = state
            .current_execution_binding
            .as_ref()
            .ok_or_else(|| anyhow!("provider result handoff has no current binding"))?;
        let activation_id = binding
            .activation_id
            .as_deref()
            .ok_or_else(|| anyhow!("provider result handoff has no activation"))?;
        if binding.source_message_id != anchor.id
            || guard
                .current_run_abort
                .as_ref()
                .is_some_and(|abort| abort.token.is_cancelled())
        {
            bail!("provider result handoff execution changed");
        }
        let execution = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&anchor.agent_id)?
            .ok_or_else(|| anyhow!("provider result handoff execution missing"))?;
        let attempt = execution
            .attempts
            .get(activation_id)
            .ok_or_else(|| anyhow!("provider result handoff attempt missing"))?;
        let turn = self
            .inner
            .storage
            .read_turn_by_id(&binding.turn_id)?
            .ok_or_else(|| anyhow!("provider result handoff turn missing"))?;
        let commit = self
            .inner
            .runtime_db
            .transitions()
            .commit_provider_result_handoff(
                attempt,
                &turn,
                self.now(),
                self.take_transition_fault(),
            )?;
        if !commit.applied {
            bail!("provider result handoff does not match canonical predecessor evidence");
        }
        drop(guard);
        self.apply_transition_commit(commit).await;
        Ok(true)
    }

    /// Restore durable live evidence before any idle-start selection can claim input.
    pub(super) async fn recover_interrupted_live_inputs(
        &self,
        anchor: &MessageEnvelope,
    ) -> Result<bool> {
        let guard = self.inner.agent.lock().await;
        let expected = &guard.state;
        let Some(binding) = expected.current_execution_binding.as_ref() else {
            return Ok(false);
        };
        let Some(activation_id) = binding.activation_id.as_deref() else {
            return Ok(false);
        };
        if expected.status != AgentStatus::AwakeRunning
            || binding.source_message_id != anchor.id
            || expected.current_turn_id.as_deref() != Some(binding.turn_id.as_str())
            || !matches!(
                binding.admission_provenance.as_ref(),
                Some(ExecutionAdmissionProvenance::Canonical {
                    activation_id: provenance_id, ..
                }) if provenance_id == activation_id
            )
            || guard
                .current_run_abort
                .as_ref()
                .is_some_and(|abort| abort.token.is_cancelled())
        {
            return Ok(false);
        }
        let Some(execution) = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&anchor.agent_id)?
        else {
            return Ok(false);
        };
        let Some(attempt) = execution.attempts.get(activation_id).filter(|attempt| {
            attempt.state == crate::domain::execution_protocol::ExecutionAttemptState::Open
                && attempt.agent_id == anchor.agent_id
                && attempt.source_message_id.as_deref() == Some(anchor.id.as_str())
                && attempt.recovery_of_attempt_id.is_some()
        }) else {
            return Ok(false);
        };
        let Some(turn) = self.inner.storage.read_turn_by_id(&binding.turn_id)? else {
            return Ok(false);
        };
        let Some(replay) = turn.replay.as_ref() else {
            return Ok(false);
        };
        let Some(predecessor) = self.inner.storage.read_turn_by_id(&replay.source_turn_id)? else {
            return Ok(false);
        };
        if predecessor.live_input_batches.is_empty() {
            return Ok(false);
        }
        let commit = self
            .inner
            .runtime_db
            .transitions()
            .commit_live_input_recovery(
                attempt,
                &turn,
                &predecessor,
                self.take_transition_fault(),
            )?;
        if !commit.applied {
            bail!("live input recovery does not match canonical predecessor evidence");
        }
        drop(guard);
        self.apply_transition_commit(commit).await;
        Ok(true)
    }

    /// Called once, after canonical turn start and before the first provider request.
    /// Queue consumption and input evidence share the existing source-neutral CAS.
    pub(super) async fn attach_idle_sender_inputs(
        &self,
        anchor: &MessageEnvelope,
        available_bytes: usize,
    ) -> Result<bool> {
        let mut guard = self.inner.agent.lock().await;
        let expected = guard.state.clone();
        let Some(binding) = expected.current_execution_binding.as_ref() else {
            return Ok(false);
        };
        if expected.status != AgentStatus::AwakeRunning
            || binding.source_message_id != anchor.id
            || expected.current_turn_id.as_deref() != Some(binding.turn_id.as_str())
        {
            return Ok(false);
        }
        let Some(activation_id) = binding.activation_id.as_deref() else {
            return Ok(false);
        };
        if !matches!(
            binding.admission_provenance.as_ref(),
            Some(ExecutionAdmissionProvenance::Canonical {
                activation_id: provenance_id, ..
            }) if provenance_id == activation_id
        ) || guard
            .current_run_abort
            .as_ref()
            .is_some_and(|abort| abort.token.is_cancelled())
        {
            return Ok(false);
        }
        let execution = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&anchor.agent_id)?;
        let Some(execution) = execution else {
            return Ok(false);
        };
        let Some(attempt) = execution.attempts.get(activation_id).filter(|attempt| {
            attempt.state == crate::domain::execution_protocol::ExecutionAttemptState::Open
                && attempt.agent_id == anchor.agent_id
                && attempt.source_message_id.as_deref() == Some(anchor.id.as_str())
        }) else {
            return Ok(false);
        };
        let mut turn = self
            .inner
            .storage
            .read_turn_by_id(&binding.turn_id)?
            .ok_or_else(|| anyhow!("idle input batch requires a persisted turn"))?;
        if turn.agent_id != anchor.agent_id
            || turn.turn_id != binding.turn_id
            || binding.owner.as_ref() != Some(&turn.effective_owner())
            || turn
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                != Some(anchor.id.as_str())
            || !turn.input_message_ids.contains(&anchor.id)
            || turn.terminal.is_some()
            || turn.idle_input_batch.is_some()
        {
            return Ok(false);
        }
        // Replay metadata selects evidence, but only the canonical interrupted
        // predecessor authorizes restoring that turn's admitted snapshot.
        if let Some(replay) = &turn.replay {
            let Some(source) = self.inner.storage.read_turn_by_id(&replay.source_turn_id)? else {
                // A claim may be interrupted before its turn is persisted.
                // Replay alone is not evidence of an admitted input batch.
                return Ok(false);
            };
            let Some(mut batch) = source.idle_input_batch.clone() else {
                return Ok(false);
            };
            let prior = attempt
                .recovery_of_attempt_id
                .as_ref()
                .and_then(|id| execution.attempts.get(id));
            let rejection = |predicate, field| {
                IdleInputRecoveryRejection::new(predicate, field)
                    .with_id("message_id", Some(&anchor.id))
                    .with_id("turn_id", Some(&turn.turn_id))
                    .with_id("activation_id", Some(activation_id))
                    .with_id(
                        "predecessor_activation_id",
                        attempt.recovery_of_attempt_id.as_deref(),
                    )
                    .with_id("source_turn_id", Some(&replay.source_turn_id))
                    .with_id("batch_activation_id", batch.activation_id.as_deref())
            };
            macro_rules! reject_if {
                ($condition:expr, $predicate:ident, $field:literal) => {
                    if $condition {
                        return Err(rejection(RecoveryPredicate::$predicate, $field).into());
                    }
                };
            }
            validate_idle_recovery_predecessor(anchor, attempt, prior, &batch)
                .map_err(|reason| rejection(reason.predicate, reason.field))?;
            reject_if!(
                anchor.source_refs.get("replay_source_turn_id") != Some(&replay.source_turn_id),
                ReplaySourceMismatch,
                "replay.source_selector"
            );
            reject_if!(
                anchor.source_refs.get("replay_reason").map(String::as_str)
                    != Some("interrupted_queue_claim_reentry"),
                ReplayReasonMismatch,
                "replay.reason"
            );
            reject_if!(
                source.agent_id != anchor.agent_id,
                SourceAgentMismatch,
                "source.agent"
            );
            reject_if!(
                source.turn_id != replay.source_turn_id,
                ReplaySourceMismatch,
                "source.turn_id"
            );
            reject_if!(
                replay.source_message_id != anchor.id,
                ReplayMessageMismatch,
                "replay.source_message"
            );
            reject_if!(
                source.effective_owner() != turn.effective_owner(),
                OwnerMismatch,
                "source.owner"
            );
            reject_if!(
                source
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref())
                    != Some(anchor.id.as_str()),
                SourceTriggerMismatch,
                "source.trigger.message"
            );
            reject_if!(
                batch.anchor_message_id != anchor.id,
                BatchAnchorMismatch,
                "batch.anchor_message"
            );
            reject_if!(
                batch.items.first().map(|item| item.message_id.as_str())
                    != Some(anchor.id.as_str()),
                BatchAnchorMismatch,
                "batch.first_item"
            );
            reject_if!(
                batch.items.len() > INPUT_BATCH_ITEMS,
                BatchIntegrityMismatch,
                "batch.item_count"
            );
            reject_if!(
                render_batch(&batch).len() != batch.rendered_bytes,
                BatchIntegrityMismatch,
                "batch.rendered_bytes"
            );
            reject_if!(
                batch.rendered_bytes > batch.max_bytes,
                BatchIntegrityMismatch,
                "batch.max_bytes"
            );
            reject_if!(
                batch.max_bytes > INPUT_BATCH_BYTES,
                BatchIntegrityMismatch,
                "batch.global_byte_budget"
            );
            let mut seen = std::collections::BTreeSet::new();
            for item in &batch.items {
                reject_if!(
                    !seen.insert(&item.message_id),
                    AttachmentEvidenceMismatch,
                    "attachment.duplicate_item"
                );
                reject_if!(
                    !source.input_message_ids.contains(&item.message_id),
                    AttachmentEvidenceMismatch,
                    "attachment.source_membership"
                );
                reject_if!(
                    item.source_ref != format!("message:{}", item.message_id),
                    AttachmentEvidenceMismatch,
                    "attachment.source_ref"
                );
                let Some(message) = self.inner.storage.read_message_by_id(&item.message_id)? else {
                    return Err(rejection(
                        RecoveryPredicate::AttachmentEvidenceMismatch,
                        "attachment.message_missing",
                    )
                    .into());
                };
                reject_if!(
                    message.agent_id != anchor.agent_id,
                    AttachmentEvidenceMismatch,
                    "attachment.message_agent"
                );
                if item.message_id != anchor.id {
                    let Some(queue) = self
                        .inner
                        .runtime_db
                        .queue_entries()
                        .latest(&item.message_id)?
                    else {
                        return Err(rejection(
                            RecoveryPredicate::AttachmentEvidenceMismatch,
                            "attachment.queue_missing",
                        )
                        .into());
                    };
                    reject_if!(
                        queue.agent_id != anchor.agent_id,
                        AttachmentEvidenceMismatch,
                        "attachment.queue_agent"
                    );
                    reject_if!(
                        queue.status != QueueEntryStatus::Interjected,
                        AttachmentEvidenceMismatch,
                        "attachment.queue_status"
                    );
                }
            }
            for item in &batch.items {
                if !turn.input_message_ids.contains(&item.message_id) {
                    turn.input_message_ids.push(item.message_id.clone());
                }
            }
            batch.activation_id = Some(activation_id.to_owned());
            turn.idle_input_batch = Some(batch);
            self.inner.storage.append_turn(&turn)?;
            return Ok(true);
        }
        if attempt.recovery_of_attempt_id.is_some() {
            // A recovery without a source snapshot must not claim a fresh batch.
            return Ok(false);
        }
        let repository = self.inner.runtime_db.task_result_settlements();
        let reply_anchor = crate::wake_contract::is_agent_message(anchor)
            && !repository
                .reply_message_candidates(
                    &anchor.agent_id,
                    binding.work_item_id.as_deref(),
                    &anchor.id,
                    turn.created_at,
                    INPUT_BATCH_SCAN,
                )?
                .is_empty();
        let scope = if reply_anchor {
            match binding.work_item_id.as_deref() {
                Some(owner) => self.valid_receiver_scope(&anchor.agent_id, owner)?,
                None => Some(InputScope::AgentLifecycle(anchor.agent_id.clone())),
            }
        } else {
            self.idle_input_scope(anchor)?
        };
        let Some(scope) = scope else {
            return Ok(false);
        };
        match &scope {
            InputScope::ReceiverOwner(owner)
                if binding.work_item_id.as_deref() == Some(owner.as_str()) => {}
            InputScope::AgentLifecycle(agent)
                if agent == &anchor.agent_id && binding.work_item_id.is_none() => {}
            InputScope::Sender(_) if binding.work_item_id.is_none() => {}
            _ => return Ok(false),
        }
        let admitted = self
            .inner
            .runtime_db
            .task_result_settlements()
            .admitted_for_activation(&anchor.agent_id, activation_id)?;
        if matches!(scope, InputScope::Sender(_))
            && (!admitted.is_empty()
                || self
                    .inner
                    .runtime_db
                    .wait_conditions()
                    .latest_for_agent(&anchor.agent_id)?
                    .iter()
                    .any(|wait| {
                        wait.status == crate::types::WaitConditionStatus::Active
                            || wait.trigger_message_id.as_deref() == Some(anchor.id.as_str())
                    }))
        {
            return Ok(false);
        }
        let anchor_is_result = admitted.iter().any(|record| record.message_id == anchor.id);
        let max_items = INPUT_BATCH_ITEMS
            .saturating_sub(admitted.len())
            .saturating_add(usize::from(anchor_is_result));
        if max_items == 0 {
            return Ok(false);
        }
        let result_bytes =
            crate::context::render_pending_task_results(&self.inner.storage, &expected, anchor)?
                .map_or(0, |rendered| rendered.len());
        let max_bytes = available_bytes.min(INPUT_BATCH_BYTES.saturating_sub(result_bytes));
        let Some(anchor_limit) = max_bytes.checked_sub(BATCH_HEADER.len()) else {
            return Ok(false);
        };
        let anchor_full = item_evidence(anchor, usize::MAX)?.expect("unbounded evidence fits");
        let anchor_item = if anchor_full.rendered.len() <= anchor_limit {
            anchor_full
        } else {
            let Some(preview) = item_evidence(anchor, anchor_limit.min(OVERSIZE_PREVIEW_BYTES))?
            else {
                return Ok(false);
            };
            preview
        };
        let cutoff = turn.created_at;
        let snapshot = guard.queue.input_snapshot(INPUT_BATCH_SCAN);
        let mut batch = IdleInputBatchRecord {
            anchor_message_id: anchor.id.clone(),
            activation_id: Some(activation_id.to_owned()),
            sender_agent_id: anchor
                .routing_context
                .as_ref()
                .and_then(|route| route.sender_agent_id.clone())
                .unwrap_or_default(),
            cutoff,
            max_items,
            max_bytes,
            rendered_bytes: BATCH_HEADER.len() + anchor_item.rendered.len(),
            items: vec![anchor_item],
        };
        turn.idle_input_batch = Some(batch.clone());
        self.inner.storage.append_turn(&turn)?;
        for candidate in snapshot {
            if batch.items.len() >= max_items {
                break;
            }
            // Do not delay pending operator/control input for a follower batch.
            if candidate.priority != anchor.priority
                || candidate.priority == Priority::Interject
                || candidate.authority_class == AuthorityClass::OperatorInstruction
            {
                break;
            }
            if candidate.created_at > cutoff
                || !crate::wake_contract::is_agent_message(&candidate)
                || self.idle_input_scope(&candidate)?.as_ref() != Some(&scope)
                || !repository
                    .reply_message_candidates(
                        &anchor.agent_id,
                        binding.work_item_id.as_deref(),
                        &candidate.id,
                        cutoff,
                        INPUT_BATCH_SCAN,
                    )?
                    .is_empty()
            {
                continue;
            }
            if self
                .inner
                .runtime_db
                .agent_message_deliveries()
                .latest_for_message(&candidate.id)?
                .is_none_or(|delivery| {
                    delivery.activation_id.is_some() || delivery.turn_id.is_some()
                })
            {
                continue;
            }
            let Some(message) = self.inner.storage.read_message_by_id(&candidate.id)? else {
                continue;
            };
            // Durable ingress assigns message_seq after the in-memory envelope
            // was queued. Compare selection facts, not that projection field.
            if message.created_at != candidate.created_at
                || message.priority != candidate.priority
                || self.idle_input_scope(&message)?.as_ref() != Some(&scope)
            {
                continue;
            }
            let full = item_evidence(&message, usize::MAX)?.expect("unbounded evidence fits");
            let remaining = max_bytes.saturating_sub(batch.rendered_bytes);
            let item = if full.rendered.len() <= remaining {
                full
            } else if full.rendered.len() > max_bytes.saturating_sub(BATCH_HEADER.len()) {
                let Some(preview) = item_evidence(&message, remaining.min(OVERSIZE_PREVIEW_BYTES))?
                else {
                    break;
                };
                preview
            } else {
                // A normal item that does not fit stays queued, preserving FIFO.
                break;
            };
            let expected_state = guard.state.clone();
            let mut committed_state = expected_state.clone();
            committed_state.pending = guard.queue.len().saturating_sub(1);
            let mut attached_turn = turn.clone();
            attached_turn.input_message_ids.push(message.id.clone());
            let mut attached_batch = batch.clone();
            attached_batch.rendered_bytes += item.rendered.len();
            attached_batch.items.push(item);
            attached_turn.idle_input_batch = Some(attached_batch.clone());
            let transcript = TranscriptEntry::new(
                &message.agent_id,
                TranscriptEntryKind::IncomingMessage,
                Some(0),
                Some(message.id.clone()),
                serde_json::json!({
                    "turn_id": turn.turn_id,
                    "boundary": "idle_input_batch",
                    "source_ref": format!("message:{}", message.id),
                    "authority_class": message.authority_class,
                    "routing_context": message.routing_context,
                }),
            );
            // Interject is the existing queue-to-turn atomic attachment primitive;
            // neither this operation nor its status confers operator authority.
            let mut commit = self.inner.runtime_db.transitions().commit_queue(
                &crate::runtime_db::transitions::QueueTransitionCommand {
                    agent_id: message.agent_id.clone(),
                    operation: crate::runtime_db::transitions::QueueOperation::Interject,
                    mutation: crate::runtime_db::transitions::QueueMutation::Consume(
                        QueueEntryRecord {
                            message_id: message.id.clone(),
                            agent_id: message.agent_id.clone(),
                            priority: message.priority.clone(),
                            status: QueueEntryStatus::Interjected,
                            created_at: message.created_at,
                            updated_at: cutoff,
                        },
                    ),
                    scheduler_claim_work_item: None,
                    agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(expected_state)),
                        record: Box::new(committed_state.clone()),
                    }),
                    message_evidence: Vec::new(),
                    transcript_entries: vec![transcript],
                    turn_record: Some(attached_turn.clone()),
                    audit_events: vec![AuditEvent::legacy(
                        "idle_input_attached",
                        serde_json::json!({
                            "agent_id": message.agent_id,
                            "turn_id": turn.turn_id,
                            "message_id": message.id,
                            "cutoff": cutoff,
                            "rendered_bytes": attached_batch.rendered_bytes,
                        }),
                    )],
                    notify_scheduler: false,
                    fault: self.take_transition_fault(),
                    brief_evidence: Vec::new(),
                },
            )?;
            if !commit.applied {
                break;
            }
            guard
                .queue
                .pop_next_matching(|queued| queued.id == message.id);
            guard.state = committed_state.clone();
            guard.last_persisted_state = committed_state;
            commit.effects.agent_state = None;
            turn = attached_turn;
            batch = attached_batch;
            drop(guard);
            self.apply_transition_commit(commit).await;
            guard = self.inner.agent.lock().await;
            if guard.state.current_execution_binding != expected.current_execution_binding
                || guard.state.status != AgentStatus::AwakeRunning
                || guard.queue.peek().is_some_and(|message| {
                    message.priority == Priority::Interject
                        || message.authority_class == AuthorityClass::OperatorInstruction
                })
            {
                break;
            }
        }
        Ok(true)
    }
}
