use std::collections::{BTreeMap, BTreeSet};

use super::{
    timestamp, update_tx, TaskResultSettlementRecord, TaskResultSettlementRepository,
    TaskResultSettlementState,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};

use crate::types::{
    AdmissionContext, AgentMessageDeliveryOutcome, AgentMessageDeliveryRecord,
    AgentMessageDeliveryState, AuthorityClass, MessageDeliverySurface, MessageEnvelope,
    MessageKind, MessageOrigin, TaskKind, TaskRecord, TaskRecoverySpec, TaskStatus, TurnOwner,
    TurnRecord,
};

const LIVE_TASK_RESULT_CANDIDATE_LIMIT: usize = 64;

struct RecoverySources {
    predecessor_turn_id: String,
    activation_turns: BTreeMap<String, String>,
}

impl TaskResultSettlementRepository<'_> {
    /// Selection only: an attached result must replay with its interrupted anchor.
    pub(crate) fn interrupted_admitted_result_anchor(
        &self,
        agent_id: &str,
        message_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<String>> {
        self.db.transaction(|tx| {
            let Some(record) = super::latest_for_message_tx(tx, message_id)? else {
                return Ok(None);
            };
            if record.agent_id != agent_id
                || record.state != TaskResultSettlementState::CallerAdmitted
            {
                return Ok(None);
            }
            let Some(activation_id) = record.activation_id.as_deref() else {
                return Ok(None);
            };
            let Some(attempt) = super::reply_admission_attempt_tx(tx, agent_id, activation_id)?
            else {
                return Ok(None);
            };
            if attempt.state
                != crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
            {
                return Ok(None);
            }
            let Some(turn) = attached_turn_for_activation_tx(tx, &attempt, true)? else {
                return Ok(None);
            };
            let Some(anchor) = attempt.source_message_id.as_deref() else {
                return Ok(None);
            };
            if turn.terminal.as_ref().map(|terminal| terminal.kind)
                != Some(crate::types::TurnTerminalKind::Interrupted)
                || turn.idle_input_batch.as_ref().is_none_or(|batch| {
                    batch.activation_id.as_deref() != Some(activation_id)
                        || batch.anchor_message_id != anchor
                })
                || !tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM queue_entries
                     WHERE agent_id = ?1 AND message_id = ?2 AND status = 'interrupted')",
                    params![agent_id, anchor],
                    |row| row.get::<_, bool>(0),
                )?
            {
                return Ok(None);
            }
            let recovery = RecoverySources {
                predecessor_turn_id: turn.turn_id.clone(),
                activation_turns: BTreeMap::from([(
                    activation_id.to_owned(),
                    turn.turn_id.clone(),
                )]),
            };
            let live: Vec<_> = turn
                .live_input_batches
                .iter()
                .filter(|batch| {
                    batch
                        .items
                        .iter()
                        .any(|item| item.message_id == record.message_id)
                })
                .collect();
            let valid = if let Some(batch) = live.first() {
                live.len() == 1
                    && turn.input_message_ids.contains(&record.message_id)
                    && batch.activation_id == activation_id
                    && batch.result_identities.contains(&record.result_identity)
                    && batch.items.iter().any(|item| {
                        item.message_id == record.message_id
                            && item.source_ref == format!("message:{}", record.message_id)
                    })
                    && source_is_live_tx(
                        tx,
                        &record,
                        agent_id,
                        &turn.turn_id,
                        Some(activation_id),
                        now,
                        Some(&recovery),
                    )?
            } else {
                // Initial results live in the admission ledger, not ordinary input IDs.
                turn.idle_input_batch
                    .as_ref()
                    .is_some_and(|batch| record.created_at <= batch.cutoff)
                    && initial_source_is_recoverable_tx(
                        tx,
                        &record,
                        &attempt,
                        activation_id,
                        now,
                        &recovery,
                    )?
            };
            Ok(valid.then(|| anchor.to_owned()))
        })
    }

    /// Discovery is not a grant. The attachment transaction rechecks every identity.
    pub(crate) fn same_turn_candidates(
        &self,
        agent_id: &str,
        turn_id: &str,
        cutoff: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.db.transaction(|tx| {
            let records = super::records_tx(
                tx,
                "SELECT payload_json FROM task_result_settlements
                 WHERE agent_id = ?1 AND parent_turn_id = ?2
                   AND state = 'persisted_pending' AND activation_id IS NULL
                   AND created_at <= ?3
                 ORDER BY created_at ASC, result_identity ASC LIMIT ?4",
                params![
                    agent_id,
                    turn_id,
                    timestamp(cutoff),
                    limit.min(LIVE_TASK_RESULT_CANDIDATE_LIMIT) as i64
                ],
            )?;
            let mut candidates = Vec::new();
            for record in records {
                if source_is_live_tx(tx, &record, agent_id, turn_id, None, cutoff, None)? {
                    candidates.push(record);
                }
            }
            Ok(candidates)
        })
    }
}

