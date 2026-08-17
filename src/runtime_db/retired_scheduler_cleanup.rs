use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde::Serialize;

use crate::domain::execution_protocol::{
    self, CommandResult, ConversationOutcome, ExecutionAttemptState, ExecutionBinding,
    ExecutionOutcome, ExecutionOutcomeRecord, WorkItemExecutionState, WorkItemOutcome,
};
use crate::ids;
use crate::runtime_db::evidence::{append_audit_event_tx, append_message_tx};
use crate::runtime_db::repositories::upsert_queue_entry_tx;
use crate::runtime_db::transitions::execution_protocol_repository::{
    load_state_unchecked_tx, persist_state_tx,
};
use crate::types::{
    AuditEvent, AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
    QueueEntryRecord, QueueEntryStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetiredSchedulerCleanupBlockerKind {
    OpenExecutionAttempt,
    InFlightExecutionWorkItem,
    DequeuedQueueEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetiredSchedulerCleanupBlocker {
    pub kind: RetiredSchedulerCleanupBlockerKind,
    pub agent_id: String,
    pub record_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RetiredSchedulerCleanupInventory {
    pub blockers: Vec<RetiredSchedulerCleanupBlocker>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetiredSchedulerFallbackActionKind {
    InterruptOpenExecutionAttempt,
    ResumeInFlightExecutionWorkItem,
    QuarantineDequeuedQueueEntry,
    EnqueueAgentRecoveryEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetiredSchedulerFallbackAction {
    pub kind: RetiredSchedulerFallbackActionKind,
    pub record_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetiredSchedulerFallbackResult {
    pub agent_id: String,
    pub recovery_message_id: Option<String>,
    pub protected_message_ids: Vec<String>,
    pub actions: Vec<RetiredSchedulerFallbackAction>,
}

impl RetiredSchedulerCleanupInventory {
    pub fn is_fixed_point(&self) -> bool {
        self.blockers.is_empty()
    }

    pub fn open_execution_attempts(&self) -> usize {
        self.count(RetiredSchedulerCleanupBlockerKind::OpenExecutionAttempt)
    }

    pub fn in_flight_execution_work_items(&self) -> usize {
        self.count(RetiredSchedulerCleanupBlockerKind::InFlightExecutionWorkItem)
    }

    pub fn dequeued_queue_entries(&self) -> usize {
        self.count(RetiredSchedulerCleanupBlockerKind::DequeuedQueueEntry)
    }

    pub fn affected_agents(&self) -> Vec<String> {
        self.blockers
            .iter()
            .map(|blocker| blocker.agent_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn count(&self, kind: RetiredSchedulerCleanupBlockerKind) -> usize {
        self.blockers
            .iter()
            .filter(|blocker| blocker.kind == kind)
            .count()
    }
}

pub(crate) fn retired_scheduler_cleanup_inventory(
    connection: &Connection,
) -> Result<RetiredSchedulerCleanupInventory> {
    let mut blockers = Vec::new();
    collect_blockers(
        connection,
        "SELECT agent_id, attempt_id
         FROM execution_protocol_attempts
         WHERE lifecycle_state = 'open'
         ORDER BY agent_id, attempt_id",
        RetiredSchedulerCleanupBlockerKind::OpenExecutionAttempt,
        &mut blockers,
    )?;
    collect_blockers(
        connection,
        "SELECT agent_id, work_item_id
         FROM execution_protocol_work_items
         WHERE lifecycle_state = 'in_flight'
         ORDER BY agent_id, work_item_id",
        RetiredSchedulerCleanupBlockerKind::InFlightExecutionWorkItem,
        &mut blockers,
    )?;
    collect_blockers(
        connection,
        "SELECT agent_id, message_id
         FROM queue_entries
         WHERE status = 'dequeued'
         ORDER BY agent_id, message_id",
        RetiredSchedulerCleanupBlockerKind::DequeuedQueueEntry,
        &mut blockers,
    )?;
    Ok(RetiredSchedulerCleanupInventory { blockers })
}

fn collect_blockers(
    connection: &Connection,
    sql: &str,
    kind: RetiredSchedulerCleanupBlockerKind,
    blockers: &mut Vec<RetiredSchedulerCleanupBlocker>,
) -> Result<()> {
    let mut statement = connection.prepare(sql)?;
    blockers.extend(
        statement
            .query_map([], |row| {
                Ok(RetiredSchedulerCleanupBlocker {
                    kind,
                    agent_id: row.get(0)?,
                    record_id: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    Ok(())
}

pub(crate) fn apply_retired_scheduler_cleanup_fallback(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<RetiredSchedulerFallbackResult> {
    let inventory = retired_scheduler_cleanup_inventory(tx)?;
    let blockers = inventory
        .blockers
        .into_iter()
        .filter(|blocker| blocker.agent_id == agent_id)
        .collect::<Vec<_>>();
    if blockers.is_empty() {
        return Ok(RetiredSchedulerFallbackResult {
            agent_id: agent_id.to_string(),
            recovery_message_id: None,
            protected_message_ids: Vec::new(),
            actions: Vec::new(),
        });
    }

    let mut state = load_state_unchecked_tx(tx, agent_id)
        .with_context(|| format!("loading damaged execution partition for agent {agent_id}"))?;
    let now = Utc::now();
    let now_text = now.to_rfc3339();
    let recovery_message_id = ids::message_id();
    let mut protected_message_ids = BTreeSet::new();
    let mut actions = Vec::new();

    for attempt in state
        .attempts
        .values()
        .filter(|attempt| attempt.state == ExecutionAttemptState::Open)
    {
        if let Some(message_id) = attempt.source_message_id.as_deref() {
            require_message_evidence(tx, agent_id, message_id)?;
            protected_message_ids.insert(message_id.to_string());
        }
    }
    for blocker in blockers
        .iter()
        .filter(|blocker| blocker.kind == RetiredSchedulerCleanupBlockerKind::DequeuedQueueEntry)
    {
        require_message_evidence(tx, agent_id, &blocker.record_id)?;
        protected_message_ids.insert(blocker.record_id.clone());
    }

    for work_item in state
        .work_items
        .values_mut()
        .filter(|record| matches!(record.state, WorkItemExecutionState::InFlight { .. }))
    {
        let generation = work_item
            .generation()
            .checked_add(1)
            .ok_or_else(|| anyhow!("WorkItem recovery generation overflow"))?;
        work_item.state = WorkItemExecutionState::Runnable {
            generation,
            recovery_ref: Some(recovery_message_id.clone()),
        };
    }
    for blocker in blockers.iter().filter(|blocker| {
        blocker.kind == RetiredSchedulerCleanupBlockerKind::InFlightExecutionWorkItem
    }) {
        actions.push(RetiredSchedulerFallbackAction {
            kind: RetiredSchedulerFallbackActionKind::ResumeInFlightExecutionWorkItem,
            record_id: blocker.record_id.clone(),
            reason: "retired_scheduler_cleanup_released_in_flight_work_item".into(),
        });
    }

    let open_attempt_ids = state
        .attempts
        .values()
        .filter(|attempt| attempt.state == ExecutionAttemptState::Open)
        .map(|attempt| attempt.attempt_id.clone())
        .collect::<Vec<_>>();
    for attempt_id in open_attempt_ids {
        let attempt = state
            .attempts
            .get(&attempt_id)
            .cloned()
            .ok_or_else(|| anyhow!("open execution attempt disappeared from recovery state"))?;
        let outcome_id = format!("retired-scheduler-cleanup:{attempt_id}");
        let outcome = ExecutionOutcomeRecord {
            outcome_id: outcome_id.clone(),
            attempt_id: attempt_id.clone(),
            outcome: match attempt.binding {
                ExecutionBinding::WorkItem { .. } => {
                    ExecutionOutcome::WorkItem(WorkItemOutcome::Interrupted {
                        reason: "retired_scheduler_cleanup".into(),
                    })
                }
                ExecutionBinding::Conversation { .. } | ExecutionBinding::AgentLifecycle { .. } => {
                    ExecutionOutcome::Conversation(ConversationOutcome::Interrupted {
                        reason: "retired_scheduler_cleanup".into(),
                    })
                }
                ExecutionBinding::Command => {
                    ExecutionOutcome::Command(CommandResult::Quarantined {
                        reason: "retired_scheduler_cleanup".into(),
                    })
                }
            },
            created_at: now_text.clone(),
        };
        let attempt = state
            .attempts
            .get_mut(&attempt_id)
            .expect("attempt was read from the same state");
        attempt.state = ExecutionAttemptState::Interrupted;
        attempt.terminal_outcome_id = Some(outcome_id.clone());
        attempt.terminal_at = Some(now_text.clone());
        state.outcomes.insert(outcome_id, outcome);
        actions.push(RetiredSchedulerFallbackAction {
            kind: RetiredSchedulerFallbackActionKind::InterruptOpenExecutionAttempt,
            record_id: attempt_id,
            reason: "retired_scheduler_cleanup_interrupted_unrecoverable_attempt".into(),
        });
    }
    execution_protocol::assert_invariants(&state).map_err(|error| {
        anyhow!("retired scheduler fallback could not repair execution partition: {error}")
    })?;
    persist_state_tx(tx, &state)?;

    let mut dequeued = tx.prepare(
        "SELECT payload_json
         FROM queue_entries
         WHERE agent_id = ?1 AND status = 'dequeued'
         ORDER BY message_id",
    )?;
    let dequeued = dequeued
        .query_map([agent_id], |row| row.get::<_, String>(0))?
        .map(|payload| {
            serde_json::from_str::<QueueEntryRecord>(&payload?)
                .context("decoding dequeued queue entry for retired scheduler fallback")
        })
        .collect::<Result<Vec<_>>>()?;
    for mut entry in dequeued {
        entry.status = QueueEntryStatus::Quarantined;
        entry.updated_at = now;
        upsert_queue_entry_tx(tx, &entry)?;
        actions.push(RetiredSchedulerFallbackAction {
            kind: RetiredSchedulerFallbackActionKind::QuarantineDequeuedQueueEntry,
            record_id: entry.message_id,
            reason: "retired_scheduler_cleanup_side_effect_unknown".into(),
        });
    }

    let protected_message_ids = protected_message_ids.into_iter().collect::<Vec<_>>();
    actions.push(RetiredSchedulerFallbackAction {
        kind: RetiredSchedulerFallbackActionKind::EnqueueAgentRecoveryEvent,
        record_id: recovery_message_id.clone(),
        reason: "retired_scheduler_cleanup_requires_agent_reconciliation".into(),
    });
    let mut recovery_message = MessageEnvelope::new(
        agent_id,
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "scheduler_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Json {
            value: serde_json::json!({
                "kind": "retired_scheduler_cleanup_recovery",
                "recovery_message_id": recovery_message_id,
                "protected_message_ids": protected_message_ids,
                "actions": actions,
                "instruction": "Inspect the protected original messages and resume or reconcile interrupted work.",
            }),
        },
    );
    recovery_message.id = recovery_message_id.clone();
    for message_id in &protected_message_ids {
        recovery_message
            .source_refs
            .insert(message_id.clone(), format!("message:{message_id}"));
    }
    let (recovery_message, _) = append_message_tx(tx, &recovery_message)?;
    upsert_queue_entry_tx(
        tx,
        &QueueEntryRecord {
            message_id: recovery_message.id.clone(),
            agent_id: agent_id.to_string(),
            priority: Priority::Next,
            status: QueueEntryStatus::Queued,
            created_at: recovery_message.created_at,
            updated_at: recovery_message.created_at,
        },
    )?;
    append_audit_event_tx(
        tx,
        Some(agent_id),
        &AuditEvent::legacy(
            "retired_scheduler_cleanup_fallback_applied",
            serde_json::json!({
                "agent_id": agent_id,
                "recovery_message_id": recovery_message.id,
                "protected_message_ids": protected_message_ids,
                "actions": actions,
            }),
        ),
    )?;

    Ok(RetiredSchedulerFallbackResult {
        agent_id: agent_id.to_string(),
        recovery_message_id: Some(recovery_message.id),
        protected_message_ids,
        actions,
    })
}

fn require_message_evidence(tx: &Transaction<'_>, agent_id: &str, message_id: &str) -> Result<()> {
    let payload = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE evidence_id = ?1",
            [message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| {
            anyhow!(
                "retired scheduler fallback cannot preserve source message {message_id}; \
                 refusing automatic recovery"
            )
        })?;
    let message: MessageEnvelope = serde_json::from_str(&payload)
        .with_context(|| format!("decoding protected source message {message_id}"))?;
    if message.id != message_id || message.agent_id != agent_id {
        bail!("retired scheduler fallback source message identity mismatch for {message_id}");
    }
    Ok(())
}
