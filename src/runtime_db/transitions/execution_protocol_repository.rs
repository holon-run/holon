use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::domain::execution_protocol::{
    self, ExecutionOutcomeRecord, ExecutionProtocolCommand, ExecutionProtocolState,
    ExecutionTransition, WorkItemExecutionState,
};
use crate::{
    runtime_db::transitions::{
        ExecutionAuthorityFences, RuntimeTransitionRepository, WorkItemMutation,
    },
    types::{AgentIdentityRecord, AgentRegistryStatus, AgentStatus},
};

pub(super) struct PreparedExecutionProtocolCommands {
    state: ExecutionProtocolState,
    initialized_partition: bool,
    results: Vec<PreparedCommandResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExecutionProtocolRecoveryCommitOutcome {
    Applied(bool),
    Rejected { reason: String },
}

impl PreparedExecutionProtocolCommands {
    pub(super) fn has_writes(&self) -> bool {
        self.initialized_partition || !self.results.is_empty()
    }
}

struct PreparedCommandResult {
    command_kind: &'static str,
    command_identity: String,
    payload_hash: String,
    references: Vec<String>,
}

pub(super) fn validate_execution_commands_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    bootstrap: Option<&ExecutionProtocolState>,
    commands: &[ExecutionProtocolCommand],
    work_item_mutations: &[WorkItemMutation],
    wait_conditions: &[crate::types::WaitConditionRecord],
    continuations: &[crate::types::WorkItemContinuationFrame],
) -> Result<Option<PreparedExecutionProtocolCommands>> {
    if commands.is_empty() {
        return Ok(None);
    }
    let initialized_partition = !partition_exists_tx(tx, agent_id)?;
    let mut state = if initialized_partition {
        let state = bootstrap
            .cloned()
            .ok_or_else(|| anyhow!("execution protocol command requires bootstrap state"))?;
        if state.agent_id != agent_id {
            bail!("execution protocol bootstrap agent does not match transaction partition");
        }
        execution_protocol::assert_invariants(&state)
            .map_err(|error| anyhow!("invalid execution protocol bootstrap: {error}"))?;
        state
    } else {
        load_state_tx(tx, agent_id)?
    };
    let mut results = Vec::new();

    for command in commands {
        let (command_kind, command_identity) = command_identity(command);
        let payload_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(command)?));
        if let Some(stored_hash) =
            stored_command_hash_tx(tx, agent_id, command_kind, command_identity)?
        {
            if stored_hash != payload_hash {
                bail!(
                    "execution protocol command identity conflict for agent {agent_id}, \
                     command {command_kind} {command_identity}"
                );
            }
            continue;
        }
        if let ExecutionProtocolCommand::Admit(command) = command {
            validate_admission_authority_tx(tx, agent_id, &command.attempt.admitted_fences)?;
        }
        if let ExecutionProtocolCommand::RegisterWorkItem(command) = command {
            validate_register_work_item(tx, agent_id, command, work_item_mutations)?;
        }
        if let ExecutionProtocolCommand::AdvanceWorkItemSourceRevision(command) = command {
            validate_work_item_source_revision_advance(command, work_item_mutations)?;
        }
        if let ExecutionProtocolCommand::SetWorkItemReadiness(command) = command {
            validate_set_work_item_readiness(command, work_item_mutations, wait_conditions)?;
        }
        if let ExecutionProtocolCommand::SuspendWorkItemContinuation(command) = command {
            validate_suspend_work_item_continuation(tx, agent_id, command, continuations)?;
        }
        if let ExecutionProtocolCommand::ResumeWorkItemContinuation(command) = command {
            validate_resume_work_item_continuation(
                tx,
                agent_id,
                command,
                work_item_mutations,
                wait_conditions,
                continuations,
            )?;
        }
        if let ExecutionProtocolCommand::ReconcileWorkItemContinuationYield(command) = command {
            validate_reconcile_work_item_continuation_yield(tx, agent_id, command)?;
        }
        if let ExecutionProtocolCommand::SetWorkItemWaiting(command) = command {
            validate_set_work_item_waiting(command, work_item_mutations, wait_conditions)?;
        }
        if let ExecutionProtocolCommand::CompleteWorkItem(command) = command {
            validate_complete_work_item(command, work_item_mutations)?;
        }
        let transition = reduce(&state, command)
            .map_err(|error| anyhow!("execution protocol command rejected: {error}"))?;
        state = transition.state;
        results.push(PreparedCommandResult {
            command_kind,
            command_identity: command_identity.to_owned(),
            payload_hash,
            references: transition.references,
        });
    }

    Ok(Some(PreparedExecutionProtocolCommands {
        state,
        initialized_partition,
        results,
    }))
}