/// The caller owns execution, lifecycle, attachment and persistent budget checks.
/// A closed predecessor is deliberately not a grant to acquire a new identity.
pub(crate) fn admit_live_task_results_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    turn_id: &str,
    activation_id: &str,
    selected: &BTreeSet<String>,
    cutoff: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<bool> {
    if selected.len() > LIVE_TASK_RESULT_CANDIDATE_LIMIT {
        return Ok(false);
    }
    let mut records = Vec::with_capacity(selected.len());
    for identity in selected {
        let payload: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM task_result_settlements
                 WHERE result_identity = ?1 AND agent_id = ?2 AND parent_turn_id = ?3
                   AND state = 'persisted_pending' AND activation_id IS NULL",
                params![identity, agent_id, turn_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(record) = payload
            .and_then(|payload| serde_json::from_str::<TaskResultSettlementRecord>(&payload).ok())
        else {
            return Ok(false);
        };
        if record.result_identity != *identity
            || !source_is_live_tx(
                tx,
                &record,
                agent_id,
                turn_id,
                Some(activation_id),
                cutoff,
                None,
            )?
        {
            return Ok(false);
        }
        records.push(record);
    }
    // No source writes occur until the exact selected set has passed validation.
    for mut record in records {
        record.state = TaskResultSettlementState::CallerAdmitted;
        record.activation_id = Some(activation_id.to_owned());
        record.admitted_at = Some(now);
        record.updated_at = now;
        record.deferred_reason = None;
        record.deferred_at = None;
        record.next_recheck_at = None;
        update_tx(tx, &record)?;
    }
    Ok(true)
}

/// The caller validates canonical binding and the predecessor's durable batches
/// in this same transaction. Recovery transfers the entire admitted ledger,
/// preserving the distinct initial owner and live same-parent source contracts.
pub(crate) fn recover_live_task_results_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    predecessor_activation_id: &str,
    activation_id: &str,
    selected: &BTreeSet<String>,
    now: DateTime<Utc>,
) -> Result<bool> {
    use crate::domain::execution_protocol::ExecutionAttemptState;

    if predecessor_activation_id == activation_id
        || selected.len() > LIVE_TASK_RESULT_CANDIDATE_LIMIT
    {
        return Ok(false);
    }
    let Some(current) = super::reply_admission_attempt_tx(tx, agent_id, activation_id)? else {
        return Ok(false);
    };
    if current.attempt_id != activation_id
        || current.agent_id != agent_id
        || current.state != ExecutionAttemptState::Open
        || current.recovery_of_attempt_id.as_deref() != Some(predecessor_activation_id)
        || current.source_message_id.is_none()
        || current.terminal_outcome_id.is_some()
        || current.terminal_at.is_some()
    {
        return Ok(false);
    }
    let mut recovery = RecoverySources {
        predecessor_turn_id: String::new(),
        activation_turns: BTreeMap::new(),
    };
    let mut predecessor_id = Some(predecessor_activation_id.to_owned());
    let mut younger_turn: Option<TurnRecord> = None;
    while let Some(id) = predecessor_id {
        if recovery.activation_turns.contains_key(&id) || id == activation_id {
            return Ok(false);
        }
        let Some(prior) = super::reply_admission_attempt_tx(tx, agent_id, &id)? else {
            return Ok(false);
        };
        if prior.attempt_id != id
            || prior.agent_id != agent_id
            || prior.state != ExecutionAttemptState::Interrupted
            || prior.source_message_id != current.source_message_id
            || prior.binding != current.binding
        {
            if recovery.activation_turns.is_empty() {
                return Ok(false);
            }
            break;
        }
        // Attempt turn_id is ingress identity, not the turn that persisted inputs.
        let Some(turn) = recovery_turn_for_activation_tx(tx, &prior, younger_turn.as_ref())? else {
            return Ok(false);
        };
        if turn.terminal.as_ref().map(|terminal| terminal.kind)
            != Some(crate::types::TurnTerminalKind::Interrupted)
            || younger_turn.as_ref().is_some_and(|younger| {
                younger.replay.as_ref().is_none_or(|replay| {
                    replay.source_turn_id != turn.turn_id
                        || Some(&replay.source_message_id) != current.source_message_id.as_ref()
                })
            })
        {
            return Ok(false);
        }
        if recovery.activation_turns.is_empty() {
            recovery.predecessor_turn_id = turn.turn_id.clone();
        }
        recovery.activation_turns.insert(id, turn.turn_id.clone());
        younger_turn = Some(turn);
        predecessor_id = prior.recovery_of_attempt_id;
    }
    let records = super::admitted_for_activation_tx(tx, agent_id, predecessor_activation_id)?;
    let identities: BTreeSet<_> = records
        .iter()
        .map(|record| record.result_identity.clone())
        .collect();
    if identities.len() != records.len()
        || !selected.is_subset(&identities)
        || records.len().saturating_sub(selected.len())
            > super::TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT
    {
        return Ok(false);
    }
    for record in &records {
        let valid = if selected.contains(&record.result_identity) {
            recovery
                .activation_turns
                .values()
                .any(|turn| turn == &record.parent_turn_id)
                && source_is_live_tx(
                    tx,
                    record,
                    agent_id,
                    &record.parent_turn_id,
                    Some(predecessor_activation_id),
                    now,
                    Some(&recovery),
                )?
        } else {
            initial_source_is_recoverable_tx(
                tx,
                record,
                &current,
                predecessor_activation_id,
                now,
                &recovery,
            )?
        };
        if !valid {
            return Ok(false);
        }
    }
    // The source reducer may already have reclaimed the initial task-result
    // anchor for this canonical successor. Recheck that grant without moving it.
    for record in super::admitted_for_activation_tx(tx, agent_id, activation_id)? {
        if current.source_message_id.as_deref() != Some(record.message_id.as_str())
            || !initial_source_is_recoverable_tx(
                tx,
                &record,
                &current,
                activation_id,
                now,
                &recovery,
            )?
        {
            return Ok(false);
        }
    }
    for mut record in records {
        record.activation_id = Some(activation_id.to_owned());
        record.updated_at = now;
        update_tx(tx, &record)?;
    }
    Ok(true)
}

