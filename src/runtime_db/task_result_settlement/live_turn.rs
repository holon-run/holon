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
    TurnRecord, TurnTerminalKind,
};

const LIVE_TASK_RESULT_CANDIDATE_LIMIT: usize = 64;

struct RecoverySources {
    predecessor_turn_id: String,
    activation_turns: BTreeMap<String, String>,
}

/// Deterministic rejection of a replay-anchored claim whose persisted
/// (attempt, turn) pair violates the interrupted-replay invariant. Unlike a
/// generic failure this describes poisoned local state: replay admission can
/// never succeed for this activation, and the queue head must be quarantined
/// instead of killing the runtime loop.
#[derive(Debug, thiserror::Error)]
#[error("interrupted replay source invariant failed: {violation}")]
pub(crate) struct InterruptedReplaySourceInvalid {
    pub violation: &'static str,
    pub agent_id: String,
    pub activation_id: String,
    pub turn_id: Option<String>,
    pub terminal_kind: Option<String>,
}

impl InterruptedReplaySourceInvalid {
    fn new(violation: &'static str, agent_id: &str, activation_id: &str) -> Self {
        Self {
            violation,
            agent_id: agent_id.to_string(),
            activation_id: activation_id.to_string(),
            turn_id: None,
            terminal_kind: None,
        }
    }
}

fn terminal_kind_str(kind: TurnTerminalKind) -> &'static str {
    match kind {
        TurnTerminalKind::Completed => "completed",
        TurnTerminalKind::Aborted => "aborted",
        TurnTerminalKind::Interrupted => "interrupted",
        TurnTerminalKind::BaselineOverBudget => "baseline_over_budget",
        TurnTerminalKind::DeferredToFallback => "deferred_to_fallback",
        TurnTerminalKind::ProviderFailedNeedsRecovery => "provider_failed_needs_recovery",
    }
}

