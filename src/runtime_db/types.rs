//! Public types for runtime_db repositories.

use crate::runtime_db::RuntimeDb;
use crate::types::{AgentState, MessageEnvelope, QueueEntryRecord, TimerRecord};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerWakeStatus {
    Pending,
    Incorporated,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerWakeRecord {
    pub timer_id: String,
    pub message_id: String,
    pub fire_count: u64,
    pub status: TimerWakeStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub incorporated_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct TimerFire {
    pub expected: TimerRecord,
    pub record: TimerRecord,
    pub message: MessageEnvelope,
    pub queue_entry: QueueEntryRecord,
    pub agent_state: (AgentState, AgentState),
}

#[derive(Debug, Clone)]
pub struct TimerCancel {
    pub expected: TimerRecord,
    pub record: TimerRecord,
    pub agent_state: Option<(AgentState, AgentState)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimerFireResult {
    pub advanced: bool,
    pub wake_created: bool,
    pub message: Option<MessageEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerCancelResult {
    pub cancelled: bool,
    pub dropped_message_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimerWakeRecoveryResult {
    pub retained_wakes: usize,
    pub created_wakes: usize,
    pub invalidated_wakes: usize,
    pub dropped_message_ids: Vec<String>,
}

pub struct WorkItemRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct TaskRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct ExternalTriggerRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct WaitConditionRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct QueueEntryRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct TimerRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct TurnRecordRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct MessageRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct TranscriptRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct EvidenceRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct AuditEventSink<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct AgentStateRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct WorkspaceEntryRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct WorkspaceOccupancyRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct ExecutionRootEntryRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct AgentIdentityRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct AgentCanonicalRelationRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct AgentBootstrapRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct AgentDeletionRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct AgentMessageDeliveryRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct WorkItemDelegationRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct WorkItemContinuationRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct ContextEpisodeRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct OperatorNotificationRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct OperatorTransportBindingRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

pub struct OperatorDeliveryRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}
