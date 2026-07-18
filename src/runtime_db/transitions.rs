//! Restricted runtime business-transition unit of work.

use anyhow::{anyhow, Result};
use rusqlite::{OptionalExtension, Transaction};

use crate::{
    runtime_db::{
        evidence::{
            append_audit_event_tx, append_transcript_entry_tx, insert_runtime_index_changes_tx,
            upsert_agent_state_tx,
        },
        repositories::{
            insert_new_work_item_tx, queue_entry_transition, task_transition,
            try_claim_queued_message_tx, update_expected_work_item_tx, upsert_queue_entry_tx,
            upsert_task_tx, upsert_wait_condition_tx, upsert_work_item_continuation_tx,
            wait_condition_transition,
        },
        RuntimeDb, RuntimeIndexChange,
    },
    runtime_error::RuntimeError,
    types::{
        AgentState, AuditEvent, QueueEntryRecord, TaskRecord, TranscriptEntry, WaitConditionRecord,
        WorkItemContinuationFrame, WorkItemRecord, WorkItemState,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransitionFaultPoint {
    AfterValidation,
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
    Claim(QueueEntryRecord),
    Upsert(QueueEntryRecord),
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
    pub audit_events: Vec<AuditEvent>,
    pub index_changes: Vec<RuntimeIndexChange>,
    pub notify_scheduler: bool,
    pub fault: Option<TransitionFaultPoint>,
}

#[derive(Debug, Clone)]
pub(crate) struct WaitTransitionCommand {
    pub agent_id: String,
    pub work_items: Vec<WorkItemMutation>,
    pub wait_conditions: Vec<WaitConditionRecord>,
    pub agent_state: Option<AgentStateMutation>,
    pub audit_events: Vec<AuditEvent>,
    pub index_changes: Vec<RuntimeIndexChange>,
    pub notify_scheduler: bool,
    pub fault: Option<TransitionFaultPoint>,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkItemFocusTransitionCommand {
    pub agent_id: String,
    pub work_items: Vec<WorkItemMutation>,
    pub wait_conditions: Vec<WaitConditionRecord>,
    pub continuations: Vec<WorkItemContinuationFrame>,
    pub agent_state: AgentStateMutation,
    pub audit_events: Vec<AuditEvent>,
    pub index_changes: Vec<RuntimeIndexChange>,
    pub notify_scheduler: bool,
    pub fault: Option<TransitionFaultPoint>,
}

#[derive(Debug, Clone)]
pub(crate) struct QueueTransitionCommand {
    pub agent_id: String,
    pub mutation: QueueMutation,
    pub agent_state: Option<AgentStateMutation>,
    pub transcript_entries: Vec<TranscriptEntry>,
    pub audit_events: Vec<AuditEvent>,
    pub notify_scheduler: bool,
    pub fault: Option<TransitionFaultPoint>,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskTransitionCommand {
    pub agent_id: String,
    pub task: TaskRecord,
    pub work_items: Vec<WorkItemMutation>,
    pub wait_conditions: Vec<WaitConditionRecord>,
    pub agent_state: Option<AgentStateMutation>,
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
}

impl RuntimeTransitionRepository<'_> {
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
        self.db.transaction(|tx| {
            for work_item in &command.work_items {
                validate_work_item_mutation_tx(tx, work_item)?;
            }
            for condition in &command.wait_conditions {
                validate_wait_condition_tx(tx, condition)?;
            }
            validate_agent_state_mutation_tx(tx, command.agent_state.as_ref())?;
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
                    notify_scheduler: command.notify_scheduler,
                    ..PostCommitEffects::default()
                },
            )
        })
    }

    pub fn commit_queue(&self, command: &QueueTransitionCommand) -> Result<TransitionCommit> {
        self.db.transaction(|tx| {
            validate_queue_mutation_tx(tx, &command.mutation)?;
            validate_agent_state_mutation_tx(tx, command.agent_state.as_ref())?;
            inject_fault(command.fault, TransitionFaultPoint::AfterValidation)?;
            let mut applied = match &command.mutation {
                QueueMutation::Claim(record) => try_claim_queued_message_tx(tx, record)?,
                QueueMutation::Upsert(record) => upsert_queue_entry_tx(tx, record)?,
            };
            let agent_state_applied =
                apply_agent_state_mutation_tx(tx, command.agent_state.as_ref())?;
            applied |= agent_state_applied;
            if !applied {
                return Ok(TransitionCommit::default());
            }
            for entry in &command.transcript_entries {
                append_transcript_entry_tx(tx, entry)?;
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
                    agent_state: agent_state_applied
                        .then(|| command.agent_state.clone())
                        .flatten(),
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
            return Err(anyhow!(
                "agent state {} changed before runtime transition commit",
                mutation.record.id
            ));
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
        QueueMutation::Claim(record) | QueueMutation::Upsert(record) => record,
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
    if let (QueueMutation::Upsert(_), Some(existing)) = (mutation, existing.as_ref()) {
        queue_entry_transition(existing, incoming)?;
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
        runtime_db::{RuntimeIndexChange, RuntimeIndexOperation},
        types::{
            Priority, QueueEntryStatus, TaskKind, TaskStatus, WaitConditionKind,
            WaitConditionStatus, WakeSource, WorkItemContinuationState, WorkItemState,
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
        let active = work_item("work-active");
        db.work_items().insert_new(&caller)?;
        db.work_items().insert_new(&active)?;
        let frame = WorkItemContinuationFrame::new_on_completed(
            "agent-a",
            caller.id.clone(),
            active.id.clone(),
            None,
        );
        db.work_item_continuations().upsert(&frame)?;
        let mut initial_state = AgentState::new("agent-a");
        initial_state.current_work_item_id = Some(active.id.clone());
        db.agent_states().upsert(&initial_state)?;
        let mut next_state = initial_state.clone();
        next_state.current_work_item_id = Some(caller.id.clone());
        let mut completed = active.clone();
        completed.revision = 2;
        completed.state = WorkItemState::Completed;
        completed.updated_at = Utc::now();
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
        assert!(error
            .to_string()
            .contains("changed before runtime transition commit"));
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
                    mutation: QueueMutation::Upsert(processed),
                    agent_state: Some(AgentStateMutation {
                        expected: Some(Box::new(initial_state.clone())),
                        record: Box::new(settled_state),
                    }),
                    transcript_entries: vec![transcript],
                    audit_events: vec![AuditEvent::legacy("queue_settled", serde_json::json!({}))],
                    notify_scheduler: true,
                    fault: Some(fault),
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