impl TaskResultSettlementRepository<'_> {
    /// Select the exact interrupted predecessor, including live-only turns.
    pub(crate) fn interrupted_attached_turn_id(
        &self,
        agent_id: &str,
        activation_id: &str,
        source_message_id: &str,
    ) -> Result<Option<String>> {
        self.db.transaction(|tx| {
            let Some(attempt) = super::reply_admission_attempt_tx(tx, agent_id, activation_id)?
            else {
                return Err(InterruptedReplaySourceInvalid::new(
                    "attempt_missing",
                    agent_id,
                    activation_id,
                )
                .into());
            };
            if attempt.agent_id != agent_id
                || attempt.source_message_id.as_deref() != Some(source_message_id)
                || attempt.state
                    != crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
            {
                return Err(InterruptedReplaySourceInvalid::new(
                    "attempt_mismatch",
                    agent_id,
                    activation_id,
                )
                .into());
            }
            if let Some(turn) = attached_turn_for_activation_tx(tx, &attempt, true)? {
                if turn.terminal.as_ref().map(|terminal| terminal.kind)
                    != Some(crate::types::TurnTerminalKind::Interrupted)
                {
                    return Err(InterruptedReplaySourceInvalid {
                        violation: "turn_not_interrupted",
                        agent_id: agent_id.to_string(),
                        activation_id: activation_id.to_string(),
                        turn_id: Some(turn.turn_id.clone()),
                        terminal_kind: turn
                            .terminal
                            .as_ref()
                            .map(|terminal| terminal_kind_str(terminal.kind).to_string()),
                    }
                    .into());
                }
                return Ok(Some(turn.turn_id));
            }
            let has_attachment: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM turn_records
                 WHERE agent_id = ?1 AND (
                   EXISTS(SELECT 1 FROM json_each(payload_json, '$.live_input_batches') batch
                     WHERE json_extract(batch.value, '$.activation_id') = ?2)
                   OR json_extract(payload_json, '$.idle_input_batch.activation_id') = ?2))",
                params![agent_id, activation_id],
                |row| row.get(0),
            )?;
            if has_attachment {
                return Err(InterruptedReplaySourceInvalid::new(
                    "attachment_invalid_or_ambiguous",
                    agent_id,
                    activation_id,
                )
                .into());
            }
            Ok(None)
        })
    }

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
        if younger_turn.as_ref().is_some_and(|younger| {
            younger.replay.as_ref().is_none_or(|replay| {
                replay.source_turn_id != turn.turn_id
                    || Some(&replay.source_message_id) != current.source_message_id.as_ref()
            })
        }) {
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

pub(crate) fn recovery_turn_for_activation_tx(
    tx: &Transaction<'_>,
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
    younger: Option<&TurnRecord>,
) -> Result<Option<TurnRecord>> {
    use crate::domain::execution_protocol::ExecutionAttemptState;
    use crate::types::TurnTerminalKind;

    if attempt.state != ExecutionAttemptState::Interrupted {
        return Ok(None);
    }
    if let Some(turn) = attached_turn_for_activation_tx(tx, attempt, true)? {
        return Ok((turn.terminal.as_ref().map(|terminal| terminal.kind)
            == Some(TurnTerminalKind::Interrupted))
        .then_some(turn));
    }
    // Older P1 turns can lack any batch. Only the persisted replay edge and
    // explicit recovery_of attempt chain can identify that predecessor.
    let Some(replay) = younger.and_then(|turn| turn.replay.as_ref()) else {
        return Ok(None);
    };
    if Some(&replay.source_message_id) != attempt.source_message_id.as_ref() {
        return Ok(None);
    }
    // An invalid or ambiguous attachment must not fall back to a batchless turn.
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM turn_records WHERE agent_id = ?1 AND turn_id = ?2
             AND NOT EXISTS (
               SELECT 1 FROM turn_records AS attached WHERE attached.agent_id = ?1 AND (
                 EXISTS (
                   SELECT 1 FROM json_each(attached.payload_json, '$.live_input_batches') batch
                   WHERE json_extract(batch.value, '$.activation_id') = ?3)
                 OR json_extract(attached.payload_json, '$.idle_input_batch.activation_id') = ?3))",
            params![attempt.agent_id, replay.source_turn_id, attempt.attempt_id],
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
        // Legacy batchless turns may predate bootstrap terminal persistence.
        && turn.terminal.as_ref().is_none_or(|terminal| {
            terminal.kind == TurnTerminalKind::Interrupted
        })
        && turn_matches_attempt_source(&turn, attempt))
    .then_some(turn))
}

/// The strictly matched turn attached to an execution attempt, if exactly one
/// qualifying turn exists. Shared by replay validation and startup repair.
pub(crate) fn attached_turn_for_activation_tx(
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

/// Followers stay interjected: only a completed canonical attachment proves
/// their content was consumed. Queue state alone never grants another waiter.
pub(super) fn reply_source_queue_is_consumed_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    reply_id: &str,
    response: &AgentMessageDeliveryRecord,
) -> Result<bool> {
    let status: Option<String> = tx
        .query_row(
            "SELECT status FROM queue_entries WHERE agent_id = ?1 AND message_id = ?2",
            params![agent_id, reply_id],
            |row| row.get(0),
        )
        .optional()?;
    if status.as_deref() == Some("processed") {
        return Ok(true);
    }
    if status.as_deref() != Some("interjected") {
        return Ok(false);
    }
    Ok(completed_reply_attachment_tx(tx, agent_id, reply_id, response)?.is_some())
}