/// Resolve a unique durable live or initial attachment, never the ingress hint.
pub(crate) fn canonical_live_turn_id_tx(
    tx: &Transaction<'_>,
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
) -> Result<Option<String>> {
    Ok(attached_turn_for_activation_tx(tx, attempt, true)?.map(|turn| turn.turn_id))
}

fn recovery_turn_for_activation_tx(
    tx: &Transaction<'_>,
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
    younger: Option<&TurnRecord>,
) -> Result<Option<TurnRecord>> {
    if let Some(turn) = attached_turn_for_activation_tx(tx, attempt, true)? {
        return Ok(Some(turn));
    }
    // Older P1 turns can lack any batch. Only the persisted replay edge and
    // explicit recovery_of attempt chain can identify that predecessor.
    let Some(replay) = younger.and_then(|turn| turn.replay.as_ref()) else {
        return Ok(None);
    };
    if Some(&replay.source_message_id) != attempt.source_message_id.as_ref() {
        return Ok(None);
    }
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM turn_records WHERE agent_id = ?1 AND turn_id = ?2",
            params![attempt.agent_id, replay.source_turn_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    let turn: TurnRecord = serde_json::from_str(&payload)?;
    Ok((turn.turn_id == replay.source_turn_id
        && turn.live_input_batches.is_empty()
        && turn.idle_input_batch.is_none()
        && turn_matches_attempt_source(&turn, attempt))
    .then_some(turn))
}

fn attached_turn_for_activation_tx(
    tx: &Transaction<'_>,
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
    include_idle: bool,
) -> Result<Option<TurnRecord>> {
    let mut statement = tx.prepare(
        "SELECT turn_id, payload_json FROM turn_records
         WHERE agent_id = ?1 AND (EXISTS(
           SELECT 1 FROM json_each(turn_records.payload_json, '$.live_input_batches') batch
           WHERE json_extract(batch.value, '$.activation_id') = ?2)
           OR (?3 AND json_extract(payload_json, '$.idle_input_batch.activation_id') = ?2))
         LIMIT 2",
    )?;
    let mut rows = statement.query(params![attempt.agent_id, attempt.attempt_id, include_idle])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let turn_id: String = row.get(0)?;
    let payload: String = row.get(1)?;
    let turn: TurnRecord = serde_json::from_str(&payload)?;
    let live_witness = turn
        .live_input_batches
        .iter()
        .any(|batch| batch.activation_id == attempt.attempt_id);
    let idle_witness = include_idle
        && turn.idle_input_batch.as_ref().is_some_and(|batch| {
            batch.activation_id.as_deref() == Some(attempt.attempt_id.as_str())
                && Some(&batch.anchor_message_id) == attempt.source_message_id.as_ref()
                && batch.items.first().is_some_and(|item| {
                    item.message_id == batch.anchor_message_id
                        && item.source_ref == format!("message:{}", batch.anchor_message_id)
                })
        });
    if rows.next()?.is_some()
        || turn.turn_id != turn_id
        || !turn_matches_attempt_source(&turn, attempt)
        || !(live_witness || idle_witness)
    {
        return Ok(None);
    }
    Ok(Some(turn))
}

fn turn_matches_attempt_source(
    turn: &TurnRecord,
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
) -> bool {
    use crate::domain::execution_protocol::ExecutionBinding;

    if turn.agent_id != attempt.agent_id
        || attempt
            .run_id
            .as_ref()
            .is_some_and(|run| turn.run_id.as_ref() != Some(run))
        || attempt.source_message_id.is_none()
        || turn
            .trigger
            .as_ref()
            .and_then(|trigger| trigger.message_id.as_ref())
            != attempt.source_message_id.as_ref()
        || !attempt
            .source_message_id
            .as_ref()
            .is_some_and(|source| turn.input_message_ids.contains(source))
    {
        return false;
    }
    let owner = match &attempt.binding {
        ExecutionBinding::Conversation { interaction_id } => TurnOwner::Conversation {
            interaction_id: interaction_id.clone(),
        },
        ExecutionBinding::WorkItem { work_item_id } => TurnOwner::WorkItem {
            work_item_id: work_item_id.clone(),
        },
        ExecutionBinding::AgentLifecycle { agent_id } => TurnOwner::AgentLifecycle {
            agent_id: agent_id.clone(),
        },
        ExecutionBinding::Command => TurnOwner::Command,
    };
    turn.effective_owner() == owner
}

fn source_is_live_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
    agent_id: &str,
    turn_id: &str,
    activation_id: Option<&str>,
    cutoff: DateTime<Utc>,
    recovery: Option<&RecoverySources>,
) -> Result<bool> {
    if !result_source_matches_tx(
        tx,
        record,
        agent_id,
        turn_id,
        activation_id,
        cutoff,
        recovery.is_some(),
    )? {
        return Ok(false);
    }
    let Some(task) = task_tx(tx, &record.task_id)? else {
        return Ok(false);
    };
    let Some(message) = message_tx(tx, &record.message_id)? else {
        return Ok(false);
    };
    if !message_is_available_tx(
        tx,
        agent_id,
        &message.id,
        turn_id,
        false,
        task.kind != TaskKind::AgentMessageWait,
        recovery,
    )? {
        return Ok(false);
    }
    // The durable same-parent fence grants only this task, independent of owner.
    match task.kind {
        TaskKind::CommandTask | TaskKind::ChildAgentTask => {
            Ok(crate::wake_contract::agent_message_reply_reference(&message).is_none())
        }
        TaskKind::AgentMessageWait if task.status == TaskStatus::Completed => reply_is_live_tx(
            tx,
            &task,
            &message,
            turn_id,
            activation_id,
            cutoff,
            recovery,
            false,
        ),
        _ => Ok(false),
    }
}

