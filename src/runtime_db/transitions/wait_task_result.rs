use anyhow::Result;
use rusqlite::{params, OptionalExtension, Transaction};

use super::{RuntimeStateTransitionConflict, WaitTaskResultAdmission};
use crate::{
    runtime_db::{
        task_result_settlement::{
            classify_obligation_tx, dropped_reply_source_tx, DroppedReplySource,
            TaskResultObligation,
        },
        TaskResultSettlementRecord,
    },
    types::{
        MessageEnvelope, MessageOrigin, QueueEntryRecord, QueueEntryStatus, WaitConditionRecord,
    },
};

#[derive(Debug, Clone)]
pub(crate) struct WaitTaskResultObligation {
    pub expected: TaskResultSettlementRecord,
    pub current_activation_id: Option<String>,
    pub waiter_work_item_id: Option<String>,
    pub canonical_recovery: bool,
    pub source_message_id: String,
    pub source_queue: Option<QueueEntryRecord>,
    pub dropped_reply_source: Option<DroppedReplySource>,
    pub wait_conditions: Vec<WaitConditionRecord>,
}

pub(super) fn validate_obligation_tx(
    tx: &Transaction<'_>,
    admission: &WaitTaskResultAdmission,
) -> Result<()> {
    let guard = &admission.obligation;
    let conflict = || {
        RuntimeStateTransitionConflict::concurrent_mutation(
            "task_result_obligation",
            &guard.expected.result_identity,
        )
    };
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM task_result_settlements WHERE result_identity = ?1",
            [&guard.expected.result_identity],
            |row| row.get(0),
        )
        .optional()?;
    if payload
        .map(|payload| serde_json::from_str::<TaskResultSettlementRecord>(&payload))
        .transpose()?
        .as_ref()
        != Some(&guard.expected)
        || classify_obligation_tx(
            tx,
            &guard.expected,
            guard.current_activation_id.as_deref(),
            guard.waiter_work_item_id.as_deref(),
        )? != TaskResultObligation::Recoverable
    {
        return Err(conflict().into());
    }
    let mut statement =
        tx.prepare("SELECT payload_json FROM wait_conditions WHERE agent_id = ?1")?;
    let mut waits = statement
        .query_map([&guard.expected.agent_id], |row| row.get::<_, String>(0))?
        .map(|payload| Ok(serde_json::from_str::<WaitConditionRecord>(&payload?)?))
        .collect::<Result<Vec<_>>>()?;
    let mut expected_waits = guard.wait_conditions.clone();
    waits.sort_by(|a, b| a.id.cmp(&b.id));
    expected_waits.sort_by(|a, b| a.id.cmp(&b.id));
    if waits != expected_waits {
        return Err(conflict().into());
    }
    if !guard.canonical_recovery {
        return Ok(());
    }
    if let Some(expected) = &guard.dropped_reply_source {
        if dropped_reply_source_tx(tx, &guard.expected)?.as_ref() != Some(expected) {
            return Err(conflict().into());
        }
    }
    // Closing the captured owner need not mutate the waiter or agent CAS.
    // Recheck both owners in the transaction that publishes the repaired wait.
    for owner in [
        guard.expected.work_item_id.as_deref(),
        guard.waiter_work_item_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_items
             WHERE agent_id = ?1 AND work_item_id = ?2 AND state = 'open')",
            params![guard.expected.agent_id, owner],
            |row| row.get::<_, bool>(0),
        )? {
            return Err(conflict().into());
        }
    }
    let source_queue: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
            [&guard.source_message_id],
            |row| row.get(0),
        )
        .optional()?;
    if source_queue
        .map(|payload| serde_json::from_str::<QueueEntryRecord>(&payload))
        .transpose()?
        != guard.source_queue
    {
        return Err(conflict().into());
    }
    // Enqueue and WaitFor repair share the canonical owner's wake identity.
    // A concurrent scanner wake must force re-preparation, not duplicate it.
    let mut statement = tx.prepare(
        "SELECT m.payload_json, q.payload_json FROM messages m
         JOIN queue_entries q ON q.message_id = m.message_id WHERE m.agent_id = ?1",
    )?;
    let rows = statement.query_map([&guard.expected.agent_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (message, queue) = row?;
        let message: MessageEnvelope = serde_json::from_str(&message)?;
        let queue: QueueEntryRecord = serde_json::from_str(&queue)?;
        if matches!(&message.origin, MessageOrigin::System { subsystem } if subsystem == "task_result_recovery")
            && message.work_item_id == admission.message.work_item_id
            && message.source_refs.get("task_result_owner_key")
                == admission.message.source_refs.get("task_result_owner_key")
            && message.id != admission.message.id
            && !matches!(
                queue.status,
                QueueEntryStatus::Processed
                    | QueueEntryStatus::Aborted
                    | QueueEntryStatus::Dropped
                    | QueueEntryStatus::Quarantined
            )
        {
            return Err(conflict().into());
        }
    }
    Ok(())
}
