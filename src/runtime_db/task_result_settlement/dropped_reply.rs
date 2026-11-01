//! A canonical ingress rejection leaves an obligation, not a consumed reply.

use anyhow::Result;
use rusqlite::{params, OptionalExtension, Transaction};

use super::{record_matches_durable_task_tx, TaskResultSettlementRecord};
use crate::types::{
    AgentMessageDeliveryOutcome, AgentMessageDeliveryRecord, AgentMessageDeliveryState,
    MessageEnvelope, QueueEntryRecord, QueueEntryStatus, TaskKind, TaskRecord, TaskStatus,
};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct DroppedReplySource {
    pub(super) task: TaskRecord,
    pub(super) result: MessageEnvelope,
    pub(super) reply: MessageEnvelope,
    pub(super) queue: QueueEntryRecord,
    pub(super) delivery: AgentMessageDeliveryRecord,
    pub(super) rejection_id: String,
    pub(super) rejection_data: serde_json::Value,
}

pub(super) const CANONICAL_REJECTION_SQL: &str =
    "SELECT audit_event_id, data_json FROM audit_events
         WHERE kind = 'scheduler_authority_input_rejected'
           AND json_extract(data_json, '$.data.agent_id') = ?1
           AND json_extract(data_json, '$.data.message_id') = ?2
           AND json_extract(data_json, '$.data.reason') =
               'canonical_internal_followup_work_item_not_runnable'
           AND json_extract(data_json, '$.data.queue_disposition') = 'dropped'
         ORDER BY rowid DESC LIMIT 1";

fn load<T: serde::de::DeserializeOwned>(
    tx: &Transaction<'_>,
    sql: &str,
    id: &str,
) -> Result<Option<T>> {
    tx.query_row(sql, [id], |row| row.get::<_, String>(0))
        .optional()?
        .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
        .transpose()
}

pub(crate) fn dropped_reply_source_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
) -> Result<Option<DroppedReplySource>> {
    if !record_matches_durable_task_tx(tx, record)? {
        return Ok(None);
    }
    let Some(result): Option<MessageEnvelope> = load(
        tx,
        "SELECT payload_json FROM messages WHERE message_id = ?1",
        &record.message_id,
    )?
    else {
        return Ok(None);
    };
    let Some(reply_id) = crate::wake_contract::agent_message_reply_reference(&result) else {
        return Ok(None);
    };
    let Some(reply): Option<MessageEnvelope> = load(
        tx,
        "SELECT payload_json FROM messages WHERE message_id = ?1",
        reply_id,
    )?
    else {
        return Ok(None);
    };
    let Some(task): Option<TaskRecord> = load(
        tx,
        "SELECT payload_json FROM tasks WHERE task_id = ?1",
        &record.task_id,
    )?
    else {
        return Ok(None);
    };
    let Some(queue): Option<QueueEntryRecord> = load(
        tx,
        "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
        reply_id,
    )?
    else {
        return Ok(None);
    };
    let delivery_id = reply
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("agent_message_delivery"))
        .and_then(|delivery| delivery.get("delivery_id"))
        .and_then(serde_json::Value::as_str);
    let Some(delivery_id) = delivery_id else {
        return Ok(None);
    };
    let Some(delivery): Option<AgentMessageDeliveryRecord> = load(
        tx,
        "SELECT payload_json FROM agent_message_deliveries WHERE delivery_id = ?1",
        delivery_id,
    )?
    else {
        return Ok(None);
    };
    if !source_matches(record, &task, &result, &reply, &queue, &delivery) {
        return Ok(None);
    }
    // A terminal queue enum alone is not authority to bypass failed delivery.
    // Missing/reclaimed rejection evidence deliberately makes repair unavailable.
    let rejection: Option<(String, String)> = tx
        .query_row(
            CANONICAL_REJECTION_SQL,
            params![record.agent_id, reply_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((rejection_id, rejection_data)) = rejection else {
        return Ok(None);
    };
    Ok(Some(DroppedReplySource {
        task,
        result,
        reply,
        queue,
        delivery,
        rejection_id,
        rejection_data: serde_json::from_str(&rejection_data)?,
    }))
}

pub(super) fn source_matches(
    record: &TaskResultSettlementRecord,
    task: &TaskRecord,
    result: &MessageEnvelope,
    reply: &MessageEnvelope,
    queue: &QueueEntryRecord,
    delivery: &AgentMessageDeliveryRecord,
) -> bool {
    let Some(reply_id) = crate::wake_contract::agent_message_reply_reference(result) else {
        return false;
    };
    let Some(delivery_id) = reply
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("agent_message_delivery"))
        .and_then(|delivery| delivery.get("delivery_id"))
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    !(result.agent_id != record.agent_id
        || result.work_item_id != record.work_item_id
        || result.task_id.as_deref() != Some(record.task_id.as_str())
        || task.kind != TaskKind::AgentMessageWait
        || task.status != TaskStatus::Completed
        || task
            .detail
            .as_ref()
            .and_then(|detail| detail.get("message_id"))
            .and_then(serde_json::Value::as_str)
            != Some(reply_id)
        || task
            .detail
            .as_ref()
            .and_then(|detail| detail.get("response_delivery_id"))
            .and_then(serde_json::Value::as_str)
            != Some(delivery_id)
        || result
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("task_detail"))
            != task.detail.as_ref()
        || !crate::wake_contract::is_agent_message(reply)
        || reply.id != reply_id
        || reply.agent_id != record.agent_id
        || !reply.routing_context.as_ref().is_some_and(|route| {
            route.message_id == reply.id && route.recipient_agent_id == record.agent_id
        })
        || reply.origin != delivery.caller.origin
        || reply.authority_class != delivery.caller.authority_class
        || reply.delivery_surface != Some(delivery.caller.delivery_surface)
        || reply.admission_context != Some(delivery.caller.admission_context)
        || delivery.message_id.as_deref() != Some(reply_id)
        || delivery.target_agent_id != record.agent_id
        || delivery.routing_context != reply.routing_context
        || delivery.outcome != AgentMessageDeliveryOutcome::Accepted
        || delivery.state != AgentMessageDeliveryState::Failed
        || delivery.retryable
        || delivery.activation_id.is_some()
        || delivery.turn_id.is_some()
        || queue.message_id != reply_id
        || queue.agent_id != record.agent_id
        || queue.status != QueueEntryStatus::Dropped)
}

