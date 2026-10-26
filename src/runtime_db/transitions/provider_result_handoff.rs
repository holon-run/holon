//! Provider recovery inherits only its validated predecessor's admitted input.
use super::*;
use crate::domain::execution_protocol::{ExecutionAttempt, ExecutionAttemptState};
use crate::runtime::provider_recovery;

#[derive(Debug, serde::Serialize)]
pub(crate) struct ProviderResultHandoffCommand {
    pub expected_attempt: ExecutionAttempt,
    pub expected_turn: TurnRecord,
    pub at: chrono::DateTime<Utc>,
    #[serde(skip)]
    pub fault: Option<TransitionFaultPoint>,
}

fn message_tx(tx: &Transaction<'_>, id: &str) -> Result<Option<MessageEnvelope>> {
    tx.query_row(
        "SELECT payload_json FROM messages WHERE message_id = ?1",
        [id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|text| serde_json::from_str(&text).map_err(Into::into))
    .transpose()
}

fn turn_tx(tx: &Transaction<'_>, id: &str) -> Result<Option<TurnRecord>> {
    tx.query_row(
        "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
        [id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|text| serde_json::from_str(&text).map_err(Into::into))
    .transpose()
}

impl RuntimeTransitionRepository<'_> {
    #[cfg(test)]
    pub(crate) fn commit_provider_result_handoff(
        &self,
        expected: &ExecutionAttempt,
        expected_turn: &TurnRecord,
        now: chrono::DateTime<Utc>,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<TransitionCommit> {
        self.db.transaction(|tx| self.commit_provider_result_handoff_tx(
            tx, None, expected, expected_turn, now, fault, None,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn commit_provider_result_handoff_tx(
        &self,
        tx: &Transaction<'_>,
        input_scope: Option<&InputEvidenceScope>,
        expected: &ExecutionAttempt,
        expected_turn: &TurnRecord,
        now: chrono::DateTime<Utc>,
        fault: Option<TransitionFaultPoint>,
        allowed_results: Option<&BTreeSet<crate::runtime::resident_task::TaskKey>>,
    ) -> Result<TransitionCommit> {
            if !live_input_attachment::canonical_input_binding_matches_tx(tx, expected, expected_turn)? {
                return Ok(TransitionCommit::default());
            }
            let agent_id = &expected.agent_id;
            let state = execution_protocol_repository::load_state_tx(tx, agent_id)?;
            let Some(message) = expected.source_message_id.as_deref()
                .map(|id| message_tx(tx, id)).transpose()?.flatten() else {
                return Ok(TransitionCommit::default());
            };
            let source = provider_recovery::resolve_with(
                &message, &state, |id| message_tx(tx, id), |id| turn_tx(tx, id),
            )?;
            if expected.binding != source.predecessor.binding {
                return Ok(TransitionCommit::default());
            }
            let mut predecessors: BTreeSet<_> = source.predecessors.iter()
                .map(|attempt| attempt.attempt_id.clone()).collect();
            let mut sources = source.source_turns.clone();

            // A daemon restart can interrupt the fallback before its request or
            // terminal commit. Validate those same-message edges separately;
            // they do not relax the cross-message provider lineage.
            let mut cursor = expected.recovery_of_attempt_id.as_ref();
            let mut younger = expected_turn.clone();
            let mut interrupted = Vec::new();
            while cursor != Some(&source.predecessor.attempt_id) {
                let Some(id) = cursor else { return Ok(TransitionCommit::default()) };
                let Some(prior) = state.attempts.get(id) else { return Ok(TransitionCommit::default()) };
                if predecessors.len() >= 128 || !predecessors.insert(id.clone())
                    || prior.state != ExecutionAttemptState::Interrupted
                    || prior.agent_id != *agent_id || prior.binding != expected.binding
                    || prior.source_message_id != expected.source_message_id
                {
                    return Ok(TransitionCommit::default());
                }
                let Some(turn) = crate::runtime_db::task_result_settlement::recovery_turn_for_activation_tx(
                    tx, prior, Some(&younger),
                )? else {
                    // A first fallback claim can crash before creating its turn.
                    // Absence is usable only for that exact canonical empty edge.
                    if !source.is_initial_attempt(&message, prior)
                        || prior.turn_id != message.turn_id
                        || prior.source != expected.source
                        || prior.provenance != expected.provenance
                        || younger.replay.as_ref().is_some_and(|replay| {
                            replay.source_message_id != message.id
                                || Some(&replay.source_turn_id) != prior.turn_id.as_ref()
                                || replay.reason != "interrupted_queue_claim_reentry"
                                || replay.prior_terminal.is_some()
                        })
                        || (younger.replay.is_none() && prior.turn_id.is_some())
                    {
                        return Ok(TransitionCommit::default());
                    }
                    let has_input: bool = tx.query_row(
                        "SELECT EXISTS(
                           SELECT 1 FROM turn_records WHERE agent_id = ?1 AND (
                             (turn_id != ?4 AND json_extract(payload_json, '$.trigger.message_id') = ?2)
                             OR json_extract(payload_json, '$.idle_input_batch.activation_id') = ?3
                             OR EXISTS(SELECT 1 FROM json_each(payload_json, '$.live_input_batches') batch
                               WHERE json_extract(batch.value, '$.activation_id') = ?3)))
                         OR EXISTS(SELECT 1 FROM task_result_settlements
                           WHERE agent_id = ?1 AND activation_id = ?3)",
                        params![agent_id, message.id, prior.attempt_id, expected_turn.turn_id],
                        |row| row.get(0),
                    )?;
                    if has_input {
                        return Ok(TransitionCommit::default());
                    }
                    cursor = prior.recovery_of_attempt_id.as_ref();
                    continue;
                };
                if younger.replay.as_ref().is_none_or(|replay| {
                    replay.source_turn_id != turn.turn_id
                        || Some(&replay.source_message_id) != expected.source_message_id.as_ref()
                        || replay.reason != "interrupted_queue_claim_reentry"
                }) || turn.terminal.as_ref().map(|terminal| terminal.kind) != Some(TurnTerminalKind::Interrupted) {
                    return Ok(TransitionCommit::default());
                }
                interrupted.push(turn.clone());
                younger = turn;
                cursor = prior.recovery_of_attempt_id.as_ref();
            }
            interrupted.reverse();
            sources.extend(interrupted);

            let mut turn = expected_turn.clone();
            // Preserve cumulative P2 budgets and immutable safe points. Context
            // already renders initial snapshots from validated source turns.
            for source_turn in &sources {
                for batch in &source_turn.live_input_batches {
                    if let Some(old) = turn.live_input_batches.iter()
                        .find(|old| old.safe_point == batch.safe_point) {
                        let mut comparable = batch.clone();
                        comparable.activation_id = old.activation_id.clone();
                        if old != &comparable {
                            return Ok(TransitionCommit::default());
                        }
                        continue;
                    }
                    let mut batch = batch.clone();
                    batch.activation_id = expected.attempt_id.clone();
                    turn.live_input_batches.push(batch);
                }
                for id in &source_turn.input_message_ids {
                    if !turn.input_message_ids.contains(id) {
                        turn.input_message_ids.push(id.clone());
                    }
                }
            }
            let live = &turn.live_input_batches;
            if live.len() > 4
                || live.iter().map(|batch| batch.result_identities.len() + batch.ordinary_message_ids.len()).sum::<usize>() > 32
                || live.iter().map(|batch| batch.rendered_bytes).sum::<usize>() > 96 * 1024
                || live.iter().any(|batch| batch.max_items > 8 || batch.max_bytes > 24 * 1024
                    || batch.rendered_bytes != batch.rendered_text().len()
                    || batch.rendered_bytes > batch.max_bytes)
            {
                return Ok(TransitionCommit::default());
            }
            inject_fault(fault, TransitionFaultPoint::AfterValidation)?;
            let mut changed_results = BTreeSet::new();
            for predecessor in &predecessors {
                let mut query = tx.prepare(
                    "SELECT result_identity FROM task_result_settlements
                     WHERE agent_id=?1 AND activation_id=?2 AND state='caller_admitted'
                     ORDER BY result_identity",
                )?;
                for id in query.query_map(params![agent_id, predecessor], |row| row.get::<_, String>(0))? {
                    let key = crate::runtime::resident_task::TaskKey::new(
                        crate::runtime::resident_task::TaskFamily::ResultSettlement, id?,
                    );
                    if let Some(allowed) = allowed_results {
                        anyhow::ensure!(
                            allowed.contains(&key),
                            "provider Result is outside its original Task proof"
                        );
                    }
                    changed_results.insert(key);
                }
            }
            let transferred = crate::runtime_db::task_result_settlement::transfer_provider_admitted_tx(
                tx, agent_id, &predecessors, &expected.attempt_id, now,
            )?;
            if changed_results.is_empty() && &turn == expected_turn {
                return Ok(TransitionCommit { applied: true, ..Default::default() });
            }
            upsert_turn_record_tx(tx, &turn)?;
            inject_fault(fault, TransitionFaultPoint::AfterCanonicalWrites)?;
            finish_transition_tx(tx, input_scope, true, agent_id, &[AuditEvent::legacy(
                "provider_task_results_transferred",
                serde_json::json!({
                    "agent_id": agent_id, "activation_id": expected.attempt_id,
                    "turn_id": turn.turn_id, "predecessor_activation_ids": predecessors,
                    "source_turn_ids": sources.iter().map(|turn| &turn.turn_id).collect::<Vec<_>>(),
                    "result_identities": transferred.iter().map(|record| &record.result_identity).collect::<Vec<_>>(),
                }),
            )], &[], fault, PostCommitEffects {
                turns: vec![turn],
                task_write_keys: changed_results,
                ..PostCommitEffects::default()
            })
    }
}
