//! Queue progress is not result-delivery evidence.

use anyhow::Result;
use rusqlite::{params, OptionalExtension, Transaction};

use super::{
    record_matches_durable_task_tx, reply_admission_attempt_tx, TaskResultSettlementDisposition,
    TaskResultSettlementRecord, TaskResultSettlementRepository, TaskResultSettlementState,
};
use crate::{
    domain::execution_protocol::ExecutionAttemptState,
    types::{
        AdmissionContext, AuthorityClass, MessageDeliverySurface, MessageEnvelope, MessageKind,
        MessageOrigin, QueueEntryRecord, QueueEntryStatus, WaitConditionKind, WaitConditionRecord,
        WaitConditionStatus, WakeSource, WorkItemRecord, WorkItemState,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TaskResultObligation {
    AlreadyAdmitted {
        activation_id: String,
    },
    InFlight {
        activation_id: String,
    },
    ClaimedByOtherWaiter {
        wait_condition_id: String,
        work_item_id: Option<String>,
    },
    Settled {
        disposition: TaskResultSettlementDisposition,
    },
    Recoverable,
    Paused,
    InvalidOrStale,
}

impl TaskResultSettlementRepository<'_> {
    pub(crate) fn settled_recovery_source(&self, source: &MessageEnvelope) -> Result<bool> {
        self.db
            .transaction(|tx| settled_recovery_source_tx(tx, source))
    }

    pub(crate) fn exact_waiter_recovery_continuation(
        &self,
        record: &TaskResultSettlementRecord,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<WaitConditionRecord>> {
        self.db.transaction(|tx| {
            if !super::recovery_budget::recovery_admission_eligible_tx(tx, record, "", now)? {
                return Ok(None);
            }
            Ok(
                super::exact_waiter::closed_continuation_tx(tx, record)?.filter(|wait| {
                    wait.cancelled_at.is_none()
                        && wait.expires_at.is_none_or(|expires| expires > now)
                }),
            )
        })
    }

    pub(crate) fn exact_waiter_recovery_source(
        &self,
        source: &MessageEnvelope,
    ) -> Result<Option<TaskResultSettlementRecord>> {
        self.db
            .transaction(|tx| exact_waiter_recovery_source_tx(tx, source, None))
    }

    /// The exact wait that owns this validated recovery source. Scanner-queued
    /// sources reach the waiter before they can carry a `wait_id` ref, so the
    /// triggered wait is the durable join and must be resolvable from it too.
    pub(crate) fn exact_waiter_recovery_wait(
        &self,
        source: &MessageEnvelope,
    ) -> Result<Option<WaitConditionRecord>> {
        self.db
            .transaction(|tx| exact_waiter_recovery_wait_tx(tx, source, None))
    }

    pub(crate) fn exact_waiter_recovery_replay_wait(
        &self,
        source: &MessageEnvelope,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<WaitConditionRecord>> {
        self.db.transaction(|tx| {
            if exact_waiter_recovery_source_tx(tx, source, None)?.is_none() {
                return Ok(None);
            }
            let Some(wait) = super::exact_waiter::exact_wait_for_source_tx(tx, source, None)?
            else {
                return Ok(None);
            };
            if wait.status != WaitConditionStatus::Resolved
                || wait.cancelled_at.is_some()
                || wait.expires_at.is_some_and(|expires| expires <= now)
                || tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM wait_conditions newer
                     WHERE newer.agent_id = ?1 AND newer.work_item_id IS ?2
                       AND newer.rowid > (SELECT rowid FROM wait_conditions
                                          WHERE wait_condition_id = ?3))",
                    params![source.agent_id, source.work_item_id, wait.id],
                    |row| row.get::<_, bool>(0),
                )?
                || !tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM agent_states
                     WHERE agent_id = ?1 AND status != 'stopped')",
                    [&source.agent_id],
                    |row| row.get::<_, bool>(0),
                )?
            {
                return Ok(None);
            }
            Ok(Some(wait))
        })
    }

    pub(crate) fn exact_waiter_recovery_candidate(
        &self,
        agent_id: &str,
        waiter_work_item_id: Option<&str>,
        activation_id: &str,
    ) -> Result<Option<TaskResultSettlementRecord>> {
        self.db.transaction(|tx| {
            super::exact_waiter::candidate_tx(tx, agent_id, waiter_work_item_id, activation_id)
        })
    }

    pub(crate) fn classify_obligation(
        &self,
        record: &TaskResultSettlementRecord,
        current_activation_id: Option<&str>,
        waiter_work_item_id: Option<&str>,
    ) -> Result<TaskResultObligation> {
        self.db.transaction(|tx| {
            classify_obligation_tx(tx, record, current_activation_id, waiter_work_item_id)
        })
    }
}