/// Recheck the terminal source in the transaction that admits rendered bytes.
#[cfg(test)]
pub(super) fn terminal_source_admits_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
    attempt: Option<&crate::domain::execution_protocol::ExecutionAttempt>,
    waiter_work_item_id: Option<&str>,
    reply_id: &str,
) -> Result<bool> {
    let Some(queue): Option<QueueEntryRecord> = load(
        tx,
        "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
        reply_id,
    )?
    else {
        return Ok(false);
    };
    match queue.status {
        QueueEntryStatus::Quarantined => Ok(false),
        QueueEntryStatus::Aborted => {
            Ok(super::live_turn::closed_admitted_reply_owner_tx(tx, record)?.is_some())
        }
        QueueEntryStatus::Dropped => {
            let Some(attempt) = attempt else {
                return Ok(false);
            };
            Ok(dropped_reply_source_tx(tx, record)?.is_some()
                && super::exact_waiter::admits_tx(tx, record, attempt, waiter_work_item_id)?)
        }
        _ => Ok(true),
    }
}

pub(super) fn exact_source_is_available_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
) -> Result<bool> {
    let Some(result): Option<MessageEnvelope> = load(
        tx,
        "SELECT payload_json FROM messages WHERE message_id = ?1",
        &record.message_id,
    )?
    else {
        return Ok(false);
    };
    let Some(reply_id) = crate::wake_contract::agent_message_reply_reference(&result) else {
        return Ok(true);
    };
    let Some(queue): Option<QueueEntryRecord> = load(
        tx,
        "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
        reply_id,
    )?
    else {
        return Ok(false);
    };
    match queue.status {
        QueueEntryStatus::Dropped => Ok(dropped_reply_source_tx(tx, record)?.is_some()),
        QueueEntryStatus::Quarantined => Ok(false),
        QueueEntryStatus::Aborted => {
            Ok(super::live_turn::closed_admitted_reply_owner_tx(tx, record)?.is_some())
        }
        _ => Ok(true),
    }
}