fn result_source_matches_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
    agent_id: &str,
    turn_id: &str,
    activation_id: Option<&str>,
    cutoff: DateTime<Utc>,
    recovering: bool,
) -> Result<bool> {
    let (expected_state, expected_activation) = if recovering {
        (TaskResultSettlementState::CallerAdmitted, activation_id)
    } else {
        (TaskResultSettlementState::PersistedPending, None)
    };
    if record.agent_id != agent_id
        || record.parent_turn_id != turn_id
        || record.created_at > cutoff
        || record.state != expected_state
        || record.activation_id.as_deref() != expected_activation
        || if recovering {
            record.admitted_at.is_none_or(|admitted| admitted > cutoff)
        } else {
            record.admitted_at.is_some()
        }
        || record.disposition.is_some()
        || record.settled_at.is_some()
    {
        return Ok(false);
    }
    let Ok(generation) = i64::try_from(record.rejoin_generation) else {
        return Ok(false);
    };
    if !tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_result_settlements
         WHERE result_identity = ?1 AND agent_id = ?2 AND task_id = ?3
           AND message_id = ?4 AND work_item_id IS ?5 AND rejoin_generation = ?6
           AND parent_turn_id = ?7 AND state = ?8 AND activation_id IS ?9)",
        params![
            record.result_identity,
            record.agent_id,
            record.task_id,
            record.message_id,
            record.work_item_id,
            generation,
            record.parent_turn_id,
            expected_state.as_str(),
            expected_activation,
        ],
        |row| row.get::<_, bool>(0),
    )? {
        return Ok(false);
    }
    let Some(task) = task_tx(tx, &record.task_id)? else {
        return Ok(false);
    };
    let Some(message) = message_tx(tx, &record.message_id)? else {
        return Ok(false);
    };
    let Some(expected) = TaskResultSettlementRecord::pending(&task, &message, record.created_at)?
    else {
        return Ok(false);
    };
    let Some(metadata) = message.metadata.as_ref() else {
        return Ok(false);
    };
    let Some(detail) = metadata.get("task_detail") else {
        return Ok(false);
    };
    if task.id != record.task_id
        || message.id != record.message_id
        || expected.result_identity != record.result_identity
        || expected.rejoin_generation != record.rejoin_generation
        || expected.parent_turn_id != turn_id
        || expected.task_status != record.task_status
        || expected.work_item_id != record.work_item_id
        || task.parent_message_id.as_deref() != Some(record.message_id.as_str())
        || task.created_at > cutoff
        || task.updated_at > cutoff
        || message.created_at > cutoff
        || message.kind != MessageKind::TaskResult
        || message.task_id.as_deref() != Some(task.id.as_str())
        || !matches!(&message.origin, MessageOrigin::Task { task_id } if task_id == &task.id)
        || message.authority_class != AuthorityClass::RuntimeInstruction
        || message.delivery_surface != Some(MessageDeliverySurface::TaskRejoin)
        || message.admission_context != Some(AdmissionContext::RuntimeOwned)
        || metadata.get("task_id").and_then(serde_json::Value::as_str) != Some(task.id.as_str())
        || metadata.get("task_kind") != Some(&serde_json::to_value(&task.kind)?)
        || metadata.get("task_status") != Some(&serde_json::to_value(&task.status)?)
        || detail
            .get("rejoin_obligation_id")
            .and_then(serde_json::Value::as_str)
            != Some(task.id.as_str())
        || detail
            .get("rejoin_generation")
            .and_then(serde_json::Value::as_u64)
            != Some(record.rejoin_generation)
        || detail
            .get("parent_turn_id")
            .and_then(serde_json::Value::as_str)
            != Some(turn_id)
    {
        return Ok(false);
    }
    Ok(true)
}

