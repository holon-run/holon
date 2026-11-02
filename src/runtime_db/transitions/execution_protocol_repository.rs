use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::domain::execution_protocol::{
    self, ExecutionAttempt, ExecutionOutcomeRecord, ExecutionProtocolCommand,
    ExecutionProtocolState, ExecutionTransition, WorkItemExecutionState,
};
use crate::{
    runtime_db::transitions::{
        ExecutionAuthorityFences, RuntimeTransitionRepository, WorkItemMutation,
    },
    types::{AgentIdentityRecord, AgentRegistryStatus, AgentStatus},
};

pub(crate) struct PreparedExecutionProtocolCommands {
    state: ExecutionProtocolState,
    touched_keys: BTreeSet<crate::runtime::resident_input_evidence::InputEvidenceKey>,
    initialized_partition: bool,
    results: Vec<PreparedCommandResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum ExecutionProtocolRecoveryCommitOutcome {
    Applied(bool),
    Rejected { reason: String },
}

impl PreparedExecutionProtocolCommands {
    pub(crate) fn input_keys(
        &self,
    ) -> Vec<crate::runtime::resident_input_evidence::InputEvidenceKey> {
        self.touched_keys.iter().cloned().collect()
    }

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

pub(crate) fn validate_execution_commands_tx(
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
    let mut touched_keys = BTreeSet::new();

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
            validate_wait_ingress_tx(tx, agent_id, &command.attempt)?;
            if let Some(projection) = command.attempt.input_projection.as_ref() {
                let message_id = command
                    .attempt
                    .source_message_id
                    .as_deref()
                    .ok_or_else(|| anyhow!("source projection requires a source message"))?;
                let payload: String = tx.query_row(
                    "SELECT payload_json FROM messages WHERE agent_id = ?1 AND message_id = ?2",
                    params![agent_id, message_id],
                    |row| row.get(0),
                )?;
                let message: crate::types::MessageEnvelope = serde_json::from_str(&payload)?;
                crate::runtime::input_consumption::projection::validate_projection(
                    &message, projection,
                )?;
            }
        }
        if let ExecutionProtocolCommand::RegisterWorkItem(command) = command {
            validate_register_work_item(tx, agent_id, command, work_item_mutations)?;
        }
        if let ExecutionProtocolCommand::AdvanceWorkItemSourceRevision(command) = command {
            validate_work_item_source_revision_advance(tx, agent_id, command, work_item_mutations)?;
        }
        if let ExecutionProtocolCommand::ReconcileWorkItemSourceRevision(command) = command {
            validate_work_item_source_revision_reconcile(
                tx,
                agent_id,
                command,
                work_item_mutations,
            )?;
        }
        if let ExecutionProtocolCommand::RecoverInterruptedTaskResultClaim(command) = command {
            validate_recover_interrupted_task_result_claim(tx, agent_id, command)?;
        }
        if let ExecutionProtocolCommand::RecoverUnadvancedTaskResultClaim(command) = command {
            validate_recover_unadvanced_task_result_claim(tx, agent_id, command)?;
        }
        if let ExecutionProtocolCommand::SetWorkItemReadiness(command) = command {
            validate_set_work_item_readiness(command, work_item_mutations, wait_conditions)?;
        }
        if let ExecutionProtocolCommand::RecheckContextBudgetStop(command) = command {
            validate_context_budget_stop_recheck(tx, agent_id, command)?;
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
        use crate::runtime::resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key,
        };
        for (id, record) in &transition.state.attempts {
            if state.attempts.get(id) != Some(record) {
                touched_keys.insert(Key::new(Family::Attempt, id));
            }
        }
        for (id, record) in &transition.state.outcomes {
            if state.outcomes.get(id) != Some(record) {
                touched_keys.insert(Key::new(Family::Outcome, id));
            }
        }
        for (id, record) in &transition.state.work_items {
            if state.work_items.get(id) != Some(record) {
                touched_keys.insert(Key::new(Family::WorkExecution, id));
            }
        }
        state = transition.state;
        results.push(PreparedCommandResult {
            command_kind,
            command_identity: command_identity.to_owned(),
            payload_hash,
            references: transition.references,
        });
    }
    // #3816 invariant: an Open WorkItem mutation and its execution
    // registration must settle on the same revision inside this
    // transaction. Synchronize inserts the auto-advance for untouched
    // registrations and the per-command validators keep constructed
    // commands atomic; this final assertion is the uniform fail-closed net
    // that turns any residual writer into a same-transaction rejection
    // instead of persisting stranded revisions.
    let mut final_open_mutations =
        std::collections::BTreeMap::<&str, &crate::runtime_db::transitions::WorkItemMutation>::new(
        );
    for mutation in work_item_mutations {
        if mutation.record().state == crate::types::WorkItemState::Open {
            final_open_mutations.insert(mutation.record().id.as_str(), mutation);
        }
    }
    for (work_item_id, mutation) in final_open_mutations {
        if let Some(execution) = state.work_items.get(work_item_id) {
            if execution.source_revision != mutation.record().revision {
                bail!(
                    "WorkItem execution source revision {} does not match committed revision {} \
                     for {}",
                    execution.source_revision,
                    mutation.record().revision,
                    work_item_id
                );
            }
        }
    }

    Ok(Some(PreparedExecutionProtocolCommands {
        state,
        touched_keys,
        initialized_partition,
        results,
    }))
}

pub(super) fn validate_model_queue_claim_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    message_id: &str,
    prepared: Option<&PreparedExecutionProtocolCommands>,
    mutations: &[WorkItemMutation],
) -> Result<()> {
    let Some(prepared) = prepared else {
        return Ok(());
    };
    // Reducer-only claims may admit and settle in the same transaction.
    let Some(attempt) = prepared.state.open_attempt().filter(|attempt| {
        attempt.source_message_id.as_deref() == Some(message_id)
            && prepared.results.iter().any(|result| {
                result.command_kind == "admit_execution"
                    && result.command_identity == attempt.attempt_id
            })
    }) else {
        return Ok(());
    };
    // A bootstrap can already include this transaction's final owner revision;
    // only a persisted partition is evidence of the pre-claim snapshot.
    let initial_work_items = if prepared.initialized_partition {
        BTreeMap::new()
    } else {
        load_state_tx(tx, agent_id)?.work_items
    };
    validate_work_item_admission_snapshot_tx(
        tx,
        agent_id,
        &initial_work_items,
        &prepared.state,
        attempt,
        mutations,
    )
}

fn validate_work_item_admission_snapshot_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    initial_work_items: &BTreeMap<String, execution_protocol::WorkItemExecutionRecord>,
    final_state: &ExecutionProtocolState,
    attempt: &ExecutionAttempt,
    mutations: &[WorkItemMutation],
) -> Result<()> {
    let execution_protocol::ExecutionBinding::WorkItem { work_item_id } = &attempt.binding else {
        return Ok(());
    };
    let durable = tx
        .query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id = ?1",
            [work_item_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<crate::types::WorkItemRecord>(&payload))
        .transpose()?;
    // Existing drift in either direction is not evidence of a legal writer.
    // Only this transaction's validated mutations may advance the snapshot.
    if let Some(initial) = initial_work_items.get(work_item_id) {
        if let Some(durable_record) = durable.as_ref() {
            if initial.source_revision != durable_record.revision {
                bail!(
                    "WorkItem admission requires an aligned initial snapshot: \
                     work_item={work_item_id}, source={}, durable={}",
                    initial.source_revision,
                    durable_record.revision
                );
            }
        }
    }
    let final_work_item = mutations
        .iter()
        .rev()
        .map(WorkItemMutation::record)
        .find(|record| record.id == *work_item_id)
        .or(durable.as_ref())
        .ok_or_else(|| anyhow!("WorkItem admission requires a durable owner"))?;
    if final_work_item.id != *work_item_id
        || final_work_item.agent_id != agent_id
        || final_work_item.state != crate::types::WorkItemState::Open
        || attempt.agent_id != agent_id
    {
        bail!("WorkItem admission requires its open transaction owner");
    }
    let execution = final_state
        .work_items
        .get(work_item_id)
        .ok_or_else(|| anyhow!("WorkItem admission requires canonical owner authority"))?;
    if final_work_item.revision == 0
        || execution.source_revision != final_work_item.revision
        || attempt.admitted_fences.work_item_source_revision != Some(final_work_item.revision)
    {
        bail!(
            "WorkItem admission final revision mismatch: work_item={work_item_id}, \
             durable={}, source={}, admitted={:?}",
            final_work_item.revision,
            execution.source_revision,
            attempt.admitted_fences.work_item_source_revision
        );
    }
    if !matches!(
        &execution.state,
        WorkItemExecutionState::InFlight { generation, attempt_id }
            if attempt_id == &attempt.attempt_id
                && Some(*generation) == attempt.admitted_fences.work_item_generation
    ) {
        bail!("WorkItem admission does not own its final execution generation");
    }
    Ok(())
}

