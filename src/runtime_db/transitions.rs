//! Restricted runtime business-transition unit of work.

// Phase 2 keeps this additive persistence seam dormant until production shadow
// wiring begins; repository tests exercise it without granting scheduler authority.
#[cfg(test)]
mod execution_protocol_fixture_repository;
mod execution_protocol_repository;
pub(crate) use execution_protocol_repository::{authority_fences_tx, persist_state_tx};
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod scheduler_protocol_repository;

use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction};
use std::collections::BTreeMap;

use crate::{
    runtime_db::{
        evidence::{
            append_audit_event_tx, append_message_tx, append_transcript_entry_tx,
            insert_brief_evidence_tx, insert_runtime_index_changes_tx, upsert_agent_state_tx,
        },
        repositories::{
            compare_and_set_queue_entry_tx, insert_new_work_item_tx, queue_entry_transition,
            task_transition, try_claim_queued_message_tx, try_interject_queued_message_tx,
            update_expected_work_item_tx, upsert_queue_entry_tx, upsert_task_tx,
            upsert_turn_record_tx, upsert_wait_condition_tx, upsert_work_item_continuation_tx,
            wait_condition_transition,
        },
        RuntimeDb, RuntimeIndexChange, RuntimeStateTransitionConflict,
    },
    runtime_error::RuntimeError,
    types::{
        AgentState, AuditEvent, BriefRecord, MessageEnvelope, QueueEntryRecord, QueueEntryStatus,
        TaskRecord, TranscriptEntry, TurnRecord, WaitConditionRecord, WorkItemContinuationFrame,
        WorkItemRecord, WorkItemSchedulingState, WorkItemState,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransitionFaultPoint {
    AfterValidation,
    AfterTerminalAgentStateWrite,
    AfterTerminalTurnRecordWrite,
    AfterCanonicalWrites,
    AfterAuditWrites,
    BeforeCommit,
    BeforeCacheUpdate,
    BeforeEventPublication,
    BeforeSchedulerNotification,
}

impl TransitionFaultPoint {
    fn is_post_commit(self) -> bool {
        matches!(
            self,
            Self::BeforeCacheUpdate
                | Self::BeforeEventPublication
                | Self::BeforeSchedulerNotification
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PostCommitWarning {
    pub effect: &'static str,
    pub message: String,
}

#[derive(Debug, Clone)]
pub(crate) enum WorkItemMutation {
    Insert {
        record: WorkItemRecord,
    },
    Update {
        record: WorkItemRecord,
        expected_revision: u64,
    },
}

impl WorkItemMutation {
    fn record(&self) -> &WorkItemRecord {
        match self {
            Self::Insert { record, .. } | Self::Update { record, .. } => record,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum QueueMutation {
    Consume(QueueEntryRecord),
    Upsert(QueueEntryRecord),
    CompareAndSet {
        expected: QueueEntryRecord,
        record: QueueEntryRecord,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueueOperation {
    Admit,
    Claim,
    Interject,
    Settle,
    RepairDrop,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PostCommitEffects {
    pub agent_state: Option<AgentStateMutation>,
    pub work_items: Vec<WorkItemRecord>,
    pub tasks: Vec<TaskRecord>,
    pub audit_events: Vec<AuditEvent>,
    pub notify_memory_index: bool,
    pub notify_scheduler: bool,
    pub fault: Option<TransitionFaultPoint>,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentStateMutation {
    pub expected: Option<Box<AgentState>>,
    pub record: Box<AgentState>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TransitionCommit {
    pub applied: bool,
    pub scheduler_authority_blocked: bool,
    pub effects: PostCommitEffects,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TransitionApplyResult {
    pub applied: bool,
    pub warnings: Vec<PostCommitWarning>,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkItemTransitionCommand {
    pub agent_id: String,
    pub mutation: WorkItemMutation,
    pub agent_state: Option<AgentStateMutation>,
    pub brief_evidence: Vec<BriefRecord>,
    pub audit_events: Vec<AuditEvent>,
    pub index_changes: Vec<RuntimeIndexChange>,
    pub notify_scheduler: bool,
    pub fault: Option<TransitionFaultPoint>,
}

#[derive(Debug, Clone)]
pub(crate) struct WaitTransitionCommand {
    pub agent_id: String,
    pub work_items: Vec<WorkItemMutation>,
    pub expected_wait_conditions: Vec<WaitConditionExpectation>,
    pub wait_conditions: Vec<WaitConditionRecord>,
    pub agent_state: Option<AgentStateMutation>,
    pub audit_events: Vec<AuditEvent>,
    pub index_changes: Vec<RuntimeIndexChange>,
    pub notify_scheduler: bool,
    pub fault: Option<TransitionFaultPoint>,
}

#[derive(Debug, Clone)]
pub(crate) struct WaitConditionExpectation {
    pub id: String,
    pub agent_id: String,
    pub status: crate::types::WaitConditionStatus,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskExpectation {
    pub id: String,
    pub agent_id: String,
    pub work_item_id: Option<String>,
    pub status: crate::types::TaskStatus,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub result_message_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct QueueWaitTransition {
    pub expected: WaitConditionExpectation,
    pub record: WaitConditionRecord,
    pub work_item: Option<WorkItemMutation>,
    pub index_changes: Vec<RuntimeIndexChange>,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkItemFocusTransitionCommand {
    pub agent_id: String,
    pub work_items: Vec<WorkItemMutation>,
    pub wait_conditions: Vec<WaitConditionRecord>,
    pub continuations: Vec<WorkItemContinuationFrame>,
    pub agent_state: AgentStateMutation,
    pub brief_evidence: Vec<BriefRecord>,
    pub audit_events: Vec<AuditEvent>,
    pub index_changes: Vec<RuntimeIndexChange>,
    pub notify_scheduler: bool,
    pub fault: Option<TransitionFaultPoint>,
}

#[derive(Debug, Clone)]
pub(crate) struct QueueTransitionCommand {
    pub agent_id: String,
    pub operation: QueueOperation,
    pub mutation: QueueMutation,
    pub scheduler_claim_work_item: Option<WorkItemRecord>,
    pub scheduler_protocol_bootstrap: Option<crate::domain::scheduler_protocol::Snapshot>,
    pub scheduler_protocol_commands: Vec<crate::domain::scheduler_protocol::ProtocolCommand>,
    pub agent_state: Option<AgentStateMutation>,
    pub message_evidence: Vec<MessageEnvelope>,
    pub transcript_entries: Vec<TranscriptEntry>,
    pub turn_record: Option<TurnRecord>,
    pub audit_events: Vec<AuditEvent>,
    pub notify_scheduler: bool,
    pub fault: Option<TransitionFaultPoint>,
    pub brief_evidence: Vec<BriefRecord>,
}

#[derive(Debug, Clone)]
pub(crate) struct QueueHeadNoProgressCommand {
    pub agent_id: String,
    pub expected: QueueEntryRecord,
    pub quarantined: QueueEntryRecord,
    pub agent_state: AgentStateMutation,
    pub reason: String,
    pub scenario_class: Option<String>,
    pub max_attempts: u32,
    pub fault: Option<TransitionFaultPoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueueHeadNoProgressOutcome {
    BoundedDefer { attempt: u32, max_attempts: u32 },
    Quarantined { attempt: u32, max_attempts: u32 },
}

#[derive(Debug, Clone)]
pub(crate) struct QueueHeadNoProgressCommit {
    pub outcome: QueueHeadNoProgressOutcome,
    pub commit: TransitionCommit,
}

#[derive(Debug, Clone)]
pub(crate) struct TurnTerminalTransitionCommand {
    pub agent_id: String,
    pub agent_state: AgentStateMutation,
    pub turn_record: TurnRecord,
    pub audit_events: Vec<AuditEvent>,
    pub fault: Option<TransitionFaultPoint>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ExecutionProtocolTransition {
    pub bootstrap: Option<crate::domain::execution_protocol::ExecutionProtocolState>,
    pub commands: Vec<crate::domain::execution_protocol::ExecutionProtocolCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExecutionAuthorityFences {
    pub agent_control_revision: u64,
    pub host_registry_revision: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskTransitionCommand {
    pub agent_id: String,
    pub task: TaskRecord,
    pub work_items: Vec<WorkItemMutation>,
    pub wait_conditions: Vec<WaitConditionRecord>,
    pub agent_state: Option<AgentStateMutation>,
    pub message_evidence: Vec<MessageEnvelope>,
    pub audit_events: Vec<AuditEvent>,
    pub index_changes: Vec<RuntimeIndexChange>,
    pub notify_scheduler: bool,
    pub commit_on_idempotent: bool,
    pub fault: Option<TransitionFaultPoint>,
}

pub(crate) struct RuntimeTransitionRepository<'a> {
    db: &'a RuntimeDb,
}

impl RuntimeDb {
    pub(crate) fn transitions(&self) -> RuntimeTransitionRepository<'_> {
        RuntimeTransitionRepository { db: self }
    }

    pub(crate) fn recover_orphaned_dequeued_claims_at_startup(&self) -> Result<Vec<String>> {
        self.reconcile_orphaned_dequeued_claims(None)
            .map(|(agents, _)| agents)
    }

    pub(crate) fn reconcile_orphaned_dequeued_claims(
        &self,
        agent_id: Option<&str>,
    ) -> Result<(Vec<String>, usize)> {
        self.transaction(|tx| {
            let sql = if agent_id.is_some() {
                "SELECT q.payload_json
                 FROM queue_entries q
                 JOIN agent_identities i
                   ON i.agent_id = q.agent_id
                  AND i.status = 'active'
                 WHERE q.status = 'dequeued'
                   AND q.agent_id = ?1
                   AND NOT EXISTS (
                     SELECT 1
                     FROM execution_protocol_attempts a
                     WHERE a.agent_id = q.agent_id
                       AND a.attempt_id = 'activation:message:' || q.message_id
                   )
                 ORDER BY q.agent_id, q.updated_at, q.message_id"
            } else {
                "SELECT q.payload_json
                 FROM queue_entries q
                 JOIN agent_identities i
                   ON i.agent_id = q.agent_id
                  AND i.status = 'active'
                 WHERE q.status = 'dequeued'
                   AND NOT EXISTS (
                     SELECT 1
                     FROM execution_protocol_attempts a
                     WHERE a.agent_id = q.agent_id
                       AND a.attempt_id = 'activation:message:' || q.message_id
                   )
                 ORDER BY q.agent_id, q.updated_at, q.message_id"
            };
            let mut statement = tx.prepare(sql)?;
            let candidates = if let Some(agent_id) = agent_id {
                statement
                    .query_map([agent_id], |row| row.get::<_, String>(0))?
                    .map(|row| Ok(serde_json::from_str::<QueueEntryRecord>(&row?)?))
                    .collect::<Result<Vec<_>>>()?
            } else {
                statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .map(|row| Ok(serde_json::from_str::<QueueEntryRecord>(&row?)?))
                    .collect::<Result<Vec<_>>>()?
            };
            drop(statement);

            let recovered_at = Utc::now();
            let mut recovered_agents = Vec::new();
            let mut changed = 0;
            for expected in candidates {
                let mut recovered = expected.clone();
                let terminal_kind = tx
                    .query_row(
                        "SELECT terminal_kind
                         FROM turn_records
                         WHERE agent_id = ?1
                           AND trigger_message_id = ?2
                           AND terminal_kind IS NOT NULL
                         ORDER BY completed_at DESC
                         LIMIT 1",
                        rusqlite::params![expected.agent_id, expected.message_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                let terminal_brief_kind = tx
                    .query_row(
                        "SELECT kind
                         FROM briefs
                         WHERE agent_id = ?1
                           AND message_id = ?2
                           AND kind IN ('result', 'failure')
                         ORDER BY created_at DESC
                         LIMIT 1",
                        rusqlite::params![expected.agent_id, expected.message_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                let delivered = tx.query_row(
                    "SELECT EXISTS(
                         SELECT 1
                         FROM delivery_summaries d
                         JOIN turn_records t
                           ON t.agent_id = d.agent_id
                          AND t.turn_id = d.turn_id
                         WHERE d.agent_id = ?1
                           AND t.trigger_message_id = ?2
                     )",
                    rusqlite::params![expected.agent_id, expected.message_id],
                    |row| row.get::<_, bool>(0),
                )?;
                let (next_status, reason) = if terminal_kind.as_deref() == Some("completed")
                    || terminal_brief_kind.as_deref() == Some("result")
                    || delivered
                {
                    (
                        QueueEntryStatus::Processed,
                        "terminal_or_result_completion_evidence",
                    )
                } else if terminal_kind.is_some()
                    || terminal_brief_kind.as_deref() == Some("failure")
                {
                    (
                        QueueEntryStatus::Aborted,
                        "terminal_failure_completion_evidence",
                    )
                } else {
                    (
                        QueueEntryStatus::Interrupted,
                        "no_execution_attempt_or_completion_evidence",
                    )
                };
                recovered.status = next_status.clone();
                recovered.updated_at = recovered_at;
                if !compare_and_set_queue_entry_tx(tx, &expected, &recovered)? {
                    continue;
                }
                changed += 1;
                let event = AuditEvent {
                    id: format!("audit:orphaned-queue-claim:{}", expected.message_id),
                    event_seq: 0,
                    event_log_epoch: String::new(),
                    created_at: recovered_at,
                    kind: "orphaned_queue_claim_recovered".into(),
                    contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
                    payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.to_string(),
                    payload_schema_version: 1,
                    data: serde_json::json!({
                        "message_id": expected.message_id,
                        "agent_id": expected.agent_id,
                        "reason": reason,
                        "previous_status": "dequeued",
                        "next_status": next_status,
                        "terminal_kind": terminal_kind,
                        "terminal_brief_kind": terminal_brief_kind,
                        "delivery_evidence": delivered,
                    }),
                };
                append_audit_event_tx(tx, Some(&recovered.agent_id), &event)?;
                if recovered.status == QueueEntryStatus::Interrupted {
                    recovered_agents.push(recovered.agent_id);
                }
            }
            recovered_agents.sort();
            recovered_agents.dedup();
            Ok((recovered_agents, changed))
        })
    }
}

impl RuntimeTransitionRepository<'_> {
    pub fn commit_queue_head_no_progress(
        &self,
        command: &QueueHeadNoProgressCommand,
    ) -> Result<Option<QueueHeadNoProgressCommit>> {
        if command.max_attempts == 0 {
            bail!("queue-head no-progress budget must be non-zero");
        }
        if command.expected.message_id != command.quarantined.message_id
            || command.expected.agent_id != command.quarantined.agent_id
            || command.expected.agent_id != command.agent_id
        {
            bail!("queue-head no-progress identity must remain unchanged");
        }
        if !matches!(
            command.expected.status,
            QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
        ) || command.quarantined.status != QueueEntryStatus::Quarantined
        {
            bail!("queue-head no-progress requires an active head and quarantined terminal record");
        }

        self.db.transaction(|tx| {
            let current = tx
                .query_row(
                    "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
                    [&command.expected.message_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|payload| serde_json::from_str::<QueueEntryRecord>(&payload))
                .transpose()?;
            if current.as_ref() != Some(&command.expected) {
                return Ok(None);
            }

            let previous = tx
                .query_row(
                    "SELECT attempts, max_attempts, status, first_reason, first_deferred_at
                     FROM queue_head_no_progress
                     WHERE message_id = ?1",
                    [&command.expected.message_id],
                    |row| {
                        Ok((
                            row.get::<_, u32>(0)?,
                            row.get::<_, u32>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .optional()?;
            if previous
                .as_ref()
                .is_some_and(|(_, _, status, _, _)| status == "quarantined")
            {
                return Ok(None);
            }
            inject_fault(command.fault, TransitionFaultPoint::AfterValidation)?;
            let attempt = previous
                .as_ref()
                .map_or(1, |(attempts, _, _, _, _)| attempts.saturating_add(1));
            let max_attempts = previous
                .as_ref()
                .map_or(command.max_attempts, |(_, max, _, _, _)| *max);
            let now = Utc::now();
            let first_reason = previous
                .as_ref()
                .map_or(command.reason.as_str(), |(_, _, _, reason, _)| {
                    reason.as_str()
                });
            let first_deferred_at = previous
                .as_ref()
                .map_or_else(|| now.to_rfc3339(), |(_, _, _, _, value)| value.clone());
            let quarantined = attempt >= max_attempts;
            let status = if quarantined {
                "quarantined"
            } else {
                "bounded_defer"
            };

            validate_agent_state_mutation_tx(tx, quarantined.then_some(&command.agent_state))?;
            if quarantined
                && !compare_and_set_queue_entry_tx(tx, &command.expected, &command.quarantined)?
            {
                return Ok(None);
            }
            tx.execute(
                "INSERT INTO queue_head_no_progress (
                    message_id, agent_id, attempts, max_attempts, status,
                    first_reason, last_reason, first_deferred_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(message_id) DO UPDATE SET
                    attempts = excluded.attempts,
                    max_attempts = excluded.max_attempts,
                    status = excluded.status,
                    last_reason = excluded.last_reason,
                    updated_at = excluded.updated_at",
                rusqlite::params![
                    command.expected.message_id,
                    command.agent_id,
                    attempt,
                    max_attempts,
                    status,
                    first_reason,
                    command.reason,
                    first_deferred_at,
                    now.to_rfc3339(),
                ],
            )?;
            let agent_state_applied = if quarantined {
                apply_agent_state_mutation_tx(tx, Some(&command.agent_state))?
            } else {
                false
            };
            inject_fault(command.fault, TransitionFaultPoint::AfterCanonicalWrites)?;
            let event_kind = if quarantined {
                "scheduler_queue_head_quarantined"
            } else {
                "scheduler_queue_head_deferred"
            };
            let event = AuditEvent::legacy(
                event_kind,
                serde_json::json!({
                    "message_id": command.expected.message_id,
                    "agent_id": command.agent_id,
                    "reason": command.reason,
                    "scenario_class": command.scenario_class,
                    "attempt": attempt,
                    "max_attempts": max_attempts,
                    "queue_disposition": status,
                }),
            );
            let commit = finish_transition_tx(
                tx,
                true,
                &command.agent_id,
                &[event],
                &[],
                command.fault,
                PostCommitEffects {
                    agent_state: agent_state_applied.then(|| command.agent_state.clone()),
                    notify_scheduler: quarantined,
                    ..PostCommitEffects::default()
                },
            )?;
            let outcome = if quarantined {
                QueueHeadNoProgressOutcome::Quarantined {
                    attempt,
                    max_attempts,
                }
            } else {
                QueueHeadNoProgressOutcome::BoundedDefer {
                    attempt,
                    max_attempts,
                }
            };
            Ok(Some(QueueHeadNoProgressCommit { outcome, commit }))
        })
    }

    pub fn commit_turn_terminal(
        &self,
        command: &TurnTerminalTransitionCommand,
    ) -> Result<TransitionCommit> {
        self.db.transaction(|tx| {
            validate_agent_state_mutation_tx(tx, Some(&command.agent_state))?;
            inject_fault(command.fault, TransitionFaultPoint::AfterValidation)?;

            let agent_state_applied =
                apply_agent_state_mutation_tx(tx, Some(&command.agent_state))?;
            inject_fault(
                command.fault,
                TransitionFaultPoint::AfterTerminalAgentStateWrite,
            )?;
            let turn_record_applied = tx
                .query_row(
                    "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
                    [&command.turn_record.turn_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|payload| serde_json::from_str::<TurnRecord>(&payload))
                .transpose()?
                .as_ref()
                != Some(&command.turn_record);
            upsert_turn_record_tx(tx, &command.turn_record)?;
            inject_fault(
                command.fault,
                TransitionFaultPoint::AfterTerminalTurnRecordWrite,
            )?;
            let applied = agent_state_applied || turn_record_applied;
            if !applied {
                return Ok(TransitionCommit::default());
            }
            inject_fault(command.fault, TransitionFaultPoint::AfterCanonicalWrites)?;

            finish_transition_tx(
                tx,
                applied,
                &command.agent_id,
                &command.audit_events,
                &[],
                command.fault,
                PostCommitEffects {
                    agent_state: agent_state_applied.then(|| command.agent_state.clone()),
                    ..PostCommitEffects::default()
                },
            )
        })
    }

    pub fn commit_work_item_focus(
        &self,
        command: &WorkItemFocusTransitionCommand,
    ) -> Result<TransitionCommit> {
        self.db.transaction(|tx| {
            for work_item in &command.work_items {
                validate_work_item_mutation_tx(tx, work_item)?;
            }
            for condition in &command.wait_conditions {
                validate_wait_condition_tx(tx, condition)?;
            }
            for continuation in &command.continuations {
                validate_work_item_continuation_tx(tx, continuation)?;
            }
            validate_agent_state_mutation_tx(tx, Some(&command.agent_state))?;
            validate_focus_target_tx(tx, &command.agent_state.record)?;
            inject_fault(command.fault, TransitionFaultPoint::AfterValidation)?;

            let agent_state_applied =
                apply_agent_state_mutation_tx(tx, Some(&command.agent_state))?;
            let mut applied = agent_state_applied;
            let mut work_items = Vec::new();
            for work_item in &command.work_items {
                let work_item_applied = apply_work_item_mutation_tx(tx, work_item)?;
                applied |= work_item_applied;
                if work_item_applied {
                    work_items.push(work_item.record().clone());
                }
            }
            for condition in &command.wait_conditions {
                applied |= upsert_wait_condition_tx(tx, condition)?;
            }
            for continuation in &command.continuations {
                applied |= upsert_work_item_continuation_tx(tx, continuation)?;
            }
            if !applied {
                return Ok(TransitionCommit::default());
            }
            for brief in &command.brief_evidence {
                insert_brief_evidence_tx(tx, brief)?;
            }
            inject_fault(command.fault, TransitionFaultPoint::AfterCanonicalWrites)?;

            finish_transition_tx(
                tx,
                applied,
                &command.agent_id,
                &command.audit_events,
                &command.index_changes,
                command.fault,
                PostCommitEffects {
                    agent_state: agent_state_applied.then(|| command.agent_state.clone()),
                    work_items,
                    notify_scheduler: command.notify_scheduler,
                    ..PostCommitEffects::default()
                },
            )
        })
    }

    pub fn commit_work_item(
        &self,
        command: &WorkItemTransitionCommand,
    ) -> Result<TransitionCommit> {
        let record = command.mutation.record().clone();
        self.db.transaction(|tx| {
            validate_work_item_mutation_tx(tx, &command.mutation)?;
            validate_agent_state_mutation_tx(tx, command.agent_state.as_ref())?;
            inject_fault(command.fault, TransitionFaultPoint::AfterValidation)?;
            let mut applied = apply_work_item_mutation_tx(tx, &command.mutation)?;
            let agent_state_applied =
                apply_agent_state_mutation_tx(tx, command.agent_state.as_ref())?;
            applied |= agent_state_applied;
            if !applied {
                return Ok(TransitionCommit::default());
            }
            for brief in &command.brief_evidence {
                insert_brief_evidence_tx(tx, brief)?;
            }
            inject_fault(command.fault, TransitionFaultPoint::AfterCanonicalWrites)?;
            finish_transition_tx(
                tx,
                applied,
                &command.agent_id,
                &command.audit_events,
                &command.index_changes,
                command.fault,
                PostCommitEffects {
                    agent_state: agent_state_applied
                        .then(|| command.agent_state.clone())
                        .flatten(),
                    work_items: applied.then_some(record.clone()).into_iter().collect(),
                    notify_scheduler: command.notify_scheduler,
                    ..PostCommitEffects::default()
                },
            )
        })
    }

    pub fn commit_wait(&self, command: &WaitTransitionCommand) -> Result<TransitionCommit> {
        self.commit_wait_with_execution_protocol(command, &ExecutionProtocolTransition::default())
    }

    pub fn commit_wait_with_execution_protocol(
        &self,
        command: &WaitTransitionCommand,
        execution_protocol: &ExecutionProtocolTransition,
    ) -> Result<TransitionCommit> {
        self.commit_wait_with_execution_protocol_and_task_expectation(
            command,
            execution_protocol,
            None,
        )
    }

    pub fn commit_wait_with_execution_protocol_and_task_expectation(
        &self,
        command: &WaitTransitionCommand,
        execution_protocol: &ExecutionProtocolTransition,
        task_expectation: Option<&TaskExpectation>,
    ) -> Result<TransitionCommit> {
        self.db.transaction(|tx| {
            for work_item in &command.work_items {
                validate_work_item_mutation_tx(tx, work_item)?;
            }
            for condition in &command.wait_conditions {
                validate_wait_condition_tx(tx, condition)?;
            }
            for expected in &command.expected_wait_conditions {
                validate_wait_condition_expectation_tx(tx, expected)?;
            }
            if let Some(task_expectation) = task_expectation {
                validate_task_expectation_tx(tx, task_expectation)?;
            }
            validate_agent_state_mutation_tx(tx, command.agent_state.as_ref())?;
            let execution_protocol = execution_protocol_repository::validate_execution_commands_tx(
                tx,
                &command.agent_id,
                execution_protocol.bootstrap.as_ref(),
                &execution_protocol.commands,
                &command.work_items,
                &command.wait_conditions,
            )?;
            inject_fault(command.fault, TransitionFaultPoint::AfterValidation)?;

            let mut applied = false;
            let mut work_items = Vec::new();
            for work_item in &command.work_items {
                let work_item_applied = apply_work_item_mutation_tx(tx, work_item)?;
                applied |= work_item_applied;
                if work_item_applied {
                    work_items.push(work_item.record().clone());
                }
            }
            for condition in &command.wait_conditions {
                applied |= upsert_wait_condition_tx(tx, condition)?;
            }
            let agent_state_applied =
                apply_agent_state_mutation_tx(tx, command.agent_state.as_ref())?;
            applied |= agent_state_applied;
            applied |= execution_protocol
                .as_ref()
                .is_some_and(|prepared| prepared.has_writes());
            if !applied {
                return Ok(TransitionCommit::default());
            }
            execution_protocol_repository::persist_execution_commands_tx(tx, execution_protocol)?;
            inject_fault(command.fault, TransitionFaultPoint::AfterCanonicalWrites)?;

            finish_transition_tx(
                tx,
                applied,
                &command.agent_id,
                &command.audit_events,
                &command.index_changes,
                command.fault,
                PostCommitEffects {
                    agent_state: agent_state_applied
                        .then(|| command.agent_state.clone())
                        .flatten(),
                    work_items,
                    notify_scheduler: command.notify_scheduler,
                    ..PostCommitEffects::default()
                },
            )
        })
    }

    pub fn commit_queue(&self, command: &QueueTransitionCommand) -> Result<TransitionCommit> {
        self.commit_queue_with_protocol_and_wait_trigger(
            command,
            &ExecutionProtocolTransition::default(),
            None,
            None,
        )
    }

    pub fn commit_queue_with_execution_protocol(
        &self,
        command: &QueueTransitionCommand,
        execution_protocol: &ExecutionProtocolTransition,
    ) -> Result<TransitionCommit> {
        self.commit_queue_with_protocol_and_wait_trigger(command, execution_protocol, None, None)
    }

    pub fn commit_queue_with_wait_trigger(
        &self,
        command: &QueueTransitionCommand,
        wait_transition: Option<&QueueWaitTransition>,
    ) -> Result<TransitionCommit> {
        self.commit_queue_with_protocol_and_wait_trigger(
            command,
            &ExecutionProtocolTransition::default(),
            wait_transition,
            None,
        )
    }

    pub fn commit_queue_with_execution_protocol_and_wait_transition(
        &self,
        command: &QueueTransitionCommand,
        execution_protocol: &ExecutionProtocolTransition,
        wait_transition: Option<&QueueWaitTransition>,
    ) -> Result<TransitionCommit> {
        self.commit_queue_with_protocol_and_wait_trigger(
            command,
            execution_protocol,
            wait_transition,
            None,
        )
    }

    pub fn commit_queue_with_execution_protocol_and_task_expectation(
        &self,
        command: &QueueTransitionCommand,
        execution_protocol: &ExecutionProtocolTransition,
        task_expectation: &TaskExpectation,
    ) -> Result<TransitionCommit> {
        self.commit_queue_with_protocol_and_wait_trigger(
            command,
            execution_protocol,
            None,
            Some(task_expectation),
        )
    }

    fn commit_queue_with_protocol_and_wait_trigger(
        &self,
        command: &QueueTransitionCommand,
        execution_protocol: &ExecutionProtocolTransition,
        wait_transition: Option<&QueueWaitTransition>,
        task_expectation: Option<&TaskExpectation>,
    ) -> Result<TransitionCommit> {
        self.db.transaction(|tx| {
            validate_queue_operation(command)?;
            validate_queue_mutation_tx(tx, &command.mutation)?;
            if let Some(wait_transition) = wait_transition {
                validate_wait_condition_expectation_tx(tx, &wait_transition.expected)?;
                validate_wait_condition_tx(tx, &wait_transition.record)?;
                if let Some(work_item) = wait_transition.work_item.as_ref() {
                    validate_work_item_mutation_tx(tx, work_item)?;
                }
            }
            if let Some(task_expectation) = task_expectation {
                validate_task_expectation_tx(tx, task_expectation)?;
            }
            if let QueueMutation::Consume(record) = &command.mutation {
                let include_interrupted = match command.operation {
                    QueueOperation::Claim => true,
                    QueueOperation::Interject => false,
                    QueueOperation::Admit | QueueOperation::Settle | QueueOperation::RepairDrop => {
                        unreachable!("queue operation validation rejects this combination")
                    }
                };
                if !crate::runtime_db::repositories::queue_entry_is_claimable_tx(
                    tx,
                    record,
                    include_interrupted,
                )? {
                    return Ok(TransitionCommit::default());
                }
            }
            validate_agent_state_mutation_tx(tx, command.agent_state.as_ref())?;
            validate_scheduler_claim_work_item_tx(
                tx,
                &command.agent_id,
                command.operation,
                command.scheduler_claim_work_item.as_ref(),
            )?;
            let scheduler_protocol = scheduler_protocol_repository::validate_protocol_commands_tx(
                tx,
                &command.agent_id,
                command.scheduler_protocol_bootstrap.as_ref(),
                &command.scheduler_protocol_commands,
                &execution_protocol.commands,
            );
            let scheduler_protocol = match scheduler_protocol {
                Ok(prepared) => prepared,
                Err(error)
                    if matches!(
                        command.operation,
                        QueueOperation::Claim | QueueOperation::Interject
                    ) =>
                {
                    let Some(conflict) =
                        scheduler_protocol_repository::scheduler_protocol_claim_contention(&error)
                    else {
                        return Err(error);
                    };
                    let event = AuditEvent::legacy(
                        "scheduler_claim_contended",
                        serde_json::json!({
                            "agent_id": command.agent_id,
                            "conflict_kind": conflict.kind,
                            "conflict_code": conflict.code,
                            "queue_operation": format!("{:?}", command.operation).to_lowercase(),
                            "queue_disposition": "retained_queued",
                        }),
                    );
                    let mut commit = finish_transition_tx(
                        tx,
                        true,
                        &command.agent_id,
                        &[event],
                        &[],
                        command.fault,
                        PostCommitEffects {
                            notify_scheduler: command.notify_scheduler,
                            ..PostCommitEffects::default()
                        },
                    )?;
                    commit.scheduler_authority_blocked = true;
                    return Ok(commit);
                }
                Err(error) => return Err(error),
            };
            let execution_protocol = execution_protocol_repository::validate_execution_commands_tx(
                tx,
                &command.agent_id,
                execution_protocol.bootstrap.as_ref(),
                &execution_protocol.commands,
                wait_transition
                    .and_then(|transition| transition.work_item.as_ref())
                    .map_or(&[], std::slice::from_ref),
                wait_transition
                    .map(|transition| std::slice::from_ref(&transition.record))
                    .unwrap_or_default(),
            )?;
            inject_fault(command.fault, TransitionFaultPoint::AfterValidation)?;
            let mutation_applied = match &command.mutation {
                QueueMutation::Consume(record) => match command.operation {
                    QueueOperation::Claim => try_claim_queued_message_tx(tx, record)?,
                    QueueOperation::Interject => try_interject_queued_message_tx(tx, record)?,
                    QueueOperation::Admit | QueueOperation::Settle | QueueOperation::RepairDrop => {
                        unreachable!("queue operation validation rejects this combination")
                    }
                },
                QueueMutation::Upsert(record) => upsert_queue_entry_tx(tx, record)?,
                QueueMutation::CompareAndSet { expected, record } => {
                    compare_and_set_queue_entry_tx(tx, expected, record)?
                }
            };
            if !matches!(&command.mutation, QueueMutation::Upsert(_)) && !mutation_applied {
                return Ok(TransitionCommit::default());
            }
            let agent_state_applied =
                apply_agent_state_mutation_tx(tx, command.agent_state.as_ref())?;
            let protocol_applied = scheduler_protocol
                .as_ref()
                .is_some_and(|prepared| prepared.has_writes());
            let execution_protocol_applied = execution_protocol
                .as_ref()
                .is_some_and(|prepared| prepared.has_writes());
            let wait_transition_applied = wait_transition
                .map(|wait_transition| upsert_wait_condition_tx(tx, &wait_transition.record))
                .transpose()?
                .unwrap_or(false);
            let mut wait_work_items = Vec::new();
            let wait_work_item_applied = if let Some(work_item) =
                wait_transition.and_then(|wait_transition| wait_transition.work_item.as_ref())
            {
                let applied = apply_work_item_mutation_tx(tx, work_item)?;
                if applied {
                    wait_work_items.push(work_item.record().clone());
                }
                applied
            } else {
                false
            };
            let applied = mutation_applied
                || agent_state_applied
                || protocol_applied
                || execution_protocol_applied
                || wait_transition_applied
                || wait_work_item_applied;
            if !applied {
                return Ok(TransitionCommit::default());
            }
            scheduler_protocol_repository::persist_protocol_commands_tx(
                tx,
                &command.agent_id,
                scheduler_protocol,
            )?;
            execution_protocol_repository::persist_execution_commands_tx(tx, execution_protocol)?;
            for message in &command.message_evidence {
                append_message_tx(tx, message)?;
            }
            for entry in &command.transcript_entries {
                append_transcript_entry_tx(tx, entry)?;
            }
            if let Some(turn_record) = command.turn_record.as_ref() {
                upsert_turn_record_tx(tx, turn_record)?;
            }
            for brief in &command.brief_evidence {
                insert_brief_evidence_tx(tx, brief)?;
            }
            inject_fault(command.fault, TransitionFaultPoint::AfterCanonicalWrites)?;
            finish_transition_tx(
                tx,
                applied,
                &command.agent_id,
                &command.audit_events,
                wait_transition.map_or(&[], |transition| transition.index_changes.as_slice()),
                command.fault,
                PostCommitEffects {
                    agent_state: agent_state_applied
                        .then(|| command.agent_state.clone())
                        .flatten(),
                    work_items: wait_work_items,
                    notify_scheduler: command.notify_scheduler,
                    ..PostCommitEffects::default()
                },
            )
        })
    }

    pub fn commit_task(&self, command: &TaskTransitionCommand) -> Result<TransitionCommit> {
        self.db.transaction(|tx| {
            validate_task_tx(tx, &command.task)?;
            for work_item in &command.work_items {
                validate_work_item_mutation_tx(tx, work_item)?;
            }
            for condition in &command.wait_conditions {
                validate_wait_condition_tx(tx, condition)?;
            }
            validate_agent_state_mutation_tx(tx, command.agent_state.as_ref())?;
            inject_fault(command.fault, TransitionFaultPoint::AfterValidation)?;

            let task_applied = upsert_task_tx(tx, &command.task)?;
            let mut applied = task_applied;
            let mut work_items = Vec::new();
            for work_item in &command.work_items {
                let work_item_applied = apply_work_item_mutation_tx(tx, work_item)?;
                applied |= work_item_applied;
                if work_item_applied {
                    work_items.push(work_item.record().clone());
                }
            }
            for condition in &command.wait_conditions {
                applied |= upsert_wait_condition_tx(tx, condition)?;
            }
            let agent_state_applied =
                apply_agent_state_mutation_tx(tx, command.agent_state.as_ref())?;
            applied |= agent_state_applied;
            for message in &command.message_evidence {
                append_message_tx(tx, message)?;
            }
            applied |= command.commit_on_idempotent;
            if !applied {
                return Ok(TransitionCommit::default());
            }
            inject_fault(command.fault, TransitionFaultPoint::AfterCanonicalWrites)?;

            finish_transition_tx(
                tx,
                applied,
                &command.agent_id,
                &command.audit_events,
                &command.index_changes,
                command.fault,
                PostCommitEffects {
                    agent_state: agent_state_applied
                        .then(|| command.agent_state.clone())
                        .flatten(),
                    work_items,
                    tasks: task_applied
                        .then_some(command.task.clone())
                        .into_iter()
                        .collect(),
                    notify_scheduler: command.notify_scheduler,
                    ..PostCommitEffects::default()
                },
            )
        })
    }
}

fn validate_wait_condition_expectation_tx(
    tx: &Transaction<'_>,
    expected: &WaitConditionExpectation,
) -> Result<()> {
    let actual = tx
        .query_row(
            "SELECT agent_id, status, updated_at
             FROM wait_conditions
             WHERE wait_condition_id = ?1",
            [&expected.id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let expected_status = crate::runtime_db::repositories::enum_string(&expected.status)?;
    let expected_updated_at = crate::runtime_db::repositories::timestamp(expected.updated_at);
    if actual
        .as_ref()
        .is_none_or(|(agent_id, status, updated_at)| {
            agent_id != &expected.agent_id
                || status != &expected_status
                || updated_at != &expected_updated_at
        })
    {
        return Err(RuntimeStateTransitionConflict::concurrent_mutation(
            "wait_condition_trigger",
            &expected.id,
        )
        .into());
    }
    Ok(())
}

fn validate_task_expectation_tx(tx: &Transaction<'_>, expected: &TaskExpectation) -> Result<()> {
    let actual = tx
        .query_row(
            "SELECT payload_json FROM tasks WHERE task_id = ?1",
            [&expected.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<TaskRecord>(&payload))
        .transpose()?;
    if actual.as_ref().is_none_or(|task| {
        task.agent_id != expected.agent_id
            || task.work_item_id != expected.work_item_id
            || task.status != expected.status
            || task.updated_at != expected.updated_at
            || task.parent_message_id != expected.result_message_id
    }) {
        return Err(
            RuntimeStateTransitionConflict::concurrent_mutation("task_wait", &expected.id).into(),
        );
    }
    Ok(())
}

fn finish_transition_tx(
    tx: &Transaction<'_>,
    applied: bool,
    agent_id: &str,
    audit_events: &[AuditEvent],
    index_changes: &[RuntimeIndexChange],
    fault: Option<TransitionFaultPoint>,
    mut effects: PostCommitEffects,
) -> Result<TransitionCommit> {
    if !applied {
        return Ok(TransitionCommit::default());
    }

    let mut committed_events = Vec::with_capacity(audit_events.len());
    for event in audit_events {
        let (event, inserted) = append_audit_event_tx(tx, Some(agent_id), event)?;
        if inserted {
            committed_events.push(event);
        }
    }
    inject_fault(fault, TransitionFaultPoint::AfterAuditWrites)?;
    insert_runtime_index_changes_tx(tx, index_changes)?;
    inject_fault(fault, TransitionFaultPoint::BeforeCommit)?;

    effects.audit_events = committed_events;
    effects.notify_memory_index = !index_changes.is_empty();
    effects.fault = fault.filter(|point| point.is_post_commit());
    Ok(TransitionCommit {
        applied: true,
        scheduler_authority_blocked: false,
        effects,
    })
}

fn agent_state_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<Option<AgentState>> {
    tx.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
        [agent_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
    .transpose()
}

fn validate_agent_state_mutation_tx(
    tx: &Transaction<'_>,
    mutation: Option<&AgentStateMutation>,
) -> Result<()> {
    let Some(mutation) = mutation else {
        return Ok(());
    };
    if let Some(expected) = mutation.expected.as_ref() {
        let actual = agent_state_tx(tx, &mutation.record.id)?;
        if actual.as_ref() != Some(expected.as_ref()) {
            return Err(RuntimeStateTransitionConflict::concurrent_mutation(
                "agent_state",
                &mutation.record.id,
            )
            .into());
        }
    }
    Ok(())
}

fn validate_focus_target_tx(tx: &Transaction<'_>, state: &AgentState) -> Result<()> {
    let Some(work_item_id) = state.current_work_item_id.as_deref() else {
        return Ok(());
    };
    let target = tx
        .query_row(
            "SELECT agent_id, payload_json FROM work_items WHERE work_item_id = ?1",
            [work_item_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((owner_agent_id, payload_json)) = target else {
        return Err(RuntimeError::not_found(
            "work_item_not_found",
            format!(
                "cannot focus missing work item {work_item_id} for agent {}",
                state.id
            ),
        )
        .with_safe_context("work_item_id", work_item_id)
        .with_safe_context("agent_id", &state.id)
        .into());
    };
    let record: WorkItemRecord = serde_json::from_str(&payload_json)?;
    if owner_agent_id != state.id || record.agent_id != state.id {
        return Err(RuntimeError::policy(
            "work_item_access_denied",
            format!("cannot focus work item {work_item_id} owned by another agent"),
        )
        .with_safe_context("work_item_id", work_item_id)
        .with_safe_context("agent_id", &state.id)
        .into());
    }
    if record.state != WorkItemState::Open {
        return Err(RuntimeError::validation(
            "work_item_completed",
            format!("cannot focus completed work item {work_item_id}"),
        )
        .with_safe_context("work_item_id", work_item_id)
        .into());
    }
    Ok(())
}

fn validate_scheduler_claim_work_item_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    operation: QueueOperation,
    expected: Option<&WorkItemRecord>,
) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    if operation != QueueOperation::Claim {
        return Err(anyhow!(
            "scheduler WorkItem claim guard is only valid for queue claim"
        ));
    }
    let actual = tx
        .query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id = ?1",
            [&expected.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<WorkItemRecord>(&payload))
        .transpose()?;
    if actual.as_ref() != Some(expected)
        || expected.agent_id != agent_id
        || expected.state != WorkItemState::Open
    {
        return Err(RuntimeStateTransitionConflict::concurrent_mutation(
            "scheduler_claim_work_item",
            &expected.id,
        )
        .into());
    }
    let mut statement = tx.prepare(
        "SELECT payload_json
         FROM wait_conditions
         WHERE agent_id = ?1
           AND work_item_id = ?2
           AND status = 'active'
         ORDER BY created_at ASC, wait_condition_id ASC",
    )?;
    let active_wait_conditions = statement
        .query_map([agent_id, expected.id.as_str()], |row| {
            row.get::<_, String>(0)
        })?
        .map(|row| Ok(serde_json::from_str::<WaitConditionRecord>(&row?)?))
        .collect::<Result<Vec<_>>>()?;
    let is_yielded = tx.query_row(
        "SELECT EXISTS(
               SELECT 1
               FROM work_item_continuations
               WHERE agent_id = ?1
                 AND suspended_work_item_id = ?2
                 AND state = 'active'
             )",
        [agent_id, expected.id.as_str()],
        |row| row.get::<_, bool>(0),
    )?;
    let trigger_delivery_by_id = BTreeMap::new();
    let scheduling = crate::work_item_scheduling::derive_work_item_scheduling(
        crate::work_item_scheduling::WorkItemSchedulingFacts {
            work_item: expected,
            is_current: false,
            is_yielded,
            active_wait_conditions: &active_wait_conditions,
            trigger_delivery_by_id: &trigger_delivery_by_id,
        },
    );
    if scheduling.scheduling_state != WorkItemSchedulingState::Runnable {
        return Err(RuntimeStateTransitionConflict::concurrent_mutation(
            "scheduler_claim_work_item",
            &expected.id,
        )
        .into());
    }
    Ok(())
}

fn validate_work_item_continuation_tx(
    tx: &Transaction<'_>,
    incoming: &WorkItemContinuationFrame,
) -> Result<()> {
    let existing = tx
        .query_row(
            "SELECT payload_json FROM work_item_continuations WHERE continuation_id = ?1",
            [&incoming.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<WorkItemContinuationFrame>(&payload))
        .transpose()?;
    if let Some(existing) = existing {
        if existing.agent_id != incoming.agent_id
            || existing.suspended_work_item_id != incoming.suspended_work_item_id
            || existing.active_work_item_id != incoming.active_work_item_id
            || existing.return_policy != incoming.return_policy
            || incoming.updated_at < existing.updated_at
        {
            return Err(anyhow!(
                "work item continuation {} changed before runtime transition commit",
                incoming.id
            ));
        }
    }
    Ok(())
}

fn apply_agent_state_mutation_tx(
    tx: &Transaction<'_>,
    mutation: Option<&AgentStateMutation>,
) -> Result<bool> {
    let Some(mutation) = mutation else {
        return Ok(false);
    };
    if agent_state_tx(tx, &mutation.record.id)?.as_ref() == Some(mutation.record.as_ref()) {
        return Ok(false);
    }
    upsert_agent_state_tx(tx, mutation.record.as_ref())?;
    Ok(true)
}

fn validate_work_item_mutation_tx(tx: &Transaction<'_>, mutation: &WorkItemMutation) -> Result<()> {
    validate_work_item_completion_contract(mutation.record())?;
    match mutation {
        WorkItemMutation::Insert { record } => {
            if record.revision != 1 {
                return Err(anyhow!(
                    "work item {} insert requires revision 1",
                    record.id
                ));
            }
            let existing = tx
                .query_row(
                    "SELECT payload_json FROM work_items WHERE work_item_id = ?1",
                    [&record.id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(payload) = existing {
                if payload != serde_json::to_string(record)? {
                    insert_new_work_item_tx(tx, record)?;
                }
            }
        }
        WorkItemMutation::Update {
            record,
            expected_revision,
        } => {
            let existing = tx
                .query_row(
                    "SELECT revision, payload_json FROM work_items WHERE work_item_id = ?1",
                    [&record.id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            let Some((actual_revision, payload)) = existing else {
                update_expected_work_item_tx(tx, record, *expected_revision)?;
                return Ok(());
            };
            let existing_record: WorkItemRecord = serde_json::from_str(&payload)?;
            validate_work_item_state_transition(&existing_record, record)?;
            let actual_revision = u64::try_from(actual_revision)?;
            if actual_revision != *expected_revision
                && !(actual_revision == record.revision
                    && payload == serde_json::to_string(record)?)
            {
                update_expected_work_item_tx(tx, record, *expected_revision)?;
            }
        }
    }
    Ok(())
}

fn validate_work_item_completion_contract(record: &WorkItemRecord) -> Result<()> {
    match record.state {
        WorkItemState::Open => {
            anyhow::ensure!(
                record.completion_intent.is_none() && record.result_brief_id.is_none(),
                "open work item {} cannot carry completion binding",
                record.id
            );
        }
        WorkItemState::Completing => {
            let intent = record.completion_intent.as_ref().ok_or_else(|| {
                anyhow!(
                    "completing work item {} requires completion intent",
                    record.id
                )
            })?;
            anyhow::ensure!(
                intent.work_item_id == record.id
                    && matches!(
                        intent.report_state,
                        crate::types::CompletionReportState::Pending
                            | crate::types::CompletionReportState::Missing
                    )
                    && intent.result_brief_id.is_none()
                    && record.result_brief_id.is_none(),
                "completing work item {} has invalid completion intent",
                record.id
            );
        }
        WorkItemState::Completed => {
            let intent = record.completion_intent.as_ref().ok_or_else(|| {
                anyhow!(
                    "completed work item {} requires completion intent",
                    record.id
                )
            })?;
            let result_brief_id = record
                .result_brief_id
                .as_deref()
                .filter(|brief_id| !brief_id.trim().is_empty())
                .ok_or_else(|| {
                    anyhow!("completed work item {} requires result brief", record.id)
                })?;
            anyhow::ensure!(
                intent.work_item_id == record.id
                    && intent.report_state == crate::types::CompletionReportState::Bound
                    && intent.result_brief_id.as_deref() == Some(result_brief_id),
                "completed work item {} has inconsistent completion binding",
                record.id
            );
        }
    }
    Ok(())
}

fn validate_work_item_state_transition(
    existing: &WorkItemRecord,
    incoming: &WorkItemRecord,
) -> Result<()> {
    let allowed = matches!(
        (&existing.state, &incoming.state),
        (
            WorkItemState::Open,
            WorkItemState::Open | WorkItemState::Completing | WorkItemState::Completed
        ) | (
            WorkItemState::Completing,
            WorkItemState::Completing | WorkItemState::Completed
        ) | (WorkItemState::Completed, WorkItemState::Completed)
    );
    anyhow::ensure!(
        allowed,
        "invalid work item lifecycle transition {:?} -> {:?} for {}",
        existing.state,
        incoming.state,
        incoming.id
    );
    Ok(())
}

fn apply_work_item_mutation_tx(tx: &Transaction<'_>, mutation: &WorkItemMutation) -> Result<bool> {
    match mutation {
        WorkItemMutation::Insert { record } => {
            let existing = tx
                .query_row(
                    "SELECT payload_json FROM work_items WHERE work_item_id = ?1",
                    [&record.id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if existing.as_deref() == Some(serde_json::to_string(record)?.as_str()) {
                Ok(false)
            } else {
                insert_new_work_item_tx(tx, record)
            }
        }
        WorkItemMutation::Update {
            record,
            expected_revision,
        } => update_expected_work_item_tx(tx, record, *expected_revision),
    }
}

fn validate_task_tx(tx: &Transaction<'_>, incoming: &TaskRecord) -> Result<()> {
    let existing = tx
        .query_row(
            "SELECT payload_json FROM tasks WHERE task_id = ?1",
            [&incoming.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<TaskRecord>(&payload))
        .transpose()?;
    if let Some(existing) = existing.as_ref() {
        task_transition(existing, incoming)?;
    }
    Ok(())
}

fn validate_wait_condition_tx(tx: &Transaction<'_>, incoming: &WaitConditionRecord) -> Result<()> {
    let existing = tx
        .query_row(
            "SELECT payload_json FROM wait_conditions WHERE wait_condition_id = ?1",
            [&incoming.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<WaitConditionRecord>(&payload))
        .transpose()?;
    if let Some(existing) = existing.as_ref() {
        wait_condition_transition(existing, incoming)?;
    }
    Ok(())
}

fn validate_queue_mutation_tx(tx: &Transaction<'_>, mutation: &QueueMutation) -> Result<()> {
    let incoming = match mutation {
        QueueMutation::Consume(record) | QueueMutation::Upsert(record) => record,
        QueueMutation::CompareAndSet { record, .. } => record,
    };
    let existing = tx
        .query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
            [&incoming.message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<QueueEntryRecord>(&payload))
        .transpose()?;
    match (mutation, existing.as_ref()) {
        (QueueMutation::Upsert(_), Some(existing)) => {
            queue_entry_transition(existing, incoming)?;
        }
        (QueueMutation::CompareAndSet { expected, record }, _) => {
            queue_entry_transition(expected, record)?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_queue_operation(command: &QueueTransitionCommand) -> Result<()> {
    let valid = matches!(
        (&command.operation, &command.mutation),
        (
            QueueOperation::Admit,
            QueueMutation::Upsert(QueueEntryRecord {
                status: QueueEntryStatus::Queued,
                ..
            })
        ) | (
            QueueOperation::Claim,
            QueueMutation::Consume(QueueEntryRecord {
                status: QueueEntryStatus::Dequeued,
                ..
            })
        ) | (
            QueueOperation::Interject,
            QueueMutation::Consume(QueueEntryRecord {
                status: QueueEntryStatus::Interjected,
                ..
            })
        ) | (
            QueueOperation::Settle,
            QueueMutation::Upsert(QueueEntryRecord {
                status: QueueEntryStatus::Processed
                    | QueueEntryStatus::Interrupted
                    | QueueEntryStatus::Aborted
                    | QueueEntryStatus::Dropped
                    | QueueEntryStatus::Quarantined,
                ..
            })
        ) | (
            QueueOperation::Settle,
            QueueMutation::CompareAndSet {
                expected: QueueEntryRecord {
                    status: QueueEntryStatus::Dequeued,
                    ..
                },
                record: QueueEntryRecord {
                    status: QueueEntryStatus::Processed
                        | QueueEntryStatus::Interrupted
                        | QueueEntryStatus::Aborted
                        | QueueEntryStatus::Dropped
                        | QueueEntryStatus::Quarantined,
                    ..
                },
            }
        ) | (
            QueueOperation::Settle,
            QueueMutation::CompareAndSet {
                expected: QueueEntryRecord {
                    status: QueueEntryStatus::Queued | QueueEntryStatus::Interrupted,
                    ..
                },
                record: QueueEntryRecord {
                    status: QueueEntryStatus::Dropped | QueueEntryStatus::Quarantined,
                    ..
                },
            }
        ) | (
            QueueOperation::RepairDrop,
            QueueMutation::CompareAndSet {
                expected: QueueEntryRecord {
                    status: QueueEntryStatus::Queued | QueueEntryStatus::Interrupted,
                    ..
                },
                record: QueueEntryRecord {
                    status: QueueEntryStatus::Dropped,
                    ..
                },
            }
        )
    );
    if !valid {
        bail!(
            "queue operation {:?} does not match its mutation or target status",
            command.operation
        );
    }
    Ok(())
}

fn inject_fault(
    configured: Option<TransitionFaultPoint>,
    current: TransitionFaultPoint,
) -> Result<()> {
    if configured == Some(current) {
        return Err(anyhow!("injected runtime transition fault at {current:?}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::execution_protocol::{
            AdmitExecution, AdmittedFences, ExecutionAttempt, ExecutionAttemptState,
            ExecutionBinding, ExecutionOrigin, ExecutionOutcome, ExecutionOutcomeRecord,
            ExecutionPriority, ExecutionProtocolCommand, ExecutionProtocolState,
            ExecutionProvenance, ExecutionSource, ExecutionSourceIdentity, ExecutionTrust,
            SettleExecution, WaitReference, WorkItemExecutionRecord, WorkItemExecutionState,
            WorkItemOutcome,
        },
        domain::scheduler_protocol::{
            ActivationSlot, AgentDispatchState, ProtocolCommand, Snapshot,
        },
        runtime_db::{RuntimeIndexChange, RuntimeIndexOperation},
        types::{
            AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
            BriefKind, CompletionReportRequirement, CompletionReportState, Priority,
            QueueEntryStatus, TaskKind, TaskStatus, WaitConditionKind, WaitConditionStatus,
            WakeSource, WorkItemCompletionIntent, WorkItemContinuationState, WorkItemState,
        },
    };
    use chrono::Utc;
    use tempfile::TempDir;

    fn runtime_db() -> Result<(TempDir, RuntimeDb)> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )?;
        Ok((dir, db))
    }

    fn index_change(kind: &str, id: &str) -> RuntimeIndexChange {
        RuntimeIndexChange {
            agent_id: "agent-a".into(),
            source_kind: kind.into(),
            source_id: id.into(),
            source_ref: format!("{kind}:{id}"),
            operation: RuntimeIndexOperation::Upsert,
            source_updated_at: Some(Utc::now()),
            reason: "transition_test".into(),
        }
    }

    fn work_item(id: &str) -> WorkItemRecord {
        let mut record = WorkItemRecord::new("agent-a", "transition test", WorkItemState::Open);
        record.id = id.into();
        record
    }

    fn wait_condition(id: &str, work_item_id: &str, task_id: &str) -> WaitConditionRecord {
        let now = Utc::now();
        WaitConditionRecord {
            id: id.into(),
            agent_id: "agent-a".into(),
            work_item_id: Some(work_item_id.into()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Task,
            source: Some("test".into()),
            subject_ref: Some(task_id.into()),
            waiting_for: "waiting for task".into(),
            wake_sources: vec![WakeSource::TaskResult {
                task_id: task_id.into(),
            }],
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        }
    }

    fn task(id: &str, status: TaskStatus) -> TaskRecord {
        let now = Utc::now();
        TaskRecord {
            id: id.into(),
            agent_id: "agent-a".into(),
            kind: TaskKind::CommandTask,
            status,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: Some("work-task".into()),
            summary: Some("transition task".into()),
            detail: None,
            recovery: None,
        }
    }

    fn execution_admission(
        message_id: &str,
        attempt_id: &str,
        work_item_id: &str,
    ) -> ExecutionProtocolTransition {
        let mut bootstrap = ExecutionProtocolState::empty("agent-a");
        bootstrap.work_items.insert(
            work_item_id.into(),
            WorkItemExecutionRecord {
                source_revision: 1,
                state: WorkItemExecutionState::Runnable {
                    generation: 1,
                    recovery_ref: None,
                },
            },
        );
        ExecutionProtocolTransition {
            bootstrap: Some(bootstrap),
            commands: vec![ExecutionProtocolCommand::Admit(Box::new(AdmitExecution {
                attempt: ExecutionAttempt {
                    attempt_id: attempt_id.into(),
                    agent_id: "agent-a".into(),
                    source_message_id: Some(message_id.into()),
                    source: ExecutionSource {
                        identity: ExecutionSourceIdentity::QueueMessage {
                            message_id: message_id.into(),
                        },
                        generation: 1,
                    },
                    binding: ExecutionBinding::WorkItem {
                        work_item_id: work_item_id.into(),
                    },
                    provenance: ExecutionProvenance {
                        origin: ExecutionOrigin::System,
                        trust: ExecutionTrust::RuntimeInstruction,
                        priority: ExecutionPriority::Normal,
                        correlation_id: None,
                        causation_id: None,
                    },
                    admitted_fences: AdmittedFences {
                        source_revision: 1,
                        work_item_source_revision: Some(1),
                        work_item_generation: Some(1),
                        rejoin: None,
                        agent_control_revision: 1,
                        host_registry_revision: 1,
                    },
                    state: ExecutionAttemptState::Open,
                    run_id: None,
                    turn_id: None,
                    recovery_of_attempt_id: None,
                    terminal_outcome_id: None,
                    admitted_at: "2026-08-01T00:00:00Z".into(),
                    terminal_at: None,
                },
            }))],
        }
    }

    #[test]
    fn work_item_transition_faults_roll_back_all_durable_facts() -> Result<()> {
        for fault in [
            TransitionFaultPoint::AfterValidation,
            TransitionFaultPoint::AfterCanonicalWrites,
            TransitionFaultPoint::AfterAuditWrites,
            TransitionFaultPoint::BeforeCommit,
        ] {
            let (_dir, db) = runtime_db()?;
            let record = work_item("work-fault");
            let error = db
                .transitions()
                .commit_work_item(&WorkItemTransitionCommand {
                    agent_id: "agent-a".into(),
                    mutation: WorkItemMutation::Insert {
                        record: record.clone(),
                    },
                    agent_state: None,
                    brief_evidence: Vec::new(),
                    audit_events: vec![AuditEvent::legacy("work_item_test", serde_json::json!({}))],
                    index_changes: vec![index_change("work_item", &record.id)],
                    notify_scheduler: true,
                    fault: Some(fault),
                })
                .unwrap_err();
            assert!(error
                .to_string()
                .contains("injected runtime transition fault"));
            assert!(db.work_items().latest(&record.id)?.is_none());
            assert!(db.audit_events().recent(Some("agent-a"), 10)?.is_empty());
            assert_eq!(
                db.runtime_index_outbox()
                    .high_watermark_for_agent("agent-a")?,
                0
            );
        }
        Ok(())
    }

    #[test]
    fn work_item_transition_replay_does_not_duplicate_audit_or_outbox() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        let record = work_item("work-replay");
        let command = WorkItemTransitionCommand {
            agent_id: "agent-a".into(),
            mutation: WorkItemMutation::Insert {
                record: record.clone(),
            },
            agent_state: None,
            brief_evidence: Vec::new(),
            audit_events: vec![AuditEvent::legacy("work_item_test", serde_json::json!({}))],
            index_changes: vec![index_change("work_item", &record.id)],
            notify_scheduler: true,
            fault: None,
        };
        assert!(db.transitions().commit_work_item(&command)?.applied);
        assert!(!db.transitions().commit_work_item(&command)?.applied);
        assert_eq!(db.audit_events().recent(Some("agent-a"), 10)?.len(), 1);
        assert_eq!(
            db.runtime_index_outbox()
                .read_after("agent-a", 0, 10)?
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn work_item_focus_transition_faults_roll_back_focus_and_continuation() -> Result<()> {
        for fault in [
            TransitionFaultPoint::AfterValidation,
            TransitionFaultPoint::AfterCanonicalWrites,
            TransitionFaultPoint::AfterAuditWrites,
            TransitionFaultPoint::BeforeCommit,
        ] {
            let (_dir, db) = runtime_db()?;
            let first = work_item("work-first");
            let second = work_item("work-second");
            db.work_items().insert_new(&first)?;
            db.work_items().insert_new(&second)?;
            let mut initial_state = AgentState::new("agent-a");
            initial_state.current_work_item_id = Some(first.id.clone());
            db.agent_states().upsert(&initial_state)?;
            let mut next_state = initial_state.clone();
            next_state.current_work_item_id = Some(second.id.clone());
            let continuation = WorkItemContinuationFrame::new_on_completed(
                "agent-a",
                first.id.clone(),
                second.id.clone(),
                None,
            );

            db.transitions()
                .commit_work_item_focus(&WorkItemFocusTransitionCommand {
                    agent_id: "agent-a".into(),
                    work_items: Vec::new(),
                    wait_conditions: Vec::new(),
                    continuations: vec![continuation],
                    agent_state: AgentStateMutation {
                        expected: Some(Box::new(initial_state.clone())),
                        record: Box::new(next_state),
                    },
                    brief_evidence: Vec::new(),
                    audit_events: vec![AuditEvent::legacy(
                        "work_item_picked",
                        serde_json::json!({}),
                    )],
                    index_changes: Vec::new(),
                    notify_scheduler: true,
                    fault: Some(fault),
                })
                .unwrap_err();

            assert_eq!(db.agent_states().latest("agent-a")?, Some(initial_state));
            assert!(db.work_item_continuations().latest_all()?.is_empty());
            assert!(db.audit_events().recent(Some("agent-a"), 10)?.is_empty());
        }
        Ok(())
    }

    #[test]
    fn work_item_focus_transition_restores_caller_atomically_with_completion() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        let caller = work_item("work-caller");
        let mut active = work_item("work-active");
        let now = Utc::now();
        active.state = WorkItemState::Completing;
        active.completion_intent = Some(WorkItemCompletionIntent {
            work_item_id: active.id.clone(),
            source_activation_id: None,
            source_message_id: None,
            source_turn_id: None,
            expected_work_revision: active.revision,
            report_requirement: CompletionReportRequirement::Required,
            report_state: CompletionReportState::Pending,
            result_brief_id: None,
            created_at: now,
            updated_at: now,
        });
        active.updated_at = now;
        db.work_items().insert_new(&caller)?;
        db.work_items().insert_new(&active)?;
        let frame = WorkItemContinuationFrame::new_on_completed(
            "agent-a",
            caller.id.clone(),
            active.id.clone(),
            None,
        );
        db.work_item_continuations().upsert(&frame)?;
        let initial_state = AgentState::new("agent-a");
        db.agent_states().upsert(&initial_state)?;
        let mut next_state = initial_state.clone();
        next_state.current_work_item_id = Some(caller.id.clone());
        next_state.current_turn_work_item_id = Some(caller.id.clone());
        let mut result_brief = BriefRecord::new(
            "agent-a",
            BriefKind::Result,
            "transition completed",
            None,
            None,
        );
        result_brief.work_item_id = Some(active.id.clone());
        let mut completed = active.clone();
        completed.revision = 2;
        completed.state = WorkItemState::Completed;
        completed.result_brief_id = Some(result_brief.id.clone());
        completed.result_summary = Some(result_brief.text.clone());
        completed.completion_intent = Some(WorkItemCompletionIntent {
            work_item_id: active.id.clone(),
            source_activation_id: None,
            source_message_id: None,
            source_turn_id: None,
            expected_work_revision: active.revision,
            report_requirement: CompletionReportRequirement::Required,
            report_state: CompletionReportState::Bound,
            result_brief_id: Some(result_brief.id.clone()),
            created_at: now,
            updated_at: now,
        });
        completed.updated_at = now;
        let resumed = frame.resume("active_work_item_completed");

        db.transitions()
            .commit_work_item_focus(&WorkItemFocusTransitionCommand {
                agent_id: "agent-a".into(),
                work_items: vec![WorkItemMutation::Update {
                    record: completed.clone(),
                    expected_revision: active.revision,
                }],
                wait_conditions: Vec::new(),
                continuations: vec![resumed],
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(initial_state)),
                    record: Box::new(next_state.clone()),
                },
                brief_evidence: vec![result_brief],
                audit_events: Vec::new(),
                index_changes: Vec::new(),
                notify_scheduler: true,
                fault: None,
            })?;

        assert_eq!(db.agent_states().latest("agent-a")?, Some(next_state));
        assert_eq!(
            db.work_items().latest(&active.id)?.unwrap().state,
            WorkItemState::Completed
        );
        assert_eq!(
            db.work_item_continuations().latest_all()?[0].state,
            WorkItemContinuationState::Resumed
        );
        Ok(())
    }

    #[test]
    fn concurrent_focus_commands_require_the_same_expected_agent_state() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        let first = work_item("work-first");
        let second = work_item("work-second");
        db.work_items().insert_new(&first)?;
        db.work_items().insert_new(&second)?;
        let initial_state = AgentState::new("agent-a");
        db.agent_states().upsert(&initial_state)?;
        let command = |target: &WorkItemRecord| {
            let mut next_state = initial_state.clone();
            next_state.current_work_item_id = Some(target.id.clone());
            WorkItemFocusTransitionCommand {
                agent_id: "agent-a".into(),
                work_items: Vec::new(),
                wait_conditions: Vec::new(),
                continuations: Vec::new(),
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(initial_state.clone())),
                    record: Box::new(next_state),
                },
                brief_evidence: Vec::new(),
                audit_events: Vec::new(),
                index_changes: Vec::new(),
                notify_scheduler: true,
                fault: None,
            }
        };

        assert!(
            db.transitions()
                .commit_work_item_focus(&command(&first))?
                .applied
        );
        let error = db
            .transitions()
            .commit_work_item_focus(&command(&second))
            .unwrap_err();
        let conflict = error
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .expect("concurrent agent state mutation should return typed conflict");
        assert_eq!(conflict.domain(), "agent_state");
        assert_eq!(conflict.record_id(), "agent-a");
        assert_eq!(conflict.code(), "revision_conflict");
        assert!(conflict.retryable());
        assert_eq!(
            db.agent_states()
                .latest("agent-a")?
                .and_then(|state| state.current_work_item_id),
            Some(first.id)
        );
        Ok(())
    }

    #[test]
    fn wait_transition_rolls_back_work_item_wait_audit_and_outbox_together() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        let initial = work_item("work-wait");
        db.work_items().insert_new(&initial)?;
        let mut blocked = initial.clone();
        blocked.revision = 2;
        blocked.blocked_by = Some("waiting for task".into());
        blocked.updated_at = Utc::now();
        let wait = wait_condition("wait-1", &initial.id, "task-1");

        db.transitions()
            .commit_wait(&WaitTransitionCommand {
                agent_id: "agent-a".into(),
                work_items: vec![WorkItemMutation::Update {
                    record: blocked,
                    expected_revision: 1,
                }],
                expected_wait_conditions: Vec::new(),
                wait_conditions: vec![wait],
                agent_state: None,
                audit_events: vec![AuditEvent::legacy("wait_registered", serde_json::json!({}))],
                index_changes: vec![index_change("work_item", &initial.id)],
                notify_scheduler: true,
                fault: Some(TransitionFaultPoint::AfterAuditWrites),
            })
            .unwrap_err();

        assert_eq!(db.work_items().latest(&initial.id)?.unwrap(), initial);
        assert!(db.wait_conditions().latest_all()?.is_empty());
        assert!(db.audit_events().recent(Some("agent-a"), 10)?.is_empty());
        assert_eq!(
            db.runtime_index_outbox()
                .high_watermark_for_agent("agent-a")?,
            0
        );
        Ok(())
    }

    #[test]
    fn queue_settlement_fault_preserves_claimable_queue_entry() -> Result<()> {
        for fault in [
            TransitionFaultPoint::AfterCanonicalWrites,
            TransitionFaultPoint::AfterAuditWrites,
            TransitionFaultPoint::BeforeCommit,
        ] {
            let (_dir, db) = runtime_db()?;
            let now = Utc::now();
            let queued = QueueEntryRecord {
                message_id: "message-1".into(),
                agent_id: "agent-a".into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Queued,
                created_at: now,
                updated_at: now,
            };
            db.queue_entries().upsert(&queued)?;
            let mut initial_state = AgentState::new("agent-a");
            initial_state.pending = 1;
            db.agent_states().upsert(&initial_state)?;
            let mut settled_state = initial_state.clone();
            settled_state.pending = 0;
            let transcript = TranscriptEntry::new(
                "agent-a",
                crate::types::TranscriptEntryKind::IncomingMessage,
                None,
                Some(queued.message_id.clone()),
                serde_json::json!({}),
            );
            let mut processed = queued.clone();
            processed.status = QueueEntryStatus::Processed;
            processed.updated_at = now + chrono::Duration::seconds(1);

            db.transitions()
                .commit_queue(&QueueTransitionCommand {
                    agent_id: "agent-a".into(),
                    operation: QueueOperation::Settle,
                    mutation: QueueMutation::Upsert(processed),
                    scheduler_claim_work_item: None,
                    scheduler_protocol_bootstrap: None,
                    scheduler_protocol_commands: Vec::new(),
                    agent_state: Some(AgentStateMutation {
                        expected: Some(Box::new(initial_state.clone())),
                        record: Box::new(settled_state),
                    }),
                    message_evidence: Vec::new(),
                    transcript_entries: vec![transcript],
                    turn_record: None,
                    audit_events: vec![AuditEvent::legacy("queue_settled", serde_json::json!({}))],
                    notify_scheduler: true,
                    fault: Some(fault),
                    brief_evidence: Vec::new(),
                })
                .unwrap_err();

            let latest = db.queue_entries().latest_all()?;
            assert_eq!(latest.len(), 1);
            assert_eq!(latest[0].status, QueueEntryStatus::Queued);
            assert_eq!(db.agent_states().latest("agent-a")?, Some(initial_state));
            assert!(db.transcript_entries().all(Some("agent-a"))?.is_empty());
            assert!(db.audit_events().recent(Some("agent-a"), 10)?.is_empty());
        }
        Ok(())
    }

    #[test]
    fn queue_claim_and_execution_admission_roll_back_together() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        let now = Utc::now();
        let queued = QueueEntryRecord {
            message_id: "message-execution-fault".into(),
            agent_id: "agent-a".into(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        db.queue_entries().upsert(&queued)?;
        let mut claimed = queued.clone();
        claimed.status = QueueEntryStatus::Dequeued;
        claimed.updated_at += chrono::Duration::seconds(1);

        db.transitions()
            .commit_queue_with_execution_protocol(
                &QueueTransitionCommand {
                    agent_id: "agent-a".into(),
                    operation: QueueOperation::Claim,
                    mutation: QueueMutation::Consume(claimed),
                    scheduler_claim_work_item: None,
                    scheduler_protocol_bootstrap: None,
                    scheduler_protocol_commands: Vec::new(),
                    agent_state: None,
                    message_evidence: Vec::new(),
                    transcript_entries: Vec::new(),
                    turn_record: None,
                    audit_events: Vec::new(),
                    notify_scheduler: false,
                    fault: Some(TransitionFaultPoint::AfterCanonicalWrites),
                    brief_evidence: Vec::new(),
                },
                &execution_admission(
                    &queued.message_id,
                    "attempt-execution-fault",
                    "work-execution-fault",
                ),
            )
            .unwrap_err();

        assert_eq!(db.queue_entries().latest_all()?, vec![queued]);
        let connection = db.connection()?;
        let partitions: i64 = connection.query_row(
            "SELECT COUNT(*) FROM execution_protocol_partitions",
            [],
            |row| row.get(0),
        )?;
        let attempts: i64 = connection.query_row(
            "SELECT COUNT(*) FROM execution_protocol_attempts",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(partitions, 0);
        assert_eq!(attempts, 0);
        Ok(())
    }

    #[test]
    fn wait_registration_and_execution_settlement_roll_back_together() -> Result<()> {
        for fault in [
            TransitionFaultPoint::AfterValidation,
            TransitionFaultPoint::AfterCanonicalWrites,
            TransitionFaultPoint::AfterAuditWrites,
            TransitionFaultPoint::BeforeCommit,
        ] {
            let (_dir, db) = runtime_db()?;
            let now = Utc::now();
            db.agent_states().upsert(&AgentState::new("agent-a"))?;
            db.agent_identities().upsert(&AgentIdentityRecord::new(
                "agent-a",
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ))?;
            let admission = execution_admission("message-wait", "attempt-wait", "work-wait");
            db.transitions().commit_queue_with_execution_protocol(
                &QueueTransitionCommand {
                    agent_id: "agent-a".into(),
                    operation: QueueOperation::Admit,
                    mutation: QueueMutation::Upsert(QueueEntryRecord {
                        message_id: "message-wait".into(),
                        agent_id: "agent-a".into(),
                        priority: Priority::Normal,
                        status: QueueEntryStatus::Queued,
                        created_at: now,
                        updated_at: now,
                    }),
                    scheduler_claim_work_item: None,
                    scheduler_protocol_bootstrap: None,
                    scheduler_protocol_commands: Vec::new(),
                    agent_state: None,
                    message_evidence: Vec::new(),
                    transcript_entries: Vec::new(),
                    turn_record: None,
                    audit_events: Vec::new(),
                    notify_scheduler: false,
                    fault: None,
                    brief_evidence: Vec::new(),
                },
                &admission,
            )?;
            let wait = wait_condition("wait-execution", "work-wait", "task-wait");
            let settlement = ExecutionProtocolTransition {
                bootstrap: None,
                commands: vec![ExecutionProtocolCommand::Settle(SettleExecution {
                    outcome: ExecutionOutcomeRecord {
                        outcome_id: "outcome-wait".into(),
                        attempt_id: "attempt-wait".into(),
                        outcome: ExecutionOutcome::WorkItem(WorkItemOutcome::Wait {
                            wait: WaitReference {
                                wait_id: wait.id.clone(),
                            },
                        }),
                        created_at: now.to_rfc3339(),
                    },
                })],
            };
            db.transitions()
                .commit_wait_with_execution_protocol(
                    &WaitTransitionCommand {
                        agent_id: "agent-a".into(),
                        work_items: Vec::new(),
                        expected_wait_conditions: Vec::new(),
                        wait_conditions: vec![wait.clone()],
                        agent_state: None,
                        audit_events: Vec::new(),
                        index_changes: Vec::new(),
                        notify_scheduler: false,
                        fault: Some(fault),
                    },
                    &settlement,
                )
                .unwrap_err();

            assert!(db.wait_conditions().latest_all()?.is_empty());
            let state = db
                .transitions()
                .load_execution_protocol_state_if_initialized("agent-a")?
                .expect("admission state");
            assert_eq!(
                state.attempts["attempt-wait"].state,
                ExecutionAttemptState::Open
            );

            db.transitions().commit_wait_with_execution_protocol(
                &WaitTransitionCommand {
                    agent_id: "agent-a".into(),
                    work_items: Vec::new(),
                    expected_wait_conditions: Vec::new(),
                    wait_conditions: vec![wait],
                    agent_state: None,
                    audit_events: Vec::new(),
                    index_changes: Vec::new(),
                    notify_scheduler: false,
                    fault: None,
                },
                &settlement,
            )?;
            let state = db
                .transitions()
                .load_execution_protocol_state_if_initialized("agent-a")?
                .expect("settled state");
            assert_eq!(
                state.attempts["attempt-wait"].state,
                ExecutionAttemptState::Settled
            );
            assert_eq!(db.wait_conditions().latest_all()?.len(), 1);
        }
        Ok(())
    }

    #[test]
    fn execution_admission_command_is_idempotent_across_queue_commits() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        db.agent_states().upsert(&AgentState::new("agent-a"))?;
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            "agent-a",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        let transition =
            execution_admission("message-execution", "attempt-execution", "work-execution");
        for message_id in ["queue-write-1", "queue-write-2"] {
            let now = Utc::now();
            let commit = db.transitions().commit_queue_with_execution_protocol(
                &QueueTransitionCommand {
                    agent_id: "agent-a".into(),
                    operation: QueueOperation::Admit,
                    mutation: QueueMutation::Upsert(QueueEntryRecord {
                        message_id: message_id.into(),
                        agent_id: "agent-a".into(),
                        priority: Priority::Normal,
                        status: QueueEntryStatus::Queued,
                        created_at: now,
                        updated_at: now,
                    }),
                    scheduler_claim_work_item: None,
                    scheduler_protocol_bootstrap: None,
                    scheduler_protocol_commands: Vec::new(),
                    agent_state: None,
                    message_evidence: Vec::new(),
                    transcript_entries: Vec::new(),
                    turn_record: None,
                    audit_events: Vec::new(),
                    notify_scheduler: false,
                    fault: None,
                    brief_evidence: Vec::new(),
                },
                &transition,
            )?;
            assert!(commit.applied);
        }

        let connection = db.connection()?;
        let attempts: i64 = connection.query_row(
            "SELECT COUNT(*) FROM execution_protocol_attempts
             WHERE agent_id = 'agent-a' AND attempt_id = 'attempt-execution'",
            [],
            |row| row.get(0),
        )?;
        let command_results: i64 = connection.query_row(
            "SELECT COUNT(*) FROM execution_protocol_command_results
             WHERE agent_id = 'agent-a'
               AND command_kind = 'admit_execution'
               AND command_identity = 'attempt-execution'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(attempts, 1);
        assert_eq!(command_results, 1);
        Ok(())
    }

    #[test]
    fn queue_compare_and_set_rejects_changed_claim_without_side_effects() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        let now = Utc::now();
        let expected = QueueEntryRecord {
            message_id: "message-recovery-cas".into(),
            agent_id: "agent-a".into(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Dequeued,
            created_at: now,
            updated_at: now,
        };
        db.queue_entries().upsert(&expected)?;
        let mut refreshed = expected.clone();
        refreshed.updated_at += chrono::Duration::seconds(1);
        db.queue_entries().upsert(&refreshed)?;
        let mut processed = expected.clone();
        processed.status = QueueEntryStatus::Processed;
        processed.updated_at += chrono::Duration::seconds(2);

        let commit = db.transitions().commit_queue(&QueueTransitionCommand {
            agent_id: "agent-a".into(),
            operation: QueueOperation::Settle,
            mutation: QueueMutation::CompareAndSet {
                expected,
                record: processed,
            },
            scheduler_claim_work_item: None,
            scheduler_protocol_bootstrap: None,
            scheduler_protocol_commands: Vec::new(),
            agent_state: None,
            message_evidence: Vec::new(),
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: vec![AuditEvent::legacy(
                "stale_recovery_claim_settled",
                serde_json::json!({}),
            )],
            notify_scheduler: true,
            fault: None,
            brief_evidence: Vec::new(),
        })?;

        assert!(!commit.applied);
        assert_eq!(db.queue_entries().latest_all()?, vec![refreshed]);
        assert!(db.audit_events().recent(Some("agent-a"), 10)?.is_empty());
        Ok(())
    }

    #[test]
    fn queue_claim_revalidates_runnable_work_item_inside_transaction() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        let work_item = work_item("work-stale-claim");
        db.work_items().insert_new(&work_item)?;
        db.wait_conditions().upsert(&wait_condition(
            "wait-stale-claim",
            &work_item.id,
            "task-1",
        ))?;
        let now = Utc::now();
        let queued = QueueEntryRecord {
            message_id: "message-stale-claim".into(),
            agent_id: "agent-a".into(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        db.queue_entries().upsert(&queued)?;
        let mut claimed = queued.clone();
        claimed.status = QueueEntryStatus::Dequeued;
        claimed.updated_at += chrono::Duration::seconds(1);

        let error = db
            .transitions()
            .commit_queue(&QueueTransitionCommand {
                agent_id: "agent-a".into(),
                operation: QueueOperation::Claim,
                mutation: QueueMutation::Consume(claimed),
                scheduler_claim_work_item: Some(work_item.clone()),
                scheduler_protocol_bootstrap: None,
                scheduler_protocol_commands: Vec::new(),
                agent_state: None,
                message_evidence: Vec::new(),
                transcript_entries: Vec::new(),
                turn_record: None,
                audit_events: vec![AuditEvent::legacy(
                    "queue_entry_claimed",
                    serde_json::json!({}),
                )],
                notify_scheduler: false,
                fault: None,
                brief_evidence: Vec::new(),
            })
            .unwrap_err();

        let conflict = error
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .expect("non-runnable WorkItem claim should return typed conflict");
        assert_eq!(conflict.domain(), "scheduler_claim_work_item");
        assert_eq!(conflict.record_id(), work_item.id);
        assert!(conflict.retryable());
        assert_eq!(db.queue_entries().latest_all()?, vec![queued]);
        assert!(db.audit_events().recent(Some("agent-a"), 10)?.is_empty());
        assert!(db
            .transitions()
            .load_scheduler_protocol_snapshot_if_initialized("agent-a")?
            .is_none());
        Ok(())
    }

    #[test]
    fn queue_claim_retains_head_when_legacy_adoption_source_changes() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        let work_item = work_item("work-stale-adoption");
        db.work_items().insert_new(&work_item)?;
        let adoption = db
            .transitions()
            .legacy_scheduler_adoption_candidates("agent-a")?
            .into_iter()
            .find(|candidate| candidate.work_item_id == work_item.id)
            .and_then(|candidate| candidate.command)
            .expect("eligible legacy adoption command");
        let mut changed = work_item.clone();
        changed.revision += 1;
        changed.updated_at += chrono::Duration::seconds(1);
        assert!(db
            .work_items()
            .update_expected(&changed, work_item.revision)?);

        let now = Utc::now();
        let queued = QueueEntryRecord {
            message_id: "message-stale-adoption".into(),
            agent_id: "agent-a".into(),
            priority: Priority::Next,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        db.queue_entries().upsert(&queued)?;
        let mut claimed = queued.clone();
        claimed.status = QueueEntryStatus::Dequeued;
        claimed.updated_at += chrono::Duration::seconds(1);
        let bootstrap = Snapshot {
            slot: ActivationSlot::Idle,
            dispatch: AgentDispatchState::Open,
            dispatch_revision: 0,
            focus: None,
            work: Default::default(),
            waits: Default::default(),
            activations: Default::default(),
            activation_admissions: Default::default(),
            settlements: Default::default(),
            missing_settlements: Default::default(),
            admitted_generations: Default::default(),
            continuation_admissions: Default::default(),
            activation_inputs: Default::default(),
        };

        let commit = db.transitions().commit_queue(&QueueTransitionCommand {
            agent_id: "agent-a".into(),
            operation: QueueOperation::Claim,
            mutation: QueueMutation::Consume(claimed),
            scheduler_claim_work_item: None,
            scheduler_protocol_bootstrap: Some(bootstrap),
            scheduler_protocol_commands: vec![adoption.clone()],
            agent_state: None,
            message_evidence: Vec::new(),
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: true,
            fault: None,
            brief_evidence: Vec::new(),
        })?;

        assert!(commit.scheduler_authority_blocked);
        assert_eq!(db.queue_entries().latest_all()?, vec![queued]);
        assert!(db
            .audit_events()
            .recent(Some("agent-a"), 10)?
            .iter()
            .any(|event| {
                event.kind == "scheduler_claim_contended"
                    && event.data["conflict_code"] == "legacy_adoption_source_changed"
                    && event.data["queue_disposition"] == "retained_queued"
            }));
        assert!(db
            .transitions()
            .load_scheduler_protocol_snapshot_if_initialized("agent-a")?
            .is_none());
        assert!(matches!(adoption, ProtocolCommand::AdoptLegacyWorkState(_)));
        Ok(())
    }

    #[test]
    fn terminal_task_wait_release_is_atomic_and_idempotent() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        let mut initial_work = work_item("work-task");
        initial_work.blocked_by = Some("waiting for task".into());
        db.work_items().insert_new(&initial_work)?;
        let active_wait = wait_condition("wait-task", &initial_work.id, "task-1");
        db.wait_conditions().upsert(&active_wait)?;
        let running = task("task-1", TaskStatus::Running);
        db.tasks().upsert(&running)?;

        let mut terminal = running.clone();
        terminal.status = TaskStatus::Completed;
        terminal.updated_at += chrono::Duration::seconds(1);
        let mut resolved = active_wait.clone();
        resolved.status = WaitConditionStatus::Resolved;
        resolved.updated_at = terminal.updated_at;
        resolved.resolved_at = Some(terminal.updated_at);
        let mut cleared = initial_work.clone();
        cleared.revision = 2;
        cleared.blocked_by = None;
        cleared.updated_at = terminal.updated_at;
        let command = TaskTransitionCommand {
            agent_id: "agent-a".into(),
            task: terminal.clone(),
            work_items: vec![WorkItemMutation::Update {
                record: cleared.clone(),
                expected_revision: 1,
            }],
            wait_conditions: vec![resolved],
            agent_state: None,
            message_evidence: Vec::new(),
            audit_events: vec![
                AuditEvent::legacy("task_terminal", serde_json::json!({})),
                AuditEvent::legacy("wait_resolved", serde_json::json!({})),
            ],
            index_changes: vec![
                index_change("task", &terminal.id),
                index_change("work_item", &cleared.id),
            ],
            notify_scheduler: true,
            commit_on_idempotent: false,
            fault: Some(TransitionFaultPoint::AfterCanonicalWrites),
        };
        db.transitions().commit_task(&command).unwrap_err();
        assert_eq!(
            db.tasks().latest(&running.id)?.unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(
            db.work_items().latest(&initial_work.id)?.unwrap(),
            initial_work
        );
        assert_eq!(
            db.wait_conditions().latest_all()?[0].status,
            WaitConditionStatus::Active
        );
        assert!(db.audit_events().recent(Some("agent-a"), 10)?.is_empty());
        assert_eq!(
            db.runtime_index_outbox()
                .high_watermark_for_agent("agent-a")?,
            0
        );

        let command = TaskTransitionCommand {
            fault: None,
            ..command
        };
        assert!(db.transitions().commit_task(&command)?.applied);
        assert!(!db.transitions().commit_task(&command)?.applied);
        assert_eq!(
            db.tasks().latest(&terminal.id)?.unwrap().status,
            TaskStatus::Completed
        );
        assert_eq!(db.work_items().latest(&cleared.id)?.unwrap(), cleared);
        assert_eq!(
            db.wait_conditions().latest_all()?[0].status,
            WaitConditionStatus::Resolved
        );
        assert_eq!(db.audit_events().recent(Some("agent-a"), 10)?.len(), 2);
        assert_eq!(
            db.runtime_index_outbox()
                .read_after("agent-a", 0, 10)?
                .len(),
            2
        );
        Ok(())
    }
}