fn initial_source_is_recoverable_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
    current: &crate::domain::execution_protocol::ExecutionAttempt,
    predecessor_activation_id: &str,
    now: DateTime<Utc>,
    recovery: &RecoverySources,
) -> Result<bool> {
    use crate::domain::execution_protocol::ExecutionBinding;

    if !result_source_matches_tx(
        tx,
        record,
        &current.agent_id,
        &record.parent_turn_id,
        Some(predecessor_activation_id),
        now,
        true,
    )? {
        return Ok(false);
    }
    let Some(task) = task_tx(tx, &record.task_id)? else {
        return Ok(false);
    };
    let Some(message) = message_tx(tx, &record.message_id)? else {
        return Ok(false);
    };
    let owner = match &current.binding {
        ExecutionBinding::WorkItem { work_item_id } => Some(work_item_id.as_str()),
        ExecutionBinding::AgentLifecycle { agent_id } if agent_id == &current.agent_id => None,
        _ => {
            return Ok(
                current.source_message_id.as_deref() == Some(message.id.as_str())
                    && message.work_item_id == record.work_item_id
                    && matches!(task.kind, TaskKind::CommandTask | TaskKind::ChildAgentTask)
                    && crate::wake_contract::agent_message_reply_reference(&message).is_none(),
            );
        }
    };
    let exact_wait: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM wait_conditions waits,
                         json_each(waits.wake_sources_json) wake
         WHERE waits.agent_id = ?1 AND waits.work_item_id IS ?2
           AND waits.status = 'resolved' AND waits.kind = 'task'
           AND waits.trigger_message_id = ?3
           AND json_extract(wake.value, '$.kind') = 'task_result'
           AND json_extract(wake.value, '$.task_id') = ?4)",
        params![current.agent_id, owner, current.source_message_id, task.id],
        |row| row.get(0),
    )?;
    if message.work_item_id != record.work_item_id
        || (record.work_item_id.as_deref() != owner && !exact_wait)
    {
        return Ok(false);
    }
    match task.kind {
        TaskKind::CommandTask | TaskKind::ChildAgentTask => {
            Ok(crate::wake_contract::agent_message_reply_reference(&message).is_none())
        }
        TaskKind::AgentMessageWait if task.status == TaskStatus::Completed => reply_is_live_tx(
            tx,
            &task,
            &message,
            &record.parent_turn_id,
            Some(predecessor_activation_id),
            now,
            Some(recovery),
            true,
        ),
        _ => Ok(false),
    }
}

