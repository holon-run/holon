//! Atomic source admission and bounded attachment to a canonical live turn.

use super::*;
use crate::domain::execution_protocol::{
    ExecutionAttempt, ExecutionAttemptState, ExecutionBinding, WorkItemExecutionState,
};
use crate::types::{
    AgentMessageDeliveryOutcome, AgentMessagePrincipalKind, AgentStatus, AuthorityClass,
    LiveInputBatchRecord, MessageKind, Priority, TranscriptEntryKind, TurnOwner,
};

#[derive(Debug, serde::Serialize)]
pub(crate) struct LiveInputAttachmentCommand {
    pub expected_attempt: ExecutionAttempt,
    pub expected_turn: TurnRecord,
    pub batch: LiveInputBatchRecord,
    pub messages: Vec<MessageEnvelope>,
    pub queues: Vec<QueueEntryRecord>,
    pub agent_state: AgentStateMutation,
    #[serde(skip)]
    pub fault: Option<TransitionFaultPoint>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct LiveInputRecoveryCommand {
    pub expected_attempt: ExecutionAttempt,
    pub expected_turn: TurnRecord,
    pub predecessor: TurnRecord,
    #[serde(skip)]
    pub fault: Option<TransitionFaultPoint>,
}

impl RuntimeTransitionRepository<'_> {
    #[cfg(test)]
    pub(crate) fn commit_live_input_recovery(
        &self,
        expected_attempt: &ExecutionAttempt,
        expected_turn: &TurnRecord,
        predecessor: &TurnRecord,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<TransitionCommit> {
        self.db.transaction(|tx| self.commit_live_input_recovery_tx(
            tx, None, expected_attempt, expected_turn, predecessor, fault, None,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn commit_live_input_recovery_tx(
        &self,
        tx: &Transaction<'_>,
        input_scope: Option<&InputEvidenceScope>,
        expected_attempt: &ExecutionAttempt,
        expected_turn: &TurnRecord,
        predecessor: &TurnRecord,
        fault: Option<TransitionFaultPoint>,
        allowed_results: Option<&BTreeSet<crate::runtime::resident_task::TaskKey>>,
    ) -> Result<TransitionCommit> {
            let agent_id = &expected_attempt.agent_id;
            if predecessor.live_input_batches.is_empty() || !expected_turn.live_input_batches.is_empty()
                || expected_turn.idle_input_batch.is_some()
            {
                return Ok(TransitionCommit::default());
            }
            let state = execution_protocol_repository::load_state_tx(tx, agent_id)?;
            let prior = expected_attempt.recovery_of_attempt_id.as_ref()
                .and_then(|id| state.attempts.get(id));
            let Some(prior) = prior else { return Ok(TransitionCommit::default()) };
            let Some(agent) = agent_state_tx(tx, agent_id)? else { return Ok(TransitionCommit::default()) };
            let command = LiveInputAttachmentCommand {
                expected_attempt: expected_attempt.clone(), expected_turn: expected_turn.clone(),
                batch: predecessor.live_input_batches[0].clone(), messages: Vec::new(),
                queues: Vec::new(), agent_state: AgentStateMutation {
                    expected: Some(Box::new(agent.clone())), record: Box::new(agent),
                }, fault,
            };
            if !live_binding_matches_tx(tx, &command)?
                || prior.state != ExecutionAttemptState::Interrupted
                || prior.agent_id != *agent_id
                || prior.source_message_id != expected_attempt.source_message_id
                || prior.binding != expected_attempt.binding
                || crate::runtime_db::task_result_settlement::canonical_live_turn_id_tx(tx, prior)?
                    .as_deref() != Some(predecessor.turn_id.as_str())
                || predecessor.agent_id != *agent_id
                || predecessor.effective_owner() != expected_turn.effective_owner()
                || expected_turn.replay.as_ref().is_none_or(|replay| {
                    replay.source_turn_id != predecessor.turn_id
                        || Some(&replay.source_message_id) != expected_attempt.source_message_id.as_ref()
                        || replay.reason != "interrupted_queue_claim_reentry"
                })
                || predecessor.live_input_batches.len() > 4
                || predecessor.live_input_batches.iter().map(live_identity_count).sum::<usize>() > 32
                || predecessor.live_input_batches.iter().map(|batch| batch.rendered_bytes).sum::<usize>() > 96 * 1024
            {
                return Ok(TransitionCommit::default());
            }
            let payload: Option<String> = tx.query_row(
                "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
                [&predecessor.turn_id], |row| row.get(0),
            ).optional()?;
            if payload.map(|text| serde_json::from_str::<TurnRecord>(&text)).transpose()?.as_ref()
                != Some(predecessor)
            {
                return Ok(TransitionCommit::default());
            }
            let Some(source_id) = &expected_attempt.source_message_id else {
                return Ok(TransitionCommit::default());
            };
            let payload: Option<String> = tx.query_row(
                "SELECT payload_json FROM messages WHERE message_id = ?1",
                [source_id], |row| row.get(0),
            ).optional()?;
            let source = payload.map(|text| serde_json::from_str::<MessageEnvelope>(&text)).transpose()?;
            let Some(source) = source.as_ref() else { return Ok(TransitionCommit::default()) };
            // Claim-local envelope annotations are not persisted source authority.
            if source.id != *source_id || source.agent_id != *agent_id
            {
                return Ok(TransitionCommit::default());
            }
            let mut identities = BTreeSet::new();
            let mut messages = BTreeSet::new();
            let mut safe_points = BTreeSet::new();
            let mut attachment_attempts = BTreeMap::new();
            let mut attachment_turns = BTreeSet::new();
            let mut ancestor = prior;
            let mut younger_turn: Option<TurnRecord> = None;
            loop {
                if ancestor.agent_id != *agent_id
                    || ancestor.source_message_id != expected_attempt.source_message_id
                    || ancestor.binding != expected_attempt.binding
                    || ancestor.state != ExecutionAttemptState::Interrupted
                    || attachment_attempts.contains_key(&ancestor.attempt_id)
                {
                    return Ok(TransitionCommit::default());
                }
                let Some(ancestor_turn) =
                    crate::runtime_db::task_result_settlement::recovery_turn_for_activation_tx(
                        tx, ancestor, younger_turn.as_ref(),
                    )?
                else {
                    return Ok(TransitionCommit::default());
                };
                if younger_turn.as_ref().is_some_and(|younger| {
                        younger.replay.as_ref().is_none_or(|replay| {
                            replay.source_turn_id != ancestor_turn.turn_id
                                || Some(&replay.source_message_id) != expected_attempt.source_message_id.as_ref()
                        })
                    })
                {
                    return Ok(TransitionCommit::default());
                }
                attachment_turns.insert(ancestor_turn.turn_id.clone());
                attachment_attempts.insert(ancestor.attempt_id.clone(), ancestor_turn.turn_id.clone());
                younger_turn = Some(ancestor_turn);
                let Some(id) = &ancestor.recovery_of_attempt_id else { break };
                let Some(previous) = state.attempts.get(id) else {
                    return Ok(TransitionCommit::default());
                };
                ancestor = previous;
            }
            let mut turn = expected_turn.clone();
            if !restore_initial_snapshot_tx(
                tx, expected_attempt, prior, predecessor, source, &attachment_attempts, &mut turn,
            )? {
                return Ok(TransitionCommit::default());
            }
            messages.extend(turn.input_message_ids.iter().cloned());
            for batch in &predecessor.live_input_batches {
                if batch.activation_id != prior.attempt_id || batch.max_items > 8
                    || !attachment_attempts.contains_key(batch.safe_point.activation_id.as_str())
                    || batch.max_bytes > 24 * 1024
                    || live_identity_count(batch) > batch.max_items
                    || batch.rendered_bytes != batch.rendered_text().len()
                    || batch.rendered_bytes > batch.max_bytes
                    || !safe_points.insert(&batch.safe_point)
                    || batch.result_identities.iter().any(|identity| !identities.insert(identity.clone()))
                {
                    return Ok(TransitionCommit::default());
                }
                for item in &batch.items {
                    if !messages.insert(item.message_id.clone())
                        || !predecessor.input_message_ids.contains(&item.message_id)
                        || item.source_ref != format!("message:{}", item.message_id)
                    {
                        return Ok(TransitionCommit::default());
                    }
                    let valid: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM messages WHERE message_id = ?1 AND agent_id = ?2)
                         AND EXISTS(SELECT 1 FROM queue_entries WHERE message_id = ?1
                           AND agent_id = ?2 AND status = 'interjected')",
                        params![item.message_id, agent_id], |row| row.get(0),
                    )?;
                    if !valid { return Ok(TransitionCommit::default()); }
                    if !turn.input_message_ids.contains(&item.message_id) {
                        turn.input_message_ids.push(item.message_id.clone());
                    }
                }
                let mut batch = batch.clone();
                batch.activation_id = expected_attempt.attempt_id.clone();
                turn.live_input_batches.push(batch);
            }
            if !restore_operator_inputs_tx(tx, predecessor, &attachment_turns, &mut turn)? {
                return Ok(TransitionCommit::default());
            }
            if predecessor.input_message_ids.iter().any(|id| !turn.input_message_ids.contains(id))
                || turn.input_message_ids.iter().any(|id| !predecessor.input_message_ids.contains(id))
            {
                return Ok(TransitionCommit::default());
            }
            turn.input_message_ids.clone_from(&predecessor.input_message_ids);
            if let Some(batch) = &turn.idle_input_batch {
                let initial: Vec<(String, String)> = tx.prepare(
                    "SELECT result_identity, message_id FROM task_result_settlements
                     WHERE agent_id = ?1 AND activation_id = ?2 AND state = 'caller_admitted'",
                )?.query_map(params![agent_id, prior.attempt_id], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?.collect::<rusqlite::Result<_>>()?;
                let initial: Vec<_> = initial.iter().filter(|(id, _)| !identities.contains(id)).collect();
                let anchor_result = usize::from(initial.iter().any(|(_, id)| {
                    id.as_str() == batch.anchor_message_id.as_str()
                }));
                if batch.max_items > 8usize.saturating_sub(initial.len()).saturating_add(anchor_result) {
                    return Ok(TransitionCommit::default());
                }
            }
            inject_fault(fault, TransitionFaultPoint::AfterValidation)?;
            let mut changed_results = BTreeSet::new();
            let mut query = tx.prepare(
                "SELECT result_identity FROM task_result_settlements
                 WHERE agent_id = ?1 AND activation_id = ?2 AND state = 'caller_admitted'",
            )?;
            for id in query.query_map(params![agent_id, prior.attempt_id], |row| {
                row.get::<_, String>(0)
            })? {
                let key = crate::runtime::resident_task::TaskKey::new(
                    crate::runtime::resident_task::TaskFamily::ResultSettlement, id?,
                );
                if let Some(allowed) = allowed_results {
                    anyhow::ensure!(
                        allowed.contains(&key),
                        "recovered live Result is outside its original Task proof"
                    );
                }
                changed_results.insert(key);
            }
            if !crate::runtime_db::task_result_settlement::recover_live_task_results_tx(
                tx, agent_id, &prior.attempt_id, &expected_attempt.attempt_id, &identities, Utc::now(),
            )? {
                return Ok(TransitionCommit::default());
            }
            upsert_turn_record_tx(tx, &turn)?;
            inject_fault(fault, TransitionFaultPoint::AfterCanonicalWrites)?;
            finish_transition_tx(tx, input_scope, true, agent_id, &[AuditEvent::legacy(
                "live_input_recovered", serde_json::json!({
                    "agent_id": agent_id, "turn_id": expected_turn.turn_id,
                    "source_turn_id": predecessor.turn_id, "activation_id": expected_attempt.attempt_id,
                    "predecessor_activation_id": prior.attempt_id, "result_identities": identities,
                }),
            )], &[], fault, PostCommitEffects {
                turns: vec![turn],
                task_write_keys: changed_results,
                ..PostCommitEffects::default()
            })
    }

    #[cfg(test)]
    pub(crate) fn commit_live_input_attachment(
        &self,
        command: &LiveInputAttachmentCommand,
    ) -> Result<TransitionCommit> {
        self.db.transaction(|tx| self.commit_live_input_attachment_tx(tx, None, command, None))
    }

    pub(super) fn commit_live_input_attachment_tx(
        &self,
        tx: &Transaction<'_>,
        input_scope: Option<&InputEvidenceScope>,
        command: &LiveInputAttachmentCommand,
        allowed_results: Option<&BTreeSet<crate::runtime::resident_task::TaskKey>>,
    ) -> Result<TransitionCommit> {
            let agent_id = &command.expected_attempt.agent_id;
            let turn_id = &command.expected_turn.turn_id;
            if !live_binding_matches_tx(tx, command)? {
                return Ok(TransitionCommit::default());
            }
            validate_agent_state_mutation_tx(tx, Some(&command.agent_state))?;
            let batch = &command.batch;
            let identities: BTreeSet<_> = batch.result_identities.iter().cloned().collect();
            let ordinary: BTreeSet<_> = batch.ordinary_message_ids.iter().cloned().collect();
            let prior = &command.expected_turn.live_input_batches;
            let identity_count = identities.len() + ordinary.len();
            if batch.activation_id != command.expected_attempt.attempt_id
                || batch.safe_point.activation_id != command.expected_attempt.attempt_id
                || batch.safe_point.provider_round < 2
                || prior.iter().any(|old| old.safe_point == batch.safe_point)
                || batch.max_items > 8
                || batch.max_bytes > 24 * 1024
                || identity_count == 0
                || identity_count > batch.max_items
                || identities.len() != batch.result_identities.len()
                || ordinary.len() != batch.ordinary_message_ids.len()
                || batch.rendered_text().len() != batch.rendered_bytes
                || batch.rendered_bytes > batch.max_bytes
                || prior.len() >= 4
                || prior.iter().map(|old| old.rendered_bytes).sum::<usize>() + batch.rendered_bytes
                    > 96 * 1024
                || prior.iter().map(live_identity_count).sum::<usize>() + identity_count > 32
            {
                return Ok(TransitionCommit::default());
            }
            let mut seen = BTreeSet::new();
            for message in &command.messages {
                let payload: Option<String> = tx
                    .query_row(
                        "SELECT payload_json FROM messages WHERE message_id = ?1",
                        [&message.id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if message.agent_id != *agent_id
                    || message.created_at > batch.cutoff
                    || !seen.insert(message.id.clone())
                    || payload
                        .map(|text| serde_json::from_str::<MessageEnvelope>(&text))
                        .transpose()?
                        .as_ref()
                        != Some(message)
                {
                    return Ok(TransitionCommit::default());
                }
                if ordinary.contains(&message.id)
                    && !ordinary_live_source_matches_tx(tx, command, message)?
                {
                    return Ok(TransitionCommit::default());
                }
                let Some(item) = batch
                    .items
                    .iter()
                    .find(|item| item.message_id == message.id)
                else {
                    return Ok(TransitionCommit::default());
                };
                // Result identities are prepended by the batch, outside source provenance.
                let source_rendered = item
                    .rendered
                    .find("- reply_context_id:")
                    .map(|start| &item.rendered[start..])
                    .unwrap_or(&item.rendered);
                let source = crate::types::IdleInputBatchItem {
                    rendered: source_rendered.to_owned(),
                    ..item.clone()
                };
                crate::runtime::input_consumption::projection::validate_projection(
                    message, &source,
                )?;
            }
            if batch.items.iter().any(|item| {
                !seen.contains(&item.message_id)
                    || item.source_ref != format!("message:{}", item.message_id)
            }) || ordinary.iter().any(|id| !seen.contains(id))
            {
                return Ok(TransitionCommit::default());
            }
            let mut granted = ordinary.clone();
            for identity in &identities {
                let payload: Option<String> = tx.query_row(
                    "SELECT payload_json FROM task_result_settlements WHERE result_identity = ?1",
                    [identity], |row| row.get(0),
                ).optional()?;
                let Some(record) = payload
                    .map(|text| {
                        serde_json::from_str::<
                            crate::runtime_db::task_result_settlement::TaskResultSettlementRecord,
                        >(&text)
                    })
                    .transpose()?
                else {
                    return Ok(TransitionCommit::default());
                };
                let Some(message) = command
                    .messages
                    .iter()
                    .find(|message| message.id == record.message_id)
                else {
                    return Ok(TransitionCommit::default());
                };
                granted.insert(record.message_id);
                if let Some(reply_id) = crate::wake_contract::agent_message_reply_reference(message)
                {
                    granted.insert(reply_id.to_owned());
                }
            }
            if seen.iter().any(|id| !granted.contains(id)) {
                return Ok(TransitionCommit::default());
            }
            let mut queue_ids = BTreeSet::new();
            for queue in &command.queues {
                if queue.agent_id != *agent_id
                    || queue.status != QueueEntryStatus::Interjected
                    || !seen.contains(&queue.message_id)
                    || !queue_ids.insert(&queue.message_id)
                    || !crate::runtime_db::repositories::queue_entry_is_claimable_tx(
                        tx, queue, false,
                    )?
                {
                    return Ok(TransitionCommit::default());
                }
            }
            if ordinary.iter().any(|id| !queue_ids.contains(id)) {
                return Ok(TransitionCommit::default());
            }
            inject_fault(command.fault, TransitionFaultPoint::AfterValidation)?;
            if let Some(allowed) = allowed_results {
                for id in &identities {
                    anyhow::ensure!(
                        allowed.contains(&crate::runtime::resident_task::TaskKey::new(
                            crate::runtime::resident_task::TaskFamily::ResultSettlement, id,
                        )),
                        "live Result is outside its original Task proof"
                    );
                }
            }
            if !crate::runtime_db::task_result_settlement::admit_live_task_results_tx(
                tx,
                agent_id,
                turn_id,
                &batch.activation_id,
                &identities,
                batch.cutoff,
                batch.cutoff,
            )? {
                return Ok(TransitionCommit::default());
            }
            // All sources have been validated before any queue or ledger write.
            for queue in &command.queues {
                anyhow::ensure!(
                    try_interject_queued_message_tx(tx, queue)?,
                    "live input queue changed after transactional validation"
                );
                advance_delivery_state_for_message_tx(
                    tx,
                    &queue.message_id,
                    AgentMessageDeliveryState::Consumed,
                    None,
                )?;
            }
            apply_agent_state_mutation_tx(tx, Some(&command.agent_state))?;
            let mut turn = command.expected_turn.clone();
            let mut transcript_entries = Vec::new();
            for message in &command.messages {
                if !turn.input_message_ids.contains(&message.id) {
                    turn.input_message_ids.push(message.id.clone());
                }
                let entry = TranscriptEntry::new(
                        agent_id,
                        TranscriptEntryKind::IncomingMessage,
                        Some(batch.safe_point.provider_round),
                        Some(message.id.clone()),
                        serde_json::json!({
                            "turn_id": turn_id, "boundary": "live_input_batch",
                            "source_ref": format!("message:{}", message.id),
                            "authority_class": message.authority_class,
                            "routing_context": message.routing_context,
                            "activation_id": batch.activation_id,
                        }),
                    );
                append_transcript_entry_tx(tx, &entry)?;
                transcript_entries.push(entry);
            }
            turn.live_input_batches.push(batch.clone());
            upsert_turn_record_tx(tx, &turn)?;
            inject_fault(command.fault, TransitionFaultPoint::AfterCanonicalWrites)?;
            finish_transition_tx(
                tx,
                input_scope,
                true,
                agent_id,
                &[AuditEvent::legacy(
                    "live_input_attached",
                    serde_json::json!({
                        "agent_id": agent_id, "turn_id": turn_id,
                        "activation_id": batch.activation_id, "safe_point": batch.safe_point,
                        "cutoff": batch.cutoff, "result_identities": batch.result_identities,
                        "message_ids": seen, "rendered_bytes": batch.rendered_bytes,
                        "max_bytes": batch.max_bytes, "max_items": batch.max_items,
                    }),
                )],
                &[],
                command.fault,
                PostCommitEffects {
                    agent_state: Some(command.agent_state.clone()),
                    turns: vec![turn],
                    transcript_entries,
                    current_queue_write_ids: command.queues.iter()
                        .map(|queue| queue.message_id.clone()).collect(),
                    task_write_keys: identities.into_iter().map(|id|
                        crate::runtime::resident_task::TaskKey::new(
                            crate::runtime::resident_task::TaskFamily::ResultSettlement, id,
                        )).collect(),
                    ..PostCommitEffects::default()
                },
            )
    }
}

fn live_identity_count(batch: &LiveInputBatchRecord) -> usize {
    batch.result_identities.len() + batch.ordinary_message_ids.len()
}

fn restore_operator_inputs_tx(
    tx: &Transaction<'_>,
    predecessor: &TurnRecord,
    attachment_turns: &BTreeSet<String>,
    turn: &mut TurnRecord,
) -> Result<bool> {
    let mut seen = BTreeSet::new();
    for message_id in &predecessor.input_message_ids {
        if !seen.insert(message_id) {
            return Ok(false);
        }
        if turn.input_message_ids.contains(message_id) {
            continue;
        }
        let payload: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM messages WHERE agent_id = ?1 AND message_id = ?2",
                params![turn.agent_id, message_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(message) = payload
            .map(|payload| serde_json::from_str::<MessageEnvelope>(&payload))
            .transpose()?
        else {
            return Ok(false);
        };
        if message.id != *message_id
            || message.agent_id != turn.agent_id
            || message.kind != MessageKind::OperatorPrompt
            || !matches!(message.origin, crate::types::MessageOrigin::Operator { .. })
            || message.authority_class != AuthorityClass::OperatorInstruction
            || message.priority != Priority::Interject
        {
            return Ok(false);
        }
        let interjected: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id = ?1
             AND message_id = ?2 AND status = 'interjected')",
            params![turn.agent_id, message_id],
            |row| row.get(0),
        )?;
        if !interjected {
            return Ok(false);
        }
        let expected = serde_json::json!({
            "authority_class": message.authority_class,
            "delivery_surface": message.delivery_surface,
            "admission_context": message.admission_context,
            "trigger_kind": message.trigger_kind,
            "work_item_id": message.work_item_id,
            "task_id": message.task_id,
            "source_refs": message.source_refs,
            "correlation_id": message.correlation_id,
            "causation_id": message.causation_id,
        });
        let entries: Vec<(String, String)> = tx
            .prepare(
                "SELECT transcript.payload_json, turn.payload_json
             FROM transcript_entries AS transcript
             JOIN turn_records AS turn ON turn.turn_id = transcript.turn_id
             WHERE transcript.agent_id = ?1 AND transcript.message_id = ?2
             AND transcript.kind = 'incoming_message'",
            )?
            .query_map(params![turn.agent_id, message_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut admitted = false;
        for (entry, source_turn) in entries {
            let entry: crate::types::TranscriptEntry = serde_json::from_str(&entry)?;
            let source_turn: TurnRecord = serde_json::from_str(&source_turn)?;
            if attachment_turns.contains(&source_turn.turn_id)
                && source_turn.agent_id == turn.agent_id
                && source_turn.effective_owner() == turn.effective_owner()
                && source_turn.input_message_ids.contains(message_id)
                && entry.agent_id == turn.agent_id
                && entry.kind == TranscriptEntryKind::IncomingMessage
                && entry.related_message_id.as_ref() == Some(message_id)
                && entry.round.is_some()
                && entry.data["turn_id"].as_str() == Some(source_turn.turn_id.as_str())
                && matches!(
                    entry.data["boundary"].as_str(),
                    Some(
                        "after_provider_round"
                            | "before_tool_execution"
                            | "after_tool_results"
                            | "before_provider_continuation"
                    )
                )
                && expected
                    .as_object()
                    .unwrap()
                    .iter()
                    .all(|(key, value)| entry.data.get(key) == Some(value))
            {
                admitted = true;
                break;
            }
        }
        if !admitted {
            return Ok(false);
        }
        turn.input_message_ids.push(message_id.clone());
    }
    Ok(true)
}

fn restore_initial_snapshot_tx(
    tx: &Transaction<'_>,
    current: &ExecutionAttempt,
    prior: &ExecutionAttempt,
    predecessor: &TurnRecord,
    source: &MessageEnvelope,
    ancestors: &BTreeMap<String, String>,
    turn: &mut TurnRecord,
) -> Result<bool> {
    if predecessor
        .trigger
        .as_ref()
        .and_then(|trigger| trigger.message_id.as_deref())
        != Some(source.id.as_str())
        || !predecessor.input_message_ids.contains(&source.id)
    {
        return Ok(false);
    }
    let Some(batch) = &predecessor.idle_input_batch else {
        return Ok(true);
    };
    let header = "Current input batch (receive order; each item retains its own authority and reply route):\n";
    if batch.activation_id.as_deref() != Some(prior.attempt_id.as_str())
        || batch.anchor_message_id != source.id
        || batch.items.first().map(|item| item.message_id.as_str()) != Some(source.id.as_str())
        || batch.max_items > 8
        || batch.items.len() > batch.max_items
        || batch.max_bytes > 24 * 1024
        || batch.rendered_bytes > batch.max_bytes
        || header.len()
            + batch
                .items
                .iter()
                .map(|item| item.rendered.len())
                .sum::<usize>()
            != batch.rendered_bytes
        || batch.cutoff > predecessor.created_at
    {
        return Ok(false);
    }
    let mut seen = BTreeSet::new();
    for item in &batch.items {
        if !seen.insert(item.message_id.clone())
            || !predecessor.input_message_ids.contains(&item.message_id)
            || item.source_ref != format!("message:{}", item.message_id)
        {
            return Ok(false);
        }
        let payload: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM messages WHERE message_id = ?1 AND agent_id = ?2",
                params![item.message_id, current.agent_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(message) = payload
            .map(|text| serde_json::from_str::<MessageEnvelope>(&text))
            .transpose()?
        else {
            return Ok(false);
        };
        if message.agent_id != current.agent_id || message.created_at > batch.cutoff {
            return Ok(false);
        }
        if item.message_id != source.id {
            let attached: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE message_id = ?1
                 AND agent_id = ?2 AND status = 'interjected')",
                params![item.message_id, current.agent_id],
                |row| row.get(0),
            )?;
            let Some(delivery) = delivery_by_message_id_tx(tx, &message.id)? else {
                return Ok(false);
            };
            if !attached
                || !crate::wake_contract::is_agent_message(&message)
                || message.kind == MessageKind::TaskResult
                || message.task_id.is_some()
                || message.priority != source.priority
                || message.priority == Priority::Interject
                || message.authority_class == AuthorityClass::OperatorInstruction
                || delivery.target_agent_id != current.agent_id
                || delivery.message_id.as_deref() != Some(message.id.as_str())
                || delivery.outcome != AgentMessageDeliveryOutcome::Accepted
                || delivery.state != AgentMessageDeliveryState::Consumed
                || delivery.routing_context != message.routing_context
                || delivery.caller.origin != message.origin
                || delivery.caller.authority_class != message.authority_class
                || Some(delivery.caller.delivery_surface) != message.delivery_surface
                || Some(delivery.caller.admission_context) != message.admission_context
                || !matches!(
                    delivery.caller.principal_kind,
                    AgentMessagePrincipalKind::PeerAgent
                        | AgentMessagePrincipalKind::SupervisingParent
                )
                || delivery
                    .caller
                    .caller_agent_id
                    .as_ref()
                    .is_none_or(|sender| {
                        sender.is_empty()
                            || message
                                .routing_context
                                .as_ref()
                                .and_then(|route| route.sender_agent_id.as_ref())
                                != Some(sender)
                    })
                || delivery
                    .turn_id
                    .as_ref()
                    .is_some_and(|id| !ancestors.values().any(|turn| turn == id))
                || delivery
                    .activation_id
                    .as_ref()
                    .is_some_and(|id| ancestors.get(id) != delivery.turn_id.as_ref())
            {
                return Ok(false);
            }
            if let Some(owner) = &message.work_item_id {
                if predecessor.effective_owner()
                    != (TurnOwner::WorkItem {
                        work_item_id: owner.clone(),
                    })
                {
                    return Ok(false);
                }
            } else {
                let Some(anchor) = delivery_by_message_id_tx(tx, &source.id)? else {
                    return Ok(false);
                };
                if anchor.caller.caller_agent_id != delivery.caller.caller_agent_id
                    || anchor.caller.principal_kind != delivery.caller.principal_kind
                    || anchor.caller.origin != delivery.caller.origin
                    || anchor.caller.authority_class != delivery.caller.authority_class
                    || anchor.caller.delivery_surface != delivery.caller.delivery_surface
                    || anchor.caller.admission_context != delivery.caller.admission_context
                    || batch.sender_agent_id
                        != delivery
                            .caller
                            .caller_agent_id
                            .as_deref()
                            .unwrap_or_default()
                {
                    return Ok(false);
                }
            }
        }
        if !turn.input_message_ids.contains(&item.message_id) {
            turn.input_message_ids.push(item.message_id.clone());
        }
    }
    let mut batch = batch.clone();
    batch.activation_id = Some(current.attempt_id.clone());
    turn.idle_input_batch = Some(batch);
    Ok(true)
}

fn live_binding_matches_tx(
    tx: &Transaction<'_>,
    command: &LiveInputAttachmentCommand,
) -> Result<bool> {
    canonical_input_binding_matches_tx(tx, &command.expected_attempt, &command.expected_turn)
}

pub(super) fn canonical_input_binding_matches_tx(
    tx: &Transaction<'_>,
    expected: &ExecutionAttempt,
    turn: &TurnRecord,
) -> Result<bool> {
    let agent_id = &expected.agent_id;
    let Some(agent) = agent_state_tx(tx, agent_id)? else {
        return Ok(false);
    };
    if agent.status != AgentStatus::AwakeRunning
        || agent.current_execution_binding.as_ref().is_none_or(|binding| {
            binding.activation_id.as_deref() != Some(expected.attempt_id.as_str())
                || binding.turn_id != turn.turn_id
                || binding.owner.as_ref() != Some(&turn.effective_owner())
                || expected.source_message_id.as_deref() != Some(binding.source_message_id.as_str())
                || !matches!(binding.admission_provenance.as_ref(),
                    Some(crate::types::ExecutionAdmissionProvenance::Canonical { activation_id, .. })
                        if activation_id == &expected.attempt_id)
        })
        || agent.current_turn_id.as_deref() != Some(turn.turn_id.as_str())
        || agent.current_run_id != turn.run_id
        || expected
            .run_id
            .as_ref()
            .is_some_and(|run| turn.run_id.as_ref() != Some(run))
    {
        return Ok(false);
    }
    let state = execution_protocol_repository::load_state_tx(tx, agent_id)?;
    let canonical_owner = match &expected.binding {
        ExecutionBinding::WorkItem { work_item_id } => TurnOwner::WorkItem {
            work_item_id: work_item_id.clone(),
        },
        ExecutionBinding::Conversation { interaction_id } => TurnOwner::Conversation {
            interaction_id: interaction_id.clone(),
        },
        ExecutionBinding::AgentLifecycle { agent_id } => TurnOwner::AgentLifecycle {
            agent_id: agent_id.clone(),
        },
        ExecutionBinding::Command => TurnOwner::Command,
    };
    if state.attempts.get(&expected.attempt_id) != Some(expected)
        || expected.state != ExecutionAttemptState::Open
        || expected.terminal_outcome_id.is_some()
        || expected.terminal_at.is_some()
        || canonical_owner != turn.effective_owner()
        || expected
            .source_message_id
            .as_ref()
            .is_none_or(|source| !turn.input_message_ids.contains(source))
    {
        return Ok(false);
    }
    let fences = authority_fences_tx(tx, agent_id)?;
    // State persistence advances control_revision; live binding and the CAS
    // guard control state, while authority_fences_tx rejects stopped identities.
    if fences.host_registry_revision != expected.admitted_fences.host_registry_revision {
        return Ok(false);
    }
    if let ExecutionBinding::WorkItem { work_item_id } = &expected.binding {
        if !state.work_items.get(work_item_id).is_some_and(|owner| {
            matches!(&owner.state, WorkItemExecutionState::InFlight { attempt_id, .. }
                if attempt_id == &expected.attempt_id)
                && Some(owner.generation()) == expected.admitted_fences.work_item_generation
        }) {
            return Ok(false);
        }
        let payload: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM work_items WHERE work_item_id = ?1",
                [work_item_id],
                |row| row.get(0),
            )
            .optional()?;
        if payload
            .map(|text| serde_json::from_str::<WorkItemRecord>(&text))
            .transpose()?
            .is_none_or(|owner| owner.agent_id != *agent_id || owner.state != WorkItemState::Open)
        {
            return Ok(false);
        }
    }
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
            [&turn.turn_id],
            |row| row.get(0),
        )
        .optional()?;
    if payload
        .map(|text| serde_json::from_str::<TurnRecord>(&text))
        .transpose()?
        .as_ref()
        != Some(turn)
        || turn.terminal.is_some()
    {
        return Ok(false);
    }
    let pending_report = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM tool_executions
         WHERE agent_id = ?1 AND json_extract(payload_json, '$.turn_id') = ?2
           AND json_extract(payload_json, '$.status') = 'deferred'
           AND json_extract(payload_json, '$.tool_name') IN ('WaitFor', 'CompleteWorkItem'))",
        params![agent_id, turn.turn_id],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(!pending_report)
}

fn ordinary_live_source_matches_tx(
    tx: &Transaction<'_>,
    command: &LiveInputAttachmentCommand,
    message: &MessageEnvelope,
) -> Result<bool> {
    if message.kind == MessageKind::TaskResult
        || message.task_id.is_some()
        || !crate::wake_contract::is_agent_message(message)
        || message.priority == Priority::Interject
        || message.authority_class == AuthorityClass::OperatorInstruction
    {
        return Ok(false);
    }
    let Some(delivery) = delivery_by_message_id_tx(tx, &message.id)? else {
        return Ok(false);
    };
    if delivery.target_agent_id != message.agent_id
        || delivery.message_id.as_deref() != Some(message.id.as_str())
        || delivery.outcome != AgentMessageDeliveryOutcome::Accepted
        || delivery.state != AgentMessageDeliveryState::Queued
        || delivery.activation_id.is_some()
        || delivery.turn_id.is_some()
        || delivery.routing_context != message.routing_context
        || delivery.caller.origin != message.origin
        || delivery.caller.authority_class != message.authority_class
        || Some(delivery.caller.delivery_surface) != message.delivery_surface
        || Some(delivery.caller.admission_context) != message.admission_context
        || !matches!(
            delivery.caller.principal_kind,
            AgentMessagePrincipalKind::PeerAgent | AgentMessagePrincipalKind::SupervisingParent
        )
        || delivery
            .caller
            .caller_agent_id
            .as_ref()
            .is_none_or(|sender| {
                sender.is_empty()
                    || message
                        .routing_context
                        .as_ref()
                        .and_then(|route| route.sender_agent_id.as_ref())
                        != Some(sender)
            })
    {
        return Ok(false);
    }
    if let Some(owner) = &message.work_item_id {
        return Ok(command.expected_turn.effective_owner()
            == TurnOwner::WorkItem {
                work_item_id: owner.clone(),
            });
    }
    let has_reply_relation: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE owner_agent_id = ?1
         AND kind = 'agent_message_wait' AND (status IN ('queued', 'running', 'cancelling')
           OR (status = 'completed' AND json_extract(payload_json, '$.detail.message_id') = ?2)))",
        params![message.agent_id, message.id],
        |row| row.get(0),
    )?;
    if has_reply_relation {
        return Ok(false);
    }
    let Some(batch) = &command.expected_turn.idle_input_batch else {
        return Ok(false);
    };
    let Some(anchor) = delivery_by_message_id_tx(tx, &batch.anchor_message_id)? else {
        return Ok(false);
    };
    // Compare the persisted sender/source boundary, not per-delivery receipt IDs.
    Ok(anchor.outcome == AgentMessageDeliveryOutcome::Accepted
        && anchor.target_agent_id == message.agent_id
        && anchor.caller.caller_agent_id == delivery.caller.caller_agent_id
        && anchor.caller.principal_kind == delivery.caller.principal_kind
        && anchor.caller.origin == delivery.caller.origin
        && anchor.caller.authority_class == delivery.caller.authority_class
        && anchor.caller.delivery_surface == delivery.caller.delivery_surface
        && anchor.caller.admission_context == delivery.caller.admission_context
        && batch.sender_agent_id
            == delivery
                .caller
                .caller_agent_id
                .as_deref()
                .unwrap_or_default()
        && command
            .expected_turn
            .input_message_ids
            .contains(&batch.anchor_message_id))
}
