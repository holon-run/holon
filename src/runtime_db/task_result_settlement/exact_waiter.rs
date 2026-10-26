//! A repaired exact wait grants its original identity, never its captured owner.

use anyhow::Result;
use rusqlite::{params, OptionalExtension, Transaction};

use super::{
    classify_obligation_tx, reply_admission_attempt_tx, TaskResultObligation,
    TaskResultSettlementRecord,
};
use crate::{
    domain::execution_protocol::{
        ExecutionAttempt, ExecutionAttemptState, ExecutionBinding, ExecutionSourceIdentity,
        ExecutionWaitIngress,
    },
    types::{
        AdmissionContext, AuthorityClass, MessageDeliverySurface, MessageEnvelope, MessageKind,
        MessageOrigin, QueueEntryStatus, WaitConditionKind, WaitConditionRecord,
        WaitConditionStatus, WakeSource,
    },
};

fn source_matches(source: &MessageEnvelope, record: &TaskResultSettlementRecord) -> bool {
    source.agent_id == record.agent_id
        && source.kind == MessageKind::InternalFollowup
        && matches!(&source.origin, MessageOrigin::System { subsystem } if subsystem == "task_result_recovery")
        && source.authority_class == AuthorityClass::RuntimeInstruction
        && source.delivery_surface == Some(MessageDeliverySurface::RuntimeSystem)
        && source.admission_context == Some(AdmissionContext::RuntimeOwned)
        && source.source_refs.get("task_result_message_id") == Some(&record.message_id)
        && source.source_refs.get("task_result_identity") == Some(&record.result_identity)
        && source.source_refs.get("task_result_parent_turn_id") == Some(&record.parent_turn_id)
        && source.source_refs.get("task_result_rejoin_generation")
            == Some(&record.rejoin_generation.to_string())
}

fn load_source_tx(tx: &Transaction<'_>, id: &str) -> Result<Option<MessageEnvelope>> {
    tx.query_row(
        "SELECT payload_json FROM messages WHERE message_id = ?1",
        [id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
    .transpose()
}

/// A failed consuming continuation retains its exact promise. A successor must
/// use a fresh wait rather than reopen the terminal predecessor.
pub(super) fn closed_continuation_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
) -> Result<Option<WaitConditionRecord>> {
    let Some((attempt, turn)) = super::live_turn::closed_recovery_admission_tx(tx, record)? else {
        return Ok(None);
    };
    let ExecutionSourceIdentity::TriggeredWait {
        wait_id,
        trigger_message_id,
        ingress: Some(ExecutionWaitIngress::RuntimeOwnedInternalFollowup),
    } = &attempt.source.identity
    else {
        return Ok(None);
    };
    let Some(source) = load_source_tx(tx, trigger_message_id)? else {
        return Ok(None);
    };
    let Some(wait) = exact_wait_for_source_tx(tx, &source, Some(wait_id))? else {
        return Ok(None);
    };
    if wait.status != WaitConditionStatus::Resolved
        || source.id != *trigger_message_id
        || source.work_item_id != wait.work_item_id
        || wait.trigger_message_id() != Some(trigger_message_id.as_str())
        || wait.work_item_id.as_deref() != turn.effective_owner().work_item_id()
        || !wait_claims_result_tx(tx, &wait, record)?
        || classify_obligation_tx(tx, record, None, wait.work_item_id.as_deref())?
            != TaskResultObligation::Recoverable
        || tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM wait_conditions newer
             WHERE newer.agent_id = ?1 AND newer.work_item_id IS ?2
               AND newer.rowid > (SELECT rowid FROM wait_conditions WHERE wait_condition_id = ?3))",
            params![record.agent_id, wait.work_item_id, wait.id],
            |row| row.get::<_, bool>(0),
        )?
        || !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_states WHERE agent_id = ?1 AND status != 'stopped')",
            [&record.agent_id],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Ok(None);
    }
    for owner in [record.work_item_id.as_deref(), wait.work_item_id.as_deref()]
        .into_iter()
        .flatten()
    {
        if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_items WHERE agent_id = ?1 AND work_item_id = ?2 AND state = 'open')",
            params![record.agent_id, owner], |row| row.get::<_, bool>(0),
        )? {
            return Ok(None);
        }
    }
    Ok(Some(wait))
}

