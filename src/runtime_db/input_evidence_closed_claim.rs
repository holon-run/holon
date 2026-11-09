//! Original cutover observation of a closed claim that never installed a Turn.

use super::*;
use crate::domain::execution_protocol::{
    ConversationOutcome, ExecutionOutcome, ExecutionOutcomeRecord, WorkItemOutcome,
};

pub(super) fn original_observation(
    tx: &Transaction<'_>,
    attempt: &ExecutionAttempt,
    state: Option<&AgentState>,
) -> Result<Option<ClosedClaimObservation>> {
    if !matches!(
        attempt.state,
        ExecutionAttemptState::Interrupted | ExecutionAttemptState::Settled
    ) || attempt.run_id.is_some()
    {
        return Ok(None);
    }
    let (Some(source), Some(outcome_id)) =
        (&attempt.source_message_id, &attempt.terminal_outcome_id)
    else {
        return Ok(None);
    };
    let tables: i64 = tx.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('execution_protocol_work_items','execution_protocol_attempts','execution_protocol_outcomes','queue_entries','messages','conversation_input_assignments','agent_states')",
        [], |row|row.get(0),
    )?;
    if tables != 7 {
        return Ok(None);
    }
    let normalized: Option<(String,Option<String>,u64,String)> = tx.query_row(
        "SELECT lifecycle_state,terminal_outcome_id,source_generation,source_identity_json FROM execution_protocol_attempts WHERE agent_id=?1 AND attempt_id=?2",
        params![attempt.agent_id,attempt.attempt_id], |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).optional()?;
    let Some((lifecycle, outcome, generation, identity)) = normalized else {
        return Ok(None);
    };
    ensure!(
        Some(lifecycle.as_str()) == serde_json::to_value(attempt.state)?.as_str()
            && outcome.as_ref() == Some(outcome_id)
            && generation == attempt.source.generation
            && serde_json::from_str::<crate::domain::execution_protocol::ExecutionSourceIdentity>(
                &identity
            )? == attempt.source.identity,
        "closed claim Attempt projection mismatch"
    );
    if state.is_some_and(|state| {
        attempt
            .turn_id
            .as_ref()
            .is_some_and(|id| state.current_turn_id.as_ref() == Some(id))
            || state
                .current_execution_binding
                .as_ref()
                .is_some_and(|binding| {
                    binding.activation_id.as_ref() == Some(&attempt.attempt_id)
                        || &binding.source_message_id == source
                        || attempt.turn_id.as_ref() == Some(&binding.turn_id)
                })
    }) || source_assignment(tx, attempt)?.is_some()
    {
        return Ok(None);
    }
    let has_owner: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM execution_protocol_work_items WHERE agent_id=?1 AND json_extract(payload_json,'$.state.attempt_id')=?2)",
        params![attempt.agent_id, attempt.attempt_id], |row|row.get(0),
    )?;
    if has_owner {
        return Ok(None);
    }
    let queue: Option<(String, String)> = tx
        .query_row(
            "SELECT status,payload_json FROM queue_entries WHERE agent_id=?1 AND message_id=?2",
            params![attempt.agent_id, source],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((status, payload)) = queue else {
        return Ok(None);
    };
    let queue: QueueEntryRecord = serde_json::from_str(&payload)?;
    ensure!(
        queue.agent_id == attempt.agent_id
            && queue.message_id == *source
            && serde_json::to_value(&queue.status)?.as_str() == Some(status.as_str()),
        "closed claim Queue projection mismatch"
    );
    if !matches!(
        queue.status,
        QueueEntryStatus::Dropped | QueueEntryStatus::Aborted | QueueEntryStatus::Quarantined
    ) {
        return Ok(None);
    }
    let message: Option<(String, Option<u64>)> = tx
        .query_row(
            "SELECT payload_json,message_seq FROM messages WHERE agent_id=?1 AND evidence_id=?2",
            params![attempt.agent_id, source],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((message, sequence)) = message else {
        return Ok(None);
    };
    let message: MessageEnvelope = serde_json::from_str(&message)?;
    ensure!(
        message.agent_id == attempt.agent_id && message.id == *source,
        "closed claim Message projection mismatch"
    );
    if message.message_seq != Some(attempt.source.generation)
        || message.message_seq != sequence
        || attempt.admitted_fences.source_revision != attempt.source.generation
        || (message.turn_id != attempt.turn_id
            && !isolated_delivery_hint_tx(tx, attempt, &message)?)
    {
        return Ok(None);
    }
    let payload: Option<String> = tx.query_row(
        "SELECT payload_json FROM execution_protocol_outcomes WHERE agent_id=?1 AND attempt_id=?2 AND outcome_id=?3",
        params![attempt.agent_id,attempt.attempt_id,outcome_id], |row|row.get(0),
    ).optional()?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    let outcome: ExecutionOutcomeRecord = serde_json::from_str(&payload)?;
    ensure!(
        outcome.attempt_id == attempt.attempt_id && outcome.outcome_id == *outcome_id,
        "closed claim Outcome projection mismatch"
    );
    let binding_matches = match (&attempt.binding, &outcome.outcome) {
        (ExecutionBinding::AgentLifecycle { agent_id }, ExecutionOutcome::Conversation(_)) => {
            agent_id == &attempt.agent_id
        }
        (ExecutionBinding::Conversation { .. }, ExecutionOutcome::Conversation(_))
        | (ExecutionBinding::WorkItem { .. }, ExecutionOutcome::WorkItem(_))
        | (ExecutionBinding::Command, ExecutionOutcome::Command(_)) => true,
        _ => false,
    };
    let terminal_matches = attempt.state == ExecutionAttemptState::Settled
        || matches!(
            outcome.outcome,
            ExecutionOutcome::Conversation(ConversationOutcome::Interrupted { .. })
                | ExecutionOutcome::WorkItem(WorkItemOutcome::Interrupted { .. })
        );
    Ok(
        (binding_matches && terminal_matches).then(|| ClosedClaimObservation {
            state: attempt.state,
            outcome_id: outcome_id.clone(),
        }),
    )
}