fn task_tx(tx: &Transaction<'_>, task_id: &str) -> Result<Option<TaskRecord>> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM tasks WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(payload.and_then(|payload| serde_json::from_str(&payload).ok()))
}

fn message_tx(tx: &Transaction<'_>, message_id: &str) -> Result<Option<MessageEnvelope>> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE message_id = ?1",
            [message_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(payload.and_then(|payload| serde_json::from_str(&payload).ok()))
}

fn message_is_available_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    message_id: &str,
    turn_id: &str,
    allow_current_attachment: bool,
    require_queue: bool,
    recovery: Option<&RecoverySources>,
) -> Result<bool> {
    let mut statement = tx.prepare(
        "SELECT turn_id, terminal_kind FROM turn_records
         WHERE agent_id = ?1 AND (trigger_message_id = ?2 OR EXISTS(
           SELECT 1 FROM json_each(turn_records.payload_json, '$.input_message_ids') input
           WHERE input.value = ?2
         ))",
    )?;
    let rows = statement.query_map(params![agent_id, message_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    let mut attached_here = false;
    for row in rows {
        let (owner_turn, terminal) = row?;
        if let Some(recovery) = recovery {
            if terminal.as_deref() != Some("interrupted")
                || !recovery
                    .activation_turns
                    .values()
                    .any(|turn| turn == &owner_turn)
            {
                return Ok(false);
            }
            attached_here |= owner_turn == recovery.predecessor_turn_id;
        } else if !allow_current_attachment || owner_turn != turn_id || terminal.is_some() {
            return Ok(false);
        } else {
            attached_here = true;
        }
    }
    if recovery.is_some() && !attached_here {
        return Ok(false);
    }
    let queue: Option<(String, String)> = tx
        .query_row(
            "SELECT agent_id, status FROM queue_entries WHERE message_id = ?1",
            [message_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(match queue {
        Some((owner, status)) => {
            owner == agent_id
                && (matches!(status.as_str(), "queued" | "interrupted")
                    || (attached_here
                        && matches!(status.as_str(), "dequeued" | "interjected" | "processed")))
        }
        None => !require_queue || attached_here,
    })
}

fn reply_is_live_tx(
    tx: &Transaction<'_>,
    task: &TaskRecord,
    message: &MessageEnvelope,
    turn_id: &str,
    activation_id: Option<&str>,
    cutoff: DateTime<Utc>,
    recovery: Option<&RecoverySources>,
    allow_consumed_owner: bool,
) -> Result<bool> {
    let Some(reply_id) = crate::wake_contract::agent_message_reply_reference(message) else {
        return Ok(false);
    };
    let Some(TaskRecoverySpec::AgentMessageWait {
        target_agent_id,
        delivery_id: Some(request_delivery_id),
        after_delivery_rowid: Some(boundary),
        ..
    }) = task.recovery.as_ref()
    else {
        return Ok(false);
    };
    let Some(detail) = task.detail.as_ref() else {
        return Ok(false);
    };
    let Some(reply) = message_tx(tx, reply_id)? else {
        return Ok(false);
    };
    let Some(response_id) = detail
        .get("response_delivery_id")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(false);
    };
    let Some((request_rowid, request)) = delivery_tx(tx, request_delivery_id)? else {
        return Ok(false);
    };
    let Some((response_rowid, response)) = delivery_tx(tx, response_id)? else {
        return Ok(false);
    };
    let consumed_owner =
        allow_consumed_owner && consumed_reply_owner_matches_tx(tx, task, reply_id, &response)?;
    if reply.id != reply_id
        || reply.agent_id != task.agent_id
        || reply.created_at > cutoff
        || !crate::wake_contract::is_agent_message(&reply)
        || reply.origin != response.caller.origin
        || reply.authority_class != response.caller.authority_class
        || reply.delivery_surface != Some(response.caller.delivery_surface)
        || reply.admission_context != Some(response.caller.admission_context)
        || !reply.routing_context.as_ref().is_some_and(|route| {
            route.message_id == reply.id
                && route.recipient_agent_id == task.agent_id
                && route.sender_agent_id.as_deref() == Some(target_agent_id.as_str())
        })
        || request_rowid != *boundary
        || request.target_agent_id != *target_agent_id
        || request.caller.caller_agent_id.as_deref() != Some(task.agent_id.as_str())
        || request.delivery_id != *request_delivery_id
        || request.outcome != AgentMessageDeliveryOutcome::Accepted
        || request.accepted_at.is_none_or(|accepted| accepted > cutoff)
        || response_rowid <= *boundary
        || response.delivery_id != response_id
        || response.target_agent_id != task.agent_id
        || response.message_id.as_deref() != Some(reply_id)
        || response.caller.caller_agent_id.as_deref() != Some(target_agent_id.as_str())
        || response.routing_context != reply.routing_context
        || response.outcome != AgentMessageDeliveryOutcome::Accepted
        || response
            .accepted_at
            .is_none_or(|accepted| accepted > cutoff)
        || !matches!(
            response.state,
            AgentMessageDeliveryState::Queued
                | AgentMessageDeliveryState::Dispatched
                | AgentMessageDeliveryState::Consumed
        )
        || if consumed_owner {
            false
        } else if let Some(recovery) = recovery {
            response
                .turn_id
                .as_ref()
                .is_some_and(|owner| !recovery.activation_turns.values().any(|turn| turn == owner))
                || response.activation_id.as_ref().is_some_and(|owner| {
                    recovery.activation_turns.get(owner) != response.turn_id.as_ref()
                })
        } else {
            response
                .turn_id
                .as_deref()
                .is_some_and(|owner| owner != turn_id)
                || (response.activation_id.is_some()
                    && response.turn_id.as_deref() != Some(turn_id))
                || activation_id.is_some_and(|activation| {
                    response
                        .activation_id
                        .as_deref()
                        .is_some_and(|owner| owner != activation)
                })
        }
        || detail.get("message_id").and_then(serde_json::Value::as_str) != Some(reply_id)
        || detail
            .get("reply_content_source")
            .and_then(serde_json::Value::as_str)
            != Some("original_message")
        || detail
            .get("message_wait_after_delivery_rowid")
            .and_then(serde_json::Value::as_i64)
            != Some(*boundary)
        || message
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("task_detail"))
            != Some(detail)
        || reply
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("agent_message_delivery"))
            .and_then(|delivery| delivery.get("delivery_id"))
            .and_then(serde_json::Value::as_str)
            != Some(response_id)
        || (!consumed_owner
            && !message_is_available_tx(
                tx,
                &task.agent_id,
                reply_id,
                turn_id,
                true,
                true,
                recovery,
            )?)
    {
        return Ok(false);
    }
    if !consumed_owner
        && response.state != AgentMessageDeliveryState::Queued
        && !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM turn_records
             WHERE agent_id = ?1 AND turn_id = ?2 AND terminal_kind IS ?4
               AND (trigger_message_id = ?3 OR EXISTS(
                 SELECT 1 FROM json_each(turn_records.payload_json, '$.input_message_ids') input
                 WHERE input.value = ?3
               )))",
            params![
                task.agent_id,
                recovery.map_or(turn_id, |recovery| recovery.predecessor_turn_id.as_str()),
                reply_id,
                recovery.map(|_| "interrupted"),
            ],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Ok(false);
    }
    // A waiter observes the first accepted response after its durable receipt.
    Ok(!tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_message_deliveries
         WHERE rowid > ?1 AND rowid < ?2 AND target_agent_id = ?3 AND outcome = 'accepted'
           AND json_extract(payload_json, '$.caller.caller_agent_id') = ?4)",
        params![boundary, response_rowid, task.agent_id, target_agent_id],
        |row| row.get::<_, bool>(0),
    )?)
}