/// Also reserve a triggered repair against a different waiter before admission.
pub(super) fn wait_claims_result_tx(
    tx: &Transaction<'_>,
    wait: &WaitConditionRecord,
    record: &TaskResultSettlementRecord,
) -> Result<bool> {
    let Some(trigger) = wait.trigger_message_id() else {
        return Ok(false);
    };
    let Some(source) = load_source_tx(tx, trigger)? else {
        return Ok(false);
    };
    // Persisted messages are append-only, so a recovery source the scanner
    // queued first cannot gain the waiter ref afterwards. The wait names its
    // trigger durably; an explicit ref must still agree when the source carries
    // one.
    Ok(source_matches(&source, record)
        && source.work_item_id == wait.work_item_id
        && source
            .source_refs
            .get("wait_id")
            .is_none_or(|wait_id| wait_id == &wait.id)
        && wait_matches_result(wait, record))
}

/// Original ingress keeps the captured owner; the exact wait names its consumer.
/// It carries neither synthetic recovery markers nor a recovery admission charge.
pub(super) fn direct_wait_claims_result(
    source: &MessageEnvelope,
    wait: &WaitConditionRecord,
    record: &TaskResultSettlementRecord,
) -> bool {
    source.id == record.message_id
        && source.agent_id == record.agent_id
        && source.work_item_id == record.work_item_id
        && source.task_id.as_deref() == Some(record.task_id.as_str())
        && source.kind == MessageKind::TaskResult
        && matches!(&source.origin, MessageOrigin::Task { task_id } if task_id == &record.task_id)
        && source.authority_class == AuthorityClass::RuntimeInstruction
        && source.delivery_surface == Some(MessageDeliverySurface::TaskRejoin)
        && source.admission_context == Some(AdmissionContext::RuntimeOwned)
        && wait.trigger_message_id() == Some(source.id.as_str())
        && wait_matches_result(wait, record)
}

fn wait_matches_result(wait: &WaitConditionRecord, record: &TaskResultSettlementRecord) -> bool {
    wait.agent_id == record.agent_id
        && wait.source.as_deref() == Some("WaitFor")
        && wait.subject_ref.as_deref() == Some(record.task_id.as_str())
        && wait.kind == WaitConditionKind::Task
        && matches!(
            wait.status,
            WaitConditionStatus::Triggered | WaitConditionStatus::Resolved
        )
        && wait.wake_sources
            == [WakeSource::TaskResult {
                task_id: record.task_id.clone(),
            }]
}

pub(crate) fn candidate_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    waiter_work_item_id: Option<&str>,
    activation_id: &str,
) -> Result<Option<TaskResultSettlementRecord>> {
    let Some(attempt) = reply_admission_attempt_tx(tx, agent_id, activation_id)? else {
        return Ok(None);
    };
    candidate_for_attempt_tx(tx, &attempt, waiter_work_item_id)
}

pub(super) fn source_candidate_tx(
    tx: &Transaction<'_>,
    source_id: &str,
    waiter_work_item_id: Option<&str>,
    current_activation_id: Option<&str>,
    expected_wait_id: Option<&str>,
) -> Result<Option<TaskResultSettlementRecord>> {
    let Some(source) = load_source_tx(tx, source_id)? else {
        return Ok(None);
    };
    let Some(message_id) = source.source_refs.get("task_result_message_id") else {
        return Ok(None);
    };
    let Some(record) = super::latest_for_message_tx(tx, message_id)? else {
        return Ok(None);
    };
    if source.id != source_id
        || source.work_item_id.as_deref() != waiter_work_item_id
        || !source_matches(&source, &record)
    {
        return Ok(None);
    }
    let Some(wait) = exact_wait_for_source_tx(tx, &source, expected_wait_id)? else {
        return Ok(None);
    };
    let obligation =
        classify_obligation_tx(tx, &record, current_activation_id, waiter_work_item_id)?;
    let admissible = match obligation {
        TaskResultObligation::Recoverable => true,
        TaskResultObligation::AlreadyAdmitted { activation_id } => {
            current_activation_id == Some(activation_id.as_str())
        }
        _ => false,
    };
    if wait.work_item_id.as_deref() != waiter_work_item_id
        || wait.trigger_message_id() != Some(source_id)
        || !wait_claims_result_tx(tx, &wait, &record)?
        || !admissible
    {
        return Ok(None);
    }
    for owner in [record.work_item_id.as_deref(), waiter_work_item_id]
        .into_iter()
        .flatten()
    {
        if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_items
             WHERE work_item_id = ?1 AND agent_id = ?2 AND state = 'open')",
            params![owner, record.agent_id],
            |row| row.get::<_, bool>(0),
        )? {
            return Ok(None);
        }
    }
    Ok(Some(record))
}

