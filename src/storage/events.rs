//! Event bus, event log page/query structs, and audit event publishing.

use tokio::sync::broadcast;

use crate::types::AuditEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventLogPageOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone)]
pub(crate) struct EventLogPage {
    pub(crate) events: Vec<AuditEvent>,
    pub(crate) has_older: bool,
    pub(crate) has_newer: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PublishedAuditEvent {
    pub(crate) agent_id: Option<String>,
    pub(crate) event: AuditEvent,
}

#[derive(Debug, Clone)]
pub(crate) struct EventBus {
    tx: broadcast::Sender<PublishedAuditEvent>,
}

impl EventBus {
    pub(crate) fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<PublishedAuditEvent> {
        self.tx.subscribe()
    }

    pub(super) fn publish(&self, event: PublishedAuditEvent) {
        let _ = self.tx.send(event);
    }
}
