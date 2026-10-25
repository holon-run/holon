//! A local send rejection parks the existing grant; it is not delivery.

use super::*;
use crate::{
    domain::execution_protocol::{
        ExecutionAttempt, ExecutionAttemptState, ExecutionBinding, ExecutionSourceIdentity,
        ExecutionWaitIngress,
    },
    runtime_error::RuntimeErrorDomain,
    types::{
        AdmissionContext, AuthorityClass, MessageDeliverySurface, MessageEnvelope, MessageKind,
        MessageOrigin, QueueEntryRecord, QueueEntryStatus, TurnOwner, TurnRecord, TurnTerminalKind,
        WaitConditionRecord, WaitConditionStatus,
    },
};

fn is_local_dispatch_rejection(turn: &TurnRecord) -> bool {
    turn.terminal
        .as_ref()
        .is_some_and(|terminal| terminal.kind == TurnTerminalKind::Aborted)
        && turn.runtime_failure.as_ref().is_some_and(|failure| {
            failure.domain == RuntimeErrorDomain::Conflict
                && failure.code == "provider_dispatch_rejected"
                && !failure.retryable
        })
}

pub(crate) fn pause_local_dispatch_rejected_tx(
    tx: &Transaction<'_>,
    queue: &QueueEntryRecord,
    turn: &TurnRecord,
    activation_id: &str,
    delivery: Option<&TaskResultActivationSettlement>,
) -> Result<usize> {
    if queue.status != QueueEntryStatus::Aborted || !is_local_dispatch_rejection(turn) {
        return Ok(0);
    }
    anyhow::ensure!(
        delivery.is_none(),
        "local dispatch rejection cannot settle task-result delivery"
    );
    let records = admitted_for_activation_tx(tx, &queue.agent_id, activation_id)?;
    if records.is_empty() {
        return Ok(0);
    }
    let attempt = reply_admission_attempt_tx(tx, &queue.agent_id, activation_id)?
        .ok_or_else(|| anyhow::anyhow!("local dispatch stop lost its execution attempt"))?;
    anyhow::ensure!(
        attempt.state == ExecutionAttemptState::Settled
            && attempt.source_message_id.as_deref() == Some(queue.message_id.as_str()),
        "local dispatch stop must retain its canonical selected grant"
    );
    let attached = live_turn::attached_turn_for_activation_tx(tx, &attempt, true)?;
    if attached.as_ref() != Some(turn) {
        for record in &records {
            anyhow::ensure!(
                batchless_exact_root_matches_tx(tx, &attempt, turn, record)?,
                "local dispatch stop must retain its canonical selected grant"
            );
        }
    }
    let now = queue.updated_at;
    let mut changed = 0;
    for mut record in records {
        if !record_matches_durable_task_tx(tx, &record)?
            || (record.recovery_paused_at.is_some()
                && record.deferred_reason.as_deref() == Some("provider_dispatch_rejected")
                && record.next_recheck_at.is_none())
        {
            continue;
        }
        record.recovery_paused_at = Some(now);
        record.deferred_reason = Some("provider_dispatch_rejected".into());
        record.deferred_at = Some(now);
        record.next_recheck_at = None;
        record.updated_at = now;
        update_tx(tx, &record)?;
        changed += 1;
    }
    Ok(changed)
}

