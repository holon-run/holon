//! Idle-start input snapshots. A batch key never grants execution authority.

pub(crate) mod projection;

#[cfg(test)]
use super::message_dispatch::message_text;
use super::*;
use crate::diagnostics::resource_io::{self, ResourceReadMaterial};
use crate::runtime_db::task_result_settlement::TaskResultSettlementRecord;
use crate::runtime_error::{
    IdleInputRecoveryPredicate as RecoveryPredicate, IdleInputRecoveryRejection,
};
use crate::types::{IdleInputBatchItem, IdleInputBatchRecord};
use std::collections::BTreeMap;
pub(in crate::runtime) mod read;

pub(crate) const INPUT_BATCH_ITEMS: usize = 8;
pub(crate) const INPUT_BATCH_BYTES: usize = 24 * 1024;
pub(crate) const INPUT_BATCH_SCAN: usize = 64;
#[cfg(test)]
const OVERSIZE_PREVIEW_BYTES: usize = 4096;
pub(crate) const BATCH_HEADER: &str =
    "Current input batch (receive order; each item retains its own authority and reply route):\n";

fn selected_pending_result<'a>(
    record: &'a TaskResultSettlementRecord,
    selected: &BTreeSet<String>,
    projections: &'a BTreeMap<String, IdleInputBatchItem>,
    reply_sources: &BTreeMap<String, String>,
) -> read::PendingResultView<'a> {
    if !selected.contains(&record.result_identity)
        || record.state
            != crate::runtime_db::task_result_settlement::TaskResultSettlementState::PersistedPending
    {
        return record.into();
    }
    read::PendingResultView {
        record,
        source_projection: projections
            .get(&record.message_id)
            .or(record.source_projection.as_ref()),
        reply_projection: reply_sources
            .get(&record.result_identity)
            .and_then(|id| projections.get(id))
            .or(record.reply_projection.as_ref()),
    }
}

fn move_admission_projection(
    source: &mut BTreeMap<String, IdleInputBatchItem>,
    destination: &mut BTreeMap<String, IdleInputBatchItem>,
    message_id: &str,
) -> Result<()> {
    if !source.contains_key(message_id) {
        return Ok(());
    }
    // Only the new map slot allocates; the original key and body move together.
    resource_io::charge_owned_metadata(
        std::mem::size_of::<(String, IdleInputBatchItem)>() as u64,
        1,
    )?;
    if let Some((key, item)) = source.remove_entry(message_id) {
        destination.insert(key, item);
    }
    Ok(())
}

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

#[cfg(test)]
pub(super) fn item_evidence(
    message: &MessageEnvelope,
    limit: usize,
) -> Result<Option<IdleInputBatchItem>> {
    // Rendering borrowed evidence allocates text, not a decoded JSON tree.
    let bytes = crate::runtime::input_evidence_budget::serialized_bytes(message)?
        .checked_mul(4)
        .and_then(|bytes| bytes.checked_add(4096))
        .context("idle evidence rendering material overflow")?;
    resource_io::charge_owned_payload(bytes)?;
    resource_io::charge_owned_metadata(4096, 64)?;
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

#[cfg(test)]
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

#[cfg(test)]
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
    fn idle_admission_projection_moves_original_keys_and_shared_reply_once() -> Result<()> {
        use crate::runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        };
        let mut source = BTreeMap::new();
        for id in ["source-a", "source-b", "reply", "ordinary"] {
            source.insert(
                id.to_owned(),
                IdleInputBatchItem {
                    message_id: id.to_owned(),
                    source_ref: format!("message:{id}"),
                    rendered: format!("{id}: {}", "汉字🙂\n".repeat(4096)),
                },
            );
        }
        let pointers: BTreeMap<_, _> = source
            .iter()
            .map(|(key, item)| (key.clone(), (key.as_ptr(), item.rendered.as_ptr())))
            .collect();
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "default".into(),
            identity_incarnation: 1,
        };
        let material = ResourceReadMaterial::with_test_limits(&ledger, &scope, 1024 * 1024, 88)?;
        let before = material.finish()?;
        let mut group = BTreeMap::new();
        material.read(|| {
            for id in ["source-a", "reply", "source-b", "reply", "missing"] {
                move_admission_projection(&mut source, &mut group, id)?;
            }
            Ok(())
        })?;
        assert_eq!(
            source.keys().map(String::as_str).collect::<Vec<_>>(),
            ["ordinary"]
        );
        assert_eq!(
            group.keys().map(String::as_str).collect::<Vec<_>>(),
            ["reply", "source-a", "source-b"]
        );
        for (key, item) in source.iter().chain(&group) {
            assert_eq!(key, &item.message_id);
            assert_eq!(item.source_ref, format!("message:{key}"));
            assert_eq!((key.as_ptr(), item.rendered.as_ptr()), pointers[key]);
        }
        let after = material.finish()?;
        assert_eq!(after.1 - before.1, 3 * 8);
        assert_eq!(
            after.0 - before.0,
            (3 * 8 * std::mem::size_of::<(String, IdleInputBatchItem)>()) as u64
        );
        Ok(())
    }

    #[test]
    fn idle_admission_projection_rejects_short_allowance_before_moving() -> Result<()> {
        use crate::runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        };
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "default".into(),
            identity_incarnation: 1,
        };
        let baseline = ResourceReadMaterial::new(&ledger, &scope)?.finish()?;
        let slot_bytes = (8 * std::mem::size_of::<(String, IdleInputBatchItem)>()) as u64;
        for (bytes, records) in [
            (baseline.0 + slot_bytes - 1, baseline.1 + 8),
            (baseline.0 + slot_bytes, baseline.1 + 8 - 1),
        ] {
            let mut source = BTreeMap::from([(
                "source".to_owned(),
                IdleInputBatchItem {
                    message_id: "source".into(),
                    source_ref: "message:source".into(),
                    rendered: "original body".into(),
                },
            )]);
            let original = source.clone();
            let mut group = BTreeMap::new();
            let material = ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
            let error = material
                .read(|| move_admission_projection(&mut source, &mut group, "source"))
                .unwrap_err();
            assert!(error.to_string().contains("ResourceExhausted"));
            assert_eq!(source, original);
            assert!(group.is_empty());
            assert!(material.finish().is_err());
        }
        Ok(())
    }

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

    #[test]
    fn p1_rendering_json_evidence_charges_text_not_source_parser_nodes() -> Result<()> {
        use crate::{
            config::StateMemoryLimits,
            runtime::{
                input_evidence_budget::InputEvidenceBudgetLedger,
                resident_input_evidence::InputEvidenceScope,
            },
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "default".into(),
            identity_incarnation: 1,
        };
        let message = MessageEnvelope::new(
            "default",
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "agent_message".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Json {
                value: serde_json::Value::Array(vec![serde_json::Value::Bool(false); 2048]),
            },
        );
        let bytes = crate::runtime::input_evidence_budget::serialized_bytes(&message)? * 4 + 4096;
        let material = ResourceReadMaterial::with_test_limits(&ledger, &scope, 1024 * 1024, 576)?;
        let before = material.finish()?;
        let item = material
            .read(|| item_evidence(&message, usize::MAX))?
            .unwrap();
        assert!(item.rendered.contains(&message_text(&message.body)));
        assert_eq!(
            material.finish()?,
            (before.0 + (bytes + 4096) * 8, before.1 + 512)
        );
        let denied = ResourceReadMaterial::with_test_limits(&ledger, &scope, 1024 * 1024, 575)?;
        assert!(denied.read(|| item_evidence(&message, usize::MAX)).is_err());
        assert!(denied.finish().is_err());
        Ok(())
    }
}

