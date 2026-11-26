//! Durable Task Result settlement records and their deterministic identity.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    execution_protocol::TaskRejoinSource, IdleInputBatchItem, MessageEnvelope, TaskRecord,
    TaskStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskResultSettlementState {
    PersistedPending,
    CallerAdmitted,
    Settled,
}

impl TaskResultSettlementState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PersistedPending => "persisted_pending",
            Self::CallerAdmitted => "caller_admitted",
            Self::Settled => "settled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskResultSettlementDisposition {
    ModelDelivered,
    ReplyConsumedElsewhere,
    OwnerClosed,
    OwnerMissing,
    InvalidOrStale,
    InterruptedReplaySourceFinal,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskResultActivationSettlement {
    pub activation_id: String,
    pub disposition: TaskResultSettlementDisposition,
    pub settled_at: DateTime<Utc>,
}

impl TaskResultSettlementDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ModelDelivered => "model_delivered",
            Self::ReplyConsumedElsewhere => "reply_consumed_elsewhere",
            Self::OwnerClosed => "owner_closed",
            Self::OwnerMissing => "owner_missing",
            Self::InvalidOrStale => "invalid_or_stale",
            Self::InterruptedReplaySourceFinal => "interrupted_replay_source_final",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResultSettlementRecord {
    pub result_identity: String,
    pub agent_id: String,
    pub task_id: String,
    pub message_id: String,
    pub work_item_id: Option<String>,
    pub rejoin_generation: u64,
    #[serde(flatten)]
    pub source: TaskRejoinSource,
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
    #[serde(default)]
    pub source_projection: Option<IdleInputBatchItem>,
    #[serde(default)]
    pub reply_projection: Option<IdleInputBatchItem>,
}

impl TaskResultSettlementRecord {
    pub fn pending(
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
            TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
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
            source: fence.source,
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
            source_projection: None,
            reply_projection: None,
        }))
    }
}

#[cfg(test)]
mod tests;
