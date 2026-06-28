//! Public types for runtime_db repositories.

use crate::runtime_db::RuntimeDb;

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

pub struct AgentIdentityRepository<'a> {
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
