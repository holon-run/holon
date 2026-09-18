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

    pub(crate) fn admit_unsettled(
        &self,
        agent_id: &str,
        work_item_id: Option<&str>,
        activation_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.db.transaction(|tx| {
            let mut records = unsettled_for_owner_tx(
                tx,
                agent_id,
                work_item_id,
                TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT,
            )?;
            let mut admitted = Vec::with_capacity(records.len());
            for mut record in records.drain(..) {
                if !record_matches_durable_task_tx(tx, &record)? {
                    settle_invalid_or_stale_tx(tx, &mut record, now)?;
                    continue;
                }
                record.state = TaskResultSettlementState::CallerAdmitted;
                record.activation_id = Some(activation_id.to_owned());
                record.admitted_at = Some(now);
                record.updated_at = now;
                record.deferred_reason = None;
                record.deferred_at = None;
                update_tx(tx, &record)?;
                admitted.push(record);
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
                record.deferred_reason = Some(reason.to_owned());
                record.deferred_at = Some(now);
                record.next_recheck_at = Some(next_recheck_at);
                record.updated_at = now;
                update_tx(tx, &record)?;
            }
            Ok(Some(record))
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

    pub(crate) fn next_recheck_at(&self, agent_id: &str) -> Result<Option<DateTime<Utc>>> {
        let connection = self.db.connection()?;
        let value = connection.query_row(
            "SELECT MIN(next_recheck_at)
             FROM task_result_settlements
             WHERE agent_id = ?1
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
             deferred_at = ?9, next_recheck_at = ?10, payload_json = ?11
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

fn unsettled_for_owner_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    work_item_id: Option<&str>,
    limit: usize,
) -> Result<Vec<TaskResultSettlementRecord>> {
    records_tx(
        tx,
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
         ORDER BY created_at ASC, result_identity ASC
         LIMIT ?3",
        params![agent_id, work_item_id, limit as i64],
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