/// Only a fully settled owner wake is no work. Exact wait continuations and
/// unresolved evidence retain their existing reducer/admission semantics.
pub(crate) fn settled_recovery_source_tx(
    tx: &Transaction<'_>,
    source: &MessageEnvelope,
) -> Result<bool> {
    if source.kind != MessageKind::InternalFollowup
        || !matches!(&source.origin, MessageOrigin::System { subsystem } if subsystem == "task_result_recovery")
        || source.authority_class != AuthorityClass::RuntimeInstruction
        || source.delivery_surface != Some(MessageDeliverySurface::RuntimeSystem)
        || source.admission_context != Some(AdmissionContext::RuntimeOwned)
        || source.source_refs.contains_key("wait_id")
        || !source.source_refs.contains_key("task_result_owner_key")
    {
        return Ok(false);
    }
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE message_id = ?1",
            [&source.id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(false);
    };
    let durable: MessageEnvelope = serde_json::from_str(&payload)?;
    if serde_json::to_value(&durable)? != serde_json::to_value(source)? {
        return Ok(false);
    }
    let Some(result_id) = source.source_refs.get("task_result_message_id") else {
        return Ok(false);
    };
    let Some(record) = super::latest_for_message_tx(tx, result_id)? else {
        return Ok(false);
    };
    if record.agent_id != source.agent_id
        || record.work_item_id != source.work_item_id
        || record.state != TaskResultSettlementState::Settled
        || record.disposition.is_none()
        || source.source_refs.get("task_result_identity") != Some(&record.result_identity)
        || source.source_refs.get("task_result_parent_turn_id") != Some(&record.parent_turn_id)
        || source.source_refs.get("task_result_rejoin_generation")
            != Some(&record.rejoin_generation.to_string())
        || !record_matches_durable_task_tx(tx, &record)?
    {
        return Ok(false);
    }
    // The anchor being settled is insufficient: another result can have arrived
    // for the same owner since this wake was queued.
    let pending: bool = tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM task_result_settlements
           WHERE agent_id = ?1 AND work_item_id IS ?2 AND state != 'settled')",
        params![source.agent_id, source.work_item_id],
        |row| row.get(0),
    )?;
    if pending {
        return Ok(false);
    }
    let mut statement = tx.prepare(
        "SELECT payload_json FROM wait_conditions
         WHERE agent_id = ?1 AND status IN ('active', 'triggered')",
    )?;
    let waits = statement.query_map([&source.agent_id], |row| row.get::<_, String>(0))?;
    for payload in waits {
        let wait: WaitConditionRecord = serde_json::from_str(&payload?)?;
        if wait.work_item_id == source.work_item_id
            || wait.wake_sources.iter().any(|wake| {
                matches!(wake, WakeSource::TaskResult { task_id } if task_id == &record.task_id)
            })
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn exact_waiter_recovery_wait_tx(
    tx: &Transaction<'_>,
    source: &MessageEnvelope,
    current_activation_id: Option<&str>,
) -> Result<Option<WaitConditionRecord>> {
    if exact_waiter_recovery_source_tx(tx, source, current_activation_id)?.is_none() {
        return Ok(None);
    }
    super::exact_waiter::exact_wait_for_source_tx(tx, source, None)
}

pub(super) fn exact_waiter_recovery_source_tx(
    tx: &Transaction<'_>,
    source: &MessageEnvelope,
    current_activation_id: Option<&str>,
) -> Result<Option<TaskResultSettlementRecord>> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE message_id = ?1",
            [&source.id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    let durable: MessageEnvelope = serde_json::from_str(&payload)?;
    if durable.agent_id != source.agent_id
        || durable.work_item_id != source.work_item_id
        || durable.kind != source.kind
        || durable.origin != source.origin
        || durable.authority_class != source.authority_class
        || durable.delivery_surface != source.delivery_surface
        || durable.admission_context != source.admission_context
        || [
            "wait_id",
            "task_result_message_id",
            "task_result_identity",
            "task_result_parent_turn_id",
            "task_result_rejoin_generation",
            "task_result_owner_key",
        ]
        .iter()
        .any(|key| durable.source_refs.get(*key) != source.source_refs.get(*key))
    {
        return Ok(None);
    }
    super::exact_waiter::source_candidate_tx(
        tx,
        &source.id,
        source.work_item_id.as_deref(),
        current_activation_id,
        None,
    )
}

pub(super) fn recovery_wake_admits_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
    waiter_work_item_id: Option<&str>,
) -> Result<bool> {
    if super::exact_waiter::admits_tx(tx, record, attempt, waiter_work_item_id)? {
        return Ok(true);
    }
    let Some(source_id) = attempt.source_message_id.as_deref() else {
        return Ok(false);
    };
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE message_id = ?1",
            [source_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(false);
    };
    let source: MessageEnvelope = serde_json::from_str(&payload)?;
    if attempt.agent_id != record.agent_id
        || attempt.state != ExecutionAttemptState::Open
        || source.agent_id != record.agent_id
        || source.work_item_id.as_deref() != waiter_work_item_id
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
    if anchor.agent_id != record.agent_id
        || source.source_refs.get("task_result_identity") != Some(&anchor.result_identity)
        || source.source_refs.get("task_result_parent_turn_id") != Some(&anchor.parent_turn_id)
        || source.source_refs.get("task_result_rejoin_generation")
            != Some(&anchor.rejoin_generation.to_string())
        || (anchor.result_identity != record.result_identity
            && (anchor.work_item_id.as_deref() != waiter_work_item_id
                || record.work_item_id.as_deref() != waiter_work_item_id))
    {
        return Ok(false);
    }
    if super::live_turn::closed_admitted_reply_owner_tx(tx, record)?
        .is_none_or(|owner| owner.work_item_id() != waiter_work_item_id)
    {
        return Ok(false);
    }
    Ok(
        classify_obligation_tx(tx, record, Some(&attempt.attempt_id), waiter_work_item_id)?
            == TaskResultObligation::Recoverable,
    )
}