pub(super) fn synchronize_work_item_revisions_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    transition: &crate::runtime_db::transitions::ExecutionProtocolTransition,
    work_item_mutations: &[WorkItemMutation],
) -> Result<crate::runtime_db::transitions::ExecutionProtocolTransition> {
    synchronize_work_item_revisions_opt(tx, agent_id, transition, work_item_mutations, true)
}

/// `register_missing` controls whether an unregistered open WorkItem gets a
/// derived `RegisterWorkItem` command. Only the wait registration channel
/// owns derived registration; the wait transition and completion channels
/// synchronize advances (#3816) without inventing registrations that the
/// canonical recovery paths are responsible for.
pub(super) fn synchronize_work_item_revisions_opt(
    tx: &Transaction<'_>,
    agent_id: &str,
    transition: &crate::runtime_db::transitions::ExecutionProtocolTransition,
    work_item_mutations: &[WorkItemMutation],
    register_missing: bool,
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
            if register_missing && !command_targets_work_item {
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
        let durable_revision: u64 = tx.query_row(
            "SELECT revision FROM work_items WHERE work_item_id = ?1 AND agent_id = ?2",
            [&record.id, agent_id],
            |row| row.get(0),
        )?;
        if authoritative.source_revision != durable_revision {
            bail!(
                "WorkItem execution source revision {} does not match durable revision {}",
                authoritative.source_revision,
                durable_revision
            );
        }
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

pub(crate) fn persist_execution_commands_tx(
    tx: &Transaction<'_>,
    prepared: Option<PreparedExecutionProtocolCommands>,
) -> Result<()> {
    let Some(prepared) = prepared else {
        return Ok(());
    };
    if prepared.initialized_partition {
        persist_state_tx(tx, &prepared.state)?;
    } else {
        // These keys come from the actual before/reducer-state diff above,
        // rather than from command arguments. Unchanged legacy bytes stay put.
        persist_state_inner_tx(tx, &prepared.state, None, Some(&prepared.touched_keys))?;
    }
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

/// Returns the canonical aggregate from the same transaction after persistence.
/// The prepared reducer state is an input, not proof of the rows SQL retained.
pub(super) fn persist_execution_commands_with_facts_tx(
    tx: &Transaction<'_>,
    prepared: Option<PreparedExecutionProtocolCommands>,
) -> Result<Option<ExecutionProtocolState>> {
    let agent_id = prepared
        .as_ref()
        .map(|prepared| prepared.state.agent_id.clone());
    persist_execution_commands_tx(tx, prepared)?;
    agent_id
        .map(|agent_id| load_state_tx(tx, &agent_id))
        .transpose()
}

pub(crate) fn persist_execution_commands_with_input_facts_tx(
    tx: &Transaction<'_>,
    prepared: Option<PreparedExecutionProtocolCommands>,
    scope: Option<&crate::runtime::resident_input_evidence::InputEvidenceScope>,
) -> Result<(
    Option<ExecutionProtocolState>,
    crate::runtime::resident_input_evidence::CommittedInputEvidenceDelta,
)> {
    persist_execution_commands_with_cleanup_input_facts_tx(tx, prepared, scope, None)
}

pub(super) fn persist_execution_commands_with_cleanup_input_facts_tx(
    tx: &Transaction<'_>,
    prepared: Option<PreparedExecutionProtocolCommands>,
    scope: Option<&crate::runtime::resident_input_evidence::InputEvidenceScope>,
    cleanup: Option<&crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof>,
) -> Result<(
    Option<ExecutionProtocolState>,
    crate::runtime::resident_input_evidence::CommittedInputEvidenceDelta,
)> {
    let (state, facts) = persist_execution_commands_with_touched_facts_tx(tx, prepared)?;
    let Some(scope) = scope else {
        return Ok((state, Default::default()));
    };
    if let Some(proof) = cleanup {
        proof.validate_scope_tx(tx, scope)?;
    } else if let Some(actual) = &state {
        super::super::input_evidence_writes::validate_input_evidence_scope_tx(
            tx,
            scope,
            &actual.agent_id,
        )?;
    }
    let delta = if let Some(proof) = cleanup {
        crate::runtime_db::input_evidence_snapshot::register_committed_cleanup_input_evidence_tx(
            tx, scope, &facts, proof,
        )?
    } else {
        crate::runtime_db::input_evidence_snapshot::register_committed_input_evidence_tx(
            tx, scope, &facts,
        )?
    };
    Ok((state, delta))
}

pub(crate) fn persist_execution_commands_with_touched_facts_tx(
    tx: &Transaction<'_>,
    prepared: Option<PreparedExecutionProtocolCommands>,
) -> Result<(
    Option<ExecutionProtocolState>,
    Vec<crate::runtime_db::input_evidence_snapshot::CanonicalInputEvidence>,
)> {
    use crate::runtime::resident_input_evidence::InputEvidenceFamily as Family;
    use crate::runtime_db::input_evidence_snapshot::CanonicalInputEvidence as Fact;
    let touched = prepared
        .as_ref()
        .map(|prepared| prepared.touched_keys.clone())
        .unwrap_or_default();
    let state = persist_execution_commands_with_facts_tx(tx, prepared)?;
    let Some(actual) = state.as_ref() else {
        return Ok((state, Vec::new()));
    };
    let mut facts = Vec::new();
    for key in touched {
        facts.push(match key.family {
            Family::Attempt => Fact::Attempt(
                actual
                    .attempts
                    .get(&key.id)
                    .context("committed touched attempt disappeared")?
                    .clone(),
            ),
            Family::Outcome => Fact::Outcome(
                actual
                    .outcomes
                    .get(&key.id)
                    .context("committed touched outcome disappeared")?
                    .clone(),
            ),
            Family::WorkExecution => Fact::WorkExecution {
                work_item_id: key.id.clone(),
                record: actual
                    .work_items
                    .get(&key.id)
                    .context("committed touched work execution disappeared")?
                    .clone(),
            },
            _ => bail!("non-protocol touched execution key"),
        });
    }
    Ok((state, facts))
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
        ExecutionProtocolCommand::ReconcileWorkItemSourceRevision(command) => {
            execution_protocol::reconcile_work_item_source_revision(state, command)
        }
        ExecutionProtocolCommand::SetWorkItemReadiness(command) => {
            execution_protocol::set_work_item_readiness(state, command)
        }
        ExecutionProtocolCommand::RecheckContextBudgetStop(command) => {
            execution_protocol::recheck_context_budget_stop(state, command)
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
        ExecutionProtocolCommand::FinalizeInterrupted(command) => {
            execution_protocol::finalize_interrupted_execution(state, command)
        }
        ExecutionProtocolCommand::RecoverInterruptedTaskResultClaim(command) => {
            execution_protocol::recover_interrupted_task_result_claim(state, command)
        }
        ExecutionProtocolCommand::RecoverUnadvancedTaskResultClaim(command) => {
            execution_protocol::recover_unadvanced_task_result_claim(state, command)
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
        ExecutionProtocolCommand::ReconcileWorkItemSourceRevision(command) => {
            ("reconcile_work_item_source_revision", &command.command_id)
        }
        ExecutionProtocolCommand::SetWorkItemReadiness(command) => {
            ("set_work_item_readiness", &command.command_id)
        }
        ExecutionProtocolCommand::RecheckContextBudgetStop(command) => {
            ("recheck_context_budget_stop", &command.command_id)
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
        ExecutionProtocolCommand::FinalizeInterrupted(command) => {
            ("finalize_interrupted_execution", &command.command_id)
        }
        ExecutionProtocolCommand::RecoverInterruptedTaskResultClaim(command) => {
            ("recover_interrupted_task_result_claim", &command.command_id)
        }
        ExecutionProtocolCommand::RecoverUnadvancedTaskResultClaim(command) => {
            ("recover_unadvanced_task_result_claim", &command.command_id)
        }
    }
}

fn validate_recover_interrupted_task_result_claim(
    tx: &Transaction<'_>,
    agent_id: &str,
    command: &execution_protocol::RecoverInterruptedTaskResultClaim,
) -> Result<()> {
    let work_item = tx
        .query_row(
            "SELECT payload_json
             FROM work_items
             WHERE work_item_id = ?1 AND agent_id = ?2",
            [&command.work_item_id, agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<crate::types::WorkItemRecord>(&payload))
        .transpose()?
        .ok_or_else(|| anyhow!("TaskResult claim recovery WorkItem is missing"))?;
    if work_item.state != crate::types::WorkItemState::Open
        || work_item.revision != command.source_revision
    {
        bail!("TaskResult claim recovery durable WorkItem fence is stale");
    }
    let task = tx
        .query_row(
            "SELECT payload_json FROM tasks WHERE task_id = ?1",
            [&command.task_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<crate::types::TaskRecord>(&payload))
        .transpose()?
        .ok_or_else(|| anyhow!("TaskResult claim recovery task is missing"))?;
    if task.agent_id != agent_id
        || task.work_item_id.as_deref() != Some(command.work_item_id.as_str())
        || task.parent_message_id.as_deref() != Some(command.result_message_id.as_str())
        || !matches!(
            task.status,
            crate::types::TaskStatus::Completed
                | crate::types::TaskStatus::Failed
                | crate::types::TaskStatus::Cancelled
                | crate::types::TaskStatus::Interrupted
        )
        || task.rejoin_fence().as_ref() != Ok(&command.rejoin)
    {
        bail!("TaskResult claim recovery durable task fence is stale");
    }
    let message = tx
        .query_row(
            "SELECT payload_json
             FROM messages
             WHERE agent_id = ?1 AND message_id = ?2",
            [agent_id, command.result_message_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<crate::types::MessageEnvelope>(&payload))
        .transpose()?
        .ok_or_else(|| anyhow!("TaskResult claim recovery message is missing"))?;
    if message.kind != crate::types::MessageKind::TaskResult
        || message.authority_class != crate::types::AuthorityClass::RuntimeInstruction
        || message.admission_context != Some(crate::types::AdmissionContext::RuntimeOwned)
        || message.delivery_surface != Some(crate::types::MessageDeliverySurface::TaskRejoin)
        || message.task_id.as_deref() != Some(command.task_id.as_str())
        || message.work_item_id.as_deref() != Some(command.work_item_id.as_str())
        || !matches!(
            &message.origin,
            crate::types::MessageOrigin::Task { task_id } if task_id == &command.task_id
        )
    {
        bail!("TaskResult claim recovery message provenance is invalid");
    }
    let matching_waits = {
        let mut statement = tx.prepare(
            "SELECT payload_json
             FROM wait_conditions
             WHERE agent_id = ?1
               AND work_item_id = ?2
               AND status = 'resolved'",
        )?;
        let count = statement
            .query_map([agent_id, command.work_item_id.as_str()], |row| {
                row.get::<_, String>(0)
            })?
            .map(|row| {
                Ok(serde_json::from_str::<crate::types::WaitConditionRecord>(
                    &row?,
                )?)
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|wait| {
                wait.id == command.wait_id
                    && wait.kind == crate::types::WaitConditionKind::Task
                    && wait.trigger_message_id() == Some(command.result_message_id.as_str())
                    && wait.wake_sources.iter().any(|source| {
                        matches!(
                            source,
                            crate::types::WakeSource::TaskResult { task_id }
                                if task_id == &command.task_id
                        )
                    })
            })
            .count();
        count
    };
    if matching_waits != 1 {
        bail!("TaskResult claim recovery requires one exact resolved task wait");
    }
    Ok(())
}

fn validate_recover_unadvanced_task_result_claim(
    tx: &Transaction<'_>,
    agent_id: &str,
    command: &execution_protocol::RecoverUnadvancedTaskResultClaim,
) -> Result<()> {
    // This path intentionally synthesizes source_revision from the admitted
    // revision because the durable WorkItem has not advanced. The shared
    // repository validator may check equality, but must not require the strict
    // monotonicity used by the interrupted-claim domain transition.
    validate_recover_interrupted_task_result_claim(
        tx,
        agent_id,
        &execution_protocol::RecoverInterruptedTaskResultClaim {
            command_id: command.command_id.clone(),
            attempt_id: command.attempt_id.clone(),
            outcome_id: command.outcome_id.clone(),
            work_item_id: command.work_item_id.clone(),
            task_id: command.task_id.clone(),
            result_message_id: command.result_message_id.clone(),
            wait_id: command.wait_id.clone(),
            rejoin: command.rejoin.clone(),
            expected_source_revision: command.expected_source_revision,
            source_revision: command.expected_source_revision,
            interrupted_at: command.interrupted_at.clone(),
        },
    )
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
                && matches!(
                    condition.status,
                    crate::types::WaitConditionStatus::Active
                        // #3463: the late terminal fast path registers the
                        // exact task-result wake already Triggered by the
                        // re-admitted result message; it is the same durable
                        // wait promise as an Active registration.
                        | crate::types::WaitConditionStatus::Triggered
                )
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
    tx: &Transaction<'_>,
    agent_id: &str,
    command: &execution_protocol::AdvanceWorkItemSourceRevision,
    mutations: &[WorkItemMutation],
) -> Result<()> {
    let atomic_record = mutations.iter().find_map(|mutation| match mutation {
        WorkItemMutation::Update {
            record,
            expected_revision,
        } if record.id == command.work_item_id
            && record.agent_id == agent_id
            && *expected_revision == command.expected_source_revision
            && record.revision == command.source_revision =>
        {
            Some(record)
        }
        _ => None,
    });
    let durable = tx
        .query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id = ?1 AND agent_id = ?2",
            [command.work_item_id.as_str(), agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<crate::types::WorkItemRecord>(&payload))
        .transpose()?;
    if atomic_record.is_some()
        && durable
            .as_ref()
            .is_some_and(|record| record.revision == command.expected_source_revision)
    {
        return Ok(());
    }
    bail!(
        "WorkItem source revision advance requires an atomic WorkItem update: \
         work_item={}, expected={}, next={}, durable={:?}, atomic_update={}",
        command.work_item_id,
        command.expected_source_revision,
        command.source_revision,
        durable.as_ref().map(|record| record.revision),
        atomic_record.is_some()
    )
}

fn validate_work_item_source_revision_reconcile(
    tx: &Transaction<'_>,
    agent_id: &str,
    command: &execution_protocol::ReconcileWorkItemSourceRevision,
    mutations: &[WorkItemMutation],
) -> Result<()> {
    anyhow::ensure!(
        mutations
            .iter()
            .all(|mutation| mutation.record().id != command.work_item_id),
        "WorkItem source revision reconcile is a repair and must not carry a durable mutation"
    );
    let durable = tx
        .query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id = ?1 AND agent_id = ?2",
            [command.work_item_id.as_str(), agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<crate::types::WorkItemRecord>(&payload))
        .transpose()?
        .ok_or_else(|| anyhow!("WorkItem source revision reconcile requires a durable row"))?;
    anyhow::ensure!(
        durable.state == crate::types::WorkItemState::Open,
        "WorkItem source revision reconcile requires an open WorkItem"
    );
    anyhow::ensure!(
        durable.revision == command.source_revision,
        "WorkItem source revision reconcile target {} does not match durable revision {}",
        command.source_revision,
        durable.revision
    );
    let missing = missing_revision_attribution_tx(
        tx,
        agent_id,
        &command.work_item_id,
        command.expected_source_revision + 1,
        command.source_revision,
    )?;
    anyhow::ensure!(
        missing.is_empty(),
        "WorkItem source revision reconcile lacks revision attribution evidence for {}",
        missing
            .iter()
            .map(|revision| revision.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    Ok(())
}

/// Revisions in `[from, to]` without a `work_item_written` attribution event.
/// Complete attribution is the precondition for adopting drifted durable
/// revisions back into the execution registration (#3816).
pub(crate) fn missing_revision_attribution_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    work_item_id: &str,
    from: u64,
    to: u64,
) -> Result<Vec<u64>> {
    let mut statement = tx.prepare(
        "SELECT data_json FROM audit_events
         WHERE agent_id = ?1 AND kind = 'work_item_written'",
    )?;
    let attributed = statement
        .query_map([agent_id], |row| row.get::<_, String>(0))?
        .map(|row| {
            let payload: serde_json::Value = serde_json::from_str(&row?)?;
            // data_json stores the full event envelope; the lifecycle
            // payload lives under `data`.
            let payload = payload.get("data").unwrap_or(&payload);
            Ok::<_, anyhow::Error>((
                payload["work_item_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                payload["revision"].as_u64().unwrap_or_default(),
            ))
        })
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    Ok((from..=to)
        .filter(|revision| !attributed.contains(&(work_item_id.to_string(), *revision)))
        .collect())
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

fn validate_context_budget_stop_recheck(
    tx: &Transaction<'_>,
    agent_id: &str,
    command: &execution_protocol::RecheckContextBudgetStop,
) -> Result<()> {
    let work: crate::types::WorkItemRecord = tx
        .query_row(
            "SELECT payload_json FROM work_items WHERE agent_id = ?1 AND work_item_id = ?2",
            params![agent_id, command.work_item_id],
            |row| row.get::<_, String>(0),
        )
        .map(|payload| serde_json::from_str(&payload))??;
    let has_wait: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM wait_conditions
         WHERE agent_id = ?1 AND work_item_id = ?2 AND status IN ('active', 'triggered'))",
        params![agent_id, command.work_item_id],
        |row| row.get(0),
    )?;
    if work.revision != command.expected.source_revision
        || work.state != crate::types::WorkItemState::Open
        || work.plan_status != crate::types::WorkItemPlanStatus::Ready
        || work.blocked_by.is_some()
        || has_wait
    {
        bail!("context budget recheck source is not eligible");
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

fn validate_wait_ingress_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    attempt: &execution_protocol::ExecutionAttempt,
) -> Result<()> {
    use crate::domain::execution_protocol::{
        ExecutionOrigin, ExecutionSourceIdentity, ExecutionTrust, ExecutionWaitIngress,
    };
    use crate::types::{AuthorityClass, MessageEnvelope, MessageOrigin};

    let ExecutionSourceIdentity::TriggeredWait {
        trigger_message_id,
        ingress: Some(ExecutionWaitIngress::RuntimeOwnedInternalFollowup),
        ..
    } = &attempt.source.identity
    else {
        return Ok(());
    };
    let payload = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE agent_id = ?1 AND message_id = ?2",
            [agent_id, trigger_message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("wait ingress requires a durable trigger message"))?;
    let message: MessageEnvelope = serde_json::from_str(&payload)?;
    let origin = match &message.origin {
        MessageOrigin::System { .. } => ExecutionOrigin::System,
        MessageOrigin::Task { .. } => ExecutionOrigin::Task,
        _ => bail!("wait ingress requires a runtime-owned trigger origin"),
    };
    let trust = match message.authority_class {
        AuthorityClass::OperatorInstruction => ExecutionTrust::OperatorInstruction,
        AuthorityClass::RuntimeInstruction => ExecutionTrust::RuntimeInstruction,
        AuthorityClass::IntegrationSignal => ExecutionTrust::IntegrationSignal,
        AuthorityClass::ExternalEvidence => ExecutionTrust::ExternalEvidence,
    };
    if !message.is_runtime_owned_internal_followup()
        || message.agent_id != attempt.agent_id
        || attempt.source_message_id.as_deref() != Some(trigger_message_id.as_str())
        || message.message_seq != Some(attempt.source.generation)
        || attempt.provenance.origin != origin
        || attempt.provenance.trust != trust
    {
        bail!("wait ingress does not match its durable runtime-owned trigger");
    }
    Ok(())
}

fn validate_admission_authority_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    admitted: &execution_protocol::AdmittedFences,
) -> Result<()> {
    let current = authority_fences_tx(tx, agent_id)?;
    // The agent_control_revision is incremented by every persist_state()
    // call, including benign wake_hint coalescing while the agent is
    // AwakeRunning. authority_fences_tx already validates that the agent
    // is not Stopped and the host identity is Active — the authority-
    // relevant invariants. Comparing control_revision here would race
    // against concurrent persist_state() calls that run between the fence
    // read (plan_execution_protocol_claim, separate transaction) and this
    // validation (inside the commit transaction). The host_registry_revision
    // comparison is retained because it reads from agent_identities, which
    // is not touched by persist_state().
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
    /// Read historical canonical evidence without loading the execution partition.
    pub(crate) fn execution_attempt_by_id(
        &self,
        agent_id: &str,
        attempt_id: &str,
    ) -> Result<Option<ExecutionAttempt>> {
        self.db.with_read(|connection| {
            let payload = connection
                .query_row(
                    "SELECT payload_json FROM execution_protocol_attempts
                     WHERE agent_id = ?1 AND attempt_id = ?2",
                    params![agent_id, attempt_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            let attempt = payload
                .map(|payload| serde_json::from_str::<ExecutionAttempt>(&payload))
                .transpose()?;
            Ok(attempt
                .filter(|attempt| attempt.agent_id == agent_id && attempt.attempt_id == attempt_id))
        })
    }

    pub(crate) fn load_execution_authority_fences(
        &self,
        agent_id: &str,
    ) -> Result<ExecutionAuthorityFences> {
        self.db
            .read_for_preparation(|tx| authority_fences_tx(tx, agent_id))
    }

    pub(crate) fn load_execution_protocol_state_if_initialized(
        &self,
        agent_id: &str,
    ) -> Result<Option<ExecutionProtocolState>> {
        self.db.with_read(|connection| {
            let transaction =
                Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Deferred)?;
            if !partition_exists_tx(&transaction, agent_id)? {
                transaction.commit()?;
                return Ok(None);
            }
            let state = load_state_tx(&transaction, agent_id)?;
            transaction.commit()?;
            Ok(Some(state))
        })
    }

    pub(crate) fn commit_execution_protocol_recovery_plan_with_maintenance(
        &self,
        agent_id: &str,
        commands: &[ExecutionProtocolCommand],
        audit_events: &[crate::types::AuditEvent],
        maintenance: Option<&crate::runtime_db::OfflineInputMaintenance<'_>>,
    ) -> Result<ExecutionProtocolRecoveryCommitOutcome> {
        Ok(self
            .commit_execution_protocol_recovery_plan_inner(
                agent_id,
                None,
                commands,
                audit_events,
                maintenance,
            )?
            .1)
    }

    fn commit_execution_protocol_recovery_plan_inner(
        &self,
        agent_id: &str,
        scope: Option<&crate::runtime::resident_input_evidence::InputEvidenceScope>,
        commands: &[ExecutionProtocolCommand],
        audit_events: &[crate::types::AuditEvent],
        maintenance: Option<&crate::runtime_db::OfflineInputMaintenance<'_>>,
    ) -> Result<(
        super::TransitionCommit,
        ExecutionProtocolRecoveryCommitOutcome,
    )> {
        self.db.transaction(|tx| {
            self.commit_execution_protocol_recovery_plan_tx(
                tx,
                agent_id,
                scope,
                commands,
                audit_events,
                maintenance,
            )
        })
    }

    pub(super) fn commit_execution_protocol_recovery_plan_tx(
        &self,
        tx: &Transaction<'_>,
        agent_id: &str,
        scope: Option<&crate::runtime::resident_input_evidence::InputEvidenceScope>,
        commands: &[ExecutionProtocolCommand],
        audit_events: &[crate::types::AuditEvent],
        maintenance: Option<&crate::runtime_db::OfflineInputMaintenance<'_>>,
    ) -> Result<(
        super::TransitionCommit,
        ExecutionProtocolRecoveryCommitOutcome,
    )> {
        if commands.is_empty() {
            return Ok((
                Default::default(),
                ExecutionProtocolRecoveryCommitOutcome::Applied(false),
            ));
        }
        if commands.iter().any(|command| {
            !matches!(
                command,
                ExecutionProtocolCommand::ReconcileWorkItemContinuationYield(_)
                    | ExecutionProtocolCommand::RecheckContextBudgetStop(_)
                    | ExecutionProtocolCommand::RecoverInterruptedTaskResultClaim(_)
                    | ExecutionProtocolCommand::RecoverUnadvancedTaskResultClaim(_)
            )
        }) {
            bail!("execution protocol recovery plan contains a non-recovery command");
        }
        if let Some(scope) = scope {
            crate::runtime_db::input_evidence_writes::validate_input_evidence_scope_tx(
                tx, scope, agent_id,
            )?;
        }
        let prepared =
            match validate_execution_commands_tx(tx, agent_id, None, commands, &[], &[], &[]) {
                Ok(prepared) => prepared,
                Err(error) => {
                    return Ok((
                        Default::default(),
                        ExecutionProtocolRecoveryCommitOutcome::Rejected {
                            reason: error.to_string(),
                        },
                    ));
                }
            };
        let input = if scope.is_none() {
            let keys = prepared
                .as_ref()
                .map(|x| x.input_keys())
                .unwrap_or_default();
            Some(crate::runtime_db::input_evidence_snapshot::maintenance::prepare_offline_input_repair_tx(
                tx, self.db, agent_id, &keys, commands, maintenance,
            )?)
        } else {
            None
        };
        let changed = prepared
            .as_ref()
            .is_some_and(PreparedExecutionProtocolCommands::has_writes);
        let (_, input_delta) = persist_execution_commands_with_input_facts_tx(tx, prepared, scope)?;
        let mut events = Vec::new();
        if changed {
            super::AuditEventAck::admit_slots(audit_events.len())?;
            events.reserve(audit_events.len());
            for event in audit_events {
                events.push(
                    crate::runtime_db::evidence::append_audit_event_ack_tx(
                        tx,
                        Some(agent_id),
                        event,
                    )?
                    .0,
                );
            }
        }
        if let Some(input) = input {
            input.finish_tx(tx)?;
        }
        Ok((
            super::TransitionCommit {
                applied: changed,
                effects: Box::new(super::PostCommitEffects {
                    audit_events: events,
                    input_evidence_delta: input_delta,
                    ..Default::default()
                }),
                ..Default::default()
            },
            ExecutionProtocolRecoveryCommitOutcome::Applied(changed),
        ))
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
    persist_state_inner_tx(tx, state, None, None)
}

/// A repair loaded this complete prior aggregate in the same transaction. Keep
/// unchanged canonical bytes intact, including legal legacy/default encodings.
pub(crate) fn persist_repaired_state_tx(
    tx: &Transaction<'_>,
    previous: &ExecutionProtocolState,
    state: &ExecutionProtocolState,
) -> Result<()> {
    anyhow::ensure!(
        previous.agent_id == state.agent_id
            && previous
                .attempts
                .keys()
                .all(|id| state.attempts.contains_key(id))
            && previous
                .work_items
                .keys()
                .all(|id| state.work_items.contains_key(id))
            && previous
                .outcomes
                .keys()
                .all(|id| state.outcomes.contains_key(id)),
        "execution repair cannot remove or replace a partition"
    );
    persist_state_inner_tx(tx, state, Some(previous), None)
}

fn persist_state_inner_tx(
    tx: &Transaction<'_>,
    state: &ExecutionProtocolState,
    previous: Option<&ExecutionProtocolState>,
    touched: Option<&BTreeSet<crate::runtime::resident_input_evidence::InputEvidenceKey>>,
) -> Result<()> {
    crate::runtime_db::reclamation::ensure_work_admission_tx(tx, &state.agent_id)?;
    persist_state_rows_tx(tx, state, previous, touched)
}

pub(super) fn validate_agent_deletion_retirement_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<()> {
    let deleting: bool = tx.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM agent_identities i
            JOIN agent_deletion_jobs j ON j.agent_id = i.agent_id
            WHERE i.agent_id = ?1 AND i.status IN ('deleting', 'deleted')
         )",
        [agent_id],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        deleting,
        "execution retirement requires an admitted agent deletion"
    );
    Ok(())
}

/// Terminal-safe cleanup, not execution admission. Keep historical attempts and
/// outcomes, but remove every live scheduling reference in the same lifecycle tx.
pub(crate) fn retire_agent_execution_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<bool> {
    validate_agent_deletion_retirement_tx(tx, agent_id)?;
    if !partition_exists_tx(tx, agent_id)? {
        return Ok(false);
    }
    let existing = load_state_tx(tx, agent_id)?;
    let mut state = existing.clone();
    let now = Utc::now().to_rfc3339();
    for attempt in existing.attempts.values() {
        if attempt.state == execution_protocol::ExecutionAttemptState::Open {
            state = execution_protocol::interrupt_execution(
                &state,
                &execution_protocol::InterruptExecution {
                    attempt_id: attempt.attempt_id.clone(),
                    outcome_id: format!("agent-deletion:{}", attempt.attempt_id),
                    reason: "agent_deleted".into(),
                    interrupted_at: now.clone(),
                },
            )
            .map_err(|error| anyhow!("agent deletion execution interruption: {error}"))?
            .state;
        }
    }
    for (work_item_id, record) in &mut state.work_items {
        let payload: String = tx.query_row(
            "SELECT payload_json FROM work_items WHERE agent_id = ?1 AND work_item_id = ?2",
            params![agent_id, work_item_id],
            |row| row.get(0),
        )?;
        let source: crate::types::WorkItemRecord = serde_json::from_str(&payload)?;
        let generation = record.generation();
        let target = if let Some(completion) = source
            .result_brief_id
            .filter(|_| source.state == crate::types::WorkItemState::Completed)
        {
            WorkItemExecutionState::Terminal {
                generation,
                completion,
            }
        } else if matches!(record.state, WorkItemExecutionState::Terminal { .. }) {
            // Legacy completed sources may not have a brief. Keep existing
            // terminal evidence rather than inventing a completion binding.
            record.state.clone()
        } else {
            WorkItemExecutionState::Paused {
                generation,
                reason: "agent_deleted".into(),
            }
        };
        if record.state != target {
            let generation = generation
                .checked_add(1)
                .ok_or_else(|| anyhow!("agent deletion scheduling generation overflow"))?;
            record.state = match target {
                WorkItemExecutionState::Terminal { completion, .. } => {
                    WorkItemExecutionState::Terminal {
                        generation,
                        completion,
                    }
                }
                _ => WorkItemExecutionState::Paused {
                    generation,
                    reason: "agent_deleted".into(),
                },
            };
        }
        record.source_revision = source.revision;
    }
    if state == existing {
        return Ok(false);
    }
    // This path can only retire already persisted records after deletion
    // admission. Ordinary protocol writes still enforce the admission fence.
    persist_state_rows_tx(tx, &state, None, None)?;
    Ok(true)
}

fn persist_state_rows_tx(
    tx: &Transaction<'_>,
    state: &ExecutionProtocolState,
    previous: Option<&ExecutionProtocolState>,
    touched: Option<&BTreeSet<crate::runtime::resident_input_evidence::InputEvidenceKey>>,
) -> Result<()> {
    use crate::runtime::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key,
    };
    execution_protocol::assert_invariants(state)
        .map_err(|error| anyhow!("invalid execution protocol state: {error}"))?;
    tx.execute(
        "INSERT INTO execution_protocol_partitions (agent_id, updated_at)
         VALUES (?1, ?2)
         ON CONFLICT(agent_id) DO UPDATE SET updated_at = excluded.updated_at",
        params![state.agent_id, Utc::now().to_rfc3339()],
    )?;
    for (work_item_id, work_record) in &state.work_items {
        if touched
            .is_some_and(|keys| !keys.contains(&Key::new(Family::WorkExecution, work_item_id)))
            || previous.is_some_and(|prior| prior.work_items.get(work_item_id) == Some(work_record))
        {
            continue;
        }
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
        if touched
            .is_some_and(|keys| !keys.contains(&Key::new(Family::Attempt, &attempt.attempt_id)))
            || previous
                .is_some_and(|prior| prior.attempts.get(&attempt.attempt_id) == Some(attempt))
        {
            continue;
        }
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
        if touched
            .is_some_and(|keys| !keys.contains(&Key::new(Family::Outcome, &outcome.outcome_id)))
            || previous
                .is_some_and(|prior| prior.outcomes.get(&outcome.outcome_id) == Some(outcome))
        {
            continue;
        }
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

/// Loads execution-protocol rows without requiring a partition marker or
/// asserting cross-row invariants.
///
/// Repair callers must restore and assert those invariants before committing.
pub(crate) fn load_state_unchecked_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<ExecutionProtocolState> {
    load_state_unchecked_on(tx, agent_id)
}

pub(crate) fn load_state_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<ExecutionProtocolState> {
    if !partition_exists_tx(tx, agent_id)? {
        bail!("execution protocol partition for agent {agent_id} is not initialized");
    }
    let state = load_state_unchecked_tx(tx, agent_id)?;
    execution_protocol::assert_invariants(&state)
        .map_err(|error| anyhow!("stored execution protocol state is invalid: {error}"))?;
    Ok(state)
}

fn load_payload_map<T: DeserializeOwned>(
    tx: &Connection,
    sql: &str,
    agent_id: &str,
) -> Result<BTreeMap<String, T>> {
    let mut statement = tx.prepare(sql)?;
    let mut rows = statement.query([agent_id])?;
    let mut records = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let id = crate::runtime_db::repositories::borrowed_sql_text(row, 0)?;
        let payload = crate::runtime_db::repositories::borrowed_sql_text(row, 1)?;
        let metadata = id
            .len()
            .checked_add(std::mem::size_of::<(String, T)>())
            .and_then(|bytes| bytes.checked_add(64))
            .context("execution source map metadata overflow")?;
        crate::diagnostics::resource_io::charge_owned_metadata(u64::try_from(metadata)?, 1)?;
        crate::diagnostics::resource_io::admit_json_payload(payload)?;
        let value = serde_json::from_str(payload)
            .with_context(|| format!("decoding execution protocol record {id}"))?;
        records.insert(id.to_owned(), value);
    }
    Ok(records)
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
    use crate::runtime_db::RuntimeDb;
    use crate::types::AgentState;
    use crate::types::{AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility};
    use tempfile::tempdir;

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

    fn setup_agent(db: &RuntimeDb, agent_id: &str) -> Result<()> {
        db.agent_states()
            .upsert_for_fixture(&AgentState::new(agent_id))?;
        let mut identity = AgentIdentityRecord::new(
            agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        identity.status = AgentRegistryStatus::Active;
        db.agent_identities().upsert(&identity)?;
        Ok(())
    }

    fn work_item_claim_fixture(
        db: &RuntimeDb,
        revision: u64,
    ) -> Result<(
        crate::types::WorkItemRecord,
        ExecutionProtocolState,
        ExecutionAttempt,
    )> {
        use crate::domain::execution_protocol::*;

        let agent_id = "agent-model-claim";
        setup_agent(db, agent_id)?;
        let work = crate::types::WorkItemRecord::new(
            agent_id,
            "claim snapshot",
            crate::types::WorkItemState::Open,
        );
        assert!(db.work_items().insert_new(&work)?);
        let mut state = ExecutionProtocolState::empty(agent_id);
        state.work_items.insert(
            work.id.clone(),
            WorkItemExecutionRecord {
                source_revision: revision,
                state: WorkItemExecutionState::Runnable {
                    generation: 1,
                    recovery_ref: None,
                },
            },
        );
        let fences = db.transitions().load_execution_authority_fences(agent_id)?;
        let attempt = ExecutionAttempt {
            attempt_id: "attempt-model-claim".into(),
            agent_id: agent_id.into(),
            source_message_id: Some("message-model-claim".into()),
            input_projection: None,
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::QueueMessage {
                    message_id: "message-model-claim".into(),
                },
                generation: 1,
            },
            binding: ExecutionBinding::WorkItem {
                work_item_id: work.id.clone(),
            },
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::Operator,
                trust: ExecutionTrust::OperatorInstruction,
                priority: ExecutionPriority::Normal,
                correlation_id: None,
                causation_id: None,
            },
            admitted_fences: AdmittedFences {
                source_revision: 1,
                work_item_source_revision: Some(revision),
                work_item_generation: Some(1),
                rejoin: None,
                agent_control_revision: fences.agent_control_revision,
                host_registry_revision: fences.host_registry_revision,
            },
            state: ExecutionAttemptState::Open,
            run_id: None,
            turn_id: None,
            recovery_of_attempt_id: None,
            terminal_outcome_id: None,
            admitted_at: Utc::now().to_rfc3339(),
            terminal_at: None,
        };
        Ok((work, state, attempt))
    }

    // #3816: the reconcile repair command is evidence-fenced. It must be
    // rejected without per-revision attribution evidence, with a concurrent
    // durable mutation, or for an in-flight registration.
    #[test]
    fn reconcile_work_item_source_revision_requires_full_evidence() -> Result<()> {
        use crate::domain::execution_protocol::{
            ReconcileWorkItemSourceRevision, WorkItemExecutionRecord,
        };

        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("state/runtime.sqlite"),
            home.path().join("state/runtime.lock"),
        )?;
        let agent_id = "agent-reconcile";
        setup_agent(&db, agent_id)?;
        let work = crate::types::WorkItemRecord::new(
            agent_id,
            "reconcile drift",
            crate::types::WorkItemState::Open,
        );
        assert!(db.work_items().insert_new(&work)?);
        let mut drifted = work.clone();
        drifted.revision = 3;
        db.transaction(|tx| {
            tx.execute(
                "UPDATE work_items SET revision = ?1, payload_json = ?2 WHERE work_item_id = ?3",
                rusqlite::params![3, serde_json::to_string(&drifted)?, work.id],
            )?;
            Ok(())
        })?;
        let mut state = ExecutionProtocolState::empty(agent_id);
        state.work_items.insert(
            work.id.clone(),
            WorkItemExecutionRecord {
                source_revision: 1,
                state: WorkItemExecutionState::Waiting {
                    generation: 1,
                    wait: WaitReference {
                        wait_id: "wait-reconcile".into(),
                    },
                },
            },
        );
        db.transaction(|tx| {
            persist_state_tx(tx, &state)?;
            for revision in [2_u64, 3] {
                crate::runtime_db::evidence::append_audit_event_tx(
                    tx,
                    Some(agent_id),
                    &crate::types::AuditEvent::legacy(
                        "work_item_written",
                        serde_json::json!({
                            "work_item_id": work.id,
                            "revision": revision,
                        }),
                    ),
                )?;
            }
            Ok(())
        })?;
        let command = ReconcileWorkItemSourceRevision {
            command_id: format!("work_item:reconcile_source_revision:{}:3", work.id),
            work_item_id: work.id.clone(),
            expected_source_revision: 1,
            source_revision: 3,
            evidence_fingerprint: "test-fingerprint".into(),
        };
        db.transaction(|tx| {
            validate_execution_commands_tx(
                tx,
                agent_id,
                None,
                &[ExecutionProtocolCommand::ReconcileWorkItemSourceRevision(
                    command.clone(),
                )],
                &[],
                &[],
                &[],
            )
        })?
        .expect("fully attributed reconcile prepares");

        // A drift without per-revision attribution must fail closed.
        let mut unattributed = crate::types::WorkItemRecord::new(
            agent_id,
            "unattributed drift",
            crate::types::WorkItemState::Open,
        );
        unattributed.id = "work-unattributed".into();
        assert!(db.work_items().insert_new(&unattributed)?);
        let mut unattributed_drifted = unattributed.clone();
        unattributed_drifted.revision = 2;
        db.transaction(|tx| {
            tx.execute(
                "UPDATE work_items SET revision = ?1, payload_json = ?2 WHERE work_item_id = ?3",
                rusqlite::params![
                    unattributed_drifted.revision,
                    serde_json::to_string(&unattributed_drifted)?,
                    unattributed.id
                ],
            )?;
            Ok(())
        })?;
        let mut state = ExecutionProtocolState::empty(agent_id);
        state.work_items.insert(
            unattributed.id.clone(),
            WorkItemExecutionRecord {
                source_revision: 1,
                state: WorkItemExecutionState::Runnable {
                    generation: 1,
                    recovery_ref: None,
                },
            },
        );
        db.transaction(|tx| persist_state_tx(tx, &state))?;
        let unattributed_command = ReconcileWorkItemSourceRevision {
            command_id: format!("work_item:reconcile_source_revision:{}:2", unattributed.id),
            work_item_id: unattributed.id.clone(),
            expected_source_revision: 1,
            source_revision: 2,
            evidence_fingerprint: "test-fingerprint".into(),
        };
        let error = match db.transaction(|tx| {
            validate_execution_commands_tx(
                tx,
                agent_id,
                None,
                &[ExecutionProtocolCommand::ReconcileWorkItemSourceRevision(
                    unattributed_command.clone(),
                )],
                &[],
                &[],
                &[],
            )
        }) {
            Ok(prepared) => panic!(
                "unattributed reconcile must fail closed, got {:#?}",
                prepared.is_some()
            ),
            Err(error) => error,
        };
        assert!(error.to_string().contains("attribution evidence"));
        Ok(())
    }

    #[test]
    fn source_revision_advance_requires_same_transaction_revision_chain() -> Result<()> {
        for case in [
            "valid",
            "no_mutation",
            "wrong_base",
            "wrong_owner",
            "already_durable",
        ] {
            let home = tempdir()?;
            let db = RuntimeDb::open_and_migrate(
                home.path().join("state/runtime.sqlite"),
                home.path().join("state/runtime.lock"),
            )?;
            let (work, _, _) = work_item_claim_fixture(&db, 1)?;
            let mut updated = work.clone();
            updated.revision += 1;
            if case == "already_durable" {
                assert!(db.work_items().update_expected(&updated, work.revision)?);
            }
            if case == "wrong_owner" {
                updated.agent_id = "other-agent".into();
            }
            let mutations = if case == "no_mutation" {
                Vec::new()
            } else {
                vec![WorkItemMutation::Update {
                    record: updated,
                    expected_revision: if case == "wrong_base" {
                        0
                    } else {
                        work.revision
                    },
                }]
            };
            let advance = execution_protocol::AdvanceWorkItemSourceRevision {
                command_id: "test-advance".into(),
                work_item_id: work.id.clone(),
                expected_source_revision: work.revision,
                source_revision: work.revision + 1,
            };
            let result = db.transaction(|tx| {
                validate_work_item_source_revision_advance(tx, &work.agent_id, &advance, &mutations)
            });
            assert_eq!(result.is_ok(), case == "valid", "{case}: {result:?}");
        }
        Ok(())
    }

    #[test]
    fn model_queue_claim_allows_same_transaction_admit_and_settle() -> Result<()> {
        use crate::domain::execution_protocol::*;

        for initialized in [false, true] {
            for outcome in [
                WorkItemOutcome::Continue,
                WorkItemOutcome::Wait {
                    wait: WaitReference {
                        wait_id: "wait-model-claim".into(),
                    },
                },
                WorkItemOutcome::Complete {
                    completion: "completed without model dispatch".into(),
                },
                WorkItemOutcome::NeedsRepair {
                    repair_id: "repair-model-claim".into(),
                },
            ] {
                let home = tempdir()?;
                let db = RuntimeDb::open_and_migrate(
                    home.path().join("state/runtime.sqlite"),
                    home.path().join("state/runtime.lock"),
                )?;
                let (work, bootstrap, attempt) = work_item_claim_fixture(&db, 1)?;
                if initialized {
                    db.transaction(|tx| persist_state_tx(tx, &bootstrap))?;
                }
                let commands = [
                    ExecutionProtocolCommand::Admit(Box::new(AdmitExecution {
                        attempt: attempt.clone(),
                    })),
                    ExecutionProtocolCommand::Settle(SettleExecution {
                        outcome: ExecutionOutcomeRecord {
                            outcome_id: "outcome-model-claim".into(),
                            attempt_id: attempt.attempt_id.clone(),
                            outcome: ExecutionOutcome::WorkItem(outcome),
                            created_at: Utc::now().to_rfc3339(),
                        },
                    }),
                ];
                db.transaction(|tx| {
                    let prepared = validate_execution_commands_tx(
                        tx,
                        &work.agent_id,
                        Some(&bootstrap),
                        &commands,
                        &[],
                        &[],
                        &[],
                    )?;
                    validate_model_queue_claim_tx(
                        tx,
                        &work.agent_id,
                        attempt.source_message_id.as_deref().unwrap(),
                        prepared.as_ref(),
                        &[],
                    )?;
                    persist_execution_commands_tx(tx, prepared)
                })?;
                let state = db
                    .transitions()
                    .load_execution_protocol_state_if_initialized(&work.agent_id)?
                    .unwrap();
                assert_eq!(
                    state.attempts[&attempt.attempt_id].state,
                    ExecutionAttemptState::Settled
                );
                assert!(state.open_attempt().is_none());
                assert!(!matches!(
                    state.work_items[&work.id].state,
                    WorkItemExecutionState::InFlight { .. }
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn model_queue_claim_bootstrap_validates_final_mutation_revision() -> Result<()> {
        use crate::domain::execution_protocol::*;

        for admitted_revision in [1, 2, 3] {
            let home = tempdir()?;
            let db = RuntimeDb::open_and_migrate(
                home.path().join("state/runtime.sqlite"),
                home.path().join("state/runtime.lock"),
            )?;
            let (work, bootstrap, attempt) = work_item_claim_fixture(&db, admitted_revision)?;
            let mut updated = work.clone();
            updated.revision = 2;
            let mutations = [WorkItemMutation::Update {
                record: updated.clone(),
                expected_revision: work.revision,
            }];
            let commands = [ExecutionProtocolCommand::Admit(Box::new(AdmitExecution {
                attempt: attempt.clone(),
            }))];
            let result = db.transaction(|tx| {
                let prepared = validate_execution_commands_tx(
                    tx,
                    &work.agent_id,
                    Some(&bootstrap),
                    &commands,
                    &mutations,
                    &[],
                    &[],
                )?;
                validate_model_queue_claim_tx(
                    tx,
                    &work.agent_id,
                    attempt.source_message_id.as_deref().unwrap(),
                    prepared.as_ref(),
                    &mutations,
                )?;
                assert!(
                    crate::runtime_db::repositories::update_expected_work_item_tx(
                        tx,
                        &updated,
                        work.revision,
                    )?
                );
                persist_execution_commands_tx(tx, prepared)
            });
            if admitted_revision == updated.revision {
                result?;
                let state = db
                    .transitions()
                    .load_execution_protocol_state_if_initialized(&work.agent_id)?
                    .unwrap();
                assert_eq!(state.work_items[&work.id].source_revision, updated.revision);
                assert_eq!(
                    db.work_items().latest(&work.id)?.unwrap().revision,
                    updated.revision,
                );
                assert!(state.open_attempt().is_some());
            } else {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    // #3816: the uniform final-revision equality assertion
                    // now rejects the mismatched admission inside command
                    // validation, before the claim-specific snapshot check.
                    .contains("does not match committed revision"));
                assert!(db
                    .transitions()
                    .load_execution_protocol_state_if_initialized(&work.agent_id)?
                    .is_none());
                assert_eq!(db.work_items().latest(&work.id)?.unwrap(), work);
            }
        }
        Ok(())
    }

    #[test]
    fn marked_wait_ingress_requires_durable_envelope_evidence() -> Result<()> {
        use crate::domain::execution_protocol::*;
        use crate::types::{
            AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageEnvelope,
            MessageKind, MessageOrigin, Priority,
        };

        for case in [
            "valid_system",
            "valid_task",
            "surface",
            "admission",
            "kind",
            "origin",
            "trust",
            "source",
            "revision",
            "missing",
        ] {
            let home = tempdir()?;
            let db = RuntimeDb::open_and_migrate(
                home.path().join("state/runtime.sqlite"),
                home.path().join("state/runtime.lock"),
            )?;
            let agent_id = "agent-wait-ingress";
            setup_agent(&db, agent_id)?;
            let mut message = MessageEnvelope::new(
                agent_id,
                MessageKind::InternalFollowup,
                MessageOrigin::System {
                    subsystem: "agent_message".into(),
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text { text: case.into() },
            );
            message.delivery_surface = Some(MessageDeliverySurface::RuntimeSystem);
            message.admission_context = Some(AdmissionContext::RuntimeOwned);
            match case {
                "valid_task" => {
                    message.origin = MessageOrigin::Task {
                        task_id: "peer-task".into(),
                    };
                }
                "surface" => message.delivery_surface = None,
                "admission" => message.admission_context = None,
                "kind" => message.kind = MessageKind::OperatorPrompt,
                "origin" => {
                    message.origin = MessageOrigin::Operator {
                        actor_id: None,
                        actor_display_name: None,
                    };
                }
                _ => {}
            }
            if case != "missing" {
                message = db.messages().append_with_index_changes(&message, &[])?;
            }
            let fences = db.transitions().load_execution_authority_fences(agent_id)?;
            let revision = message.message_seq.unwrap_or(1) + u64::from(case == "revision");
            let commands = [ExecutionProtocolCommand::Admit(Box::new(AdmitExecution {
                attempt: ExecutionAttempt {
                    input_projection: None,
                    attempt_id: format!("attempt-{case}"),
                    agent_id: agent_id.into(),
                    source_message_id: Some(if case == "source" {
                        "different-message".into()
                    } else {
                        message.id.clone()
                    }),
                    source: ExecutionSource {
                        identity: ExecutionSourceIdentity::TriggeredWait {
                            wait_id: "peer-wait".into(),
                            trigger_message_id: message.id.clone(),
                            ingress: Some(ExecutionWaitIngress::RuntimeOwnedInternalFollowup),
                        },
                        generation: revision,
                    },
                    binding: ExecutionBinding::AgentLifecycle {
                        agent_id: agent_id.into(),
                    },
                    provenance: ExecutionProvenance {
                        origin: if case == "valid_task" {
                            ExecutionOrigin::Task
                        } else {
                            ExecutionOrigin::System
                        },
                        trust: if case == "trust" {
                            ExecutionTrust::RuntimeInstruction
                        } else {
                            ExecutionTrust::OperatorInstruction
                        },
                        priority: ExecutionPriority::Normal,
                        correlation_id: None,
                        causation_id: None,
                    },
                    admitted_fences: AdmittedFences {
                        source_revision: revision,
                        work_item_source_revision: None,
                        work_item_generation: None,
                        rejoin: None,
                        agent_control_revision: fences.agent_control_revision,
                        host_registry_revision: fences.host_registry_revision,
                    },
                    state: ExecutionAttemptState::Open,
                    run_id: None,
                    turn_id: None,
                    recovery_of_attempt_id: None,
                    terminal_outcome_id: None,
                    admitted_at: Utc::now().to_rfc3339(),
                    terminal_at: None,
                },
            }))];
            let result = db.transaction(|tx| {
                let prepared = validate_execution_commands_tx(
                    tx,
                    agent_id,
                    Some(&ExecutionProtocolState::empty(agent_id)),
                    &commands,
                    &[],
                    &[],
                    &[],
                )?;
                persist_execution_commands_tx(tx, prepared)?;
                Ok(())
            });
            let persisted = db
                .transitions()
                .load_execution_protocol_state_if_initialized(agent_id)?;
            if case.starts_with("valid_") {
                result?;
                assert!(persisted.unwrap().open_attempt().is_some(), "{case}");
            } else {
                let error = result.unwrap_err();
                assert!(
                    error.to_string().contains("wait ingress"),
                    "{case}: {error}"
                );
                assert!(persisted.is_none(), "{case}");
            }
        }
        Ok(())
    }

    /// When `control_revision` changes between the fence read (in
    /// `plan_execution_protocol_claim`) and validation (inside the commit
    /// transaction), the admission must not be rejected. This can happen
    /// when a concurrent `persist_state()` call (e.g. wake_hint coalescing)
    /// increments `control_revision` in the TOCTOU window.
    #[test]
    fn admission_accepts_stale_control_revision_after_concurrent_persist() -> Result<()> {
        let temp_dir = tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            temp_dir.path().join("state/runtime.sqlite"),
            temp_dir.path().join("state/runtime.lock"),
        )?;
        let agent_id = "agent-race";
        setup_agent(&db, agent_id)?;

        // Read authority fences (simulates plan_execution_protocol_claim).
        let fences = db.transitions().load_execution_authority_fences(agent_id)?;
        let admitted_revision = fences.agent_control_revision;

        // Simulate a concurrent persist_state() that increments control_revision
        // (e.g. wake_hint coalescing setting pending_wake_hint).
        db.agent_states()
            .upsert_for_fixture(&AgentState::new(agent_id))?;

        // The current control_revision should now differ from the admitted one.
        let current = db.transitions().load_execution_authority_fences(agent_id)?;
        assert_ne!(admitted_revision, current.agent_control_revision);

        // Validation inside the commit transaction must accept the stale
        // control_revision instead of bailing.
        let connection = db.connection()?;
        let tx = Transaction::new_unchecked(&connection, rusqlite::TransactionBehavior::Deferred)?;
        validate_admission_authority_tx(
            &tx,
            agent_id,
            &execution_protocol::AdmittedFences {
                source_revision: 1,
                work_item_source_revision: None,
                work_item_generation: None,
                rejoin: None,
                agent_control_revision: admitted_revision,
                host_registry_revision: fences.host_registry_revision,
            },
        )?;
        tx.commit()?;
        Ok(())
    }

    /// A stopped agent must still be rejected even when control_revision is relaxed.
    #[test]
    fn admission_rejects_stopped_agent_regardless_of_control_revision() -> Result<()> {
        let temp_dir = tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            temp_dir.path().join("state/runtime.sqlite"),
            temp_dir.path().join("state/runtime.lock"),
        )?;
        let agent_id = "agent-stopped";
        setup_agent(&db, agent_id)?;

        let fences = db.transitions().load_execution_authority_fences(agent_id)?;

        // Stop the agent.
        let mut stopped = AgentState::new(agent_id);
        stopped.status = crate::types::AgentStatus::Stopped;
        db.agent_states().upsert_for_fixture(&stopped)?;

        let connection = db.connection()?;
        let tx = Transaction::new_unchecked(&connection, rusqlite::TransactionBehavior::Deferred)?;
        let result = validate_admission_authority_tx(
            &tx,
            agent_id,
            &execution_protocol::AdmittedFences {
                source_revision: 1,
                work_item_source_revision: None,
                work_item_generation: None,
                rejoin: None,
                agent_control_revision: fences.agent_control_revision,
                host_registry_revision: fences.host_registry_revision,
            },
        );
        tx.commit()?;
        assert!(
            result.is_err(),
            "admission of a stopped agent must be rejected"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("stopped"),
            "expected stopped rejection, got: {err}"
        );
        Ok(())
    }
}

fn load_state_unchecked_on(
    connection: &Connection,
    agent_id: &str,
) -> Result<ExecutionProtocolState> {
    Ok(ExecutionProtocolState {
        agent_id: agent_id.to_owned(),
        attempts: load_payload_map(
            connection,
            "SELECT attempt_id, payload_json
             FROM execution_protocol_attempts WHERE agent_id = ?1",
            agent_id,
        )?,
        work_items: load_payload_map(
            connection,
            "SELECT work_item_id, payload_json
             FROM execution_protocol_work_items WHERE agent_id = ?1",
            agent_id,
        )?,
        outcomes: load_payload_map(
            connection,
            "SELECT outcome_id, payload_json
             FROM execution_protocol_outcomes WHERE agent_id = ?1",
            agent_id,
        )?,
    })
}

pub(crate) fn context_execution_protocol_on(
    connection: &Connection,
    agent_id: &str,
) -> Result<Option<ExecutionProtocolState>> {
    let initialized: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM execution_protocol_partitions WHERE agent_id = ?1)",
        [agent_id],
        |row| row.get(0),
    )?;
    if !initialized {
        return Ok(None);
    }
    let state = load_state_unchecked_on(connection, agent_id)?;
    execution_protocol::assert_invariants(&state)
        .map_err(|error| anyhow!("stored execution protocol state is invalid: {error}"))?;
    Ok(Some(state))
}