/// The exact wait an already validated recovery source belongs to. The source
/// ref must agree when present. A scanner-queued source can reach the waiter
/// before that ref exists, so its durable trigger join survives resolution too.
pub(super) fn exact_wait_for_source_tx(
    tx: &Transaction<'_>,
    source: &MessageEnvelope,
    expected_wait_id: Option<&str>,
) -> Result<Option<WaitConditionRecord>> {
    let Some(wait_id) = source.source_refs.get("wait_id") else {
        return wait_triggered_by_source_tx(tx, source, expected_wait_id);
    };
    if expected_wait_id.is_some_and(|expected| expected != wait_id) {
        return Ok(None);
    }
    let Some(payload) = tx
        .query_row(
            "SELECT payload_json FROM wait_conditions
             WHERE wait_condition_id = ?1 AND agent_id = ?2",
            params![wait_id, source.agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_str::<WaitConditionRecord>(&payload)?))
}

/// The exact wait a runtime recovery source triggers for the same work item.
/// This is the durable join for a scanner-queued source that reached the waiter
/// before the source could carry its own `wait_id` ref.
fn wait_triggered_by_source_tx(
    tx: &Transaction<'_>,
    source: &MessageEnvelope,
    expected_wait_id: Option<&str>,
) -> Result<Option<WaitConditionRecord>> {
    let mut statement = tx.prepare(
        "SELECT payload_json FROM wait_conditions
         WHERE agent_id = ?1 AND work_item_id IS ?2
         ORDER BY wait_condition_id ASC",
    )?;
    let rows = statement.query_map(params![source.agent_id, source.work_item_id], |row| {
        row.get::<_, String>(0)
    })?;
    for payload in rows {
        let wait: WaitConditionRecord = serde_json::from_str(&payload?)?;
        if wait.kind != WaitConditionKind::Task
            || !matches!(
                wait.status,
                WaitConditionStatus::Triggered | WaitConditionStatus::Resolved
            )
            || wait.trigger_message_id() != Some(source.id.as_str())
            || expected_wait_id.is_some_and(|expected| expected != wait.id)
        {
            continue;
        }
        return Ok(Some(wait));
    }
    Ok(None)
}

fn candidate_for_attempt_tx(
    tx: &Transaction<'_>,
    attempt: &ExecutionAttempt,
    waiter_work_item_id: Option<&str>,
) -> Result<Option<TaskResultSettlementRecord>> {
    let owner_matches = match &attempt.binding {
        ExecutionBinding::WorkItem { work_item_id } => {
            Some(work_item_id.as_str()) == waiter_work_item_id
        }
        ExecutionBinding::AgentLifecycle { agent_id } => {
            agent_id == &attempt.agent_id && waiter_work_item_id.is_none()
        }
        _ => false,
    };
    if attempt.state != ExecutionAttemptState::Open || !owner_matches {
        return Ok(None);
    }
    let Some(source_id) = attempt.source_message_id.as_deref() else {
        return Ok(None);
    };
    let ExecutionSourceIdentity::TriggeredWait {
        wait_id,
        trigger_message_id,
        ingress: Some(ExecutionWaitIngress::RuntimeOwnedInternalFollowup),
    } = &attempt.source.identity
    else {
        return Ok(None);
    };
    if trigger_message_id != source_id {
        return Ok(None);
    }
    let Some(record) = source_candidate_tx(
        tx,
        source_id,
        waiter_work_item_id,
        Some(&attempt.attempt_id),
        Some(wait_id),
    )?
    else {
        return Ok(None);
    };
    if record.agent_id != attempt.agent_id {
        return Ok(None);
    }
    if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM queue_entries
             WHERE message_id = ?1 AND agent_id = ?2 AND status = ?3)",
            params![source_id, record.agent_id, serde_json::to_value(QueueEntryStatus::Dequeued)?.as_str()],
            |row| row.get::<_, bool>(0),
        )?
        || !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM turn_records
             WHERE agent_id = ?1 AND trigger_message_id = ?2
               AND current_work_item_id IS ?3 AND terminal_kind IS NULL
               AND EXISTS(SELECT 1 FROM json_each(turn_records.payload_json, '$.input_message_ids') input
                          WHERE input.value = ?2))",
            params![record.agent_id, source_id, waiter_work_item_id],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Ok(None);
    }
    Ok(Some(record))
}

pub(super) fn admits_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
    attempt: &ExecutionAttempt,
    waiter_work_item_id: Option<&str>,
) -> Result<bool> {
    Ok(candidate_for_attempt_tx(tx, attempt, waiter_work_item_id)?
        .is_some_and(|candidate| candidate.result_identity == record.result_identity))
}