pub(crate) fn classify_obligation_tx(
    tx: &Transaction<'_>,
    expected: &TaskResultSettlementRecord,
    current_activation_id: Option<&str>,
    waiter_work_item_id: Option<&str>,
) -> Result<TaskResultObligation> {
    classify_obligation_inner_tx(
        tx,
        expected,
        current_activation_id,
        waiter_work_item_id,
        None,
    )
}

/// The caller validates completeness and the Work revision in this transaction.
pub(crate) fn classify_obligation_with_waits_tx(
    tx: &Transaction<'_>,
    expected: &TaskResultSettlementRecord,
    current_activation_id: Option<&str>,
    waiter_work_item_id: Option<&str>,
    waits: &[&WaitConditionRecord],
) -> Result<TaskResultObligation> {
    classify_obligation_inner_tx(
        tx,
        expected,
        current_activation_id,
        waiter_work_item_id,
        Some(waits),
    )
}

fn classify_obligation_inner_tx(
    tx: &Transaction<'_>,
    expected: &TaskResultSettlementRecord,
    current_activation_id: Option<&str>,
    waiter_work_item_id: Option<&str>,
    observed_waits: Option<&[&WaitConditionRecord]>,
) -> Result<TaskResultObligation> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM task_result_settlements WHERE result_identity = ?1",
            [&expected.result_identity],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(TaskResultObligation::InvalidOrStale);
    };
    let record: TaskResultSettlementRecord = serde_json::from_str(&payload)?;
    if record.agent_id != expected.agent_id
        || record.task_id != expected.task_id
        || record.message_id != expected.message_id
        || record.work_item_id != expected.work_item_id
        || record.rejoin_generation != expected.rejoin_generation
        || record.parent_turn_id != expected.parent_turn_id
    {
        return Ok(TaskResultObligation::InvalidOrStale);
    }
    if record.state == TaskResultSettlementState::Settled {
        return Ok(match record.disposition {
            Some(disposition) => TaskResultObligation::Settled { disposition },
            None => TaskResultObligation::InvalidOrStale,
        });
    }
    if !record_matches_durable_task_tx(tx, &record)? {
        return Ok(TaskResultObligation::InvalidOrStale);
    }
    let message: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE message_id = ?1",
            [&record.message_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(message) = message else {
        return Ok(TaskResultObligation::InvalidOrStale);
    };
    let message: MessageEnvelope = serde_json::from_str(&message)?;
    if message.agent_id != record.agent_id
        || message.work_item_id != record.work_item_id
        || message.task_id.as_deref() != Some(record.task_id.as_str())
        || message.kind != MessageKind::TaskResult
        || !matches!(&message.origin, MessageOrigin::Task { task_id } if task_id == &record.task_id)
        || message.authority_class != AuthorityClass::RuntimeInstruction
        || message.delivery_surface != Some(MessageDeliverySurface::TaskRejoin)
        || message.admission_context != Some(AdmissionContext::RuntimeOwned)
    {
        return Ok(TaskResultObligation::InvalidOrStale);
    }
    // Only an open attempt owns in-flight delivery. Its processed ingress
    // message does not make the obligation settled.
    if let Some(activation_id) = record.activation_id.as_deref() {
        if let Some(attempt) = reply_admission_attempt_tx(tx, &record.agent_id, activation_id)? {
            if attempt.state == ExecutionAttemptState::Open {
                return Ok(if current_activation_id == Some(activation_id) {
                    TaskResultObligation::AlreadyAdmitted {
                        activation_id: activation_id.to_owned(),
                    }
                } else {
                    TaskResultObligation::InFlight {
                        activation_id: activation_id.to_owned(),
                    }
                });
            }
        }
    }
    let open_attempt: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM execution_protocol_attempts
             WHERE agent_id = ?1 AND lifecycle_state = 'open'",
            [&record.agent_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(payload) = open_attempt {
        let attempt: crate::domain::execution_protocol::ExecutionAttempt =
            serde_json::from_str(&payload)?;
        if let Some(source_id) = attempt.source_message_id.as_deref() {
            let source: Option<String> = tx
                .query_row(
                    "SELECT payload_json FROM messages WHERE message_id = ?1",
                    [source_id],
                    |row| row.get(0),
                )
                .optional()?;
            let recovery_claim = source
                .map(|source| serde_json::from_str::<MessageEnvelope>(&source))
                .transpose()?
                .is_some_and(|source| {
                    matches!(&source.origin, MessageOrigin::System { subsystem } if subsystem == "task_result_recovery")
                        && source.authority_class == AuthorityClass::RuntimeInstruction
                        && source.source_refs.get("task_result_identity") == Some(&record.result_identity)
                });
            // A recovery wake reserves delivery against other activations,
            // but its own snapshot still needs to admit this obligation.
            // Actual admission is checked above, independently of the wake.
            if source_id == record.message_id
                || (recovery_claim && current_activation_id != Some(attempt.attempt_id.as_str()))
            {
                return Ok(TaskResultObligation::InFlight {
                    activation_id: attempt.attempt_id,
                });
            }
        }
    }
    let queue: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
            [&record.message_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(queue) = queue {
        let queue: QueueEntryRecord = serde_json::from_str(&queue)?;
        if matches!(
            queue.status,
            QueueEntryStatus::Aborted | QueueEntryStatus::Dropped | QueueEntryStatus::Quarantined
        ) && !(queue.status == QueueEntryStatus::Aborted
            && super::local_dispatch::source_was_locally_rejected_tx(tx, &record)?)
        {
            return Ok(TaskResultObligation::InvalidOrStale);
        }
    }
    let classify_wait = |wait: &WaitConditionRecord| -> Result<Option<TaskResultObligation>> {
        if wait.kind != WaitConditionKind::Task
            || wait.work_item_id.as_deref() == waiter_work_item_id
            || !wait.wake_sources.iter().any(
                |source| matches!(source, WakeSource::TaskResult { task_id } if task_id == &record.task_id),
            )
            || (wait.status != WaitConditionStatus::Active
                && wait.trigger_message_id() != Some(record.message_id.as_str())
                && !super::exact_waiter::wait_claims_result_tx(tx, &wait, &record)?)
        {
            return Ok(None);
        }
        if let Some(owner_id) = wait.work_item_id.as_deref() {
            let owner: Option<String> = tx
                .query_row(
                    "SELECT payload_json FROM work_items WHERE work_item_id = ?1 AND agent_id = ?2",
                    params![owner_id, record.agent_id],
                    |row| row.get(0),
                )
                .optional()?;
            let owner = owner
                .map(|payload| serde_json::from_str::<WorkItemRecord>(&payload))
                .transpose()?;
            if owner
                .as_ref()
                .is_none_or(|owner| owner.state != WorkItemState::Open)
            {
                // Closing an admitted consuming waiter revokes automatic
                // continuation; it does not transfer its promise back to the
                // task's captured owner.
                if record.state == TaskResultSettlementState::CallerAdmitted
                    && (super::exact_waiter::direct_wait_claims_result(&message, &wait, &record)
                        || super::exact_waiter::wait_claims_result_tx(tx, &wait, &record)?)
                {
                    return Ok(Some(TaskResultObligation::InvalidOrStale));
                }
                return Ok(None);
            }
        }
        Ok(Some(TaskResultObligation::ClaimedByOtherWaiter {
            wait_condition_id: wait.id.clone(),
            work_item_id: wait.work_item_id.clone(),
        }))
    };
    if let Some(waits) = observed_waits {
        for wait in waits {
            if let Some(obligation) = classify_wait(wait)? {
                return Ok(obligation);
            }
        }
    } else {
        let mut statement = tx.prepare(
            "SELECT payload_json FROM wait_conditions
             WHERE agent_id = ?1 AND status IN ('active', 'triggered', 'resolved')
             ORDER BY created_at ASC, wait_condition_id ASC",
        )?;
        let waits = statement
            .query_map([&record.agent_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for payload in waits {
            let wait: WaitConditionRecord = serde_json::from_str(&payload)?;
            if let Some(obligation) = classify_wait(&wait)? {
                return Ok(obligation);
            }
        }
    }
    if record.recovery_paused_at.is_some() {
        return Ok(TaskResultObligation::Paused);
    }
    Ok(TaskResultObligation::Recoverable)
}
