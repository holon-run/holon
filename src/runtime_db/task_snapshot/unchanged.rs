//! Borrowed observations for writers that do not change any Task dependency.
use super::*;
use crate::types::{MessageEnvelope, QueueEntryRecord};

/// No coverage decode, capture, body copy or new selection is needed.
pub(in crate::runtime_db) fn resident_is_current_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    resident: &ResidentTaskDomain,
    request: &TaskDomainLoadRequest,
) -> Result<bool> {
    let coverage = resident.coverage();
    let query = &request.query;
    Ok(resident.scope() == scope
        && coverage.active_complete
        && coverage.unsettled_complete
        && coverage.recovery_sources.is_empty()
        && request.recovery_at.is_none()
        && request.capture.recovery_source_ids.is_empty()
        && (coverage.terminal.is_exhaustive
            || request.capture.terminal_limit <= coverage.terminal.limit)
        && request
            .capture
            .required_keys
            .is_subset(&coverage.required_keys)
        && query.required_keys.is_subset(&coverage.required_keys)
        && query
            .output_task_ids
            .iter()
            .all(|id| coverage.output.heads.contains_key(id))
        && query
            .reply_message_ids
            .iter()
            .all(|id| coverage.query.reply_task_ids.contains_key(id))
        && query
            .parent_turn_ids
            .iter()
            .all(|id| coverage.query.parent_turn_tasks.contains_key(id))
        && query
            .birth_after
            .is_none_or(|after| coverage.query.birth_after == Some(after))
        && loader::current_revision(tx, scope)? == Some(coverage.revision))
}

/// Delivery may update header revision/digest, but not the global Output order,
/// a Task Source, or Result reply metadata. All SQL observations are fixed keys.
pub(in crate::runtime_db) fn message_is_unrelated_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    message: &MessageEnvelope,
) -> Result<bool> {
    if message.agent_id != scope.agent_id
        || query_sources::message_source_task_id(message).is_some()
    {
        return Ok(false);
    }
    Ok(tx.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM task_message_order_headers h
            JOIN messages m ON m.agent_id=h.agent_id AND m.evidence_id=h.evidence_id
            WHERE h.agent_id=?1 AND h.identity_incarnation=?2 AND h.evidence_id=?3
              AND h.message_id IS m.message_id AND h.message_seq IS m.message_seq
              AND h.created_at=m.created_at)
          AND NOT EXISTS(SELECT 1 FROM task_message_sources
            WHERE agent_id=?1 AND identity_incarnation=?2 AND source_id=?3)
          AND NOT EXISTS(SELECT 1 FROM task_result_settlements WHERE message_id=?3)",
        params![scope.agent_id, scope.identity_incarnation, message.id],
        |row| row.get(0),
    )?)
}

/// Running observes membership, not transitions inside its live-status set.
pub(in crate::runtime_db) fn queue_stays_live_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    queue: &QueueEntryRecord,
) -> Result<bool> {
    if queue.agent_id != scope.agent_id
        || queue.status != crate::types::QueueEntryStatus::Interjected
    {
        return Ok(false);
    }
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id=?1 AND message_id=?2
          AND status IN ('queued','interrupted','dequeued','interjected'))",
        params![scope.agent_id, queue.message_id],
        |row| row.get(0),
    )?)
}