fn consumed_reply_owner_matches_tx(
    tx: &Transaction<'_>,
    task: &TaskRecord,
    reply_id: &str,
    response: &AgentMessageDeliveryRecord,
) -> Result<bool> {
    if response.state != AgentMessageDeliveryState::Consumed {
        return Ok(false);
    }
    let mut witness_turn_id = response.turn_id.clone();
    if let Some(activation_id) = response.activation_id.as_deref() {
        let Some(attempt) = super::reply_admission_attempt_tx(tx, &task.agent_id, activation_id)?
        else {
            return Ok(false);
        };
        if attempt.attempt_id != activation_id || attempt.agent_id != task.agent_id {
            return Ok(false);
        }
        let turn =
            if let Some(turn) = attached_turn_for_activation_tx(tx, &attempt, true)? {
                turn
            } else {
                // A P1 anchor can have no batch. Its consumed delivery must name
                // the exact completed turn whose canonical source is this reply.
                let Some(turn_id) = response.turn_id.as_deref() else {
                    return Ok(false);
                };
                let payload: Option<String> = tx.query_row(
                "SELECT payload_json FROM turn_records WHERE agent_id = ?1 AND turn_id = ?2",
                params![task.agent_id, turn_id],
                |row| row.get(0),
            ).optional()?;
                let Some(payload) = payload else {
                    return Ok(false);
                };
                let turn: TurnRecord = serde_json::from_str(&payload)?;
                if turn.turn_id != turn_id
                    || !turn_matches_attempt_source(&turn, &attempt)
                    || attempt.source_message_id.as_deref() != Some(reply_id)
                    || !turn.live_input_batches.is_empty()
                    || turn.idle_input_batch.is_some()
                {
                    return Ok(false);
                }
                turn
            };
        let batch_admits_reply = turn.live_input_batches.iter().any(|batch| {
            batch.activation_id == activation_id
                && batch.items.iter().any(|item| {
                    item.message_id == reply_id && item.source_ref == format!("message:{reply_id}")
                })
        }) || turn.idle_input_batch.as_ref().is_some_and(|batch| {
            batch.activation_id.as_deref() == Some(activation_id)
                && batch.items.iter().any(|item| {
                    item.message_id == reply_id && item.source_ref == format!("message:{reply_id}")
                })
        });
        if witness_turn_id
            .as_ref()
            .is_some_and(|id| id != &turn.turn_id)
            || (attempt.source_message_id.as_deref() != Some(reply_id) && !batch_admits_reply)
        {
            return Ok(false);
        }
        witness_turn_id = Some(turn.turn_id);
    }
    // P1's completed-source grant remains owner/exact-waiter scoped. Transfer
    // its existing obligation, never consume the shared delivery a second time.
    Ok(tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM turn_records turns
           WHERE turns.agent_id = ?1 AND turns.terminal_kind = 'completed'
             AND (turns.trigger_message_id = ?2 OR EXISTS(
               SELECT 1 FROM json_each(turns.payload_json, '$.input_message_ids') input
               WHERE input.value = ?2))
             AND (?5 IS NULL OR turns.turn_id = ?5)
             AND (turns.current_work_item_id IS ?3 OR EXISTS(
               SELECT 1 FROM wait_conditions waits,
                            json_each(waits.wake_sources_json) wake
               WHERE waits.agent_id = ?1 AND waits.work_item_id IS turns.current_work_item_id
                 AND waits.status = 'resolved' AND waits.kind = 'task'
                 AND waits.trigger_message_id = ?2
                 AND json_extract(wake.value, '$.kind') = 'task_result'
                 AND json_extract(wake.value, '$.task_id') = ?4))
         ) AND EXISTS(SELECT 1 FROM queue_entries
           WHERE message_id = ?2 AND agent_id = ?1 AND status = 'processed')",
        params![
            task.agent_id,
            reply_id,
            task.work_item_id,
            task.id,
            witness_turn_id,
        ],
        |row| row.get(0),
    )?)
}

fn delivery_tx(
    tx: &Transaction<'_>,
    delivery_id: &str,
) -> Result<Option<(i64, AgentMessageDeliveryRecord)>> {
    let row: Option<(i64, String)> = tx
        .query_row(
            "SELECT rowid, payload_json FROM agent_message_deliveries WHERE delivery_id = ?1",
            [delivery_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(rowid, payload)| {
        serde_json::from_str(&payload)
            .ok()
            .map(|delivery| (rowid, delivery))
    }))
}
