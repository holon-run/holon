use anyhow::{bail, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    runtime_db::{migrations::timestamp, RuntimeDb},
    types::{MessageEnvelope, TaskRecord},
};

pub(crate) const TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT: usize = 8;

mod exact_waiter;
mod live_turn;
mod obligation;
pub(crate) use obligation::{classify_obligation_tx, TaskResultObligation};
mod provider_handoff;
pub(crate) use provider_handoff::transfer_provider_admitted_tx;
mod recovery_budget;
pub(crate) use live_turn::{
    admit_live_task_results_tx, canonical_live_turn_id_tx, recover_live_task_results_tx,
    recovery_turn_for_activation_tx,
};
pub(crate) use recovery_budget::{charge_recovery_admission_tx, TASK_RESULT_RECOVERY_LIMIT};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskResultSettlementState {
    PersistedPending,
    CallerAdmitted,
    Settled,
}

impl TaskResultSettlementState {
    fn as_str(self) -> &'static str {
        match self {
            Self::PersistedPending => "persisted_pending",
            Self::CallerAdmitted => "caller_admitted",
            Self::Settled => "settled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskResultSettlementDisposition {
    ModelDelivered,
    ReplyConsumedElsewhere,
    OwnerClosed,
    OwnerMissing,
    InvalidOrStale,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskResultActivationSettlement {
    pub activation_id: String,
    pub disposition: TaskResultSettlementDisposition,
    pub settled_at: DateTime<Utc>,
}

impl TaskResultSettlementDisposition {
    fn as_str(self) -> &'static str {
        match self {
            Self::ModelDelivered => "model_delivered",
            Self::ReplyConsumedElsewhere => "reply_consumed_elsewhere",
            Self::OwnerClosed => "owner_closed",
            Self::OwnerMissing => "owner_missing",
            Self::InvalidOrStale => "invalid_or_stale",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskResultSettlementRecord {
    pub result_identity: String,
    pub agent_id: String,
    pub task_id: String,
    pub message_id: String,
    pub work_item_id: Option<String>,
    pub rejoin_generation: u64,
    pub parent_turn_id: String,
    pub task_status: String,
    pub state: TaskResultSettlementState,
    pub activation_id: Option<String>,
    pub disposition: Option<TaskResultSettlementDisposition>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub admitted_at: Option<DateTime<Utc>>,
    pub settled_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub deferred_reason: Option<String>,
    #[serde(default)]
    pub deferred_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub next_recheck_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub recovery_epoch: u64,
    #[serde(default)]
    pub recovery_attempts: u32,
    #[serde(default)]
    pub last_recovery_root_attempt_id: Option<String>,
    #[serde(default)]
    pub last_recovery_admitted_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub recovery_paused_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub recovery_diagnostic_at: Option<DateTime<Utc>>,
}

impl TaskResultSettlementRecord {
    pub(crate) fn pending(
        task: &TaskRecord,
        message: &MessageEnvelope,
        now: DateTime<Utc>,
    ) -> Result<Option<Self>> {
        if task.agent_id != message.agent_id
            || task.work_item_id != message.work_item_id
            || task
                .parent_message_id
                .as_deref()
                .is_some_and(|parent_message_id| parent_message_id != message.id)
        {
            return Ok(None);
        }
        if !matches!(
            task.status,
            crate::types::TaskStatus::Completed
                | crate::types::TaskStatus::Failed
                | crate::types::TaskStatus::Cancelled
                | crate::types::TaskStatus::Interrupted
        ) {
            return Ok(None);
        }
        let work_item_id = task.work_item_id.clone();
        let Ok(fence) = task.rejoin_fence() else {
            return Ok(None);
        };
        let mut hasher = Sha256::new();
        hasher.update(message.agent_id.as_bytes());
        hasher.update([0]);
        hasher.update(task.id.as_bytes());
        hasher.update([0]);
        hasher.update(message.id.as_bytes());
        hasher.update([0]);
        hasher.update(fence.generation.to_be_bytes());
        let digest = format!("{:x}", hasher.finalize());
        let task_status = serde_json::to_value(&task.status)?
            .as_str()
            .unwrap_or("unknown")
            .to_owned();
        Ok(Some(Self {
            result_identity: format!("task_result_{}", &digest[..24]),
            agent_id: message.agent_id.clone(),
            task_id: task.id.clone(),
            message_id: message.id.clone(),
            work_item_id,
            rejoin_generation: fence.generation,
            parent_turn_id: fence.parent_turn_id,
            task_status,
            state: TaskResultSettlementState::PersistedPending,
            activation_id: None,
            disposition: None,
            created_at: now,
            updated_at: now,
            admitted_at: None,
            settled_at: None,
            deferred_reason: None,
            deferred_at: None,
            next_recheck_at: Some(now + Duration::seconds(30)),
            recovery_epoch: 0,
            recovery_attempts: 0,
            last_recovery_root_attempt_id: None,
            last_recovery_admitted_at: None,
            recovery_paused_at: None,
            recovery_diagnostic_at: None,
        }))
    }
}

pub(crate) struct TaskResultSettlementRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

impl RuntimeDb {
    pub(crate) fn task_result_settlements(&self) -> TaskResultSettlementRepository<'_> {
        TaskResultSettlementRepository { db: self }
    }
}

impl TaskResultSettlementRepository<'_> {
    pub(crate) fn ensure_pending(
        &self,
        task: &TaskRecord,
        message: &MessageEnvelope,
        now: DateTime<Utc>,
    ) -> Result<Option<TaskResultSettlementRecord>> {
        let Some(durable_task) = self.db.tasks().latest(&task.id)? else {
            return Ok(None);
        };
        let Some(record) = TaskResultSettlementRecord::pending(&durable_task, message, now)? else {
            return Ok(None);
        };
        self.db.transaction(|tx| {
            if existing_for_task_generation_tx(tx, &record.task_id, record.rejoin_generation)?
                .is_some_and(|existing| existing.message_id != record.message_id)
            {
                return Ok(None);
            }
            upsert_pending_tx(tx, &record)?;
            latest_for_message_tx(tx, &record.message_id)
        })
    }

    #[cfg(test)]
    pub(crate) fn admit_unsettled(
        &self,
        agent_id: &str,
        work_item_id: Option<&str>,
        activation_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.admit_unsettled_for_input(agent_id, work_item_id, activation_id, None, now)
    }

    #[cfg(test)]
    pub(crate) fn admit_unsettled_for_input(
        &self,
        agent_id: &str,
        work_item_id: Option<&str>,
        activation_id: &str,
        anchor_message_id: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.admit_unsettled_snapshot(
            agent_id,
            work_item_id,
            activation_id,
            anchor_message_id,
            None,
            now,
        )
    }

    pub(crate) fn admit_unsettled_snapshot(
        &self,
        agent_id: &str,
        work_item_id: Option<&str>,
        activation_id: &str,
        anchor_message_id: Option<&str>,
        selected: Option<&std::collections::BTreeSet<String>>,
        now: DateTime<Utc>,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.db.transaction(|tx| {
            let already_admitted = admitted_for_activation_tx(tx, agent_id, activation_id)?;
            let recovering_anchor = if let Some(anchor) = anchor_message_id {
                match latest_for_message_tx(tx, anchor)? {
                    Some(record)
                        if record.agent_id == agent_id
                            && record.work_item_id.as_deref() == work_item_id
                            && record.state == TaskResultSettlementState::CallerAdmitted
                            && selected.is_none_or(|ids| ids.contains(&record.result_identity)) =>
                    {
                        interrupted_predecessor_owns_record_tx(tx, &record, activation_id, anchor)?
                    }
                    _ => false,
                }
            } else {
                false
            };
            let anchor_reserved = usize::from(anchor_message_id.is_some_and(|anchor| {
                !recovering_anchor
                    && !already_admitted
                        .iter()
                        .any(|record| record.message_id == anchor)
            }));
            let mut remaining = TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT
                .saturating_sub(already_admitted.len() + anchor_reserved);
            let mut records = unsettled_for_owner_tx(tx, agent_id, work_item_id, selected, 64)?;
            if let Some(record) =
                exact_waiter::candidate_tx(tx, agent_id, work_item_id, activation_id)?
            {
                let payload: String = tx.query_row(
                    "SELECT payload_json FROM messages WHERE message_id = ?1",
                    [&record.message_id],
                    |row| row.get(0),
                )?;
                let message: MessageEnvelope = serde_json::from_str(&payload)?;
                if crate::wake_contract::agent_message_reply_reference(&message).is_none()
                    && !records
                        .iter()
                        .any(|prior| prior.result_identity == record.result_identity)
                {
                    records.insert(0, record);
                }
            }
            let mut admitted = Vec::with_capacity(records.len());
            for mut record in records.drain(..) {
                if record.created_at > now
                    || (record.state == TaskResultSettlementState::CallerAdmitted
                        && record.activation_id.as_deref() == Some(activation_id))
                {
                    continue;
                }
                if !record_matches_durable_task_tx(tx, &record)? {
                    settle_invalid_or_stale_tx(tx, &mut record, now)?;
                    continue;
                }
                // The source reducer admits the anchor independently. Reserve
                // its slot unless reclaiming that same interrupted admission.
                if anchor_message_id == Some(record.message_id.as_str()) && !recovering_anchor {
                    continue;
                }
                if selected.is_some_and(|ids| !ids.contains(&record.result_identity)) {
                    continue;
                }
                if remaining == 0 {
                    break;
                }
                if !charge_selected_recovery_tx(tx, &mut record, activation_id, now)? {
                    continue;
                }
                record.state = TaskResultSettlementState::CallerAdmitted;
                record.activation_id = Some(activation_id.to_owned());
                record.admitted_at.get_or_insert(now);
                record.updated_at = now;
                record.deferred_reason = None;
                record.deferred_at = None;
                update_tx(tx, &record)?;
                admitted.push(record);
                remaining -= 1;
            }
            Ok(admitted)
        })
    }

    #[cfg(test)]
    pub(crate) fn admit_reply_message(
        &self,
        agent_id: &str,
        work_item_id: Option<&str>,
        message_id: &str,
        activation_id: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let selected = self
            .reply_message_candidates(agent_id, work_item_id, message_id, now, 64)?
            .into_iter()
            .take(TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT)
            .map(|record| record.result_identity)
            .collect();
        self.admit_reply_message_snapshot(
            agent_id,
            work_item_id,
            message_id,
            activation_id,
            &selected,
            now,
        )?;
        Ok(())
    }

    pub(crate) fn reply_message_candidates(
        &self,
        agent_id: &str,
        work_item_id: Option<&str>,
        message_id: &str,
        cutoff: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.db.transaction(|tx| {
            let records =
                reply_message_candidates_tx(tx, agent_id, work_item_id, message_id, limit)?;
            let mut eligible = Vec::new();
            for mut record in records {
                if record.created_at > cutoff {
                    continue;
                }
                if !record_matches_durable_task_tx(tx, &record)? {
                    settle_invalid_or_stale_tx(tx, &mut record, cutoff)?;
                    continue;
                }
                eligible.push(record);
            }
            Ok(eligible)
        })
    }

    pub(crate) fn reply_source_is_recoverable(
        &self,
        record: &TaskResultSettlementRecord,
    ) -> Result<bool> {
        if record.state == TaskResultSettlementState::Settled {
            return Ok(false);
        }
        if self.db.transaction(|tx| {
            Ok(live_turn::closed_admitted_reply_owner_tx(tx, record)?.is_some())
        })? {
            return Ok(true);
        }
        self.reply_source_is_consumed_for_owner(record)
    }

    pub(crate) fn non_reply_source_matches_owner(
        &self,
        record: &TaskResultSettlementRecord,
    ) -> Result<bool> {
        self.db.transaction(|tx| {
            if !record_matches_durable_task_tx(tx, record)? {
                return Ok(false);
            }
            let payload: Option<String> = tx
                .query_row(
                    "SELECT payload_json FROM messages WHERE message_id = ?1",
                    [&record.message_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(payload) = payload else {
                return Ok(false);
            };
            let message: MessageEnvelope = serde_json::from_str(&payload)?;
            let task_payload: String = tx.query_row(
                "SELECT payload_json FROM tasks WHERE task_id = ?1",
                [&record.task_id],
                |row| row.get(0),
            )?;
            let task: TaskRecord = serde_json::from_str(&task_payload)?;
            Ok(matches!(
                task.kind,
                crate::types::TaskKind::CommandTask | crate::types::TaskKind::ChildAgentTask
            ) && serde_json::to_value(&task.status)?.as_str()
                == Some(record.task_status.as_str())
                && task.parent_message_id.as_deref() == Some(record.message_id.as_str())
                && message.kind == crate::types::MessageKind::TaskResult
                && message.agent_id == record.agent_id
                && message.work_item_id == record.work_item_id
                && message.task_id.as_deref() == Some(record.task_id.as_str())
                && matches!(
                    &message.origin,
                    crate::types::MessageOrigin::Task { task_id } if task_id == &record.task_id
                )
                && message.authority_class == crate::types::AuthorityClass::RuntimeInstruction
                && message.delivery_surface
                    == Some(crate::types::MessageDeliverySurface::TaskRejoin)
                && message.admission_context == Some(crate::types::AdmissionContext::RuntimeOwned)
                && crate::wake_contract::agent_message_reply_reference(&message).is_none())
        })
    }

    /// An owner wake may outlive its selector, but never its consumption grant.
    pub(crate) fn reply_source_is_consumed_for_owner(
        &self,
        record: &TaskResultSettlementRecord,
    ) -> Result<bool> {
        self.reply_source_consumption_matches(record, true)
    }

    /// Consumption evidence is not itself a grant to admit another waiter.
    pub(crate) fn reply_source_is_consumed(
        &self,
        record: &TaskResultSettlementRecord,
    ) -> Result<bool> {
        self.reply_source_consumption_matches(record, false)
    }

    fn reply_source_consumption_matches(
        &self,
        record: &TaskResultSettlementRecord,
        require_owner_grant: bool,
    ) -> Result<bool> {
        self.db.transaction(|tx| {
            if !record_matches_durable_task_tx(tx, record)? {
                return Ok(false);
            }
            let Some(payload) = tx
                .query_row(
                    "SELECT payload_json FROM messages WHERE message_id = ?1",
                    [&record.message_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            else {
                return Ok(false);
            };
            let message: MessageEnvelope = serde_json::from_str(&payload)?;
            let Some(reply_id) = crate::wake_contract::agent_message_reply_reference(&message)
            else {
                return Ok(false);
            };
            let Some(payload) = tx
                .query_row(
                    "SELECT payload_json FROM messages WHERE message_id = ?1",
                    [reply_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            else {
                return Ok(false);
            };
            let reply: MessageEnvelope = serde_json::from_str(&payload)?;
            if message.agent_id != record.agent_id
                || message.work_item_id != record.work_item_id
                || message.task_id.as_deref() != Some(record.task_id.as_str())
                || reply.agent_id != record.agent_id
                || !crate::wake_contract::is_agent_message(&reply)
                || !reply.routing_context.as_ref().is_some_and(|route| {
                    route.message_id == reply.id && route.recipient_agent_id == record.agent_id
                })
            {
                return Ok(false);
            }
            let payload: String = tx.query_row(
                "SELECT payload_json FROM tasks WHERE task_id = ?1",
                [&record.task_id],
                |row| row.get(0),
            )?;
            let task: TaskRecord = serde_json::from_str(&payload)?;
            let detail = task.detail.as_ref();
            let delivery_id = reply
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("agent_message_delivery"))
                .and_then(|delivery| delivery.get("delivery_id"))
                .and_then(serde_json::Value::as_str);
            if task.kind != crate::types::TaskKind::AgentMessageWait
                || detail
                    .and_then(|detail| detail.get("message_id"))
                    .and_then(serde_json::Value::as_str)
                    != Some(reply_id)
                || detail
                    .and_then(|detail| detail.get("reply_content_source"))
                    .and_then(serde_json::Value::as_str)
                    != Some("original_message")
                || delivery_id.is_none_or(str::is_empty)
                || detail
                    .and_then(|detail| detail.get("response_delivery_id"))
                    .and_then(serde_json::Value::as_str)
                    != delivery_id
            {
                return Ok(false);
            }
            let Some(payload) = tx
                .query_row(
                    "SELECT payload_json FROM agent_message_deliveries
                     WHERE delivery_id = ?1 AND message_id = ?2",
                    params![delivery_id, reply_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            else {
                return Ok(false);
            };
            let delivery: crate::types::AgentMessageDeliveryRecord =
                serde_json::from_str(&payload)?;
            if delivery.target_agent_id != record.agent_id
                || delivery.routing_context != reply.routing_context
                || delivery.outcome != crate::types::AgentMessageDeliveryOutcome::Accepted
                || delivery.state != crate::types::AgentMessageDeliveryState::Consumed
            {
                return Ok(false);
            }
            // The original queue source owns the first delivery. Its terminal
            // consumption permits recovery of this independently fenced waiter.
            if require_owner_grant {
                live_turn::consumed_reply_owner_matches_tx(tx, &task, reply_id, &delivery)
            } else {
                live_turn::reply_source_queue_is_consumed_tx(
                    tx,
                    &record.agent_id,
                    reply_id,
                    &delivery,
                )
            }
        })
    }

    /// The caller reserves rendered bytes and every waiter identity before this
    /// source-specific transaction independently rechecks their eligibility.
    pub(crate) fn admit_reply_message_snapshot(
        &self,
        agent_id: &str,
        work_item_id: Option<&str>,
        message_id: &str,
        activation_id: &str,
        selected: &std::collections::BTreeSet<String>,
        cutoff: DateTime<Utc>,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.db.transaction(|tx| {
            let already = admitted_for_activation_tx(tx, agent_id, activation_id)?;
            let current = reply_admission_attempt_tx(tx, agent_id, activation_id)?;
            let anchor_reserved = if let Some(attempt) = current.as_ref() {
                use crate::domain::execution_protocol::{
                    ExecutionAttemptState, ExecutionBinding,
                };
                let owner_matches = match &attempt.binding {
                    ExecutionBinding::WorkItem { work_item_id: owner } => {
                        work_item_id == Some(owner.as_str())
                    }
                    ExecutionBinding::AgentLifecycle { agent_id: owner } => {
                        owner == agent_id && work_item_id.is_none()
                    }
                    _ => false,
                };
                let Some(source_id) = attempt.source_message_id.as_deref() else {
                    return Ok(Vec::new());
                };
                if attempt.agent_id != agent_id
                    || attempt.state != ExecutionAttemptState::Open
                    || !owner_matches
                    || !tx.query_row(
                        "SELECT EXISTS(
                           SELECT 1 FROM turn_records
                           WHERE agent_id = ?1 AND trigger_message_id = ?2
                             AND current_work_item_id IS ?3 AND terminal_kind IS NULL
                             AND EXISTS(
                               SELECT 1 FROM json_each(turn_records.payload_json, '$.input_message_ids') input
                               WHERE input.value = ?2
                             )
                         )",
                        params![agent_id, source_id, work_item_id],
                        |row| row.get::<_, bool>(0),
                    )?
                {
                    return Ok(Vec::new());
                }
                let payload: String = tx.query_row(
                    "SELECT payload_json FROM messages WHERE message_id = ?1",
                    [source_id],
                    |row| row.get(0),
                )?;
                let source: MessageEnvelope = serde_json::from_str(&payload)?;
                crate::wake_contract::is_agent_message(&source)
            } else {
                false
            };
            let mut remaining = TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT
                .saturating_sub(already.len() + usize::from(anchor_reserved));
            let mut records =
                reply_message_candidates_tx(tx, agent_id, work_item_id, message_id, 64)?;
            if let Some(record) = exact_waiter::candidate_tx(
                tx, agent_id, work_item_id, activation_id,
            )? {
                let payload: String = tx.query_row(
                    "SELECT payload_json FROM messages WHERE message_id = ?1",
                    [&record.message_id], |row| row.get(0),
                )?;
                let message: MessageEnvelope = serde_json::from_str(&payload)?;
                if crate::wake_contract::agent_message_reply_reference(&message) == Some(message_id)
                    && !records.iter().any(|prior| prior.result_identity == record.result_identity)
                {
                    records.insert(0, record);
                }
            }
            let mut admitted = Vec::new();
            for mut record in records {
                if remaining == 0 {
                    break;
                }
                if record.created_at > cutoff
                    || !selected.contains(&record.result_identity)
                    || already
                        .iter()
                        .any(|prior| prior.result_identity == record.result_identity)
                {
                    continue;
                }
                if !record_matches_durable_task_tx(tx, &record)? {
                    continue;
                }
                if record.state == TaskResultSettlementState::CallerAdmitted
                    && record.activation_id.as_deref() != Some(activation_id)
                {
                    let Some(current) = current.as_ref() else {
                        continue;
                    };
                    let Some(prior_id) = record.activation_id.as_deref() else {
                        continue;
                    };
                    let prior = reply_admission_attempt_tx(tx, agent_id, prior_id)?;
                    let interrupted_replay = prior.as_ref().is_some_and(|prior| {
                        current.recovery_of_attempt_id.as_deref() == Some(prior_id)
                            && prior.state
                                == crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
                            && prior.agent_id == current.agent_id
                            && prior.source_message_id == current.source_message_id
                            && prior.binding == current.binding
                    });
                    // A closed attempt alone is not a grant. A runtime recovery
                    // wake must independently revalidate this exact obligation.
                    if !interrupted_replay
                        && !obligation::recovery_wake_admits_tx(
                            tx, &record, current, work_item_id,
                        )?
                    {
                        continue;
                    }
                }
                if !charge_selected_recovery_tx(tx, &mut record, activation_id, cutoff)? {
                    continue;
                }
                record.state = TaskResultSettlementState::CallerAdmitted;
                record.activation_id = Some(activation_id.to_owned());
                record.admitted_at = Some(cutoff);
                record.updated_at = cutoff;
                record.deferred_reason = None;
                record.deferred_at = None;
                update_tx(tx, &record)?;
                admitted.push(record);
                remaining -= 1;
            }
            Ok(admitted)
        })
    }

    pub(crate) fn admitted_for_activation(
        &self,
        agent_id: &str,
        activation_id: &str,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM task_result_settlements
             WHERE agent_id = ?1 AND activation_id = ?2 AND state = 'caller_admitted'
             ORDER BY created_at ASC, result_identity ASC",
        )?;
        let rows = statement.query_map(params![agent_id, activation_id], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|row| {
            let payload = row?;
            serde_json::from_str(&payload).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn admit_message(
        &self,
        agent_id: &str,
        message_id: &str,
        activation_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<TaskResultSettlementRecord>> {
        self.db.transaction(|tx| {
            let Some(mut record) = latest_for_message_tx(tx, message_id)? else {
                return Ok(None);
            };
            if record.agent_id != agent_id {
                bail!("task result settlement agent mismatch for message {message_id}");
            }
            if !record_matches_durable_task_tx(tx, &record)? {
                settle_invalid_or_stale_tx(tx, &mut record, now)?;
                return Ok(None);
            }
            match record.state {
                TaskResultSettlementState::PersistedPending => {
                    record.state = TaskResultSettlementState::CallerAdmitted;
                    record.activation_id = Some(activation_id.to_owned());
                    record.admitted_at = Some(now);
                    record.updated_at = now;
                    record.deferred_reason = None;
                    record.deferred_at = None;
                    update_tx(tx, &record)?;
                    Ok(Some(record))
                }
                TaskResultSettlementState::CallerAdmitted
                    if record.activation_id.as_deref() == Some(activation_id) =>
                {
                    Ok(Some(record))
                }
                TaskResultSettlementState::CallerAdmitted
                    if interrupted_predecessor_owns_record_tx(
                        tx,
                        &record,
                        activation_id,
                        message_id,
                    )? =>
                {
                    // Reclaim the original admission; this is not a second result.
                    record.activation_id = Some(activation_id.to_owned());
                    record.updated_at = now;
                    record.deferred_reason = None;
                    record.deferred_at = None;
                    update_tx(tx, &record)?;
                    Ok(Some(record))
                }
                TaskResultSettlementState::CallerAdmitted | TaskResultSettlementState::Settled => {
                    Ok(None)
                }
            }
        })
    }

    #[cfg(test)]
    pub(crate) fn settle_activation(
        &self,
        agent_id: &str,
        activation_id: &str,
        disposition: TaskResultSettlementDisposition,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        self.db
            .transaction(|tx| settle_activation_tx(tx, agent_id, activation_id, disposition, now))
    }

    pub(crate) fn settle_owner_unavailable(
        &self,
        message_id: &str,
        disposition: TaskResultSettlementDisposition,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        self.db.transaction(|tx| {
            let Some(mut record) = latest_for_message_tx(tx, message_id)? else {
                return Ok(false);
            };
            if record.state == TaskResultSettlementState::Settled {
                return Ok(false);
            }
            record.state = TaskResultSettlementState::Settled;
            record.disposition = Some(disposition);
            record.settled_at = Some(now);
            record.next_recheck_at = None;
            record.updated_at = now;
            update_tx(tx, &record)?;
            Ok(true)
        })
    }

    pub(crate) fn settle_reply_consumed_elsewhere(
        &self,
        message_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        self.settle_owner_unavailable(
            message_id,
            TaskResultSettlementDisposition::ReplyConsumedElsewhere,
            now,
        )
    }

    pub(crate) fn mark_deferred(
        &self,
        message_id: &str,
        reason: &str,
        now: DateTime<Utc>,
        next_recheck_at: DateTime<Utc>,
    ) -> Result<Option<TaskResultSettlementRecord>> {
        self.db.transaction(|tx| {
            let Some(mut record) = latest_for_message_tx(tx, message_id)? else {
                return Ok(None);
            };
            if record.state != TaskResultSettlementState::Settled {
                if record.recovery_paused_at.is_some() {
                    return Ok(Some(record));
                }
                record.deferred_reason = Some(reason.to_owned());
                record.deferred_at = Some(now);
                record.next_recheck_at =
                    Some(record.next_recheck_at.map_or(next_recheck_at, |existing| {
                        if record.recovery_attempts > 0 {
                            existing.max(next_recheck_at)
                        } else {
                            next_recheck_at
                        }
                    }));
                record.updated_at = now;
                defer_update_tx(tx, &record)?;
            }
            Ok(Some(record))
        })
    }

    pub(crate) fn wake_deferred_for_owner(
        &self,
        agent_id: &str,
        work_item_id: &str,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        self.db.transaction(|tx| {
            let mut statement = tx.prepare(
                "SELECT payload_json
                 FROM task_result_settlements
                 WHERE agent_id = ?1
                   AND work_item_id = ?2
                   AND state != 'settled'
                   AND recovery_paused_at IS NULL
                   AND recovery_epoch = 0
                   AND recovery_attempts = 0
                   AND next_recheck_at IS NOT NULL",
            )?;
            let records = statement
                .query_map(params![agent_id, work_item_id], |row| {
                    row.get::<_, String>(0)
                })?
                .map(|row| {
                    serde_json::from_str::<TaskResultSettlementRecord>(&row?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut woken = 0;
            for mut record in records {
                record.next_recheck_at = Some(now);
                record.updated_at = now;
                defer_update_tx(tx, &record)?;
                woken += 1;
            }
            Ok(woken)
        })
    }

    pub(crate) fn due_deferred(
        &self,
        agent_id: &str,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM task_result_settlements
             WHERE agent_id = ?1
               AND recovery_paused_at IS NULL
               AND next_recheck_at IS NOT NULL
               AND next_recheck_at <= ?2
               AND (
                 state = 'persisted_pending'
                 OR (
                   state = 'caller_admitted'
                   AND NOT EXISTS (
                     SELECT 1
                     FROM execution_protocol_attempts attempts
                     WHERE attempts.agent_id = task_result_settlements.agent_id
                       AND attempts.attempt_id = task_result_settlements.activation_id
                       AND attempts.lifecycle_state = 'open'
                   )
                 )
               )
             ORDER BY next_recheck_at ASC, created_at ASC, result_identity ASC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(params![agent_id, timestamp(now), limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|row| {
            serde_json::from_str(&row?).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn unsettled_for_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM task_result_settlements
             WHERE agent_id = ?1
               AND (
                 state = 'persisted_pending'
                 OR (
                   state = 'caller_admitted'
                   AND NOT EXISTS (
                     SELECT 1
                     FROM execution_protocol_attempts attempts
                     WHERE attempts.agent_id = task_result_settlements.agent_id
                       AND attempts.attempt_id = task_result_settlements.activation_id
                       AND attempts.lifecycle_state = 'open'
                   )
                 )
               )
             ORDER BY created_at ASC, result_identity ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![agent_id, limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|row| {
            serde_json::from_str(&row?).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn unsettled_for_owner(
        &self,
        agent_id: &str,
        work_item_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        let connection = self.db.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json
             FROM task_result_settlements
             WHERE agent_id = ?1
               AND work_item_id IS ?2
               AND (
                 state = 'persisted_pending'
                 OR (
                   state = 'caller_admitted'
                   AND NOT EXISTS (
                     SELECT 1
                     FROM execution_protocol_attempts attempts
                     WHERE attempts.agent_id = task_result_settlements.agent_id
                       AND attempts.attempt_id = task_result_settlements.activation_id
                       AND attempts.lifecycle_state = 'open'
                   )
                 )
               )
             ORDER BY (recovery_paused_at IS NOT NULL), created_at ASC, result_identity ASC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(params![agent_id, work_item_id, limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|row| {
            serde_json::from_str(&row?).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn next_recheck_at(&self, agent_id: &str) -> Result<Option<DateTime<Utc>>> {
        let connection = self.db.connection()?;
        let value = connection.query_row(
            "SELECT MIN(next_recheck_at)
             FROM task_result_settlements
             WHERE agent_id = ?1
               AND recovery_paused_at IS NULL
               AND next_recheck_at IS NOT NULL
               AND (
                 state = 'persisted_pending'
                 OR (
                   state = 'caller_admitted'
                   AND NOT EXISTS (
                     SELECT 1
                     FROM execution_protocol_attempts attempts
                     WHERE attempts.agent_id = task_result_settlements.agent_id
                       AND attempts.attempt_id = task_result_settlements.activation_id
                       AND attempts.lifecycle_state = 'open'
                   )
                 )
               )",
            params![agent_id],
            |row| row.get::<_, Option<String>>(0),
        )?;
        value
            .map(|value| {
                DateTime::parse_from_rfc3339(&value).map(|value| value.with_timezone(&Utc))
            })
            .transpose()
            .map_err(Into::into)
    }

    pub(crate) fn latest_for_message(
        &self,
        message_id: &str,
    ) -> Result<Option<TaskResultSettlementRecord>> {
        let connection = self.db.connection()?;
        latest_for_message_connection(&connection, message_id)
    }
}

pub(crate) fn upsert_pending_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
) -> Result<bool> {
    if let Some(existing) = latest_for_message_tx(tx, &record.message_id)? {
        if existing.result_identity != record.result_identity
            || existing.agent_id != record.agent_id
            || existing.task_id != record.task_id
            || existing.work_item_id != record.work_item_id
            || existing.rejoin_generation != record.rejoin_generation
            || existing.parent_turn_id != record.parent_turn_id
        {
            bail!(
                "task result settlement identity conflict for message {}",
                record.message_id
            );
        }
        return Ok(false);
    }
    super::reclamation::ensure_work_admission_tx(tx, &record.agent_id)?;
    let payload = serde_json::to_string(record)?;
    let inserted = tx.execute(
        "INSERT INTO task_result_settlements (
           result_identity, agent_id, task_id, message_id, work_item_id,
           rejoin_generation, parent_turn_id, state, activation_id, disposition,
           created_at, updated_at, admitted_at, settled_at, deferred_reason, deferred_at, next_recheck_at, payload_json
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
         )",
        params![
            record.result_identity,
            record.agent_id,
            record.task_id,
            record.message_id,
            record.work_item_id,
            record.rejoin_generation,
            record.parent_turn_id,
            record.state.as_str(),
            record.activation_id,
            record
                .disposition
                .map(TaskResultSettlementDisposition::as_str),
            timestamp(record.created_at),
            timestamp(record.updated_at),
            record.admitted_at.map(timestamp),
            record.settled_at.map(timestamp),
            record.deferred_reason,
            record.deferred_at.map(timestamp),
            record.next_recheck_at.map(timestamp),
            payload,
        ],
    )?;
    Ok(inserted == 1)
}

fn update_tx(tx: &Transaction<'_>, record: &TaskResultSettlementRecord) -> Result<()> {
    let payload = serde_json::to_string(record)?;
    tx.execute(
        "UPDATE task_result_settlements
         SET state = ?2, activation_id = ?3, disposition = ?4, updated_at = ?5,
             admitted_at = ?6, settled_at = ?7, deferred_reason = ?8,
           deferred_at = ?9, next_recheck_at = ?10, payload_json = ?11,
           recovery_epoch = ?12, recovery_attempts = ?13,
           last_recovery_root_attempt_id = ?14, last_recovery_admitted_at = ?15,
           recovery_paused_at = ?16, recovery_diagnostic_at = ?17
         WHERE result_identity = ?1",
        params![
            record.result_identity,
            record.state.as_str(),
            record.activation_id,
            record
                .disposition
                .map(TaskResultSettlementDisposition::as_str),
            timestamp(record.updated_at),
            record.admitted_at.map(timestamp),
            record.settled_at.map(timestamp),
            record.deferred_reason,
            record.deferred_at.map(timestamp),
            record.next_recheck_at.map(timestamp),
            payload,
            record.recovery_epoch,
            record.recovery_attempts,
            record.last_recovery_root_attempt_id,
            record.last_recovery_admitted_at.map(timestamp),
            record.recovery_paused_at.map(timestamp),
            record.recovery_diagnostic_at.map(timestamp),
        ],
    )?;
    Ok(())
}

/// Narrowed UPDATE for the deferred-recheck hot path. The recovery loop
/// rewrites the same due rows every recheck interval; a full-column UPDATE
/// touches `state`/`activation_id` and churns every index referencing them
/// even though those values never change on deferral.
fn defer_update_tx(tx: &Transaction<'_>, record: &TaskResultSettlementRecord) -> Result<()> {
    let payload = serde_json::to_string(record)?;
    tx.execute(
        "UPDATE task_result_settlements
         SET deferred_reason = ?2, deferred_at = ?3, next_recheck_at = ?4,
             updated_at = ?5, payload_json = ?6
         WHERE result_identity = ?1",
        params![
            record.result_identity,
            record.deferred_reason,
            record.deferred_at.map(timestamp),
            record.next_recheck_at.map(timestamp),
            timestamp(record.updated_at),
            payload,
        ],
    )?;
    Ok(())
}

fn record_matches_durable_task_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
) -> Result<bool> {
    let durable_task = tx
        .query_row(
            "SELECT payload_json FROM tasks WHERE task_id = ?1",
            [&record.task_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<TaskRecord>(&payload))
        .transpose()?;
    let Some(task) = durable_task else {
        return Ok(false);
    };
    let Ok(fence) = task.rejoin_fence() else {
        return Ok(false);
    };
    Ok(task.agent_id == record.agent_id
        && task.work_item_id == record.work_item_id
        && task
            .parent_message_id
            .as_deref()
            .is_none_or(|parent_message_id| parent_message_id == record.message_id)
        && matches!(
            task.status,
            crate::types::TaskStatus::Completed
                | crate::types::TaskStatus::Failed
                | crate::types::TaskStatus::Cancelled
                | crate::types::TaskStatus::Interrupted
        )
        && fence.generation == record.rejoin_generation
        && fence.parent_turn_id == record.parent_turn_id)
}

fn settle_invalid_or_stale_tx(
    tx: &Transaction<'_>,
    record: &mut TaskResultSettlementRecord,
    now: DateTime<Utc>,
) -> Result<()> {
    record.state = TaskResultSettlementState::Settled;
    record.disposition = Some(TaskResultSettlementDisposition::InvalidOrStale);
    record.settled_at = Some(now);
    record.updated_at = now;
    record.next_recheck_at = None;
    update_tx(tx, record)
}

fn existing_for_task_generation_tx(
    tx: &Transaction<'_>,
    task_id: &str,
    rejoin_generation: u64,
) -> Result<Option<TaskResultSettlementRecord>> {
    tx.query_row(
        "SELECT payload_json
         FROM task_result_settlements
         WHERE task_id = ?1 AND rejoin_generation = ?2
         ORDER BY created_at ASC, result_identity ASC
         LIMIT 1",
        params![task_id, rejoin_generation],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| serde_json::from_str(&payload))
    .transpose()
    .map_err(Into::into)
}

enum RecoverySnapshotScope {
    NormalInput,
    RecoveryRoot(String),
    Invalid,
}

fn recovery_snapshot_scope_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    activation_id: &str,
) -> Result<RecoverySnapshotScope> {
    use crate::types::{AdmissionContext, AuthorityClass, MessageDeliverySurface, MessageOrigin};
    let Some(attempt) = reply_admission_attempt_tx(tx, agent_id, activation_id)? else {
        return Ok(RecoverySnapshotScope::NormalInput);
    };
    let Some(source_id) = attempt.source_message_id.as_deref() else {
        return Ok(RecoverySnapshotScope::NormalInput);
    };
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE message_id = ?1",
            [source_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(RecoverySnapshotScope::NormalInput);
    };
    let source: MessageEnvelope = serde_json::from_str(&payload)?;
    if !matches!(&source.origin, MessageOrigin::System { subsystem } if subsystem == "task_result_recovery")
    {
        return Ok(RecoverySnapshotScope::NormalInput);
    }
    if attempt.state != crate::domain::execution_protocol::ExecutionAttemptState::Open
        || source.agent_id != agent_id
        || source.authority_class != AuthorityClass::RuntimeInstruction
        || source.delivery_surface != Some(MessageDeliverySurface::RuntimeSystem)
        || source.admission_context != Some(AdmissionContext::RuntimeOwned)
    {
        return Ok(RecoverySnapshotScope::Invalid);
    }
    let mut root = attempt;
    let mut seen = std::collections::BTreeSet::from([root.attempt_id.clone()]);
    while let Some(prior_id) = root.recovery_of_attempt_id.as_deref() {
        let Some(prior) = reply_admission_attempt_tx(tx, agent_id, prior_id)? else {
            return Ok(RecoverySnapshotScope::Invalid);
        };
        if prior.source_message_id != root.source_message_id {
            break;
        }
        if seen.len() >= 64
            || !seen.insert(prior.attempt_id.clone())
            || prior.binding != root.binding
            || prior.state != crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
        {
            return Ok(RecoverySnapshotScope::Invalid);
        }
        root = prior;
    }
    Ok(RecoverySnapshotScope::RecoveryRoot(root.attempt_id))
}

impl TaskResultSettlementRepository<'_> {
    /// Ineligible recovery roots must not reserve snapshot slots or bytes.
    /// Normal authorized input can still deliver a paused result.
    pub(crate) fn recovery_snapshot_candidates_for_owner(
        &self,
        agent_id: &str,
        work_item_id: Option<&str>,
        activation_id: &str,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.db.transaction(|tx| {
            let RecoverySnapshotScope::RecoveryRoot(root) =
                recovery_snapshot_scope_tx(tx, agent_id, activation_id)?
            else {
                return Ok(Vec::new());
            };
            recovery_budget::candidates_for_owner_tx(tx, agent_id, work_item_id, &root, now, limit)
        })
    }

    pub(crate) fn filter_recovery_snapshot_candidates(
        &self,
        agent_id: &str,
        activation_id: &str,
        records: &[TaskResultSettlementRecord],
        now: DateTime<Utc>,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.db.transaction(|tx| {
            let root = match recovery_snapshot_scope_tx(tx, agent_id, activation_id)? {
                RecoverySnapshotScope::NormalInput => return Ok(records.to_vec()),
                RecoverySnapshotScope::Invalid => return Ok(Vec::new()),
                RecoverySnapshotScope::RecoveryRoot(root) => root,
            };
            let mut eligible = Vec::with_capacity(records.len());
            for record in records {
                let Some(current) = latest_for_message_tx(tx, &record.message_id)? else {
                    continue;
                };
                if current.agent_id == agent_id
                    && current.result_identity == record.result_identity
                    && recovery_budget::recovery_admission_eligible_tx(tx, &current, &root, now)?
                {
                    eligible.push(current);
                }
            }
            Ok(eligible)
        })
    }
}

/// Budget is charged with the selected snapshot, not scanner probes or fallback
/// hops. Reload after charging so the admission write preserves retry metadata.
fn charge_selected_recovery_tx(
    tx: &Transaction<'_>,
    record: &mut TaskResultSettlementRecord,
    activation_id: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    let root = match recovery_snapshot_scope_tx(tx, &record.agent_id, activation_id)? {
        RecoverySnapshotScope::NormalInput => return Ok(true),
        RecoverySnapshotScope::Invalid => return Ok(false),
        RecoverySnapshotScope::RecoveryRoot(root) => root,
    };
    if !charge_recovery_admission_tx(tx, &record.result_identity, &root, now)? {
        return Ok(false);
    }
    *record = latest_for_message_tx(tx, &record.message_id)?
        .ok_or_else(|| anyhow::anyhow!("charged result disappeared during snapshot admission"))?;
    Ok(true)
}

fn interrupted_predecessor_owns_record_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
    activation_id: &str,
    source_message_id: &str,
) -> Result<bool> {
    use crate::domain::execution_protocol::ExecutionAttemptState;
    let Some(current) = reply_admission_attempt_tx(tx, &record.agent_id, activation_id)? else {
        return Ok(false);
    };
    let Some(prior_id) = record.activation_id.as_deref() else {
        return Ok(false);
    };
    if current.agent_id != record.agent_id
        || current.state != ExecutionAttemptState::Open
        || current.recovery_of_attempt_id.as_deref() != Some(prior_id)
        || current.source_message_id.as_deref() != Some(source_message_id)
    {
        return Ok(false);
    }
    let Some(prior) = reply_admission_attempt_tx(tx, &record.agent_id, prior_id)? else {
        return Ok(false);
    };
    // The consuming owner is inherited, not replaced by the captured task
    // owner: a canonical exact waiter may intentionally be a different owner.
    Ok(prior.state == ExecutionAttemptState::Interrupted
        && prior.agent_id == current.agent_id
        && prior.source_message_id == current.source_message_id
        && prior.binding == current.binding)
}

fn reply_admission_attempt_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    activation_id: &str,
) -> Result<Option<crate::domain::execution_protocol::ExecutionAttempt>> {
    tx.query_row(
        "SELECT payload_json FROM execution_protocol_attempts
         WHERE agent_id = ?1 AND attempt_id = ?2",
        params![agent_id, activation_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
    .transpose()
}

fn reply_message_candidates_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    work_item_id: Option<&str>,
    message_id: &str,
    limit: usize,
) -> Result<Vec<TaskResultSettlementRecord>> {
    // A shared body is not a shared grant. Only this owner's task or this
    // owner's resolved exact waiter authorizes each individual identity.
    let records = records_tx(
        tx,
        "SELECT settlements.payload_json FROM task_result_settlements settlements
         JOIN messages ON messages.message_id = settlements.message_id
         WHERE settlements.agent_id = ?1
           AND (settlements.work_item_id IS ?2 OR settlements.task_id IN (
             SELECT json_extract(wake.value, '$.task_id')
             FROM wait_conditions AS waits, json_each(waits.wake_sources_json) AS wake
             WHERE waits.agent_id = ?1 AND waits.work_item_id IS ?2
               AND waits.status = 'resolved' AND waits.kind = 'task'
               AND waits.trigger_message_id = ?3
               AND json_extract(wake.value, '$.kind') = 'task_result'
           ))
           AND json_extract(messages.payload_json, '$.metadata.task_detail.message_id') = ?3
           AND json_extract(messages.payload_json, '$.metadata.task_kind') = 'agent_message_wait'
           AND json_extract(messages.payload_json, '$.metadata.task_status') = 'completed'
           AND json_extract(messages.payload_json, '$.metadata.task_detail.reply_content_source') = 'original_message'
           AND settlements.state != 'settled'
         ORDER BY CASE settlements.state WHEN 'caller_admitted' THEN 0 ELSE 1 END,
                  CASE WHEN settlements.task_id IN (
                    SELECT json_extract(wake.value, '$.task_id')
                    FROM wait_conditions AS waits, json_each(waits.wake_sources_json) AS wake
                    WHERE waits.agent_id = ?1 AND waits.work_item_id IS ?2
                      AND waits.status = 'resolved' AND waits.kind = 'task'
                      AND waits.trigger_message_id = ?3
                      AND json_extract(wake.value, '$.kind') = 'task_result'
                  ) THEN 0 ELSE 1 END,
                  settlements.created_at ASC, settlements.result_identity ASC
         LIMIT ?4",
        params![agent_id, work_item_id, message_id, limit.min(64) as i64],
    )?;
    let mut eligible = Vec::new();
    for record in records {
        let payload: String = tx.query_row(
            "SELECT payload_json FROM messages WHERE message_id = ?1",
            [&record.message_id],
            |row| row.get(0),
        )?;
        let message: MessageEnvelope = serde_json::from_str(&payload)?;
        if message.agent_id == agent_id
            && message.task_id.as_deref() == Some(record.task_id.as_str())
            && crate::wake_contract::agent_message_reply_reference(&message) == Some(message_id)
        {
            eligible.push(record);
        }
    }
    Ok(eligible)
}

fn unsettled_for_owner_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    work_item_id: Option<&str>,
    selected: Option<&std::collections::BTreeSet<String>>,
    limit: usize,
) -> Result<Vec<TaskResultSettlementRecord>> {
    records_tx(
        tx,
        "SELECT payload_json
         FROM task_result_settlements
         WHERE agent_id = ?1
           AND work_item_id IS ?2
           AND (?4 IS NULL OR result_identity IN (SELECT value FROM json_each(?4)))
           AND NOT EXISTS (
             SELECT 1 FROM messages
             WHERE messages.message_id = task_result_settlements.message_id
               AND json_extract(messages.payload_json, '$.metadata.task_kind') = 'agent_message_wait'
               AND json_extract(messages.payload_json, '$.metadata.task_status') = 'completed'
               AND json_extract(messages.payload_json, '$.metadata.task_detail.reply_content_source') = 'original_message'
               AND json_extract(messages.payload_json, '$.metadata.task_detail.message_id') IS NOT NULL
           )
           AND (
             state = 'persisted_pending'
             OR (
               state = 'caller_admitted'
               AND NOT EXISTS (
                 SELECT 1
                 FROM execution_protocol_attempts attempts
                 WHERE attempts.agent_id = task_result_settlements.agent_id
                   AND attempts.attempt_id = task_result_settlements.activation_id
                   AND attempts.lifecycle_state = 'open'
               )
             )
           )
         ORDER BY created_at ASC, result_identity ASC
         LIMIT ?3",
        params![
            agent_id,
            work_item_id,
            limit as i64,
            selected.map(serde_json::to_string).transpose()?,
        ],
    )
}

fn admitted_for_activation_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    activation_id: &str,
) -> Result<Vec<TaskResultSettlementRecord>> {
    records_tx(
        tx,
        "SELECT payload_json
         FROM task_result_settlements
         WHERE agent_id = ?1 AND activation_id = ?2 AND state = 'caller_admitted'
         ORDER BY created_at ASC, result_identity ASC",
        params![agent_id, activation_id],
    )
}

pub(crate) fn settle_activation_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    activation_id: &str,
    disposition: TaskResultSettlementDisposition,
    now: DateTime<Utc>,
) -> Result<usize> {
    let mut records = admitted_for_activation_tx(tx, agent_id, activation_id)?;
    for record in &mut records {
        record.state = TaskResultSettlementState::Settled;
        record.disposition = Some(disposition);
        record.settled_at = Some(now);
        record.updated_at = now;
        update_tx(tx, record)?;
    }
    Ok(records.len())
}

fn records_tx<P>(
    tx: &Transaction<'_>,
    sql: &str,
    params: P,
) -> Result<Vec<TaskResultSettlementRecord>>
where
    P: rusqlite::Params,
{
    let mut statement = tx.prepare(sql)?;
    let rows = statement.query_map(params, |row| row.get::<_, String>(0))?;
    rows.map(|row| {
        let payload = row?;
        serde_json::from_str(&payload).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    })
    .collect::<std::result::Result<Vec<_>, _>>()
    .map_err(Into::into)
}

fn latest_for_message_connection(
    connection: &rusqlite::Connection,
    message_id: &str,
) -> Result<Option<TaskResultSettlementRecord>> {
    connection
        .query_row(
            "SELECT payload_json FROM task_result_settlements WHERE message_id = ?1",
            params![message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str(&payload))
        .transpose()
        .map_err(Into::into)
}

fn latest_for_message_tx(
    tx: &Transaction<'_>,
    message_id: &str,
) -> Result<Option<TaskResultSettlementRecord>> {
    let payload = tx
        .query_row(
            "SELECT payload_json FROM task_result_settlements WHERE message_id = ?1",
            [message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    payload
        .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
        .transpose()
}
