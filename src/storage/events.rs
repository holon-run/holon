//! Runtime audit event persistence, queries, and publication.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::sync::broadcast;

use crate::{
    runtime_db::{
        transitions::{PostCommitEffects, PostCommitWarning},
        RuntimeDb,
    },
    types::AuditEvent,
};

use super::activity::FileActivityMarker;

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

#[derive(Debug, Clone)]
struct AuditEventIndexSink {
    runtime_db: RuntimeDb,
    agent_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeEventLog {
    runtime_db: RuntimeDb,
    agent_id: Option<String>,
    read_only: bool,
    append_mutex: Arc<Mutex<()>>,
    audit_event_index: Arc<Mutex<Option<AuditEventIndexSink>>>,
    event_bus: Arc<Mutex<Option<EventBus>>>,
}

impl RuntimeEventLog {
    pub(crate) fn new(
        runtime_db: RuntimeDb,
        agent_id: Option<String>,
        read_only: bool,
        append_mutex: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            runtime_db,
            agent_id,
            read_only,
            append_mutex,
            audit_event_index: Arc::new(Mutex::new(None)),
            event_bus: Arc::new(Mutex::new(None)),
        }
    }

    fn ensure_writable(&self) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "cannot write through read-only runtime storage"
        );
        Ok(())
    }

    pub(crate) fn enable_audit_event_index(
        &self,
        runtime_db: RuntimeDb,
        agent_id: Option<String>,
    ) -> Result<()> {
        self.ensure_writable()?;
        let mut guard = self
            .audit_event_index
            .lock()
            .map_err(|_| anyhow::anyhow!("audit event index mutex poisoned"))?;
        *guard = Some(AuditEventIndexSink {
            runtime_db,
            agent_id,
        });
        Ok(())
    }

    pub(crate) fn enable_event_bus(&self, event_bus: EventBus) -> Result<()> {
        let mut guard = self
            .event_bus
            .lock()
            .map_err(|_| anyhow::anyhow!("event bus mutex poisoned"))?;
        *guard = Some(event_bus);
        Ok(())
    }

    pub(crate) fn subscribe(&self) -> Result<Option<broadcast::Receiver<PublishedAuditEvent>>> {
        Ok(self
            .event_bus
            .lock()
            .map_err(|_| anyhow::anyhow!("event bus mutex poisoned"))?
            .as_ref()
            .map(EventBus::subscribe))
    }

    fn publish(&self, agent_id: Option<String>, event: &AuditEvent) -> Result<()> {
        if let Some(event_bus) = self
            .event_bus
            .lock()
            .map_err(|_| anyhow::anyhow!("event bus mutex poisoned"))?
            .clone()
        {
            event_bus.publish(PublishedAuditEvent {
                agent_id,
                event: event.clone(),
            });
        }
        Ok(())
    }

    pub(crate) fn publish_transition_events(
        &self,
        effects: &PostCommitEffects,
    ) -> Vec<PostCommitWarning> {
        let mut warnings = Vec::new();
        for event in &effects.audit_events {
            if let Err(error) = self.publish(self.agent_id.clone(), event) {
                tracing::warn!(
                    error = %error,
                    event_id = %event.id,
                    event_kind = %event.kind,
                    event_seq = event.event_seq,
                    "failed to publish committed transition audit event"
                );
                warnings.push(PostCommitWarning {
                    effect: "event_publication",
                    message: error.to_string(),
                });
            }
        }
        warnings
    }

    #[cfg(test)]
    fn flush_writes_for_tests(&self) -> Result<()> {
        if let Some(sink) = self
            .audit_event_index
            .lock()
            .map_err(|_| anyhow::anyhow!("audit event index mutex poisoned"))?
            .clone()
        {
            sink.runtime_db.flush_background_writes_for_tests()?;
        }
        self.runtime_db.flush_background_writes_for_tests()?;
        Ok(())
    }

    pub(crate) fn activity_marker(&self) -> Result<FileActivityMarker> {
        let latest_seq = self
            .runtime_db
            .audit_events()
            .latest_event_seq(self.agent_id.as_deref())?
            .unwrap_or(0);
        Ok(FileActivityMarker {
            exists: latest_seq > 0,
            len: latest_seq,
            modified_unix_ms: u128::from(latest_seq),
        })
    }

    pub fn append(&self, event: &AuditEvent) -> Result<()> {
        let started = std::time::Instant::now();
        self.ensure_writable()?;
        let _guard = self
            .append_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("storage append mutex poisoned"))?;
        let result = self.append_with_append_mutex_held(event);
        crate::diagnostics::record_storage_append_event(started.elapsed());
        result
    }

    pub(super) fn append_with_append_mutex_held(&self, event: &AuditEvent) -> Result<()> {
        self.ensure_writable()?;
        let sink = self
            .audit_event_index
            .lock()
            .map_err(|_| anyhow::anyhow!("audit event index mutex poisoned"))?
            .clone();
        let (agent_id, event) = if let Some(sink) = sink {
            let agent_id = sink.agent_id.clone();
            let event = sink
                .runtime_db
                .audit_events()
                .append(agent_id.as_deref(), event)?;
            (agent_id, event)
        } else {
            let event = self
                .runtime_db
                .audit_events()
                .append(self.agent_id.as_deref(), event)?;
            (self.agent_id.clone(), event)
        };
        if let Err(error) = self.publish(agent_id.clone(), &event) {
            tracing::warn!(
                error = %error,
                event_id = %event.id,
                event_kind = %event.kind,
                event_seq = event.event_seq,
                agent_id = agent_id.as_deref().unwrap_or("<global>"),
                "failed to publish committed audit event"
            );
        }
        Ok(())
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<AuditEvent>> {
        #[cfg(test)]
        self.flush_writes_for_tests()?;
        self.runtime_db
            .audit_events()
            .recent(self.agent_id.as_deref(), limit)
    }

    pub fn latest_seq(&self) -> Result<Option<u64>> {
        #[cfg(test)]
        self.flush_writes_for_tests()?;
        self.runtime_db
            .audit_events()
            .latest_event_seq(self.agent_id.as_deref())
    }

    pub fn epoch(&self) -> Result<String> {
        self.runtime_db.event_log_epoch()
    }

    pub(crate) fn page_matching<F>(
        &self,
        before_seq: Option<u64>,
        after_seq: Option<u64>,
        limit: usize,
        order: EventLogPageOrder,
        mut matches: F,
    ) -> Result<EventLogPage>
    where
        F: FnMut(&AuditEvent) -> bool,
    {
        #[cfg(test)]
        self.flush_writes_for_tests()?;
        if limit == 0 {
            return Ok(EventLogPage {
                events: Vec::new(),
                has_older: false,
                has_newer: false,
            });
        }
        let descending = matches!(order, EventLogPageOrder::Desc);
        let mut page = Vec::with_capacity(limit.saturating_add(1).min(1024));
        let chunk_limit = limit.saturating_add(1).clamp(64, 1024);
        let mut next_before_seq = before_seq;
        let mut next_after_seq = after_seq;
        loop {
            let chunk = self.runtime_db.audit_events().range(
                self.agent_id.as_deref(),
                next_before_seq,
                next_after_seq,
                descending,
                chunk_limit,
            )?;
            let Some(last_seq) = chunk.last().map(|event| event.event_seq) else {
                break;
            };
            for event in chunk {
                if matches(&event) {
                    page.push(event);
                }
                if page.len() > limit {
                    break;
                }
            }
            if page.len() > limit {
                break;
            }
            if descending {
                next_before_seq = Some(last_seq);
            } else {
                next_after_seq = Some(last_seq);
            }
        }
        let has_more = page.len() > limit;
        if has_more {
            page.truncate(limit);
        }
        Ok(match order {
            EventLogPageOrder::Desc => EventLogPage {
                events: page,
                has_older: has_more,
                has_newer: false,
            },
            EventLogPageOrder::Asc => EventLogPage {
                events: page,
                has_older: false,
                has_newer: has_more,
            },
        })
    }
}