pub(in crate::runtime) struct AttachmentPlan {
    pub prior: read::QualifiedTurnRead,
    pub turn: TurnRecord,
    pub candidates: Vec<(read::QualifiedMessageRead, IdleInputBatchItem)>,
}

impl read::IdleRead<'_, '_> {
    /// Receiver bindings precede sender fallback; invalid bindings never fall back.
    pub(super) fn idle_input_scope(&self, message: &MessageEnvelope) -> Result<Option<InputScope>> {
        if message.kind == MessageKind::TaskResult {
            let Some(record) = self.latest_for_message(&message.id)? else {
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
        let has_reply_relation: bool = self.tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM tasks WHERE owner_agent_id = ?1 AND kind = 'agent_message_wait'
                  AND (status IN ('queued', 'running', 'cancelling')
                    OR (status = 'completed'
                      AND json_extract(payload_json, '$.detail.message_id') = ?2))
            )",
            rusqlite::params![message.agent_id, message.id],
            |row| row.get(0),
        )?;
        let Some(delivery) = self.delivery(&message.id)? else {
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
        self.with_work(owner_id, |owner| {
            if owner.agent_id != agent_id || owner.state != WorkItemState::Open {
                return Ok(None);
            }
            resource_io::admit_cloned_value(&owner_id)?;
            Ok(Some(InputScope::ReceiverOwner(owner_id.to_owned())))
        })
        .map(Option::flatten)
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
        // Replay adds its own refs to the claimed envelope. Recovery identity
        // and provenance must still come from the durable source, not the hint.
        let source_matches = self.with_message(&anchor.id, |source| {
            Ok(!(source.agent_id != anchor.agent_id
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
                    "task_result_command_creation_id",
                    "wait_id",
                ]
                .iter()
                .any(|key| source.source_refs.get(*key) != anchor.source_refs.get(*key))))
        })?;
        if source_matches != Some(true)
            || self.queue(&anchor.id)?.is_none_or(|entry| {
                entry.agent_id != anchor.agent_id || entry.status != QueueEntryStatus::Dequeued
            })
        {
            return Ok(false);
        }
        let Some(message_id) = anchor.source_refs.get("task_result_message_id") else {
            return Ok(false);
        };
        let Some(record) = self.latest_for_message(message_id)? else {
            return Ok(false);
        };
        resource_io::admit_cloned_value(&*record)?;
        let mut consuming_record = (*record).clone();
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
            || !record.source.matches_result_source_refs(&anchor.source_refs)
        {
            return Ok(false);
        }
        let exact_waiter = self
            .exact_waiter(
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
        if self.source_matches(&record, false)? {
            return Ok(true);
        }
        if record.state
            == crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled
        {
            self.source_matches(&record, true)
        } else {
            Ok(self.reply_source_recoverable(&record)?
                || (exact_waiter && self.reply_source_available_for_exact_wait(&record)?))
        }
    }
}

impl read::IdleRead<'_, '_> {
    // Share the exact recovery fences with admission before any result is rebound.
    fn validated_recovered_idle_batch(
        &self,
        anchor: &MessageEnvelope,
        attempt: &crate::domain::execution_protocol::ExecutionAttempt,
        execution: &crate::domain::execution_protocol::ExecutionProtocolState,
        turn: &TurnRecord,
    ) -> Result<Option<IdleInputBatchRecord>> {
        let Some(replay) = &turn.replay else {
            return Ok(None);
        };
        let activation_id = attempt.attempt_id.as_str();
        let Some(source) = self.shared_turn(&replay.source_turn_id)? else {
            // A claim may be interrupted before its turn is persisted.
            // Replay alone is not evidence of an admitted input batch.
            return Ok(None);
        };
        resource_io::admit_cloned_value(&source.idle_input_batch)?;
        let Some(batch) = source.idle_input_batch.clone() else {
            return Ok(None);
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
            batch.items.first().map(|item| item.message_id.as_str()) != Some(anchor.id.as_str()),
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
            let Some(message) = self.shared_message(&item.message_id)? else {
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
                let Some(queue) = self.queue(&item.message_id)? else {
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
        Ok(Some(batch))
    }
}

impl RuntimeHandle {
    /// Budget a fixed candidate set before the source ledger admits results.
    pub(super) async fn admit_idle_task_results(
        &self,
        anchor: &MessageEnvelope,
        available: impl Into<projection::InputBudget>,
    ) -> Result<bool> {
        let budget = available.into();
        let available_bytes = budget.bytes;
        let mut mutation = self.begin_mutation().await?;
        let material = ResourceReadMaterial::new(
            &self.inner.input_evidence_budget,
            &mutation.input_evidence_scope(),
        )?;
        let (state, snapshot, cancelled) = mutation.idle_selection_snapshot(&material)?;
        let selected = self.with_idle_read(Some(&state), &material, |read| {
        let state = &state;
        let Some(binding) = state.current_execution_binding.as_ref() else {
            return Ok(None);
        };
        let Some(activation_id) = binding.activation_id.as_deref() else {
            return Ok(None);
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
            || cancelled
        {
            return Ok(None);
        }
        let Some(turn) = read.shared_turn(&binding.turn_id)? else {
            return Ok(None);
        };
        let anchor_replies = if crate::wake_contract::is_agent_message(anchor) {
            read.reply_candidates(
                binding.work_item_id.as_deref(),
                &anchor.id,
                turn.created_at,
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
        let result_recovery = read.idle_reply_recovery_source_valid(anchor, binding)?;
        let scope = if is_reply_anchor || result_recovery {
            match binding.work_item_id.as_deref() {
                Some(owner) => read.valid_receiver_scope(&anchor.agent_id, owner)?,
                None => Some(InputScope::AgentLifecycle(anchor.agent_id.clone())),
            }
        } else {
            read.idle_input_scope(anchor)?
        };
        let Some(scope) = scope else {
            return Ok(None);
        };
        match &scope {
            InputScope::ReceiverOwner(owner)
                if binding.work_item_id.as_deref() == Some(owner.as_str()) => {}
            InputScope::AgentLifecycle(agent)
                if agent == &anchor.agent_id && binding.work_item_id.is_none() => {}
            _ => return Ok(None),
        }
        let Some(execution) = read.protocol()?
        else {
            return Ok(None);
        };
        let Some(attempt) = execution.attempts.get(activation_id).filter(|attempt| {
            attempt.state == crate::domain::execution_protocol::ExecutionAttemptState::Open
                && attempt.agent_id == anchor.agent_id
                && attempt.source_message_id.as_deref() == Some(anchor.id.as_str())
        }) else {
            return Ok(None);
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
            _ => return Ok(None),
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
            return Ok(None);
        }
        let admitted_records = read.admitted(activation_id)?;
        resource_io::charge_owned_metadata(
            u64::try_from(admitted_records.len()
                * std::mem::size_of::<&TaskResultSettlementRecord>())?,
            admitted_records.len(),
        )?;
        let mut admitted: Vec<&TaskResultSettlementRecord> =
            admitted_records.iter().map(|record| &**record).collect();
        let cutoff = turn.created_at;
        // The frozen input cutoff is not the recovery budget's clock.
        let admission_at = self.now();
        let recovery_snapshot = read.recovery_snapshot(activation_id)?;
        let recovering = attempt.recovery_of_attempt_id.as_deref();
        // Interrupted recovery restores its original promises below. An owner
        // scan cannot add an identity, so do not materialize unrelated Results.
        let mut candidates = if recovering.is_some() {
            Vec::new()
        } else if result_recovery {
            read.recovery_candidates(
                binding.work_item_id.as_deref(),
                &recovery_snapshot,
                admission_at,
            )?
        } else {
            read.unsettled(
                binding.work_item_id.as_deref(),
            )?
        };
        let exact_waiter = if result_recovery {
            read.exact_waiter(
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
        candidates = read.filter_recovery_candidates(
            &recovery_snapshot,
            &candidates,
            admission_at,
        )?;
        let mut candidate_ids = std::collections::BTreeSet::new();
        candidates.retain(|record| candidate_ids.insert(record.result_identity.clone()));
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
            let previous = read.admitted(prior_id)?;
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
            candidates.extend(read.admitted(prior_id)?.into_iter().filter(|record| {
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
                    && read.idle_input_scope(message)?.as_ref() == Some(&scope)
                    && read
                        .reply_candidates(
                            binding.work_item_id.as_deref(),
                            &message.id,
                            cutoff,
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
        let scan_end = candidates.iter().position(|record| {
            if !scanned.contains(&record.message_id) {
                if scanned.len() == INPUT_BATCH_SCAN {
                    return true;
                }
                scanned.insert(record.message_id.clone());
            }
            false
        }).unwrap_or(candidates.len());
        candidates.truncate(scan_end);
        read.prepare_reply_source_work(&candidates)?;
        for record in candidates {
            let Some((eligible, reply_id)) = read.with_message(&record.message_id, |message| {
                if let Some(reply_id) = crate::wake_contract::agent_message_reply_reference(message) {
                    if (reply_id == anchor.id
                        && anchor_reply_order.contains_key(&record.result_identity))
                        || read.reply_source_recoverable(&record)?
                        || (exact_waiter.as_ref().is_some_and(|exact| exact.result_identity == record.result_identity)
                            && read.reply_source_available_for_exact_wait(&record)?)
                    {
                        resource_io::admit_cloned_value(&reply_id)?;
                        return Ok((true, Some(reply_id.to_owned())));
                    }
                    Ok((false, None))
                } else {
                    Ok((
                        (!result_recovery && message.priority == anchor.priority)
                            || (result_recovery && read.source_matches(&record, false)?),
                        None,
                    ))
                }
            })? else {
                continue;
            };
            if eligible {
                if let Some(reply_id) = reply_id {
                    reply_sources.insert(record.result_identity.clone(), reply_id);
                }
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
        let anchor_item = if let Some(item) = attempt.input_projection.clone() {
            item
        } else {
            let Some(item) = projection::qualified_item_after(
                read,
                anchor,
                budget.tokens,
                anchor_limit,
                &format!("[current_input]\n{BATCH_HEADER}"),
            )?
            else {
                return Ok(None);
            };
            item
        };
        if anchor_item.rendered.len() > anchor_limit {
            return Ok(None);
        }
        let recovered_batch = if turn.idle_input_batch.is_none() {
            read.validated_recovered_idle_batch(anchor, attempt, &execution, &turn)?
        } else {
            None
        };
        let frozen_batch = turn.idle_input_batch.as_ref().or(recovered_batch.as_ref());
        let mut current = frozen_batch
            .map(render_batch)
            .unwrap_or_else(|| format!("{BATCH_HEADER}{}", anchor_item.rendered));
        let anchor_bytes = current.len();
        let previous_pending = read.pending_measure(anchor, &admitted)?.0;
        let max_bytes = INPUT_BATCH_BYTES.min(available_bytes.saturating_add(previous_pending));
        let mut reserved_ordinary_bytes = 0usize;
        for message in snapshot
            .iter()
            .filter(|message| ordinary_ids.contains(&message.id))
            .take(2 - usize::from(ordinary_anchor))
        {
            let remaining = max_bytes.saturating_sub(anchor_bytes + reserved_ordinary_bytes);
            let Some(item) = projection::qualified_item_after(
                read,
                message,
                budget.tokens,
                remaining,
                &format!("[current_input]\n{current}"),
            )?
            else {
                break;
            };
            reserved_ordinary_bytes += item.rendered.len();
            current.push_str(&item.rendered);
        }
        let mut selected = std::collections::BTreeSet::new();
        let mut projections = std::collections::BTreeMap::new();
        projections.insert(anchor.id.clone(), anchor_item.clone());
        if let Some(batch) = frozen_batch {
            for item in &batch.items {
                projections.insert(item.message_id.clone(), item.clone());
            }
        }
        for prior in &admitted {
            for item in [
                prior.source_projection.as_ref(),
                prior.reply_projection.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                projections.insert(item.message_id.clone(), item.clone());
            }
        }
        for record in &eligible_results {
            if admitted.len() >= result_limit || record.created_at > cutoff {
                break;
            }
            if admitted
                .iter()
                .any(|prior| prior.result_identity == record.result_identity)
            {
                continue;
            }
            if record.state
                != crate::runtime_db::task_result_settlement::TaskResultSettlementState::PersistedPending
            {
                // Recovery must charge the representation the transaction retains,
                // including the legacy None representation, without selecting again.
                let (pending_bytes, pending_tokens) =
                    read.pending_measure_views(
                        anchor,
                        admitted
                            .iter()
                            .map(|prior| selected_pending_result(
                                prior, &selected, &projections, &reply_sources,
                            ))
                            .chain(std::iter::once(read::PendingResultView::from(&**record))),
                    )?;
                if current
                    .len()
                    .saturating_add(pending_bytes)
                    > max_bytes
                    || !budget.fits_idle_tokens(&current, pending_tokens)
                {
                    break;
                }
                for item in [
                    record.source_projection.as_ref(),
                    record.reply_projection.as_ref(),
                ]
                .into_iter()
                .flatten()
                {
                    projections.insert(item.message_id.clone(), item.clone());
                }
                selected.insert(record.result_identity.clone());
                resource_io::charge_owned_metadata(
                    std::mem::size_of::<&TaskResultSettlementRecord>() as u64, 1,
                )?;
                admitted.push(record);
                continue;
            }
            let Some(message) = read.shared_message(&record.message_id)? else {
                break;
            };
            let reply_id =
                crate::wake_contract::agent_message_reply_reference(&message).map(str::to_owned);
            let mut sources = vec![message];
            if let Some(reply_id) = reply_id
                .as_ref()
                .filter(|id| !projections.contains_key(*id))
            {
                let Some(reply) = read.shared_message(reply_id)? else {
                    break;
                };
                sources.push(reply);
            }
            let Some(items) =
                projection::qualified_group(read, &sources, |items| {
                    let reply = reply_id.as_ref().and_then(|id| {
                        projections
                            .get(id)
                            .map(projection::ProjectionView::from)
                            .or_else(|| items.get(1).copied())
                    });
                    let (pending_bytes, pending_tokens) =
                        read.pending_measure_with_projected_views(
                            anchor,
                            admitted.iter().map(|prior| selected_pending_result(
                                prior, &selected, &projections, &reply_sources,
                            )),
                            record, items[0], reply,
                        )?;
                    Ok(current
                        .len()
                        .saturating_add(pending_bytes)
                        <= max_bytes
                        && budget.fits_idle_tokens(&current, pending_tokens))
                })?
            else {
                break;
            };
            for item in items {
                resource_io::admit_cloned_value(&item.message_id)?;
                resource_io::charge_owned_metadata(
                    std::mem::size_of::<(String, IdleInputBatchItem)>() as u64, 1,
                )?;
                projections.insert(item.message_id.clone(), item);
            }
            resource_io::admit_cloned_value(&record.result_identity)?;
            selected.insert(record.result_identity.clone());
            resource_io::charge_owned_metadata(
                std::mem::size_of::<&TaskResultSettlementRecord>() as u64, 1,
            )?;
            admitted.push(record);
        }
        if selected.is_empty() && admitted.is_empty() {
            return Ok(None);
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
        let ordinary_selection: BTreeSet<_> = selected
            .iter()
            .filter(|identity| !reply_sources.contains_key(*identity))
            .cloned()
            .collect();
        // The projections table is keyed by message_id, while reply groups are
        // selected by result identity. Map each group back to the exact source
        // message ids plus the one shared reply body it must admit and bill.
        let mut reply_group_projections = BTreeMap::new();
        for (reply_id, identities) in reply_selection {
            resource_io::charge_owned_metadata(
                std::mem::size_of::<(
                    String,
                    (BTreeSet<String>, BTreeMap<String, IdleInputBatchItem>),
                )>() as u64,
                1,
            )?;
            let mut group = BTreeMap::new();
            for record in &admitted {
                if reply_sources.get(&record.result_identity) != Some(&reply_id) {
                    continue;
                }
                let projected = selected.contains(&record.result_identity)
                    && record.state
                        == crate::runtime_db::task_result_settlement::TaskResultSettlementState::PersistedPending;
                if projected || record.source_projection.is_some() {
                    move_admission_projection(&mut projections, &mut group, &record.message_id)?;
                }
                if projected || record.reply_projection.is_some() {
                    move_admission_projection(&mut projections, &mut group, &reply_id)?;
                }
            }
            reply_group_projections.insert(reply_id, (identities, group));
        }
        let recovery_attachment = if result_recovery && turn.idle_input_batch.is_none() {
            resource_io::admit_cloned_value(&*turn)?;
            let mut recovery_turn = (*turn).clone();
            let mut batch = recovered_batch.unwrap_or_else(|| IdleInputBatchRecord {
                anchor_message_id: anchor.id.clone(), activation_id: Some(activation_id.to_owned()),
                sender_agent_id: String::new(), cutoff, max_items: 1, max_bytes,
                rendered_bytes: anchor_bytes, items: vec![anchor_item],
            });
            batch.activation_id = Some(activation_id.to_owned());
            for item in &batch.items {
                if !recovery_turn.input_message_ids.contains(&item.message_id) {
                    recovery_turn.input_message_ids.push(item.message_id.clone());
                }
            }
            recovery_turn.idle_input_batch = Some(batch);
            Some((turn, recovery_turn))
        } else { None };
        Ok(Some((binding.clone(), admission_at,
            (!is_reply_anchor && !result_recovery).then_some(anchor.id.clone()),
                ordinary_selection, projections, reply_group_projections,
                recovery_attachment,
            result_recovery && !admitted.is_empty())))
        })?;
        let Some((
            binding,
            admission_at,
            anchor_message_id,
            ordinary_selection,
            projections,
            reply_group_projections,
            recovery_attachment,
            already_admitted,
        )) = selected
        else {
            return Ok(false);
        };
        if let Some((prior, turn)) = recovery_attachment {
            mutation
                .commit_idle_recovery_batch(prior, turn, &material)
                .await?;
        }
        if ordinary_selection.is_empty() && reply_group_projections.is_empty() {
            return Ok(already_admitted);
        }
        let activation_id = binding
            .activation_id
            .as_ref()
            .context("idle selection lost activation")?;
        let mut outcomes = Vec::new();
        // Keep each source's original admission transaction and whole ACK. The
        // same owner installs Task/Input/Current before moving to the next phase.
        let ordinary_group =
            (!ordinary_selection.is_empty()).then_some((None, ordinary_selection, projections));
        for (reply_message_id, identities, group_projections) in ordinary_group.into_iter().chain(
            reply_group_projections
                .into_iter()
                .map(|(id, (ids, group))| (Some(id), ids, group)),
        ) {
            let operation = material.read(|| {
                resource_io::admit_cloned_value(&(
                    &binding.work_item_id,
                    activation_id,
                    &anchor_message_id,
                ))?;
                Ok(
                    crate::runtime_db::transitions::TaskResultOperation::AdmitSnapshot {
                        work_item_id: binding.work_item_id.clone(),
                        activation_id: activation_id.clone(),
                        anchor_message_id: anchor_message_id.clone(),
                        reply_message_id,
                        selected: identities,
                        projections: Some(group_projections),
                        at: admission_at,
                    },
                )
            })?;
            let read = mutation.commit_task_result_operation(operation).await?;
            material.read(|| {
                read.records()?;
                resource_io::charge_owned_metadata(std::mem::size_of_val(&read) as u64, 1)?;
                outcomes.push(read);
                Ok(())
            })?;
        }
        let admitted: Vec<_> = material.read(|| {
            let mut identities = Vec::new();
            for outcome in &outcomes {
                let records = outcome.records()?;
                resource_io::charge_owned_metadata(
                    (records.len() * std::mem::size_of::<&str>()) as u64,
                    records.len(),
                )?;
                identities.extend(records.iter().map(|record| record.result_identity.as_str()));
            }
            Ok(identities)
        })?;
        mutation.append_original_audit_event(AuditEvent::legacy(
            "task_result_settlements_admitted",
            material.read(|| { resource_io::admit_serialized_value(&(
                &anchor.agent_id, &binding.work_item_id, activation_id, admission_at, &admitted,
            ))?;
                Ok(serde_json::json!({ "agent_id": anchor.agent_id, "work_item_id": binding.work_item_id,
                    "activation_id": activation_id, "cutoff": admission_at,
                    "result_identities": admitted })) })?,
        )).await?;
        Ok(!admitted.is_empty())
    }

    pub(super) async fn recover_provider_result_inputs(
        &self,
        anchor: &MessageEnvelope,
    ) -> Result<bool> {
        let mut mutation = self.begin_mutation().await?;
        let material = ResourceReadMaterial::new(
            &self.inner.input_evidence_budget,
            &mutation.input_evidence_scope(),
        )?;
        let (state, _, cancelled) = mutation.idle_selection_snapshot(&material)?;
        let plan = self.with_idle_read(Some(&state), &material, |read| {
            let binding = state
                .current_execution_binding
                .as_ref()
                .ok_or_else(|| anyhow!("provider result handoff has no current binding"))?;
            let activation_id = binding
                .activation_id
                .as_deref()
                .ok_or_else(|| anyhow!("provider result handoff has no activation"))?;
            if binding.source_message_id != anchor.id || cancelled {
                bail!("provider result handoff execution changed");
            }
            let execution = read
                .protocol()?
                .ok_or_else(|| anyhow!("provider result handoff execution missing"))?;
            let attempt = execution
                .attempts
                .get(activation_id)
                .ok_or_else(|| anyhow!("provider result handoff attempt missing"))?;
            let turn = read
                .shared_turn(&binding.turn_id)?
                .ok_or_else(|| anyhow!("provider result handoff turn missing"))?;
            resource_io::admit_cloned_value(attempt)?;
            resource_io::admit_cloned_value(&*turn)?;
            Ok((attempt.clone(), (*turn).clone()))
        })?;
        if !mutation
            .commit_provider_result_handoff(plan.0, plan.1, self.now())
            .await?
        {
            bail!("provider result handoff does not match canonical predecessor evidence");
        }
        Ok(true)
    }

    /// Restore durable live evidence before any idle-start selection can claim input.
    pub(super) async fn recover_interrupted_live_inputs(
        &self,
        anchor: &MessageEnvelope,
    ) -> Result<bool> {
        let mut mutation = self.begin_mutation().await?;
        let material = ResourceReadMaterial::new(
            &self.inner.input_evidence_budget,
            &mutation.input_evidence_scope(),
        )?;
        let (state, _, cancelled) = mutation.idle_selection_snapshot(&material)?;
        let plan = self.with_idle_read(Some(&state), &material, |read| {
            let expected = &state;
            let Some(binding) = expected.current_execution_binding.as_ref() else {
                return Ok(None);
            };
            let Some(activation_id) = binding.activation_id.as_deref() else {
                return Ok(None);
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
                || cancelled
            {
                return Ok(None);
            }
            let Some(execution) = read.protocol()? else {
                return Ok(None);
            };
            let Some(attempt) = execution.attempts.get(activation_id).filter(|attempt| {
                attempt.state == crate::domain::execution_protocol::ExecutionAttemptState::Open
                    && attempt.agent_id == anchor.agent_id
                    && attempt.source_message_id.as_deref() == Some(anchor.id.as_str())
                    && attempt.recovery_of_attempt_id.is_some()
            }) else {
                return Ok(None);
            };
            let Some(turn) = read.shared_turn(&binding.turn_id)? else {
                return Ok(None);
            };
            let Some(replay) = turn.replay.as_ref() else {
                return Ok(None);
            };
            let Some(predecessor) = read.shared_turn(&replay.source_turn_id)? else {
                return Ok(None);
            };
            if predecessor.live_input_batches.is_empty() {
                return Ok(None);
            }
            resource_io::admit_cloned_value(attempt)?;
            resource_io::admit_cloned_value(&*turn)?;
            resource_io::admit_cloned_value(&*predecessor)?;
            Ok(Some((
                attempt.clone(),
                (*turn).clone(),
                (*predecessor).clone(),
            )))
        })?;
        let Some((attempt, turn, predecessor)) = plan else {
            return Ok(false);
        };
        if !mutation
            .commit_live_input_recovery(attempt, turn, predecessor)
            .await?
        {
            bail!("live input recovery does not match canonical predecessor evidence");
        }
        Ok(true)
    }

    /// Called once, after canonical turn start and before the first provider request.
    /// Queue consumption and input evidence share the existing source-neutral CAS.
    pub(super) async fn attach_idle_sender_inputs(
        &self,
        anchor: &MessageEnvelope,
        available: impl Into<projection::InputBudget>,
    ) -> Result<bool> {
        let budget = available.into();
        let available_bytes = budget.bytes;
        let mut mutation = self.begin_mutation().await?;
        let material = ResourceReadMaterial::new(
            &self.inner.input_evidence_budget,
            &mutation.input_evidence_scope(),
        )?;
        let (expected, snapshot, cancelled) = mutation.idle_selection_snapshot(&material)?;
        let plan = self.with_idle_read(Some(&expected), &material, |read| {
            let Some(binding) = expected.current_execution_binding.as_ref() else {
                return Ok(None);
            };
            if expected.status != AgentStatus::AwakeRunning
                || binding.source_message_id != anchor.id
                || expected.current_turn_id.as_deref() != Some(binding.turn_id.as_str())
            {
                return Ok(None);
            }
            let Some(activation_id) = binding.activation_id.as_deref() else {
                return Ok(None);
            };
            if !matches!(
                binding.admission_provenance.as_ref(),
                Some(ExecutionAdmissionProvenance::Canonical {
                    activation_id: provenance_id, ..
                }) if provenance_id == activation_id
            ) || cancelled
            {
                return Ok(None);
            }
            let execution = read.protocol()?;
            let Some(execution) = execution else {
                return Ok(None);
            };
            let Some(attempt) = execution.attempts.get(activation_id).filter(|attempt| {
                attempt.state == crate::domain::execution_protocol::ExecutionAttemptState::Open
                    && attempt.agent_id == anchor.agent_id
                    && attempt.source_message_id.as_deref() == Some(anchor.id.as_str())
            }) else {
                return Ok(None);
            };
            let prior = read
                .shared_turn(&binding.turn_id)?
                .ok_or_else(|| anyhow!("idle input batch requires a persisted turn"))?;
            let turn = &*prior;
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
                return Ok(None);
            }
            resource_io::admit_cloned_value(&turn)?;
            let mut turn = turn.clone();
            if let Some(mut batch) =
                read.validated_recovered_idle_batch(anchor, attempt, &execution, &turn)?
            {
                for item in &batch.items {
                    if !turn.input_message_ids.contains(&item.message_id) {
                        turn.input_message_ids.push(item.message_id.clone());
                    }
                }
                batch.activation_id = Some(activation_id.to_owned());
                turn.idle_input_batch = Some(batch);
                return Ok(Some(AttachmentPlan {
                    prior,
                    turn,
                    candidates: Vec::new(),
                }));
            }
            if attempt.recovery_of_attempt_id.is_some() {
                // A recovery without a source snapshot must not claim a fresh batch.
                return Ok(None);
            }
            let reply_anchor = crate::wake_contract::is_agent_message(anchor)
                && !read
                    .reply_candidates(binding.work_item_id.as_deref(), &anchor.id, turn.created_at)?
                    .is_empty();
            let result_recovery = read.idle_reply_recovery_source_valid(anchor, binding)?;
            let scope = if reply_anchor || result_recovery {
                match binding.work_item_id.as_deref() {
                    Some(owner) => read.valid_receiver_scope(&anchor.agent_id, owner)?,
                    None => Some(InputScope::AgentLifecycle(anchor.agent_id.clone())),
                }
            } else {
                read.idle_input_scope(anchor)?
            };
            let Some(scope) = scope else {
                return Ok(None);
            };
            match &scope {
                InputScope::ReceiverOwner(owner)
                    if binding.work_item_id.as_deref() == Some(owner.as_str()) => {}
                InputScope::AgentLifecycle(agent)
                    if agent == &anchor.agent_id && binding.work_item_id.is_none() => {}
                InputScope::Sender(_) if binding.work_item_id.is_none() => {}
                _ => return Ok(None),
            }
            let admitted = read.admitted(activation_id)?;
            if matches!(scope, InputScope::Sender(_))
                && (!admitted.is_empty() || read.sender_has_wait(&anchor.id)?)
            {
                return Ok(None);
            }
            let anchor_is_result = admitted.iter().any(|record| record.message_id == anchor.id);
            let max_items = INPUT_BATCH_ITEMS
                .saturating_sub(admitted.len())
                .saturating_add(usize::from(anchor_is_result));
            if max_items == 0 {
                return Ok(None);
            }
            let pending_results = read.pending_text(anchor, &admitted)?;
            let result_bytes = pending_results.as_ref().map_or(0, String::len);
            let result_tokens = pending_results.as_ref().map_or(0, |rendered| {
                crate::context::estimate_text_tokens(&format!("[pending_task_results]\n{rendered}"))
            });
            let current_tokens = budget.tokens.saturating_sub(result_tokens);
            let max_bytes = available_bytes.min(INPUT_BATCH_BYTES.saturating_sub(result_bytes));
            let Some(anchor_limit) = max_bytes.checked_sub(BATCH_HEADER.len()) else {
                return Ok(None);
            };
            let anchor_item = if let Some(item) = attempt.input_projection.clone() {
                item
            } else {
                let Some(item) = projection::qualified_item_after(
                    read,
                    anchor,
                    current_tokens,
                    anchor_limit,
                    &format!("[current_input]\n{BATCH_HEADER}"),
                )?
                else {
                    return Ok(None);
                };
                item
            };
            if anchor_item.rendered.len() > anchor_limit
                || !budget.fits_idle(
                    &format!("{BATCH_HEADER}{}", anchor_item.rendered),
                    pending_results.as_deref(),
                )
            {
                return Ok(None);
            }
            let cutoff = turn.created_at;
            let batch = IdleInputBatchRecord {
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
            let mut selected_items = batch.items.len();
            let mut selected_bytes = batch.rendered_bytes;
            let mut selected_current = render_batch(&batch);
            turn.idle_input_batch = Some(batch);
            let mut candidates = Vec::new();
            for candidate in &snapshot {
                if selected_items >= max_items {
                    break;
                }
                if read.source_revision_repair_retains_source(&candidate.id)? {
                    continue;
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
                    || read.idle_input_scope(&candidate)?.as_ref() != Some(&scope)
                    || !read
                        .reply_candidates(binding.work_item_id.as_deref(), &candidate.id, cutoff)?
                        .is_empty()
                {
                    continue;
                }
                if read.delivery(&candidate.id)?.is_none_or(|delivery| {
                    delivery.activation_id.is_some() || delivery.turn_id.is_some()
                }) {
                    continue;
                }
                let Some(message) = read.shared_message(&candidate.id)? else {
                    continue;
                };
                // Durable ingress assigns message_seq after the in-memory envelope
                // was queued. Compare selection facts, not that projection field.
                if message.created_at != candidate.created_at
                    || message.priority != candidate.priority
                    || read.idle_input_scope(&message)?.as_ref() != Some(&scope)
                {
                    continue;
                }
                let remaining = max_bytes.saturating_sub(selected_bytes);
                let Some(item) = projection::qualified_item_after(
                    read,
                    &message,
                    current_tokens,
                    remaining,
                    &format!("[current_input]\n{selected_current}"),
                )?
                else {
                    break;
                };
                selected_current.push_str(&item.rendered);
                selected_bytes += item.rendered.len();
                selected_items += 1;
                candidates.push((message, item));
            }
            Ok(Some(AttachmentPlan {
                prior,
                turn,
                candidates,
            }))
        })?;
        let Some(plan) = plan else {
            return Ok(false);
        };
        mutation.commit_idle_attachment(plan, material).await?;
        Ok(true)
    }
    pub(super) fn canonical_idle_batch_replay_source(
        inputs: &super::resident_input_evidence::CapturedInputEvidence,
        execution: Option<&crate::domain::execution_protocol::ExecutionProtocolState>,
        message: &MessageEnvelope,
        provenance: Option<&ExecutionAdmissionProvenance>,
    ) -> Result<Option<(String, TurnRecord)>> {
        use super::resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key, PreparedInputEvidence,
        };
        use crate::domain::execution_protocol::{ExecutionAttemptState, ExecutionBinding};
        use crate::types::TurnOwner;
        let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) = provenance else {
            return Ok(None);
        };
        let Some(execution) = execution else {
            return Ok(None);
        };
        let Some(attempt) = execution.attempts.get(activation_id) else {
            return Ok(None);
        };
        let Some(prior) = attempt
            .recovery_of_attempt_id
            .as_ref()
            .and_then(|id| execution.attempts.get(id))
        else {
            return Ok(None);
        };
        if attempt.state != ExecutionAttemptState::Open
            || prior.state != ExecutionAttemptState::Interrupted
            || attempt.agent_id != message.agent_id
            || prior.agent_id != attempt.agent_id
            || attempt.source_message_id.as_deref() != Some(message.id.as_str())
            || prior.source_message_id != attempt.source_message_id
            || prior.binding != attempt.binding
        {
            return Ok(None);
        }
        let Some(id) = message.source_refs.get("replay_source_turn_id") else {
            return Ok(None);
        };
        let Some(evidence) = inputs.lookup(&Key::new(Family::Turn, id))? else {
            return Ok(None);
        };
        let PreparedInputEvidence::Turn(turn) = &evidence.record else {
            bail!("canonical replay Turn family mismatch");
        };
        let owner_matches = match (&attempt.binding, turn.effective_owner()) {
            (
                ExecutionBinding::WorkItem { work_item_id },
                TurnOwner::WorkItem {
                    work_item_id: owner,
                },
            ) => work_item_id == &owner,
            (
                ExecutionBinding::AgentLifecycle { agent_id },
                TurnOwner::AgentLifecycle { agent_id: owner },
            ) => agent_id == &owner,
            _ => false,
        };
        if evidence.locator.scope != inputs.scope
            || turn.agent_id != message.agent_id
            || turn.turn_id != *id
            || !owner_matches
            || !turn.input_message_ids.contains(&message.id)
            || turn
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                != Some(message.id.as_str())
            || !turn.terminal.as_ref().is_some_and(|terminal| {
                terminal.kind == crate::types::TurnTerminalKind::Interrupted
            })
            || !(turn.idle_input_batch.as_ref().is_some_and(|batch| {
                batch.anchor_message_id == message.id
                    && batch.activation_id.as_deref() == Some(prior.attempt_id.as_str())
            }) || turn
                .live_input_batches
                .iter()
                .any(|batch| batch.activation_id == prior.attempt_id))
        {
            return Ok(None);
        }
        resource_io::admit_cloned_value(&(id, turn))?;
        Ok(Some((id.clone(), turn.clone())))
    }

    pub(super) fn idle_batch_recovery_message(
        &self,
        queue: &crate::queue::RuntimeQueue,
    ) -> Result<Option<String>> {
        let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
            agent_id: self.inner.mutation_owner.agent_id().to_owned(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        let material = ResourceReadMaterial::new(&self.inner.input_evidence_budget, &scope)?;
        self.with_idle_read(None, &material, |read| {
            let mut result_anchor = None;
            for message in queue.shared_messages().take(INPUT_BATCH_SCAN) {
                if message.priority == Priority::Interject
                    || message.authority_class == AuthorityClass::OperatorInstruction
                {
                    break;
                }
                if message.delivery_surface == Some(MessageDeliverySurface::TaskRejoin) {
                    let Some(anchor) = read.interrupted_result_anchor(&message.id, self.now())?
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
                if read
                    .queue(&message.id)?
                    .is_some_and(|entry| entry.status == QueueEntryStatus::Interrupted)
                    && read.source_turn(&message.id)?.is_some()
                {
                    if result_anchor
                        .as_ref()
                        .is_some_and(|anchor| anchor != &message.id)
                    {
                        break;
                    }
                    resource_io::admit_cloned_value(&message.id)?;
                    return Ok(Some(message.id.clone()));
                }
            }
            Ok(None)
        })
    }
    #[cfg(test)]
    pub(super) fn idle_reply_recovery_source_valid(
        &self,
        anchor: &MessageEnvelope,
        binding: &crate::types::WorkItemExecutionBinding,
    ) -> Result<bool> {
        let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
            agent_id: anchor.agent_id.clone(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        let material = ResourceReadMaterial::new(&self.inner.input_evidence_budget, &scope)?;
        self.with_idle_read(None, &material, |read| {
            read.idle_reply_recovery_source_valid(anchor, binding)
        })
    }
}