fn completed_reply_attachment_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    reply_id: &str,
    response: &AgentMessageDeliveryRecord,
) -> Result<Option<TurnRecord>> {
    use crate::domain::execution_protocol::ExecutionAttemptState;

    if response.state != AgentMessageDeliveryState::Consumed
        || response.outcome != AgentMessageDeliveryOutcome::Accepted
        || response.target_agent_id != agent_id
        || response.message_id.as_deref() != Some(reply_id)
    {
        return Ok(None);
    }
    // Live followers need not carry delivery bindings. Resolve the original reply,
    // not an arbitrary completed turn with a compatible owner. Interrupted
    // predecessors are not consumption witnesses; their completed replay is.
    let mut statement = tx.prepare(
        "SELECT turn_id, payload_json FROM turn_records
         WHERE agent_id = ?1 AND terminal_kind = 'completed'
           AND (trigger_message_id = ?2 OR EXISTS(
             SELECT 1 FROM json_each(turn_records.payload_json, '$.input_message_ids') input
             WHERE input.value = ?2))
           AND (?3 IS NULL OR turn_id = ?3)
         LIMIT 2",
    )?;
    let mut rows = statement.query(params![agent_id, reply_id, response.turn_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let turn_id: String = row.get(0)?;
    let payload: String = row.get(1)?;
    let Ok(turn) = serde_json::from_str::<TurnRecord>(&payload) else {
        return Ok(None);
    };
    if rows.next()?.is_some()
        || turn.turn_id != turn_id
        || turn.agent_id != agent_id
        || !turn
            .terminal
            .as_ref()
            .is_some_and(|terminal| terminal.kind == TurnTerminalKind::Completed)
        || !turn.input_message_ids.iter().any(|id| id == reply_id)
        || response.turn_id.as_ref().is_some_and(|id| id != &turn_id)
    {
        return Ok(None);
    }
    let source_ref = format!("message:{reply_id}");
    let mut activations = Vec::new();
    for batch in &turn.live_input_batches {
        for item in &batch.items {
            if item.message_id == reply_id {
                // Result batches render shared originals in items without
                // classifying them as ordinary live inputs.
                if item.source_ref != source_ref {
                    return Ok(None);
                }
                activations.push(batch.activation_id.as_str());
            }
        }
    }
    if let Some(batch) = &turn.idle_input_batch {
        for item in &batch.items {
            if item.message_id == reply_id {
                let Some(activation_id) = batch.activation_id.as_deref() else {
                    return Ok(None);
                };
                if item.source_ref != source_ref {
                    return Ok(None);
                }
                activations.push(activation_id);
            }
        }
    }
    let [activation_id] = activations.as_slice() else {
        return Ok(None);
    };
    if response
        .activation_id
        .as_deref()
        .is_some_and(|id| id != *activation_id)
    {
        return Ok(None);
    }
    let Some(attempt) = super::reply_admission_attempt_tx(tx, agent_id, activation_id)? else {
        return Ok(None);
    };
    if attempt.attempt_id != *activation_id
        || attempt.agent_id != agent_id
        || attempt.state != ExecutionAttemptState::Settled
    {
        return Ok(None);
    }
    Ok(attached_turn_for_activation_tx(tx, &attempt, true)?
        .filter(|attached| attached.turn_id == turn_id))
}

pub(super) fn closed_admitted_reply_owner_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
) -> Result<Option<crate::types::TurnOwner>> {
    use crate::domain::execution_protocol::ExecutionAttemptState;
    use crate::types::TurnTerminalKind;

    if record.state != TaskResultSettlementState::CallerAdmitted
        || !super::record_matches_durable_task_tx(tx, record)?
        || super::latest_for_message_tx(tx, &record.message_id)?.as_ref() != Some(record)
    {
        return Ok(None);
    }
    let Some(id) = record.activation_id.as_deref() else {
        return Ok(None);
    };
    let Some(attempt) = super::reply_admission_attempt_tx(tx, &record.agent_id, id)? else {
        return Ok(None);
    };
    if !matches!(
        attempt.state,
        ExecutionAttemptState::Settled | ExecutionAttemptState::Interrupted
    ) {
        return Ok(None);
    }
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE message_id = ?1",
            [&record.message_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    let result: MessageEnvelope = serde_json::from_str(&payload)?;
    let Some(reply_id) = crate::wake_contract::agent_message_reply_reference(&result) else {
        return Ok(None);
    };
    let turn = if let Some(turn) = attached_turn_for_activation_tx(tx, &attempt, true)? {
        turn
    } else {
        // Batchless initial reply anchors are identified by their canonical
        // attempt and source, never by a different message's recovery lineage.
        if attempt.source_message_id.as_deref() != Some(reply_id)
            || attempt.recovery_of_attempt_id.is_some()
        {
            return Ok(None);
        }
        let payload: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM turn_records WHERE agent_id = ?1 AND turn_id = ?2",
                params![record.agent_id, attempt.turn_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        serde_json::from_str::<TurnRecord>(&payload)?
    };
    Ok((turn_matches_attempt_source(&turn, &attempt)
        && (turn.input_message_ids.iter().any(|id| id == reply_id)
            || admitted_recovery_snapshot_matches_tx(tx, record, &attempt, &turn)?)
        && turn.terminal.as_ref().is_some_and(|terminal| {
            matches!(
                terminal.kind,
                TurnTerminalKind::Aborted
                    | TurnTerminalKind::Interrupted
                    | TurnTerminalKind::DeferredToFallback
                    | TurnTerminalKind::ProviderFailedNeedsRecovery
            )
        }))
    .then(|| turn.effective_owner()))
}

/// Recovery renders admitted results separately from the wake's ordinary input
/// batch. Its selected-identity ledger, not processed queue state, is the grant.
fn admitted_recovery_snapshot_matches_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
    turn: &TurnRecord,
) -> Result<bool> {
    use crate::domain::execution_protocol::{
        ExecutionAttemptState, ExecutionSourceIdentity, ExecutionWaitIngress,
    };

    let Some(source_id) = attempt.source_message_id.as_deref() else {
        return Ok(false);
    };
    let Some(source) = message_tx(tx, source_id)? else {
        return Ok(false);
    };
    let canonical_source = match &attempt.source.identity {
        ExecutionSourceIdentity::QueueMessage { message_id }
        | ExecutionSourceIdentity::InternalFollowup { message_id } => message_id == source_id,
        ExecutionSourceIdentity::TriggeredWait {
            trigger_message_id,
            ingress: Some(ExecutionWaitIngress::RuntimeOwnedInternalFollowup),
            ..
        } => trigger_message_id == source_id,
        _ => false,
    };
    if !canonical_source
        || source.id != source_id
        || source.agent_id != record.agent_id
        || source.work_item_id.as_deref() != turn.effective_owner().work_item_id()
        || source.kind != MessageKind::InternalFollowup
        || !matches!(&source.origin, MessageOrigin::System { subsystem } if subsystem == "task_result_recovery")
        || source.authority_class != AuthorityClass::RuntimeInstruction
        || source.delivery_surface != Some(MessageDeliverySurface::RuntimeSystem)
        || source.admission_context != Some(AdmissionContext::RuntimeOwned)
    {
        return Ok(false);
    }
    let Some(selector) = source.source_refs.get("task_result_message_id") else {
        return Ok(false);
    };
    let Some(anchor) = super::latest_for_message_tx(tx, selector)? else {
        return Ok(false);
    };
    if !super::record_matches_durable_task_tx(tx, &anchor)?
        || anchor.agent_id != record.agent_id
        || source.source_refs.get("task_result_identity") != Some(&anchor.result_identity)
        || source.source_refs.get("task_result_parent_turn_id") != Some(&anchor.parent_turn_id)
        || source.source_refs.get("task_result_rejoin_generation")
            != Some(&anchor.rejoin_generation.to_string())
        || (anchor.result_identity != record.result_identity
            && (anchor.work_item_id.as_deref() != source.work_item_id.as_deref()
                || record.work_item_id != source.work_item_id))
    {
        return Ok(false);
    }
    let mut root = attempt.clone();
    let mut seen = BTreeSet::from([root.attempt_id.clone()]);
    while let Some(prior_id) = root.recovery_of_attempt_id.as_deref() {
        let Some(prior) = super::reply_admission_attempt_tx(tx, &record.agent_id, prior_id)? else {
            return Ok(false);
        };
        if prior.source_message_id != root.source_message_id {
            break;
        }
        if seen.len() >= 64
            || !seen.insert(prior.attempt_id.clone())
            || prior.binding != root.binding
            || prior.state != ExecutionAttemptState::Interrupted
        {
            return Ok(false);
        }
        root = prior;
    }
    // Rearming resets the budget epoch, not the last admitted snapshot's grant.
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_result_recovery_admissions
         WHERE result_identity = ?1 AND root_attempt_id = ?2 AND recovery_epoch <= ?3)",
        params![
            record.result_identity,
            root.attempt_id,
            record.recovery_epoch
        ],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

pub(super) fn closed_recovery_admission_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
) -> Result<
    Option<(
        crate::domain::execution_protocol::ExecutionAttempt,
        TurnRecord,
    )>,
> {
    use crate::domain::execution_protocol::ExecutionAttemptState;

    if record.state != TaskResultSettlementState::CallerAdmitted
        || !super::record_matches_durable_task_tx(tx, record)?
        || super::latest_for_message_tx(tx, &record.message_id)?.as_ref() != Some(record)
    {
        return Ok(None);
    }
    let Some(activation_id) = record.activation_id.as_deref() else {
        return Ok(None);
    };
    let Some(attempt) = super::reply_admission_attempt_tx(tx, &record.agent_id, activation_id)?
    else {
        return Ok(None);
    };
    if !matches!(
        attempt.state,
        ExecutionAttemptState::Settled | ExecutionAttemptState::Interrupted
    ) {
        return Ok(None);
    }
    let Some(turn) = attached_turn_for_activation_tx(tx, &attempt, true)? else {
        return Ok(None);
    };
    if !turn.terminal.as_ref().is_some_and(|terminal| {
        matches!(
            terminal.kind,
            TurnTerminalKind::Interrupted
                | TurnTerminalKind::DeferredToFallback
                | TurnTerminalKind::ProviderFailedNeedsRecovery
        )
    }) || !admitted_recovery_snapshot_matches_tx(tx, record, &attempt, &turn)?
    {
        return Ok(None);
    }
    Ok(Some((attempt, turn)))
}

pub(super) fn consumed_reply_owner_matches_tx(
    tx: &Transaction<'_>,
    task: &TaskRecord,
    reply_id: &str,
    response: &AgentMessageDeliveryRecord,
) -> Result<bool> {
    if response.state != AgentMessageDeliveryState::Consumed
        || !reply_source_queue_is_consumed_tx(tx, &task.agent_id, reply_id, response)?
    {
        return Ok(false);
    }
    let witness_turn_id = if let Some(turn) =
        completed_reply_attachment_tx(tx, &task.agent_id, reply_id, response)?
    {
        turn.turn_id
    } else if let Some(activation_id) = response.activation_id.as_deref() {
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
        if response
            .turn_id
            .as_ref()
            .is_some_and(|id| id != &turn.turn_id)
            || (attempt.source_message_id.as_deref() != Some(reply_id) && !batch_admits_reply)
        {
            return Ok(false);
        }
        turn.turn_id
    } else {
        return Ok(false);
    };
    // P1's completed-source grant remains owner/exact-waiter scoped. Transfer
    // its existing obligation, never consume the shared delivery a second time.
    Ok(tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM turn_records turns
           WHERE turns.agent_id = ?1 AND turns.terminal_kind = 'completed'
             AND (turns.trigger_message_id = ?2 OR EXISTS(
               SELECT 1 FROM json_each(turns.payload_json, '$.input_message_ids') input
               WHERE input.value = ?2))
           AND turns.turn_id = ?5
             AND (turns.current_work_item_id IS ?3 OR EXISTS(
               SELECT 1 FROM wait_conditions waits,
                            json_each(waits.wake_sources_json) wake
               WHERE waits.agent_id = ?1 AND waits.work_item_id IS turns.current_work_item_id
                 AND waits.status = 'resolved' AND waits.kind = 'task'
                 AND waits.trigger_message_id = ?2
                 AND json_extract(wake.value, '$.kind') = 'task_result'
                 AND json_extract(wake.value, '$.task_id') = ?4))
         )",
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