// Exact WaitFor roots admit their selected grant without a sender-input batch.
// The resolved wait and its source retain the captured task and consuming owner.
fn batchless_exact_root_matches_tx(
    tx: &Transaction<'_>,
    attempt: &ExecutionAttempt,
    turn: &TurnRecord,
    record: &TaskResultSettlementRecord,
) -> Result<bool> {
    let ExecutionSourceIdentity::TriggeredWait {
        wait_id,
        trigger_message_id,
        ingress,
    } = &attempt.source.identity
    else {
        return Ok(false);
    };
    // Direct task-result wakes have no synthetic marker or recovery charge.
    // A synthetic wake must never fall through to this original-ingress path.
    let direct = ingress.is_none() && trigger_message_id == &record.message_id;
    if !direct && *ingress != Some(ExecutionWaitIngress::RuntimeOwnedInternalFollowup) {
        return Ok(false);
    }
    let owner = match &attempt.binding {
        ExecutionBinding::WorkItem { work_item_id } => TurnOwner::WorkItem {
            work_item_id: work_item_id.clone(),
        },
        ExecutionBinding::AgentLifecycle { agent_id } if agent_id == &attempt.agent_id => {
            TurnOwner::AgentLifecycle {
                agent_id: agent_id.clone(),
            }
        }
        _ => return Ok(false),
    };
    if attempt.state != ExecutionAttemptState::Settled
        || record.state != TaskResultSettlementState::CallerAdmitted
        || record.agent_id != attempt.agent_id
        || attempt.source_message_id.as_ref() != Some(trigger_message_id)
        || attempt.recovery_of_attempt_id.is_some()
        || attempt
            .turn_id
            .as_ref()
            .is_some_and(|id| id != &turn.turn_id)
        || turn.replay.is_some()
        || turn.idle_input_batch.is_some()
        || !turn.live_input_batches.is_empty()
        || turn.agent_id != attempt.agent_id
        || turn.effective_owner() != owner
        || turn.current_work_item_id.as_deref() != owner.work_item_id()
        || attempt
            .run_id
            .as_ref()
            .is_some_and(|run| turn.run_id.as_ref() != Some(run))
        || !is_local_dispatch_rejection(turn)
        || turn
            .trigger
            .as_ref()
            .and_then(|trigger| trigger.message_id.as_ref())
            != Some(trigger_message_id)
        || !turn.input_message_ids.contains(trigger_message_id)
    {
        return Ok(false);
    }
    if reply_admission_attempt_tx(tx, &attempt.agent_id, &attempt.attempt_id)?.as_ref()
        != Some(attempt)
        || latest_for_message_tx(tx, &record.message_id)?.as_ref() != Some(record)
        || !record_matches_durable_task_tx(tx, record)?
        || (!direct
            && !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM task_result_recovery_admissions
             WHERE result_identity = ?1 AND root_attempt_id = ?2 AND recovery_epoch <= ?3)",
                params![
                    record.result_identity,
                    attempt.attempt_id,
                    record.recovery_epoch
                ],
                |row| row.get::<_, bool>(0),
            )?)
    {
        return Ok(false);
    }
    let source: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE message_id = ?1",
            [trigger_message_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(source) = source else {
        return Ok(false);
    };
    let source: MessageEnvelope = serde_json::from_str(&source)?;
    if source.id != *trigger_message_id {
        return Ok(false);
    }
    let original: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE message_id = ?1",
            [&record.message_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(original) = original else {
        return Ok(false);
    };
    let original: MessageEnvelope = serde_json::from_str(&original)?;
    if original.id != record.message_id
        || original.agent_id != record.agent_id
        || original.work_item_id != record.work_item_id
        || original.task_id.as_deref() != Some(record.task_id.as_str())
        || original.kind != MessageKind::TaskResult
        || !matches!(&original.origin, MessageOrigin::Task { task_id } if task_id == &record.task_id)
        || original.authority_class != AuthorityClass::RuntimeInstruction
        || original.delivery_surface != Some(MessageDeliverySurface::TaskRejoin)
        || original.admission_context != Some(AdmissionContext::RuntimeOwned)
    {
        return Ok(false);
    }
    let queue: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id = ?1 AND agent_id = ?2",
            params![trigger_message_id, attempt.agent_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(queue) = queue else {
        return Ok(false);
    };
    let queue: QueueEntryRecord = serde_json::from_str(&queue)?;
    if queue.message_id != *trigger_message_id
        || queue.agent_id != attempt.agent_id
        || queue.status != QueueEntryStatus::Aborted
    {
        return Ok(false);
    }
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM wait_conditions
             WHERE agent_id = ?1 AND wait_condition_id = ?2",
            params![attempt.agent_id, wait_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(false);
    };
    let wait: WaitConditionRecord = serde_json::from_str(&payload)?;
    let claims_result = if direct {
        exact_waiter::direct_wait_claims_result(&source, &wait, record)
    } else {
        exact_waiter::wait_claims_result_tx(tx, &wait, record)?
    };
    if wait.status != WaitConditionStatus::Resolved
        || wait.id != *wait_id
        || wait.work_item_id.as_deref() != owner.work_item_id()
        || wait.trigger_message_id() != Some(trigger_message_id.as_str())
        || !claims_result
    {
        return Ok(false);
    }
    Ok(batchless_root_turn_tx(tx, attempt)?.as_ref() == Some(turn))
}

fn batchless_root_turn_tx(
    tx: &Transaction<'_>,
    attempt: &ExecutionAttempt,
) -> Result<Option<TurnRecord>> {
    // Without an activation batch, never guess between multiple root Turns.
    let mut statement = tx.prepare(
        "SELECT turn_id, payload_json FROM turn_records
         WHERE agent_id = ?1 AND trigger_message_id = ?2 LIMIT 2",
    )?;
    let mut rows = statement.query(params![attempt.agent_id, attempt.source_message_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let turn_id: String = row.get(0)?;
    let payload: String = row.get(1)?;
    let durable: TurnRecord = serde_json::from_str(&payload)?;
    Ok((durable.turn_id == turn_id && rows.next()?.is_none()).then_some(durable))
}

pub(super) fn source_was_locally_rejected_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
) -> Result<bool> {
    if record.state != TaskResultSettlementState::CallerAdmitted {
        return Ok(false);
    }
    // Keep the canonical rejection witness after operator rearm clears the
    // pause and a successor inherits the grant. Interrupted cooling is not repair.
    let mut statement = tx.prepare(
        "SELECT payload_json FROM turn_records
         WHERE agent_id = ?1 AND trigger_message_id = ?2 AND terminal_kind = 'aborted'",
    )?;
    let turns = statement.query_map(params![record.agent_id, record.message_id], |row| {
        row.get::<_, String>(0)
    })?;
    for payload in turns {
        let turn: TurnRecord = serde_json::from_str(&payload?)?;
        if !is_local_dispatch_rejection(&turn) {
            continue;
        }
        let activations = turn
            .idle_input_batch
            .iter()
            .filter_map(|batch| batch.activation_id.as_deref())
            .chain(
                turn.live_input_batches
                    .iter()
                    .map(|batch| batch.activation_id.as_str()),
            );
        for activation_id in activations {
            let Some(attempt) = reply_admission_attempt_tx(tx, &record.agent_id, activation_id)?
            else {
                continue;
            };
            if attempt.state == ExecutionAttemptState::Settled
                && attempt.source_message_id.as_deref() == Some(record.message_id.as_str())
                && live_turn::attached_turn_for_activation_tx(tx, &attempt, true)?.as_ref()
                    == Some(&turn)
            {
                return Ok(true);
            }
        }
    }
    // Direct exact waits use the original ingress; synthetic waits require
    // this identity's charged root as well as their distinct queue message.
    let mut statement = tx.prepare(
        "SELECT DISTINCT attempts.payload_json FROM execution_protocol_attempts attempts
         LEFT JOIN task_result_recovery_admissions admissions
           ON admissions.root_attempt_id = attempts.attempt_id
         WHERE attempts.agent_id = ?1 AND attempts.lifecycle_state = 'settled'
           AND (json_extract(attempts.payload_json, '$.source_message_id') = ?4 OR
                (admissions.result_identity = ?2 AND admissions.recovery_epoch <= ?3))",
    )?;
    let attempts = statement.query_map(
        params![
            record.agent_id,
            record.result_identity,
            record.recovery_epoch,
            record.message_id
        ],
        |row| row.get::<_, String>(0),
    )?;
    for payload in attempts {
        let attempt: ExecutionAttempt = serde_json::from_str(&payload?)?;
        let Some(turn) = batchless_root_turn_tx(tx, &attempt)? else {
            continue;
        };
        if batchless_exact_root_matches_tx(tx, &attempt, &turn, record)? {
            return Ok(true);
        }
    }
    Ok(false)
}