pub(super) fn synchronize_work_item_revisions_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    transition: &crate::runtime_db::transitions::ExecutionProtocolTransition,
    work_item_mutations: &[WorkItemMutation],
) -> Result<crate::runtime_db::transitions::ExecutionProtocolTransition> {
    if !partition_exists_tx(tx, agent_id)? {
        return Ok(transition.clone());
    }
    let state = load_state_tx(tx, agent_id)?;
    let mut synchronized = transition.clone();
    for mutation in work_item_mutations {
        let WorkItemMutation::Update { record, .. } = mutation else {
            continue;
        };
        if record.state != crate::types::WorkItemState::Open {
            continue;
        }
        let command_targets_work_item = synchronized.commands.iter().any(|command| match command {
            ExecutionProtocolCommand::RegisterWorkItem(command) => {
                command.work_item_id == record.id
            }
            ExecutionProtocolCommand::AdvanceWorkItemSourceRevision(command) => {
                command.work_item_id == record.id
            }
            ExecutionProtocolCommand::SetWorkItemReadiness(command) => {
                command.work_item_id == record.id
            }
            ExecutionProtocolCommand::SuspendWorkItemContinuation(command) => {
                command.work_item_id == record.id
            }
            ExecutionProtocolCommand::ResumeWorkItemContinuation(command) => {
                command.work_item_id == record.id
            }
            ExecutionProtocolCommand::SetWorkItemWaiting(command) => {
                command.work_item_id == record.id
            }
            _ => false,
        });
        let Some(authoritative) = state.work_items.get(&record.id) else {
            if !command_targets_work_item {
                let mut statement = tx.prepare(
                    "SELECT wait_condition_id
                     FROM wait_conditions
                     WHERE agent_id = ?1
                       AND work_item_id = ?2
                       AND status IN ('active', 'triggered')
                     ORDER BY created_at ASC, wait_condition_id ASC",
                )?;
                let wait_ids = statement
                    .query_map([agent_id, record.id.as_str()], |row| {
                        row.get::<_, String>(0)
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let execution_state = match wait_ids.as_slice() {
                    [wait_id] => WorkItemExecutionState::Waiting {
                        generation: record.revision.max(1),
                        wait: execution_protocol::WaitReference {
                            wait_id: wait_id.clone(),
                        },
                    },
                    [] if record.blocked_by.is_some() => WorkItemExecutionState::Paused {
                        generation: record.revision.max(1),
                        reason: record
                            .blocked_by
                            .clone()
                            .expect("manual blocker checked above"),
                    },
                    [] => WorkItemExecutionState::Runnable {
                        generation: record.revision.max(1),
                        recovery_ref: None,
                    },
                    _ => WorkItemExecutionState::NeedsRepair {
                        generation: record.revision.max(1),
                        repair_id: format!("work_item_waits_ambiguous:{}", record.id),
                    },
                };
                synchronized.commands.insert(
                    0,
                    ExecutionProtocolCommand::RegisterWorkItem(Box::new(
                        execution_protocol::RegisterWorkItemExecution {
                            work_item_id: record.id.clone(),
                            record: execution_protocol::WorkItemExecutionRecord {
                                source_revision: record.revision,
                                state: execution_state,
                            },
                        },
                    )),
                );
            }
            continue;
        };
        if authoritative.source_revision == record.revision {
            continue;
        }
        if authoritative.source_revision > record.revision {
            bail!(
                "WorkItem execution source revision {} exceeds durable revision {}",
                authoritative.source_revision,
                record.revision
            );
        }
        if !command_targets_work_item {
            synchronized.commands.insert(
                0,
                ExecutionProtocolCommand::AdvanceWorkItemSourceRevision(
                    execution_protocol::AdvanceWorkItemSourceRevision {
                        command_id: format!(
                            "work_item:auto_revision:{}:{}",
                            record.id, record.revision
                        ),
                        work_item_id: record.id.clone(),
                        expected_source_revision: authoritative.source_revision,
                        source_revision: record.revision,
                    },
                ),
            );
        }
    }
    Ok(synchronized)
}

pub(super) fn persist_execution_commands_tx(
    tx: &Transaction<'_>,
    prepared: Option<PreparedExecutionProtocolCommands>,
) -> Result<()> {
    let Some(prepared) = prepared else {
        return Ok(());
    };
    persist_state_tx(tx, &prepared.state)?;
    for result in prepared.results {
        tx.execute(
            "INSERT INTO execution_protocol_command_results (
               agent_id, command_kind, command_identity,
               payload_hash, references_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                prepared.state.agent_id,
                result.command_kind,
                result.command_identity,
                result.payload_hash,
                serde_json::to_string(&result.references)?,
                Utc::now().to_rfc3339(),
            ],
        )?;
    }
    Ok(())
}

fn reduce(
    state: &ExecutionProtocolState,
    command: &ExecutionProtocolCommand,
) -> Result<ExecutionTransition, String> {
    match command {
        ExecutionProtocolCommand::RegisterWorkItem(command) => {
            execution_protocol::register_work_item_execution(state, command)
        }
        ExecutionProtocolCommand::AdvanceWorkItemSourceRevision(command) => {
            execution_protocol::advance_work_item_source_revision(state, command)
        }
        ExecutionProtocolCommand::SetWorkItemReadiness(command) => {
            execution_protocol::set_work_item_readiness(state, command)
        }
        ExecutionProtocolCommand::SuspendWorkItemContinuation(command) => {
            execution_protocol::suspend_work_item_continuation(state, command)
        }
        ExecutionProtocolCommand::ResumeWorkItemContinuation(command) => {
            execution_protocol::resume_work_item_continuation(state, command)
        }
        ExecutionProtocolCommand::ReconcileWorkItemContinuationYield(command) => {
            execution_protocol::reconcile_work_item_continuation_yield(state, command)
        }
        ExecutionProtocolCommand::SetWorkItemWaiting(command) => {
            execution_protocol::set_work_item_waiting(state, command)
        }
        ExecutionProtocolCommand::CompleteWorkItem(command) => {
            execution_protocol::complete_work_item_execution(state, command)
        }
        ExecutionProtocolCommand::Admit(command) => {
            execution_protocol::admit_execution(state, command)
        }
        ExecutionProtocolCommand::Settle(command) => {
            execution_protocol::settle_execution(state, command)
        }
        ExecutionProtocolCommand::Interrupt(command) => {
            execution_protocol::interrupt_execution(state, command)
        }
    }
}

fn command_identity(command: &ExecutionProtocolCommand) -> (&'static str, &str) {
    match command {
        ExecutionProtocolCommand::RegisterWorkItem(command) => {
            ("register_work_item_execution", &command.work_item_id)
        }
        ExecutionProtocolCommand::AdvanceWorkItemSourceRevision(command) => {
            ("advance_work_item_source_revision", &command.command_id)
        }
        ExecutionProtocolCommand::SetWorkItemReadiness(command) => {
            ("set_work_item_readiness", &command.command_id)
        }
        ExecutionProtocolCommand::SuspendWorkItemContinuation(command) => {
            ("suspend_work_item_continuation", &command.command_id)
        }
        ExecutionProtocolCommand::ResumeWorkItemContinuation(command) => {
            ("resume_work_item_continuation", &command.command_id)
        }
        ExecutionProtocolCommand::ReconcileWorkItemContinuationYield(command) => (
            "reconcile_work_item_continuation_yield",
            &command.command_id,
        ),
        ExecutionProtocolCommand::SetWorkItemWaiting(command) => {
            ("set_work_item_waiting", &command.command_id)
        }
        ExecutionProtocolCommand::CompleteWorkItem(command) => {
            ("complete_work_item_execution", &command.command_id)
        }
        ExecutionProtocolCommand::Admit(command) => {
            ("admit_execution", &command.attempt.attempt_id)
        }
        ExecutionProtocolCommand::Settle(command) => {
            ("settle_execution", &command.outcome.outcome_id)
        }
        ExecutionProtocolCommand::Interrupt(command) => {
            ("interrupt_execution", &command.outcome_id)
        }
    }
}

fn validate_set_work_item_waiting(
    command: &execution_protocol::SetWorkItemWaiting,
    mutations: &[WorkItemMutation],
    wait_conditions: &[crate::types::WaitConditionRecord],
) -> Result<()> {
    let Some(record) = mutations.iter().find_map(|mutation| match mutation {
        WorkItemMutation::Update { record, .. }
            if record.id == command.work_item_id
                && record.revision == command.record.source_revision =>
        {
            Some(record)
        }
        _ => None,
    }) else {
        bail!("WorkItem waiting transition requires an atomic WorkItem update");
    };
    let execution_protocol::WorkItemExecutionState::Waiting { wait, .. } = &command.record.state
    else {
        bail!("WorkItem waiting transition record is not Waiting");
    };
    if record.agent_id.is_empty()
        || !wait_conditions.iter().any(|condition| {
            condition.id == wait.wait_id
                && condition.agent_id == record.agent_id
                && condition.work_item_id.as_deref() == Some(record.id.as_str())
                && condition.status == crate::types::WaitConditionStatus::Active
        })
    {
        bail!("WorkItem waiting transition does not match its atomic wait condition");
    }
    Ok(())
}

fn validate_complete_work_item(
    command: &execution_protocol::CompleteWorkItemExecution,
    mutations: &[WorkItemMutation],
) -> Result<()> {
    let Some(record) = mutations.iter().find_map(|mutation| match mutation {
        WorkItemMutation::Update { record, .. } if record.id == command.work_item_id => {
            Some(record)
        }
        _ => None,
    }) else {
        bail!("WorkItem completion transition requires an atomic WorkItem update");
    };
    if record.state != crate::types::WorkItemState::Completed
        || record.result_brief_id.as_deref() != Some(command.completion.as_str())
    {
        bail!("WorkItem completion transition does not match its atomic WorkItem update");
    }
    Ok(())
}

fn validate_register_work_item(
    tx: &Transaction<'_>,
    agent_id: &str,
    command: &execution_protocol::RegisterWorkItemExecution,
    mutations: &[WorkItemMutation],
) -> Result<()> {
    let atomic_record = mutations.iter().find_map(|mutation| {
        let record = mutation.record();
        (record.id == command.work_item_id
            && record.revision == command.record.source_revision
            && !record.agent_id.is_empty())
        .then_some(record)
    });
    if atomic_record.is_some_and(|record| record.state == crate::types::WorkItemState::Open) {
        return Ok(());
    }
    let compatible_record = tx
        .query_row(
            "SELECT payload_json
             FROM work_items
             WHERE work_item_id = ?1 AND agent_id = ?2",
            [&command.work_item_id, agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<crate::types::WorkItemRecord>(&payload))
        .transpose()?;
    if !compatible_record.is_some_and(|record| {
        record.state == crate::types::WorkItemState::Open
            && record.revision == command.record.source_revision
    }) {
        bail!("WorkItem registration requires an open WorkItem");
    }
    Ok(())
}

fn validate_work_item_source_revision_advance(
    command: &execution_protocol::AdvanceWorkItemSourceRevision,
    mutations: &[WorkItemMutation],
) -> Result<()> {
    let Some(record) = mutations.iter().find_map(|mutation| match mutation {
        WorkItemMutation::Update { record, .. }
            if record.id == command.work_item_id && record.revision == command.source_revision =>
        {
            Some(record)
        }
        _ => None,
    }) else {
        bail!("WorkItem source revision advance requires an atomic WorkItem update");
    };
    if record.agent_id.is_empty() {
        bail!("WorkItem source revision advance does not match its atomic WorkItem update");
    }
    Ok(())
}

fn validate_set_work_item_readiness(
    command: &execution_protocol::SetWorkItemReadiness,
    mutations: &[WorkItemMutation],
    wait_conditions: &[crate::types::WaitConditionRecord],
) -> Result<()> {
    let mutation = mutations.iter().find_map(|mutation| match mutation {
        WorkItemMutation::Update { record, .. } if record.id == command.work_item_id => {
            Some(record)
        }
        _ => None,
    });
    if let Some(record) = mutation {
        if record.revision != command.record.source_revision || record.agent_id.is_empty() {
            bail!("WorkItem readiness transition does not match its atomic WorkItem update");
        }
    } else if command.record.source_revision != command.expected.source_revision {
        bail!("WorkItem readiness revision advance requires an atomic WorkItem update");
    }
    match &command.record.state {
        WorkItemExecutionState::Paused { .. } => {
            if mutation.is_none_or(|record| record.blocked_by.is_none()) {
                bail!("WorkItem pause transition requires an atomic blocker update");
            }
        }
        WorkItemExecutionState::Runnable { .. } => {
            let clears_blocker = mutation.is_some_and(|record| record.blocked_by.is_none());
            let cancels_wait = wait_conditions.iter().any(|condition| {
                condition.work_item_id.as_deref() == Some(command.work_item_id.as_str())
                    && condition.status == crate::types::WaitConditionStatus::Cancelled
            });
            if !clears_blocker && !cancels_wait {
                bail!("WorkItem runnable transition requires blocker or wait clearance");
            }
        }
        _ => bail!("WorkItem readiness transition must target Runnable or Paused"),
    }
    Ok(())
}

fn validate_resume_work_item_continuation(
    tx: &Transaction<'_>,
    agent_id: &str,
    command: &execution_protocol::ResumeWorkItemContinuation,
    mutations: &[WorkItemMutation],
    wait_conditions: &[crate::types::WaitConditionRecord],
    continuations: &[crate::types::WorkItemContinuationFrame],
) -> Result<()> {
    let frame = tx
        .query_row(
            "SELECT payload_json
             FROM work_item_continuations
             WHERE continuation_id = ?1 AND agent_id = ?2",
            [&command.continuation_id, agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| {
            serde_json::from_str::<crate::types::WorkItemContinuationFrame>(&payload)
                .context("decoding continuation resume frame")
        })
        .transpose()?
        .ok_or_else(|| anyhow!("continuation resume requires an existing frame"))?;
    let resolved = continuations
        .iter()
        .find(|candidate| candidate.id == command.continuation_id)
        .ok_or_else(|| anyhow!("continuation resume requires an atomic frame resolution"))?;
    // A resolved durable frame means its matching canonical resume already committed.
    // Retries are fenced by the atomic batch rather than replayed from Resumed state.
    if frame.suspended_work_item_id != command.work_item_id
        || frame.active_work_item_id != command.active_work_item_id
        || frame.state != crate::types::WorkItemContinuationState::Active
        || resolved.agent_id != agent_id
        || resolved.suspended_work_item_id != command.work_item_id
        || resolved.active_work_item_id != command.active_work_item_id
        || !matches!(
            resolved.state,
            crate::types::WorkItemContinuationState::Resumed
                | crate::types::WorkItemContinuationState::Cancelled
        )
    {
        bail!("continuation resume command does not match its atomic frame resolution");
    }
    let durable_parent = tx
        .query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id = ?1",
            [&command.work_item_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<crate::types::WorkItemRecord>(&payload))
        .transpose()?
        .ok_or_else(|| anyhow!("continuation resume parent WorkItem is missing"))?;
    let parent = mutations
        .iter()
        .find_map(|mutation| match mutation {
            WorkItemMutation::Update { record, .. } if record.id == command.work_item_id => {
                Some(record)
            }
            _ => None,
        })
        .unwrap_or(&durable_parent);
    let mut active_wait_ids = {
        let mut statement = tx.prepare(
            "SELECT wait_condition_id
             FROM wait_conditions
             WHERE agent_id = ?1 AND work_item_id = ?2 AND status = 'active'
             ORDER BY wait_condition_id ASC",
        )?;
        let wait_ids = statement
            .query_map([agent_id, command.work_item_id.as_str()], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<std::collections::BTreeSet<_>, _>>()?;
        wait_ids
    };
    for wait in wait_conditions {
        active_wait_ids.remove(&wait.id);
        if wait.agent_id == agent_id
            && wait.work_item_id.as_deref() == Some(command.work_item_id.as_str())
            && wait.status == crate::types::WaitConditionStatus::Active
        {
            active_wait_ids.insert(wait.id.clone());
        }
    }
    let source = execution_protocol::WorkItemContinuationResumeSource {
        work_item_revision: parent.revision,
        blocked_by: parent.blocked_by.clone(),
        active_wait_ids: active_wait_ids.into_iter().collect(),
    };
    if parent.agent_id != agent_id
        || parent.state != crate::types::WorkItemState::Open
        || source != command.source
        || execution_protocol::continuation_resume_outcome(&command.work_item_id, &source)
            .map_err(anyhow::Error::msg)?
            != command.outcome
    {
        bail!("continuation resume source fence is stale");
    }
    Ok(())
}

fn validate_suspend_work_item_continuation(
    tx: &Transaction<'_>,
    agent_id: &str,
    command: &execution_protocol::SuspendWorkItemContinuation,
    continuations: &[crate::types::WorkItemContinuationFrame],
) -> Result<()> {
    let frame = continuations
        .iter()
        .find(|frame| frame.id == command.continuation_id)
        .ok_or_else(|| anyhow!("continuation suspension requires an atomic continuation frame"))?;
    if frame.agent_id != agent_id
        || frame.suspended_work_item_id != command.work_item_id
        || frame.active_work_item_id != command.active_work_item_id
        || frame.state != crate::types::WorkItemContinuationState::Active
    {
        bail!("continuation suspension command does not match its active frame");
    }
    let parent = tx
        .query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id = ?1",
            [&command.work_item_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<crate::types::WorkItemRecord>(&payload))
        .transpose()?
        .ok_or_else(|| anyhow!("continuation suspension parent WorkItem is missing"))?;
    if parent.agent_id != agent_id
        || parent.state != crate::types::WorkItemState::Open
        || parent.revision != command.expected.source_revision
    {
        bail!("continuation suspension parent WorkItem fence is stale");
    }
    Ok(())
}

fn validate_reconcile_work_item_continuation_yield(
    tx: &Transaction<'_>,
    agent_id: &str,
    command: &execution_protocol::ReconcileWorkItemContinuationYield,
) -> Result<()> {
    let active_frames = {
        let mut statement = tx.prepare(
            "SELECT payload_json
             FROM work_item_continuations
             WHERE agent_id = ?1 AND state = 'active'
             ORDER BY continuation_id ASC",
        )?;
        let records = statement
            .query_map([agent_id], |row| row.get::<_, String>(0))?
            .map(|row| {
                serde_json::from_str::<crate::types::WorkItemContinuationFrame>(&row?)
                    .context("decoding continuation reconciliation frame")
            })
            .collect::<Result<Vec<_>>>()?;
        records
    };
    let frame = active_frames
        .iter()
        .find(|frame| frame.id == command.continuation_id)
        .ok_or_else(|| anyhow!("continuation reconciliation requires an active frame"))?;
    if frame.suspended_work_item_id != command.work_item_id
        || frame.active_work_item_id != command.active_work_item_id
        || frame.updated_at.to_rfc3339() != command.expected_frame_updated_at
    {
        bail!("continuation reconciliation frame fence is stale");
    }
    if active_frames
        .iter()
        .filter(|candidate| {
            candidate.suspended_work_item_id == command.work_item_id
                || candidate.active_work_item_id == command.active_work_item_id
        })
        .count()
        != 1
    {
        bail!("continuation reconciliation has a competing active frame");
    }
    let mut cursor = command.active_work_item_id.as_str();
    let mut visited = std::collections::BTreeSet::new();
    while let Some(next) = active_frames
        .iter()
        .find(|candidate| candidate.suspended_work_item_id == cursor)
    {
        if !visited.insert(next.id.as_str()) || next.active_work_item_id == command.work_item_id {
            bail!("continuation reconciliation active topology contains a cycle");
        }
        cursor = next.active_work_item_id.as_str();
    }
    let load_work_item = |work_item_id: &str| -> Result<crate::types::WorkItemRecord> {
        tx.query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id = ?1",
            [work_item_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str(&payload).context("decoding reconciliation WorkItem"))
        .transpose()?
        .ok_or_else(|| anyhow!("continuation reconciliation WorkItem is missing"))
    };
    let parent = load_work_item(&command.work_item_id)?;
    let active = load_work_item(&command.active_work_item_id)?;
    let stale = load_work_item(&command.stale_active_work_item_id)?;
    if parent.agent_id != agent_id
        || parent.state != crate::types::WorkItemState::Open
        || parent.revision != command.expected.source_revision
        || active.agent_id != agent_id
        || active.state != crate::types::WorkItemState::Open
        || active.revision != command.active_work_item_source_revision
        || stale.agent_id != agent_id
        || stale.state != crate::types::WorkItemState::Completed
        || stale.revision != command.stale_active_work_item_source_revision
    {
        bail!("continuation reconciliation WorkItem source fence is stale");
    }
    Ok(())
}

fn partition_exists_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<bool> {
    tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM execution_protocol_partitions WHERE agent_id = ?1
         )",
        [agent_id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

pub(crate) fn authority_fences_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<ExecutionAuthorityFences> {
    let (status, control_revision) = tx
        .query_row(
            "SELECT status, control_revision
             FROM agent_states
             WHERE agent_id = ?1",
            [agent_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("execution admission requires durable agent control state"))?;
    let status: AgentStatus =
        serde_json::from_str(&format!("\"{status}\"")).context("decoding agent control status")?;
    if status == AgentStatus::Stopped {
        bail!("execution admission rejected because agent control state is stopped");
    }
    let agent_control_revision =
        u64::try_from(control_revision).context("agent control revision is negative")?;
    if agent_control_revision == 0 {
        bail!("agent control revision must be nonzero");
    }

    let identity_payload = tx
        .query_row(
            "SELECT payload_json
             FROM agent_identities
             WHERE agent_id = ?1",
            [agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("execution admission requires durable host identity"))?;
    let identity: AgentIdentityRecord =
        serde_json::from_str(&identity_payload).context("decoding host identity")?;
    if identity.status != AgentRegistryStatus::Active {
        bail!("execution admission rejected because host identity is not active");
    }
    let host_registry_revision = identity
        .revision
        .checked_add(1)
        .ok_or_else(|| anyhow!("host identity revision overflow"))?;

    Ok(ExecutionAuthorityFences {
        agent_control_revision,
        host_registry_revision,
    })
}

fn validate_admission_authority_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    admitted: &execution_protocol::AdmittedFences,
) -> Result<()> {
    let current = authority_fences_tx(tx, agent_id)?;
    if admitted.agent_control_revision != current.agent_control_revision {
        bail!(
            "execution admission agent control fence is stale: admitted {}, current {}",
            admitted.agent_control_revision,
            current.agent_control_revision
        );
    }
    if admitted.host_registry_revision != current.host_registry_revision {
        bail!(
            "execution admission host registry fence is stale: admitted {}, current {}",
            admitted.host_registry_revision,
            current.host_registry_revision
        );
    }
    Ok(())
}

impl RuntimeTransitionRepository<'_> {
    pub(crate) fn load_execution_authority_fences(
        &self,
        agent_id: &str,
    ) -> Result<ExecutionAuthorityFences> {
        let connection = self.db.connection()?;
        let transaction =
            Transaction::new_unchecked(&connection, rusqlite::TransactionBehavior::Deferred)?;
        let fences = authority_fences_tx(&transaction, agent_id)?;
        transaction.commit()?;
        Ok(fences)
    }

    pub(crate) fn load_execution_protocol_state_if_initialized(
        &self,
        agent_id: &str,
    ) -> Result<Option<ExecutionProtocolState>> {
        let connection = self.db.connection()?;
        let transaction =
            Transaction::new_unchecked(&connection, rusqlite::TransactionBehavior::Deferred)?;
        if !partition_exists_tx(&transaction, agent_id)? {
            transaction.commit()?;
            return Ok(None);
        }
        let state = load_state_tx(&transaction, agent_id)?;
        transaction.commit()?;
        Ok(Some(state))
    }

    pub(crate) fn commit_execution_protocol_recovery_plan(
        &self,
        agent_id: &str,
        commands: &[ExecutionProtocolCommand],
        audit_events: &[crate::types::AuditEvent],
    ) -> Result<ExecutionProtocolRecoveryCommitOutcome> {
        if commands.is_empty() {
            return Ok(ExecutionProtocolRecoveryCommitOutcome::Applied(false));
        }
        if commands.iter().any(|command| {
            !matches!(
                command,
                ExecutionProtocolCommand::ReconcileWorkItemContinuationYield(_)
            )
        }) {
            bail!("execution protocol recovery plan contains a non-recovery command");
        }
        self.db.transaction(|tx| {
            let prepared =
                match validate_execution_commands_tx(tx, agent_id, None, commands, &[], &[], &[]) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        return Ok(ExecutionProtocolRecoveryCommitOutcome::Rejected {
                            reason: error.to_string(),
                        });
                    }
                };
            let changed = prepared
                .as_ref()
                .is_some_and(PreparedExecutionProtocolCommands::has_writes);
            persist_execution_commands_tx(tx, prepared)?;
            if changed {
                for event in audit_events {
                    crate::runtime_db::evidence::append_audit_event_tx(tx, Some(agent_id), event)?;
                }
            }
            Ok(ExecutionProtocolRecoveryCommitOutcome::Applied(changed))
        })
    }
}

fn stored_command_hash_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    command_kind: &str,
    command_identity: &str,
) -> Result<Option<String>> {
    tx.query_row(
        "SELECT payload_hash
         FROM execution_protocol_command_results
         WHERE agent_id = ?1
           AND command_kind = ?2
           AND command_identity = ?3",
        params![agent_id, command_kind, command_identity],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn persist_state_tx(tx: &Transaction<'_>, state: &ExecutionProtocolState) -> Result<()> {
    execution_protocol::assert_invariants(state)
        .map_err(|error| anyhow!("invalid execution protocol state: {error}"))?;
    tx.execute(
        "INSERT INTO execution_protocol_partitions (agent_id, updated_at)
         VALUES (?1, ?2)
         ON CONFLICT(agent_id) DO UPDATE SET updated_at = excluded.updated_at",
        params![state.agent_id, Utc::now().to_rfc3339()],
    )?;
    for (work_item_id, work_record) in &state.work_items {
        tx.execute(
            "INSERT INTO execution_protocol_work_items (
               agent_id, work_item_id, source_revision, generation,
               lifecycle_state, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(agent_id, work_item_id) DO UPDATE SET
               source_revision = excluded.source_revision,
               generation = excluded.generation,
               lifecycle_state = excluded.lifecycle_state,
               payload_json = excluded.payload_json",
            params![
                state.agent_id,
                work_item_id,
                to_i64(work_record.source_revision)?,
                to_i64(work_record.generation())?,
                work_item_state_token(&work_record.state),
                serde_json::to_string(work_record)?,
            ],
        )?;
    }
    for attempt in state.attempts.values() {
        tx.execute(
            "INSERT INTO execution_protocol_attempts (
               agent_id, attempt_id, lifecycle_state,
               source_identity_json, source_generation, recovery_of_attempt_id,
               terminal_outcome_id, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(agent_id, attempt_id) DO UPDATE SET
               lifecycle_state = excluded.lifecycle_state,
               terminal_outcome_id = excluded.terminal_outcome_id,
               payload_json = excluded.payload_json",
            params![
                state.agent_id,
                attempt.attempt_id,
                enum_token(&attempt.state)?,
                serde_json::to_string(&attempt.source.identity)?,
                to_i64(attempt.source.generation)?,
                attempt.recovery_of_attempt_id,
                attempt.terminal_outcome_id,
                serde_json::to_string(attempt)?,
            ],
        )?;
    }
    for outcome in state.outcomes.values() {
        let payload = serde_json::to_string(outcome)?;
        let existing = tx
            .query_row(
                "SELECT payload_json
                 FROM execution_protocol_outcomes
                 WHERE agent_id = ?1 AND outcome_id = ?2",
                params![state.agent_id, outcome.outcome_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if !stored_outcome_matches(&existing, outcome)? {
                bail!(
                    "execution outcome identity conflict for {}",
                    outcome.outcome_id
                );
            }
            continue;
        }
        tx.execute(
            "INSERT INTO execution_protocol_outcomes (
               agent_id, outcome_id, attempt_id, payload_json
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                state.agent_id,
                outcome.outcome_id,
                outcome.attempt_id,
                payload,
            ],
        )?;
    }
    Ok(())
}

fn stored_outcome_matches(existing: &str, outcome: &ExecutionOutcomeRecord) -> Result<bool> {
    let existing: ExecutionOutcomeRecord = serde_json::from_str(existing)
        .with_context(|| format!("decoding stored execution outcome {}", outcome.outcome_id))?;
    Ok(existing == *outcome)
}

pub(super) fn load_state_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<ExecutionProtocolState> {
    if !partition_exists_tx(tx, agent_id)? {
        bail!("execution protocol partition for agent {agent_id} is not initialized");
    }
    let state = ExecutionProtocolState {
        agent_id: agent_id.to_owned(),
        attempts: load_payload_map(
            tx,
            "SELECT attempt_id, payload_json
             FROM execution_protocol_attempts WHERE agent_id = ?1",
            agent_id,
        )?,
        work_items: load_payload_map(
            tx,
            "SELECT work_item_id, payload_json
             FROM execution_protocol_work_items WHERE agent_id = ?1",
            agent_id,
        )?,
        outcomes: load_payload_map(
            tx,
            "SELECT outcome_id, payload_json
             FROM execution_protocol_outcomes WHERE agent_id = ?1",
            agent_id,
        )?,
    };
    execution_protocol::assert_invariants(&state)
        .map_err(|error| anyhow!("stored execution protocol state is invalid: {error}"))?;
    Ok(state)
}

fn load_payload_map<T: DeserializeOwned>(
    tx: &Transaction<'_>,
    sql: &str,
    agent_id: &str,
) -> Result<BTreeMap<String, T>> {
    let mut statement = tx.prepare(sql)?;
    let records = statement
        .query_map([agent_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .map(|row| {
            let (id, payload) = row?;
            let value = serde_json::from_str(&payload)
                .with_context(|| format!("decoding execution protocol record {id}"))?;
            Ok((id, value))
        })
        .collect();
    records
}

fn enum_token<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("execution protocol enum did not serialize to a string"))
}

fn work_item_state_token(state: &WorkItemExecutionState) -> &'static str {
    match state {
        WorkItemExecutionState::Runnable { .. } => "runnable",
        WorkItemExecutionState::InFlight { .. } => "in_flight",
        WorkItemExecutionState::Waiting { .. } => "waiting",
        WorkItemExecutionState::Paused { .. } => "paused",
        WorkItemExecutionState::NeedsRepair { .. } => "needs_repair",
        WorkItemExecutionState::Terminal { .. } => "terminal",
    }
}

fn to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).context("execution protocol generation exceeds SQLite INTEGER")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::execution_protocol::{ExecutionOutcome, WaitReference, WorkItemOutcome};

    fn wait_outcome(wait_id: &str) -> ExecutionOutcomeRecord {
        ExecutionOutcomeRecord {
            outcome_id: "outcome-1".into(),
            attempt_id: "attempt-1".into(),
            outcome: ExecutionOutcome::WorkItem(WorkItemOutcome::Wait {
                wait: WaitReference {
                    wait_id: wait_id.into(),
                },
            }),
            created_at: "2026-08-05T00:00:00Z".into(),
        }
    }

    #[test]
    fn stored_outcome_identity_ignores_removed_wait_generation_field() {
        let existing = serde_json::json!({
            "outcome_id": "outcome-1",
            "attempt_id": "attempt-1",
            "outcome": {
                "owner": "work_item",
                "outcome": {
                    "kind": "wait",
                    "wait": {
                        "wait_id": "wait-1",
                        "generation": 7
                    }
                }
            },
            "created_at": "2026-08-05T00:00:00Z"
        });
        assert!(stored_outcome_matches(&existing.to_string(), &wait_outcome("wait-1")).unwrap());
        assert!(!stored_outcome_matches(&existing.to_string(), &wait_outcome("wait-2")).unwrap());
    }
}
