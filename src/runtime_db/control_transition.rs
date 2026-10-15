//! Focused atomic lifecycle Control transitions and immutable original outcomes.
//!
//! Dormant until all lifecycle callers and their exact cancellation leases use
//! this family. Process/Git cleanup is deliberately outside this SQLite ACK.

use std::{collections::BTreeSet, sync::Arc};

use anyhow::{anyhow, ensure, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    domain::execution_protocol::{
        self, ExecutionAttemptState, ExecutionBinding, ExecutionProtocolCommand,
        ExecutionProtocolState, InterruptExecution,
    },
    runtime_db::{
        evidence::{
            append_audit_event_tx, insert_runtime_index_changes_at_tx, upsert_agent_state_at_tx,
            upsert_workspace_occupancy_tx,
        },
        repositories::{
            compare_and_set_queue_entry_tx, decode_wait_condition_row,
            slim_task_record_for_payload, task_transition, upsert_external_trigger_with_write_tx,
            upsert_queue_entry_tx, upsert_task_tx, upsert_timer_with_write_tx,
            upsert_turn_record_tx, upsert_wait_condition_with_writes_tx, WaitConditionWrites,
        },
        task_result_settlement::{self, TaskResultSettlementRecord},
        transitions::{
            execution_protocol_repository, AgentStateMutation, PostCommitEffects, TransitionCommit,
        },
        RuntimeDb, RuntimeIndexChange, RuntimeIndexOperation, RuntimeStateTransitionConflict,
    },
    runtime_event::RuntimeEventKind,
    types::{
        AgentIdentityRecord, AgentRegistryStatus, AgentState, AgentStatus, AuditEvent,
        AuthorityClass, ControlAction, ExternalTriggerRecord, ExternalTriggerStatus, MessageBody,
        MessageEnvelope, MessageKind, MessageLifecycleAuditEvent, MessageOrigin, QueueEntryRecord,
        QueueEntryStatus, TaskLifecycleAuditEvent, TaskRecord, TaskStatus, TimerRecord,
        TimerStatus, TurnNoBriefReason, TurnRecord, TurnTerminalKind, TurnTerminalSummary,
        WaitConditionRecord, WaitConditionStatus, WorkspaceOccupancyRecord,
    },
};

mod current_reads;
pub(in crate::runtime_db) use current_reads::OriginalControlCurrentReadRecovery;
mod scheduling_writes;
mod task_writes;

pub(crate) const CONTROL_FAMILY_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ControlKey {
    pub agent_id: String,
    pub identity_incarnation: u64,
    pub command_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ControlSource {
    DirectApi,
    ControlMessage { message_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ControlInvocation {
    pub action: ControlAction,
    pub source: ControlSource,
    pub authority_class: AuthorityClass,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ControlTimerWake {
    pub timer_id: String,
    pub message_id: String,
    pub fire_count: u64,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub incorporated_at: Option<String>,
    pub cancelled_at: Option<String>,
}

/// One coherent observation; commit rechecks the complete target sets, not only
/// the rows that happened to exist during planning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ControlSnapshot {
    pub identity: AgentIdentityRecord,
    pub state: AgentState,
    pub control_revision: u64,
    pub message_count: usize,
    pub tasks: Vec<TaskRecord>,
    pub waits: Vec<WaitConditionRecord>,
    pub timers: Vec<TimerRecord>,
    pub pending_timer_wakes: Vec<ControlTimerWake>,
    pub queue_entries: Vec<QueueEntryRecord>,
    pub external_triggers: Vec<ExternalTriggerRecord>,
    pub workspace_occupancy: Option<WorkspaceOccupancyRecord>,
    pub execution_protocol: Option<ExecutionProtocolState>,
    pub attempt_turns: Vec<TurnRecord>,
    /// Same-view proof of an exact claimed input before its Turn starts.
    pub unstarted_attempt_ids: BTreeSet<String>,
    /// Exact reducer-only Control input, qualified from this same read view.
    pub reducer_control_source_message_id: Option<String>,
    /// Same-view active reducer input, including non-Control reducer messages.
    pub reducer_source_message_id: Option<String>,
}

/// Pure runtime preparation freezes the existing TaskResult contract and IDs.
/// No handle cancellation or worktree cleanup may occur while preparing this.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ControlTaskResult {
    pub task: TaskRecord,
    pub message: MessageEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ControlRunningProof {
    pub run_id: String,
    /// None is only valid for a qualified reducer-only started Turn.
    pub attempt_id: Option<String>,
    pub source_message_id: String,
    /// None covers the admitted claim before begin_interactive_turn.
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlCancellationKind {
    Run,
    Task,
    TimerLoop,
    Worktree,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ControlCancellationTarget {
    pub obligation_id: String,
    pub target_kind: ControlCancellationKind,
    pub target_identity: String,
    /// Runtime freezes its exact lease/generation here. It is never inferred
    /// from current handles during replay or obligation completion.
    pub target_generation: Value,
    pub target: Value,
}

pub(crate) struct ControlPlanInput {
    pub key: ControlKey,
    pub invocation: ControlInvocation,
    pub snapshot: ControlSnapshot,
    pub prepared_at: DateTime<Utc>,
    pub task_results: Vec<ControlTaskResult>,
    pub healthy_running: Option<ControlRunningProof>,
    pub protected_control_attempt: Option<String>,
    pub protected_control_run_id: Option<String>,
    pub cancellation_targets: Vec<ControlCancellationTarget>,
    pub audit_events: Vec<AuditEvent>,
    pub index_changes: Vec<RuntimeIndexChange>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedControl {
    key: ControlKey,
    invocation: ControlInvocation,
    invocation_digest: String,
    command_digest: String,
    expected: Box<ControlSnapshot>,
    candidate: Box<AgentState>,
    prepared_at: DateTime<Utc>,
    task_results: Vec<ControlTaskResult>,
    protected_control_attempt: Option<String>,
    protected_control_run_id: Option<String>,
    execution_commands: Vec<ExecutionProtocolCommand>,
    interrupted_turns: Vec<TurnRecord>,
    cancellation_targets: Vec<ControlCancellationTarget>,
    audit_events: Vec<AuditEvent>,
    index_changes: Vec<RuntimeIndexChange>,
    outcome: ControlOutcome,
    #[cfg(test)]
    fault: Option<ControlFaultPoint>,
}

/// Private construction preserves the real original Control command and snapshot.
#[cfg(test)]
pub(crate) struct OriginalControlTaskCleanupView<'a> {
    command: &'a PreparedControl,
    before: &'a TaskRecord,
    result: &'a ControlTaskResult,
}
#[cfg(test)]
impl OriginalControlTaskCleanupView<'_> {
    pub(crate) fn key(&self) -> &ControlKey {
        &self.command.key
    }
    pub(crate) fn command_digest(&self) -> &str {
        &self.command.command_digest
    }
    pub(crate) fn prepared_at(&self) -> DateTime<Utc> {
        self.command.prepared_at
    }
    pub(crate) fn before(&self) -> &TaskRecord {
        self.before
    }
    pub(crate) fn task(&self) -> &TaskRecord {
        &self.result.task
    }
    pub(crate) fn message(&self) -> &MessageEnvelope {
        &self.result.message
    }
}

impl PreparedControl {
    #[cfg(test)]
    pub(crate) fn original_task_cleanup(
        &self,
        task_id: &str,
    ) -> Result<OriginalControlTaskCleanupView<'_>> {
        ensure!(
            self.invocation.action == ControlAction::Stop,
            "Task cleanup requires original Stop"
        );
        let before = self
            .expected
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .context("original Control Task target missing")?;
        let result = self
            .task_results
            .iter()
            .find(|result| result.task.id == task_id)
            .context("original Control Task result missing")?;
        ensure!(
            self.expected
                .tasks
                .iter()
                .filter(|task| task.id == task_id)
                .count()
                == 1
                && self
                    .task_results
                    .iter()
                    .filter(|result| result.task.id == task_id)
                    .count()
                    == 1,
            "ambiguous original Control Task target"
        );
        Ok(OriginalControlTaskCleanupView {
            command: self,
            before,
            result,
        })
    }

    pub(crate) fn key(&self) -> &ControlKey {
        &self.key
    }
    pub(crate) fn invocation(&self) -> &ControlInvocation {
        &self.invocation
    }
    pub(crate) fn command_digest(&self) -> &str {
        &self.command_digest
    }
    #[cfg(test)]
    pub(crate) fn cancellation_targets(&self) -> &[ControlCancellationTarget] {
        &self.cancellation_targets
    }
    #[cfg(test)]
    pub(crate) fn with_fault(mut self, fault: ControlFaultPoint) -> Self {
        self.fault = Some(fault);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ControlOutcome {
    pub action: ControlAction,
    pub original_status: AgentStatus,
    pub original_current_run_id: Option<String>,
    pub status: AgentStatus,
    pub current_run_id: Option<String>,
    pub aborted_run_id: Option<String>,
    pub released_occupancy_id: Option<String>,
    pub interrupted_task_ids: Vec<String>,
    pub result_message_ids: Vec<String>,
    pub cancelled_wait_ids: Vec<String>,
    pub cancelled_timer_ids: Vec<String>,
    pub cancelled_timer_wake_message_ids: Vec<String>,
    pub aborted_queue_message_ids: Vec<String>,
    pub dropped_timer_queue_message_ids: Vec<String>,
    pub revoked_trigger_ids: Vec<String>,
    pub interrupted_attempt_ids: Vec<String>,
    pub interrupted_turn_ids: Vec<String>,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ControlAuditBinding {
    pub audit_event_id: String,
    pub event_seq: u64,
    pub event_log_epoch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ControlIndexBinding {
    pub change_seq: i64,
    pub source_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ControlReceipt {
    pub family_version: u32,
    pub key: ControlKey,
    pub invocation: ControlInvocation,
    pub invocation_digest: String,
    pub command_digest: String,
    pub outcome: ControlOutcome,
    pub agent_control_revision: u64,
    pub audit_bindings: Vec<ControlAuditBinding>,
    pub index_bindings: Vec<ControlIndexBinding>,
    pub obligation_ids: Vec<String>,
    pub committed_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub(crate) struct ControlCommittedDelta {
    pub state: AgentState,
    pub tasks: Vec<TaskRecord>,
    pub waits: Vec<WaitConditionRecord>,
    pub timers: Vec<TimerRecord>,
    pub timer_wakes: Vec<ControlTimerWake>,
    pub queue_entries: Vec<QueueEntryRecord>,
    pub external_triggers: Vec<ExternalTriggerRecord>,
    pub workspace_occupancy: Option<WorkspaceOccupancyRecord>,
    pub result_messages: Vec<MessageEnvelope>,
    pub turns: Vec<TurnRecord>,
    pub execution_protocol: Option<ExecutionProtocolState>,
    pub transition: TransitionCommit,
}

#[derive(Debug, Clone)]
pub(crate) enum ControlCommit {
    Applied {
        receipt: ControlReceipt,
        delta: Box<ControlCommittedDelta>,
    },
    Replayed {
        receipt: ControlReceipt,
    },
}

/// Current canonical facts used to rebuild RAM after a lost ACK. No historical
/// State/delta is resurrected from the receipt. Exact live leases are separate.
#[derive(Debug, Clone)]
pub(crate) struct ControlRecoveryFacts {
    pub current_queue_messages: Vec<MessageEnvelope>,
    pub waits: Vec<WaitConditionRecord>,
    pub timer_wakes: Vec<ControlTimerWake>,
    pub workspace_occupancy: Option<WorkspaceOccupancyRecord>,
    pub transition: TransitionCommit,
}

#[derive(Debug, Clone)]
pub(crate) struct ControlRecoveryDomains {
    pub identity_publication: Arc<crate::runtime::mutation_owner::IdentityMutationFacts>,
    pub current_reads: Option<Arc<crate::runtime::current_read_facts::ResidentCurrentReadFacts>>,
    pub scheduling: Arc<crate::runtime::resident_scheduling::ResidentSchedulingDomain>,
    pub inputs: Arc<crate::runtime::resident_input_evidence::ResidentInputEvidence>,
    pub work: Arc<crate::runtime::resident_work::ResidentWorkDomain>,
    pub tasks: Arc<crate::runtime::resident_task::ResidentTaskDomain>,
}

#[derive(Debug, Clone)]
pub(crate) enum ControlRecoveryObservation {
    Absent {
        obligations: Vec<ControlCancellationObligation>,
    },
    Current {
        snapshot: Box<ControlSnapshot>,
        episodes: Arc<crate::runtime::resident_state::ResidentEpisodes>,
        domains: Option<ControlRecoveryDomains>,
        receipt: Option<ControlReceipt>,
        obligations: Vec<ControlCancellationObligation>,
        facts: Box<ControlRecoveryFacts>,
    },
    Retired {
        current_identity: Option<AgentIdentityRecord>,
        receipt: Option<ControlReceipt>,
        obligations: Vec<ControlCancellationObligation>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ControlError {
    #[error("Control command identity has a different frozen payload")]
    CommandConflict,
    #[error("Control command invocation does not match the original intent")]
    InvocationConflict,
}

/// This reducer owns no SQL/clock/IDs/handles; all nondeterministic input is
/// frozen once by the admitted runtime operation.
pub(crate) fn prepare_control(input: ControlPlanInput) -> Result<PreparedControl> {
    validate_key(&input.key)?;
    let ControlPlanInput {
        key,
        invocation,
        snapshot,
        prepared_at,
        mut task_results,
        healthy_running,
        protected_control_attempt,
        protected_control_run_id,
        cancellation_targets,
        mut audit_events,
        mut index_changes,
    } = input;
    ensure!(
        snapshot.state.id == key.agent_id
            && snapshot.identity.agent_id == key.agent_id
            && snapshot.identity.incarnation == key.identity_incarnation
            && snapshot.identity.status == AgentRegistryStatus::Active,
        "Control planning requires the exact current Active identity"
    );
    validate_protected_attempt(&invocation, &snapshot, protected_control_attempt.as_deref())?;
    validate_protected_run(
        &invocation,
        &snapshot,
        protected_control_attempt.as_deref(),
        protected_control_run_id.as_deref(),
    )?;
    task_results.sort_by(|left, right| left.task.id.cmp(&right.task.id));
    let scope_id = digest(&serde_json::to_value(&key)?)?;
    let mut candidate = snapshot.state.clone();
    let mut execution_commands = Vec::new();
    let mut interrupted_turns = Vec::new();
    let is_stop = invocation.action == ControlAction::Stop;
    if is_stop {
        validate_task_results(&snapshot, &task_results, prepared_at)?;
        validate_cancellation_targets(
            &snapshot,
            protected_control_run_id.as_deref(),
            &cancellation_targets,
            &task_results,
        )?;
        candidate.status = AgentStatus::Stopped;
        candidate.current_run_id = None;
        candidate.sleeping_until = None;
        candidate.pending_wake_hint = None;
        candidate.active_workspace_entry = None;
        candidate.pending = 0;
        if !task_results.is_empty() {
            candidate.last_wake_reason = Some("TaskResult".into());
            candidate.total_message_count =
                snapshot.message_count.saturating_add(task_results.len());
        }
        if let Some(protocol) = &snapshot.execution_protocol {
            let mut reduced = protocol.clone();
            for attempt in protocol.attempts.values().filter(|attempt| {
                attempt.state == ExecutionAttemptState::Open
                    && Some(attempt.attempt_id.as_str()) != protected_control_attempt.as_deref()
            }) {
                let interrupt = InterruptExecution {
                    attempt_id: attempt.attempt_id.clone(),
                    outcome_id: format!("control:{scope_id}:interrupt:{}", attempt.attempt_id),
                    reason: "agent_stopped".into(),
                    interrupted_at: prepared_at.to_rfc3339(),
                };
                reduced = execution_protocol::interrupt_execution(&reduced, &interrupt)
                    .map_err(|error| anyhow!("Control interruption rejected: {error}"))?
                    .state;
                execution_commands.push(ExecutionProtocolCommand::Interrupt(interrupt));
                let turn_id = attempt.turn_id.as_deref().or_else(|| {
                    snapshot
                        .state
                        .current_execution_binding
                        .as_ref()
                        .filter(|binding| binding_matches_attempt(binding, attempt))
                        .map(|binding| binding.turn_id.as_str())
                });
                if let Some(turn_id) = turn_id {
                    if let Some(turn) = snapshot
                        .attempt_turns
                        .iter()
                        .find(|turn| turn.turn_id == turn_id)
                    {
                        if turn.terminal.is_none() {
                            let mut turn = turn.clone();
                            turn.terminal = Some(TurnTerminalSummary {
                                kind: TurnTerminalKind::Interrupted,
                                reason: Some("agent_stopped".into()),
                                no_brief_reason: turn
                                    .produced_brief_ids
                                    .is_empty()
                                    .then_some(TurnNoBriefReason::Interrupted),
                                completed_at: prepared_at,
                                duration_ms: prepared_at
                                    .signed_duration_since(turn.created_at)
                                    .num_milliseconds()
                                    .max(0) as u64,
                            });
                            interrupted_turns.push(turn);
                        }
                    }
                }
            }
        }
        // A reducer-only input owns a real Turn even though it has no
        // execution attempt. An external Stop must terminalize that exact
        // Turn; the consuming Control itself retains it for normal settlement.
        if snapshot
            .reducer_source_message_id
            .as_deref()
            .is_some_and(|source| protected_message(&invocation) != Some(source))
        {
            let binding = snapshot
                .state
                .current_execution_binding
                .as_ref()
                .context("qualified reducer binding is missing")?;
            let mut turn = snapshot
                .attempt_turns
                .iter()
                .find(|turn| turn.turn_id == binding.turn_id)
                .context("qualified reducer Turn is missing")?
                .clone();
            turn.terminal = Some(TurnTerminalSummary {
                kind: TurnTerminalKind::Interrupted,
                reason: Some("agent_stopped".into()),
                no_brief_reason: turn
                    .produced_brief_ids
                    .is_empty()
                    .then_some(TurnNoBriefReason::Interrupted),
                completed_at: prepared_at,
                duration_ms: prepared_at
                    .signed_duration_since(turn.created_at)
                    .num_milliseconds()
                    .max(0) as u64,
            });
            interrupted_turns.push(turn);
        }
    } else {
        ensure!(
            task_results.is_empty() && cancellation_targets.is_empty(),
            "Start must not contain Stop effects"
        );
        match snapshot.state.status {
            AgentStatus::AwakeRunning => {
                let proof = healthy_running
                    .as_ref()
                    .context("Start requires healthy live run proof")?;
                ensure!(
                    snapshot.state.current_run_id.as_deref() == Some(proof.run_id.as_str()),
                    "Start run proof does not match State"
                );
                if let Some(attempt_id) = proof.attempt_id.as_deref() {
                    let attempt = snapshot
                        .execution_protocol
                        .as_ref()
                        .and_then(|protocol| protocol.attempts.get(attempt_id))
                        .context("Start proof has no canonical attempt")?;
                    ensure!(
                        attempt.state == ExecutionAttemptState::Open
                            && attempt.source_message_id.as_deref()
                                == Some(proof.source_message_id.as_str()),
                        "Start proof does not match its canonical claim/source"
                    );
                    match proof.turn_id.as_deref() {
                        Some(turn_id) => {
                            let binding = snapshot
                                .state
                                .current_execution_binding
                                .as_ref()
                                .context("Start proof has no State execution binding")?;
                            let turn = snapshot
                                .attempt_turns
                                .iter()
                                .find(|turn| turn.turn_id == turn_id)
                                .context("Start proof has no started Turn")?;
                            ensure!(
                                binding_matches_attempt(binding, attempt)
                                    && binding.turn_id == turn_id
                                    && binding.source_message_id == proof.source_message_id
                                    && turn.agent_id == key.agent_id
                                    && turn.run_id.as_deref() == Some(proof.run_id.as_str())
                                    && turn
                                        .trigger
                                        .as_ref()
                                        .and_then(|trigger| trigger.message_id.as_deref())
                                        == Some(proof.source_message_id.as_str())
                                    && turn.terminal.is_none(),
                                "Start proof does not match canonical started execution"
                            );
                        }
                        None => {
                            // A replay claim may still name its persisted prior Turn.
                            // Only a missing preallocated hint needs the qualified
                            // unstarted-input proof; a matching live binding below
                            // still forbids omitting the current started Turn.
                            ensure!(
                                snapshot.unstarted_attempt_ids.contains(attempt_id)
                                    || attempt.turn_id.as_ref().is_some_and(|turn_id| {
                                        snapshot
                                            .attempt_turns
                                            .iter()
                                            .any(|turn| turn.turn_id == *turn_id)
                                    }),
                                "Start pre-Turn proof has no canonical unstarted claim"
                            );
                            ensure!(
                                snapshot.queue_entries.iter().any(|entry| {
                                    entry.message_id == proof.source_message_id
                                        && matches!(
                                            entry.status,
                                            QueueEntryStatus::Dequeued
                                                | QueueEntryStatus::Interjected
                                        )
                                }),
                                "Start pre-Turn proof has no exact claimed input"
                            );
                            ensure!(
                                !snapshot
                                    .state
                                    .current_execution_binding
                                    .as_ref()
                                    .is_some_and(|binding| {
                                        binding_matches_attempt(binding, attempt)
                                    }),
                                "Start proof omitted its already-started Turn"
                            );
                        }
                    }
                } else {
                    let binding = snapshot
                        .state
                        .current_execution_binding
                        .as_ref()
                        .context("Start reducer proof has no State binding")?;
                    ensure!(
                        snapshot.reducer_source_message_id.as_deref()
                            == Some(proof.source_message_id.as_str())
                            && proof.turn_id.as_deref() == Some(binding.turn_id.as_str()),
                        "Start proof does not match its qualified reducer-only Turn/source"
                    );
                }
                // Runtime additionally holds the exact live abort handle/token.
            }
            _ => {
                candidate.status = AgentStatus::AwakeIdle;
                candidate.current_run_id = None;
            }
        }
    }
    let mut outcome = ControlOutcome {
        action: invocation.action.clone(),
        original_status: snapshot.state.status.clone(),
        original_current_run_id: snapshot.state.current_run_id.clone(),
        status: candidate.status.clone(),
        current_run_id: candidate.current_run_id.clone(),
        aborted_run_id: cancellation_targets
            .iter()
            .find(|target| target.target_kind == ControlCancellationKind::Run)
            .map(|target| target.target_identity.clone()),
        released_occupancy_id: is_stop
            .then(|| {
                snapshot
                    .workspace_occupancy
                    .as_ref()
                    .map(|record| record.occupancy_id.clone())
            })
            .flatten(),
        interrupted_task_ids: if is_stop {
            snapshot.tasks.iter().map(|task| task.id.clone()).collect()
        } else {
            Vec::new()
        },
        result_message_ids: task_results
            .iter()
            .map(|result| result.message.id.clone())
            .collect(),
        cancelled_wait_ids: if is_stop {
            snapshot.waits.iter().map(|wait| wait.id.clone()).collect()
        } else {
            Vec::new()
        },
        cancelled_timer_ids: if is_stop {
            snapshot
                .timers
                .iter()
                .map(|timer| timer.id.clone())
                .collect()
        } else {
            Vec::new()
        },
        cancelled_timer_wake_message_ids: if is_stop {
            snapshot
                .pending_timer_wakes
                .iter()
                .map(|wake| wake.message_id.clone())
                .collect()
        } else {
            Vec::new()
        },
        aborted_queue_message_ids: Vec::new(),
        dropped_timer_queue_message_ids: Vec::new(),
        revoked_trigger_ids: if is_stop {
            snapshot
                .external_triggers
                .iter()
                .map(|trigger| trigger.external_trigger_id.clone())
                .collect()
        } else {
            Vec::new()
        },
        interrupted_attempt_ids: execution_commands
            .iter()
            .filter_map(|command| match command {
                ExecutionProtocolCommand::Interrupt(command) => Some(command.attempt_id.clone()),
                _ => None,
            })
            .collect(),
        interrupted_turn_ids: interrupted_turns
            .iter()
            .map(|turn| turn.turn_id.clone())
            .collect(),
        changed: false,
    };
    if is_stop {
        for entry in &snapshot.queue_entries {
            if protected_message(&invocation) == Some(entry.message_id.as_str()) {
                continue;
            }
            if matches!(
                entry.status,
                QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
            ) && snapshot
                .pending_timer_wakes
                .iter()
                .any(|wake| wake.message_id == entry.message_id)
            {
                outcome
                    .dropped_timer_queue_message_ids
                    .push(entry.message_id.clone());
            } else {
                outcome
                    .aborted_queue_message_ids
                    .push(entry.message_id.clone());
            }
        }
        outcome
            .aborted_queue_message_ids
            .extend(task_results.iter().map(|result| result.message.id.clone()));
    }
    outcome.changed = candidate != snapshot.state
        || !outcome.interrupted_task_ids.is_empty()
        || !outcome.cancelled_wait_ids.is_empty()
        || !outcome.cancelled_timer_ids.is_empty()
        || !outcome.cancelled_timer_wake_message_ids.is_empty()
        || !outcome.aborted_queue_message_ids.is_empty()
        || !outcome.dropped_timer_queue_message_ids.is_empty()
        || !outcome.revoked_trigger_ids.is_empty()
        || !execution_commands.is_empty()
        || outcome.released_occupancy_id.is_some();
    // Preserve the old audit contracts inside the same ACK. IDs contain the
    // complete durable namespace rather than assuming caller IDs are global.
    let mut family_events = vec![
        control_audit(
            &scope_id,
            "posture",
            prepared_at,
            "scheduler_posture_decision",
            json!({
                "boundary": "lifecycle_control", "reason": action_name(&invocation.action),
                "previous_status": snapshot.state.status, "next_status": candidate.status,
                "evidence": [format!("previous_run_id={:?}", snapshot.state.current_run_id),
                    format!("next_run_id={:?}", candidate.current_run_id),
                    format!("aborted_run_id={:?}", outcome.aborted_run_id),
                    format!("occupancy_to_release={:?}", outcome.released_occupancy_id)],
            }),
        ),
        control_audit(
            &scope_id,
            "applied",
            prepared_at,
            "control_applied",
            json!({
                "requested_action": invocation.action, "action": invocation.action,
                "status": candidate.status, "current_run_id": candidate.current_run_id,
                "aborted_run_id": outcome.aborted_run_id,
            }),
        ),
    ];
    if is_stop {
        family_events.push(control_audit(&scope_id, "discarded", prepared_at, "agent_stop_state_discarded", json!({
            "agent_id": key.agent_id, "waits_cancelled": outcome.cancelled_wait_ids.len(),
            "timers_cancelled": outcome.cancelled_timer_ids.len(), "queue_entries_aborted": outcome.aborted_queue_message_ids.len(),
        })));
        if let Some(run_id) = &outcome.aborted_run_id {
            family_events.push(control_audit(&scope_id, "run-aborted", prepared_at, "current_run_aborted", json!({
                "agent_id": key.agent_id, "run_id": run_id, "mode": "stop", "reason": "agent_stopped",
            })));
        }
        for trigger in &snapshot.external_triggers {
            family_events.push(control_audit(
                &scope_id,
                &format!("trigger:{}", trigger.external_trigger_id),
                prepared_at,
                "external_trigger_revoked",
                json!({"external_trigger_id": trigger.external_trigger_id,
                    "agent_id": trigger.target_agent_id, "delivery_mode": trigger.delivery_mode}),
            ));
        }
    }
    for result in &task_results {
        family_events.push(control_audit(
            &scope_id,
            &format!("task:{}", result.task.id),
            prepared_at,
            "task_interrupted_on_agent_stop",
            serde_json::to_value(TaskLifecycleAuditEvent::from_task(&result.task))?,
        ));
        let payload = MessageLifecycleAuditEvent::from_message(&result.message);
        family_events.push(control_audit(
            &scope_id,
            &format!("admit:{}", result.message.id),
            prepared_at,
            "message_admitted",
            serde_json::to_value(&payload)?,
        ));
        let mut enqueued = AuditEvent::typed(RuntimeEventKind::MessageEnqueued, &payload)?;
        enqueued.id = format!("audit:control:{scope_id}:enqueue:{}", result.message.id);
        enqueued.created_at = prepared_at;
        family_events.push(enqueued);
        index_changes.push(RuntimeIndexChange {
            agent_id: key.agent_id.clone(),
            source_kind: "task".into(),
            source_id: result.task.id.clone(),
            source_ref: format!("task:{}", result.task.id),
            operation: RuntimeIndexOperation::Upsert,
            source_updated_at: Some(result.task.updated_at),
            reason: "task_written".into(),
        });
    }
    audit_events.extend(family_events);
    let audit_ids: BTreeSet<_> = audit_events.iter().map(|event| event.id.as_str()).collect();
    ensure!(
        audit_ids.len() == audit_events.len(),
        "Control audit identities collide"
    );
    ensure!(
        index_changes
            .iter()
            .all(|change| change.agent_id == key.agent_id),
        "Control index intent scope mismatch"
    );
    let invocation_digest = digest(
        &json!({"family_version": CONTROL_FAMILY_VERSION, "key": key, "invocation": invocation}),
    )?;
    let command_digest = digest(
        &json!({"invocation_digest": invocation_digest, "snapshot": snapshot,
        "candidate": candidate, "prepared_at": prepared_at, "task_results": task_results,
        "protected_control_attempt": protected_control_attempt, "protected_control_run_id": protected_control_run_id,
        "execution_commands": execution_commands,
        "interrupted_turns": interrupted_turns, "cancellation_targets": cancellation_targets,
        "audit_events": audit_events, "index_changes": index_changes.iter().map(index_value).collect::<Vec<_>>(),
        "outcome": outcome}),
    )?;
    Ok(PreparedControl {
        key,
        invocation,
        invocation_digest,
        command_digest,
        expected: Box::new(snapshot),
        candidate: Box::new(candidate),
        prepared_at,
        task_results,
        protected_control_attempt,
        protected_control_run_id,
        execution_commands,
        interrupted_turns,
        cancellation_targets,
        audit_events,
        index_changes,
        outcome,
        #[cfg(test)]
        fault: None,
    })
}

impl RuntimeDb {
    pub(crate) fn load_control_snapshot(&self, key: &ControlKey) -> Result<ControlSnapshot> {
        validate_key(key)?;
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let snapshot = load_snapshot_in(&tx, key)?;
        tx.commit()?;
        Ok(snapshot)
    }

    pub(crate) fn control_receipt(
        &self,
        key: &ControlKey,
        invocation: &ControlInvocation,
        command_digest: Option<&str>,
    ) -> Result<Option<ControlReceipt>> {
        validate_key(key)?;
        load_control_receipt_in(&self.connection()?, key, invocation, command_digest)
    }

    /// Trusted recovery starts from the immutable original key, including retired
    /// scopes. This verifies receipt facts only, not permission to retry an
    /// unresolved attempt or to install a historical State.
    pub(crate) fn control_receipt_for_recovery(
        &self,
        key: &ControlKey,
    ) -> Result<Option<ControlReceipt>> {
        validate_key(key)?;
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let payload: Option<String> = tx
            .query_row(
                "SELECT receipt_json FROM control_command_receipts WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id=?3",
                params![key.agent_id, i64::try_from(key.identity_incarnation)?, key.command_id],
                |row| row.get(0),
            )
            .optional()?;
        let receipt = match payload {
            None => None,
            Some(payload) => {
                let original: ControlReceipt = serde_json::from_str(&payload)?;
                load_control_receipt_in(
                    &tx,
                    key,
                    &original.invocation,
                    Some(&original.command_digest),
                )?
            }
        };
        tx.commit()?;
        Ok(receipt)
    }

    #[cfg(test)]
    pub(crate) fn load_control_recovery(
        &self,
        key: &ControlKey,
        invocation: &ControlInvocation,
        command_digest: &str,
        recent_episode_limit: usize,
    ) -> Result<ControlRecoveryObservation> {
        self.load_control_recovery_inner(
            key,
            invocation,
            command_digest,
            recent_episode_limit,
            None,
        )
    }

    /// The original owner is held. Positive recovery prepares all domains from
    /// one fresh transaction; receipt absence never starts body preparation.
    #[cfg(test)]
    pub(crate) fn load_control_recovery_with_domains(
        &self,
        key: &ControlKey,
        invocation: &ControlInvocation,
        command_digest: &str,
        recent_episode_limit: usize,
        input_request: &super::input_evidence_snapshot::InputEvidenceLoadRequest,
        work_request: &crate::runtime::resident_work::WorkCaptureRequest,
        budget: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<ControlRecoveryObservation> {
        self.load_control_recovery_inner(
            key,
            invocation,
            command_digest,
            recent_episode_limit,
            Some((input_request, work_request, budget, None, None)),
        )
    }

    pub(crate) fn load_control_recovery_with_queries(
        &self,
        key: &ControlKey,
        invocation: &ControlInvocation,
        command_digest: &str,
        recent_episode_limit: usize,
        input_request: &super::input_evidence_snapshot::InputEvidenceLoadRequest,
        work_request: &crate::runtime::resident_work::WorkCaptureRequest,
        budget: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        query: Option<&crate::runtime::task_queries::TaskQuerySelection>,
        event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
    ) -> Result<ControlRecoveryObservation> {
        self.load_control_recovery_inner(
            key,
            invocation,
            command_digest,
            recent_episode_limit,
            Some((input_request, work_request, budget, query, event_roles)),
        )
    }

    fn load_control_recovery_inner(
        &self,
        key: &ControlKey,
        invocation: &ControlInvocation,
        command_digest: &str,
        recent_episode_limit: usize,
        preparation: Option<(
            &super::input_evidence_snapshot::InputEvidenceLoadRequest,
            &crate::runtime::resident_work::WorkCaptureRequest,
            &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
            Option<&crate::runtime::task_queries::TaskQuerySelection>,
            Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
        )>,
    ) -> Result<ControlRecoveryObservation> {
        validate_key(key)?;
        ensure!(
            !command_digest.is_empty(),
            "Control witness digest is empty"
        );
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let identity: Option<AgentIdentityRecord> = payload_optional(
            &tx,
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
            &key.agent_id,
        )?;
        let receipt = load_control_receipt_in(&tx, key, invocation, Some(command_digest))?;
        if receipt.is_none() && preparation.is_some() {
            let obligations = load_obligations_in(&tx, key)?;
            ensure!(
                obligations.is_empty(),
                "Control obligations exist without their original receipt"
            );
            tx.commit()?;
            return Ok(ControlRecoveryObservation::Absent { obligations });
        }
        let observation = if identity.as_ref().is_some_and(|identity| {
            identity.agent_id == key.agent_id
                && identity.incarnation == key.identity_incarnation
                && (identity.status == AgentRegistryStatus::Active
                    || (receipt.is_some() && identity.status == AgentRegistryStatus::Deleting))
        }) {
            let snapshot = load_snapshot_in_with_original_scope(&tx, key, true)?;
            let (episodes, domains, pending) =
                if let (Some(receipt), Some((input, capture, budget, query, event_roles))) =
                    (receipt.as_ref(), preparation)
                {
                    use crate::runtime::resident_work::{WorkFamily, WorkKey};
                    let mut capture = capture.clone();
                    capture.include_live = true;
                    capture.mandatory_roots.extend(
                        receipt
                            .outcome
                            .cancelled_wait_ids
                            .iter()
                            .map(|id| WorkKey::new(WorkFamily::Wait, id)),
                    );
                    capture
                        .required_task_ids
                        .extend(receipt.outcome.interrupted_task_ids.iter().cloned());
                    capture
                        .required_turn_ids
                        .extend(receipt.outcome.interrupted_turn_ids.iter().cloned());
                    capture
                        .required_trigger_message_ids
                        .extend(receipt.outcome.result_message_ids.iter().cloned());
                    if let Some(binding) = &snapshot.state.current_execution_binding {
                        capture.required_turn_ids.insert(binding.turn_id.clone());
                        capture
                            .required_trigger_message_ids
                            .insert(binding.source_message_id.clone());
                    }
                    let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
                        agent_id: key.agent_id.clone(),
                        identity_incarnation: key.identity_incarnation,
                    };
                    let mut input = input.clone();
                    use crate::runtime::resident_input_evidence::{
                        InputEvidenceFamily as Family, InputEvidenceKey as Key,
                    };
                    input.mandatory_roots.extend(
                        snapshot
                            .state
                            .current_turn_id
                            .iter()
                            .map(|id| Key::new(Family::Turn, id)),
                    );
                    if let Some(binding) = &snapshot.state.current_execution_binding {
                        input
                            .mandatory_roots
                            .insert(Key::new(Family::Turn, &binding.turn_id));
                        input
                            .mandatory_roots
                            .insert(Key::new(Family::Message, &binding.source_message_id));
                        input.mandatory_roots.extend(
                            binding
                                .activation_id
                                .iter()
                                .map(|id| Key::new(Family::Attempt, id)),
                        );
                    }
                    let mut task_request =
                        crate::runtime::resident_task::TaskCaptureRequest::current();
                    task_request.required_keys.extend(
                        receipt.outcome.interrupted_task_ids.iter().map(|id| {
                            crate::runtime::resident_task::TaskKey::new(
                                crate::runtime::resident_task::TaskFamily::Task,
                                id,
                            )
                        }),
                    );
                    let current_recovery = current_reads::original_recovery(&tx, receipt);
                    let prepared =
                        super::input_preparation::prepare_control_recovery_with_queries_tx(
                            &tx,
                            &scope,
                            &snapshot.state,
                            &input,
                            recent_episode_limit,
                            budget,
                            &capture,
                            &task_request,
                            query,
                            &current_recovery,
                            event_roles,
                        )?;
                    ensure!(
                        prepared.facts.state.as_ref() == Some(&snapshot.state)
                            && prepared.facts.control_revision == Some(snapshot.control_revision)
                            && prepared.facts.identity.as_ref() == Some(&snapshot.identity),
                        "Control current recovery domains are incoherent"
                    );
                    let work = prepared
                        .work
                        .context("Control recovery Work domain is absent")?;
                    (
                        Arc::new(prepared.episodes),
                        Some(ControlRecoveryDomains {
                            identity_publication: Arc::new(
                                crate::runtime::mutation_owner::IdentityMutationFacts::budgeted(
                                    prepared.facts.identity.as_ref(),
                                    None,
                                    budget,
                                )?,
                            ),
                            current_reads: prepared.current_reads,
                            scheduling: prepared.scheduling,
                            inputs: Arc::new(prepared.inputs.with_budget(budget.clone())?),
                            work,
                            tasks: prepared
                                .tasks
                                .context("Control recovery Task domain is absent")?,
                        }),
                        Some(prepared.pending.messages),
                    )
                } else {
                    let episode_scope = super::episode_snapshot::EpisodeMemoryLoadScope {
                        agent_id: key.agent_id.clone(),
                        identity_incarnation: key.identity_incarnation,
                    };
                    let episodes =
                        super::episode_snapshot::load_episode_memory_for_input_recovery_in(
                            &tx,
                            &episode_scope,
                            recent_episode_limit,
                        )?
                        .require_current()?;
                    ensure!(
                        episodes.state.as_ref() == Some(&snapshot.state)
                            && episodes.control_revision == Some(snapshot.control_revision),
                        "Control/Episode recovery observation is incoherent"
                    );
                    (
                        Arc::new(crate::runtime::resident_state::ResidentEpisodes::bootstrap(
                            &key.agent_id,
                            key.identity_incarnation,
                            recent_episode_limit,
                            !episodes.has_more,
                            episodes.episodes,
                        )?),
                        None,
                        None,
                    )
                };
            let obligations = load_obligations_in(&tx, key)?;
            let facts = load_recovery_facts_in(&tx, &snapshot, receipt.as_ref(), pending)?;
            ControlRecoveryObservation::Current {
                snapshot: Box::new(snapshot),
                episodes,
                domains,
                receipt,
                obligations,
                facts: Box::new(facts),
            }
        } else {
            ControlRecoveryObservation::Retired {
                current_identity: identity,
                receipt,
                obligations: load_obligations_in(&tx, key)?,
            }
        };
        tx.commit()?;
        Ok(observation)
    }

    #[cfg(test)]
    pub(crate) fn commit_control(&self, command: &PreparedControl) -> Result<ControlCommit> {
        self.commit_control_inner(command, None, None, None, None, None)
    }

    #[cfg(test)]
    pub(crate) fn commit_control_with_work_request(
        &self,
        command: &PreparedControl,
        request: &super::work_snapshot::WorkDomainLoadRequest,
    ) -> Result<ControlCommit> {
        self.commit_control_inner(command, Some(request), None, None, None, None)
    }

    pub(crate) fn commit_control_with_domain_requests(
        &self,
        command: &PreparedControl,
        work_request: &super::work_snapshot::WorkDomainLoadRequest,
        task_request: &super::task_snapshot::TaskDomainLoadRequest,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<ControlCommit> {
        self.commit_control_with_domain_requests_and_event_roles(
            command,
            work_request,
            task_request,
            ledger,
            None,
        )
    }
    pub(crate) fn commit_control_with_domain_requests_and_event_roles(
        &self,
        command: &PreparedControl,
        work_request: &super::work_snapshot::WorkDomainLoadRequest,
        task_request: &super::task_snapshot::TaskDomainLoadRequest,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
    ) -> Result<ControlCommit> {
        self.commit_control_with_domain_requests_and_input_source(
            command, work_request, task_request, ledger, event_roles, None,
        )
    }
    pub(crate) fn commit_control_with_domain_requests_and_input_source(
        &self,
        command: &PreparedControl,
        work_request: &super::work_snapshot::WorkDomainLoadRequest,
        task_request: &super::task_snapshot::TaskDomainLoadRequest,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
        input_source: Option<&std::sync::Arc<crate::runtime::mutation_owner::OriginalInputQuerySource>>,
    ) -> Result<ControlCommit> {
        let scheduling_request = super::scheduling_snapshot::SchedulingDomainLoadRequest {
            capture: crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
            record_limit: work_request.record_limit,
            byte_limit: work_request.byte_limit,
        };
        self.commit_control_inner(
            command,
            Some(work_request),
            Some((task_request, ledger)),
            Some((&scheduling_request, ledger)),
            event_roles,
            input_source,
        )
    }

    #[cfg(test)]
    fn commit_control_with_scheduling_request(
        &self,
        command: &PreparedControl,
        work_request: &super::work_snapshot::WorkDomainLoadRequest,
        request: &super::scheduling_snapshot::SchedulingDomainLoadRequest,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<ControlCommit> {
        self.commit_control_inner(
            command,
            Some(work_request),
            None,
            Some((request, ledger)),
            None,
            None,
        )
    }

    fn commit_control_inner(
        &self,
        command: &PreparedControl,
        work_request: Option<&super::work_snapshot::WorkDomainLoadRequest>,
        task_request: Option<(
            &super::task_snapshot::TaskDomainLoadRequest,
            &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        )>,
        scheduling_request: Option<(
            &super::scheduling_snapshot::SchedulingDomainLoadRequest,
            &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        )>,
        event_roles: Option<&crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
        input_source: Option<&std::sync::Arc<crate::runtime::mutation_owner::OriginalInputQuerySource>>,
    ) -> Result<ControlCommit> {
        let result = self.transaction(|tx| {
            // Authentication of an old namespace is performed by the caller.
            if let Some(receipt) = load_control_receipt_in(tx, &command.key, &command.invocation,
                Some(&command.command_digest))? {
                return Ok(ControlCommit::Replayed { receipt });
            }
            let mut scheduling_writes = if command.invocation.action == ControlAction::Stop {
                scheduling_request.map(|(request, ledger)|
                    scheduling_writes::prepare_control_scheduling_writes_tx(tx, command, request, ledger)
                ).transpose()?
            } else { None };
            let mut task_writes = task_request.map(|(request, ledger)|
                task_writes::prepare_control_task_writes_tx(tx, command, request, ledger)
            ).transpose()?;
            let mut current_reads = task_request.map(|(_, ledger)| ledger)
                .or_else(|| scheduling_request.map(|(_, ledger)| ledger))
                .map(|ledger| current_reads::prepare_tx(tx, command, ledger, event_roles)).transpose()?;
            let actual = load_snapshot_in(tx, &command.key)?;
            if actual != *command.expected {
                return Err(RuntimeStateTransitionConflict::concurrent_mutation("control_targets", &command.key.agent_id).into());
            }
            validate_source_tx(tx, command)?;
            for turn in &command.interrupted_turns {
                    super::input_evidence_snapshot::validate_existing_input_evidence_tx(tx,&crate::runtime::resident_input_evidence::InputEvidenceScope { agent_id: command.key.agent_id.clone(), identity_incarnation: command.key.identity_incarnation },
                        &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                            crate::runtime::resident_input_evidence::InputEvidenceFamily::Turn,&turn.turn_id))?;
            }
            for result in &command.task_results {
                ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE evidence_id = ?1)",
                    [&result.message.id], |row| row.get::<_, bool>(0))?,
                    "new Stop TaskResult already has durable evidence without this receipt");
            }
            let prepared_execution = execution_protocol_repository::validate_execution_commands_tx(
                tx, &command.key.agent_id, command.expected.execution_protocol.as_ref(),
                &command.execution_commands, &[], &[], &[])?;
            let input_scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
                agent_id: command.key.agent_id.clone(), identity_incarnation: command.key.identity_incarnation,
            };
            // Observe original addresses before the first business write. The
            // final actual D3 ACK, rather than a command's claimed ID, mints birth.
            let native_input = task_request.map(|(_, ledger)| ledger)
                .or_else(|| scheduling_request.map(|(_, ledger)| ledger))
                .map(|ledger| {
                    let request = work_request.context("Control native Input requires its original Work request")?;
                    super::input_evidence_snapshot::native_birth::NativeInputWriteProof::prepare_with_query_source_tx(
                        tx, &input_scope, |visit| {
                            use crate::runtime::resident_input_evidence::InputEvidenceFamily as Family;
                            for result in &command.task_results {
                                visit(Family::Message, &result.message.id)?;
                            }
                            for turn in &command.interrupted_turns {
                                visit(Family::Turn, &turn.turn_id)?;
                            }
                            // PreparedControl freezes only original Interrupt
                            // commands; their existing Attempt is an update.
                            for execution in &command.execution_commands {
                                let ExecutionProtocolCommand::Interrupt(interrupt) = execution else {
                                    anyhow::bail!("Control native footprint has another execution operation");
                                };
                                visit(Family::Outcome, &interrupt.outcome_id)?;
                            }
                            Ok(())
                        }, ledger, (request.byte_limit, request.record_limit), input_source,
                    )
                }).transpose()?.flatten();
            let is_stop = command.invocation.action == ControlAction::Stop;
            let work_proof = if is_stop {
                work_request.map(|request| {
                    let ids = command.expected.waits.iter().map(|wait| wait.id.clone()).collect();
                    super::work_snapshot::prepare_existing_work_wait_cleanup_tx(tx, &input_scope, &ids, request, false)
                }).transpose()?
            } else { None };
            #[cfg(test)] inject_fault(command, ControlFaultPoint::AfterValidation)?;
            let mut wait_condition_writes = WaitConditionWrites::default();
            let mut result_messages = Vec::new();
            let mut message_index_changes = Vec::new();
            let mut derived_audit_ids = Vec::new();
            for result in &command.task_results {
                ensure!(upsert_task_tx(tx, &result.task)?, "Stop task write unexpectedly became a no-op");
                if let Some(writes) = &mut task_writes { writes.task_written(&result.task.id); }
                let queued = QueueEntryRecord { message_id: result.message.id.clone(),
                    agent_id: result.message.agent_id.clone(), priority: result.message.priority.clone(),
                    status: QueueEntryStatus::Queued, created_at: result.message.created_at,
                    updated_at: command.prepared_at };
                // Keep exact wake/rejoin admission and result evidence together.
                let admission = super::transitions::trigger_wait_and_enqueue_with_current_read_tx(
                    tx, &result.message, &queued, Some(command.prepared_at),
                    current_reads.as_mut().map(|writes| writes.business_mut()),
                )?;
                ensure!(admission.message_inserted && admission.queue_applied,
                    "Stop TaskResult admission was not exact/new");
                message_index_changes.push(RuntimeIndexChange::for_message(&admission.message));
                if let Some(writes) = &mut task_writes {
                    writes.execute_pending(&result.task.id)?;
                } else if let Some(settlement) = TaskResultSettlementRecord::pending(&result.task, &admission.message, command.prepared_at)? {
                    // Explicit legacy/cfg entry preserves its original SQL adapter.
                    task_result_settlement::upsert_pending_tx(tx, &settlement)?;
                }
                derived_audit_ids.extend([
                    format!("audit:wait-triggered:{}", result.message.id),
                    format!("audit:wait-trigger-stale:{}", result.message.id),
                ]);
                wait_condition_writes.extend(admission.wait_writes);
                result_messages.push(admission.message);
            }
            #[cfg(test)] inject_fault(command, ControlFaultPoint::AfterTaskResultWrites)?;
            if is_stop {
                // TriggerWaitAndEnqueue may have made a wait Triggered. Cancel
                // that exact resulting record, preserving its correlation.
                let mut waits = load_waits_in(tx, &command.key.agent_id)?;
                ensure!(waits.iter().map(|wait| &wait.id).collect::<Vec<_>>()
                    == command.expected.waits.iter().map(|wait| &wait.id).collect::<Vec<_>>(),
                    "Stop wait target set changed during TaskResult admission");
                for wait in &mut waits {
                    wait.status = WaitConditionStatus::Cancelled; wait.updated_at = command.prepared_at;
                    wait.cancelled_at = Some(command.prepared_at);
                    wait_condition_writes.extend(upsert_wait_condition_with_writes_tx(tx, wait)?);
                }
                for timer in &command.expected.timers {
                    let mut timer = timer.clone(); timer.status = TimerStatus::Cancelled;
                    // Existing lifecycle Stop retains next_fire_at as history.
                    if upsert_timer_with_write_tx(tx, &timer)? {
                        if let Some(writes) = &mut scheduling_writes { writes.timer_written(&timer.id); }
                    }
                }
                for wake in &command.expected.pending_timer_wakes {
                    let changed = tx.execute("UPDATE timer_wakes SET status = 'cancelled', updated_at = ?3, cancelled_at = ?3
                        WHERE timer_id = ?1 AND message_id = ?2 AND status = 'pending' AND fire_count = ?4",
                        params![wake.timer_id, wake.message_id, command.prepared_at.to_rfc3339(), i64::try_from(wake.fire_count)?])?;
                    ensure!(changed == 1, "Stop timer wake CAS failed");
                    if let Some(writes) = &mut scheduling_writes { writes.wake_written(&wake.timer_id, &wake.message_id); }
                }
            }
            #[cfg(test)] inject_fault(command, ControlFaultPoint::AfterWaitTimerWrites)?;
            let mut final_queue = Vec::new();
            if is_stop {
                for expected in &command.expected.queue_entries {
                    if protected_message(&command.invocation) == Some(expected.message_id.as_str()) { continue; }
                    let mut record = expected.clone(); record.updated_at = command.prepared_at;
                    record.status = if command.outcome.dropped_timer_queue_message_ids.contains(&record.message_id) {
                        QueueEntryStatus::Dropped
                    } else { QueueEntryStatus::Aborted };
                    ensure!(match &mut current_reads {
                        Some(writes) => writes.compare_queue_tx(tx, expected, &record)?,
                        None => compare_and_set_queue_entry_tx(tx, expected, &record)?,
                    }, "Stop queue CAS failed");
                    final_queue.push(record);
                }
                for message in &result_messages {
                    let mut record: QueueEntryRecord = payload_optional(tx,
                        "SELECT payload_json FROM queue_entries WHERE message_id = ?1", &message.id)?.context("Stop result queue missing")?;
                    record.status = QueueEntryStatus::Aborted; record.updated_at = command.prepared_at;
                    match &mut current_reads {
                        Some(writes) => { writes.business_mut().upsert_queue_tx(tx, &record)?; }
                        None => { upsert_queue_entry_tx(tx, &record)?; }
                    }
                    final_queue.push(record);
                }
                for trigger in &command.expected.external_triggers {
                    let mut trigger = trigger.clone(); trigger.status = ExternalTriggerStatus::Revoked;
                    trigger.revoked_at = Some(command.prepared_at);
                    if upsert_external_trigger_with_write_tx(tx, &trigger)? {
                        if let Some(writes) = &mut scheduling_writes { writes.trigger_written(&trigger.external_trigger_id); }
                    }
                }
            }
            #[cfg(test)] inject_fault(command, ControlFaultPoint::AfterQueueTriggerWrites)?;
            let mut occupancy = command.expected.workspace_occupancy.clone();
            if is_stop {
                if let Some(record) = &mut occupancy {
                    record.released_at = Some(command.prepared_at); upsert_workspace_occupancy_tx(tx, record)?;
                }
                for turn in &command.interrupted_turns {
                    upsert_turn_record_tx(tx, turn)?;
                }
            }
            let (_, mut input_delta) = execution_protocol_repository::persist_execution_commands_with_input_facts_tx(tx, prepared_execution, Some(&input_scope))?;
            #[cfg(test)] inject_fault(command, ControlFaultPoint::AfterExecutionWorkspaceWrites)?;
            if command.candidate.as_ref() != &command.expected.state {
                upsert_agent_state_at_tx(tx, &command.candidate, command.prepared_at)?;
            }
            let persisted: AgentState = payload_optional(tx, "SELECT payload_json FROM agent_states WHERE agent_id = ?1", &command.key.agent_id)?
                .context("Control State disappeared")?;
            ensure!(persisted == *command.candidate, "Control candidate was not persisted exactly");
            #[cfg(test)] inject_fault(command, ControlFaultPoint::AfterStateWrite)?;
            // Trigger admission and Stop cancellation share one final Work
            // version, after the exact State epilogue and before the receipt.
            let work = work_proof.as_ref().zip(work_request).map(|(proof, request)| {
                super::work_snapshot::register_existing_work_wait_cleanup_tx(
                    tx, proof, &wait_condition_writes, Some(&persisted), request,
                )
            }).transpose()?.flatten();
            // Actual D3 Message ACKs also advance TaskOutput source metadata.
            // Join those exact sources into the single final Task coverage.
            let mut input_facts = result_messages.iter().cloned().map(super::input_evidence_snapshot::CanonicalInputEvidence::Message).collect::<Vec<_>>();
            input_facts.extend(command.interrupted_turns.iter().cloned().map(super::input_evidence_snapshot::CanonicalInputEvidence::Turn));
            let evidence_delta = super::input_evidence_snapshot::register_committed_input_evidence_tx(tx, &input_scope, &input_facts)?;
            super::input_evidence_writes::merge_input_evidence_delta(&mut input_delta,evidence_delta)?;
            if let Some(native) = native_input {
                native.finish_delta_tx(tx, &mut input_delta)?;
            }
            let task_facts = task_writes.map(|writes|
                writes.finish(tx, &persisted, task_request.expect("Task request retained").1)
            ).transpose()?;
            let scheduling_facts = scheduling_writes.map(|writes| writes.finish(tx, &persisted)).transpose()?;
            let mut audit_bindings = Vec::new();
            let mut audit_events = Vec::new();
            for event in &command.audit_events {
                let (event, inserted) = match &mut current_reads {
                    Some(writes) => writes.business_mut().append_event_tx(tx, Some(&command.key.agent_id), event)?,
                    None => append_audit_event_tx(tx, Some(&command.key.agent_id), event)?,
                };
                audit_bindings.push(ControlAuditBinding { audit_event_id: event.id.clone(), event_seq: event.event_seq,
                    event_log_epoch: event.event_log_epoch.clone() });
                if inserted { audit_events.push(event); }
            }
            // Include helper-produced correlation events in the receipt/effects.
            for id in derived_audit_ids {
                if let Some(event) = payload_optional::<AuditEvent>(tx,
                    "SELECT data_json FROM audit_events WHERE audit_event_id = ?1", &id)? {
                    if !audit_bindings.iter().any(|binding| binding.audit_event_id == id) {
                        audit_bindings.push(ControlAuditBinding { audit_event_id: event.id.clone(), event_seq: event.event_seq,
                            event_log_epoch: event.event_log_epoch.clone() });
                        audit_events.push(event);
                    }
                }
            }
            #[cfg(test)] inject_fault(command, ControlFaultPoint::AfterAuditWrites)?;
            let mut changes = command.index_changes.clone(); changes.extend(message_index_changes);
            let mut index_bindings = Vec::new();
            for change in &changes {
                insert_runtime_index_changes_at_tx(tx, std::slice::from_ref(change), command.prepared_at)?;
                let change_seq = tx.query_row("SELECT produced_change_seq FROM runtime_index_outbox_watermarks WHERE agent_id = ?1",
                    [&command.key.agent_id], |row| row.get::<_, i64>(0))?;
                index_bindings.push(ControlIndexBinding { change_seq, source_ref: change.source_ref.clone() });
            }
            #[cfg(test)] inject_fault(command, ControlFaultPoint::AfterIndexWrites)?;
            let current_facts = current_reads.map(|writes| writes.finish(&persisted)).transpose()?;
            let revision: i64 = tx.query_row("SELECT control_revision FROM agent_states WHERE agent_id = ?1",
                [&command.key.agent_id], |row| row.get(0))?;
            let receipt = ControlReceipt { family_version: CONTROL_FAMILY_VERSION, key: command.key.clone(),
                invocation: command.invocation.clone(), invocation_digest: command.invocation_digest.clone(),
                command_digest: command.command_digest.clone(), outcome: command.outcome.clone(),
                agent_control_revision: u64::try_from(revision)?, audit_bindings, index_bindings,
                obligation_ids: command.cancellation_targets.iter().map(|target| target.obligation_id.clone()).collect(),
                committed_at: command.prepared_at };
            tx.execute("INSERT INTO control_command_receipts (agent_id,identity_incarnation,command_id,family_version,
                action,invocation_digest,command_digest,committed_at,outcome_json,receipt_json)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![receipt.key.agent_id,
                i64::try_from(receipt.key.identity_incarnation)?, receipt.key.command_id, CONTROL_FAMILY_VERSION,
                action_name(&receipt.invocation.action), receipt.invocation_digest, receipt.command_digest,
                receipt.committed_at.to_rfc3339(), serde_json::to_string(&receipt.outcome)?, serde_json::to_string(&receipt)?])?;
            #[cfg(test)] inject_fault(command, ControlFaultPoint::AfterReceiptWrite)?;
            for target in &command.cancellation_targets {
                tx.execute("INSERT INTO control_cancellation_obligations (agent_id,identity_incarnation,command_id,obligation_id,
                    target_kind,target_identity,target_generation_json,created_at,state,updated_at,target_json)
                    VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'pending',?8,?9)", params![command.key.agent_id,
                    i64::try_from(command.key.identity_incarnation)?, command.key.command_id, target.obligation_id,
                    cancellation_kind_name(target.target_kind), target.target_identity,
                    serde_json::to_string(&target.target_generation)?, command.prepared_at.to_rfc3339(), serde_json::to_string(target)?])?;
            }
            #[cfg(test)] inject_fault(command, ControlFaultPoint::AfterObligationWrites)?;
            let tasks = records_by_ids::<TaskRecord>(tx, "tasks", "task_id", &receipt.outcome.interrupted_task_ids)?;
            let waits = receipt.outcome.cancelled_wait_ids.iter().map(|id| load_wait_by_id(tx, id)).collect::<Result<Vec<_>>>()?;
            let timers = records_by_ids::<TimerRecord>(tx, "timers", "timer_id", &receipt.outcome.cancelled_timer_ids)?;
            let external_triggers = records_by_ids::<ExternalTriggerRecord>(tx, "external_triggers", "external_trigger_id", &receipt.outcome.revoked_trigger_ids)?;
            let timer_wakes = load_timer_wakes_by_message_ids_in(tx, &command.key.agent_id,
                &receipt.outcome.cancelled_timer_wake_message_ids)?;
            let turns = records_by_ids::<TurnRecord>(tx, "turn_records", "turn_id", &receipt.outcome.interrupted_turn_ids)?;
            let execution_protocol = if command.expected.execution_protocol.is_some() {
                Some(execution_protocol_repository::load_state_tx(tx, &command.key.agent_id)?)
            } else { None };
            for (task, planned) in tasks.iter().zip(&command.task_results) {
                ensure!(task == &slim_task_record_for_payload(&planned.task), "Control Task was not persisted exactly");
            }
            ensure!(waits.iter().all(|wait| wait.status == WaitConditionStatus::Cancelled
                && wait.cancelled_at.is_some_and(|at| at.timestamp_millis() == command.prepared_at.timestamp_millis())
                && wait.updated_at.timestamp_millis() == command.prepared_at.timestamp_millis()),
                "Control wait cancellation was not persisted exactly");
            for (timer, before) in timers.iter().zip(&command.expected.timers) {
                let mut expected = before.clone(); expected.status = TimerStatus::Cancelled;
                ensure!(timer == &expected, "Control Timer was not persisted exactly");
            }
            for (trigger, before) in external_triggers.iter().zip(&command.expected.external_triggers) {
                let mut expected = before.clone(); expected.status = ExternalTriggerStatus::Revoked; expected.revoked_at = Some(command.prepared_at);
                ensure!(trigger == &expected, "Control trigger revoke was not persisted exactly");
            }
            if let Some(expected) = &occupancy {
                let stored: WorkspaceOccupancyRecord = payload_optional(tx,
                    "SELECT payload_json FROM workspace_occupancies WHERE occupancy_id = ?1", &expected.occupancy_id)?.context("Control occupancy disappeared")?;
                ensure!(stored == *expected, "Control occupancy release was not persisted exactly");
            }
            let work_input_preparation = work.as_ref().map(|work| super::transitions::WorkInputPreparation {
                scope: input_scope.clone(), work_revision: work.delta.revision,
                owning_roots: work.owning_input_roots.clone(),
                completion_heads: work.delta.coverage.latest_results.iter()
                    .filter_map(|(id, head)| head.clone().map(|head| (id.clone(), head))).collect(),
            });
            let mut transition = TransitionCommit { applied: true, effects: Box::new(PostCommitEffects {
                work_delta_requires_prepared_domain: work.is_some(),
                work_delta: work.map(|work| work.delta), work_input_preparation, wait_condition_writes,
                agent_state: (persisted != command.expected.state).then(|| AgentStateMutation { expected: Some(Box::new(command.expected.state.clone())), record: Box::new(persisted.clone()) }),
                removed_queue_message_ids: final_queue.iter().map(|entry| entry.message_id.clone()).collect(),
                timers: timers.clone(), tasks: tasks.clone(), external_triggers: external_triggers.clone(),
                messages: result_messages.clone(), turns: turns.clone(), execution_protocol: execution_protocol.clone(), input_evidence_delta: input_delta,
                audit_events, current_read_maintenance: current_facts.map(Arc::new), notify_memory_index: !changes.is_empty(), notify_scheduler: true, ..Default::default()
            }), ..Default::default() };
            if let Some(mut scheduling) = scheduling_facts {
                transition.effects.scheduling_material_budget_views = scheduling.facts.take_budget_views();
                transition.effects.scheduling_delta_requires_prepared_domain = true;
                transition.effects.scheduling_write_keys = scheduling.actual;
                transition.effects.scheduling_delta = Some(Box::new(scheduling.facts.delta));
            }
            if let Some(tasks) = task_facts {
                transition.effects.task_delta = Some(tasks.delta);
                transition.effects.task_delta_requires_prepared_domain = true;
                transition.effects.task_write_keys = tasks.actual;
                transition.effects.task_observation_keys = tasks.observations;
                transition.effects.task_material_budget_views = tasks.guards;
            }
            Ok(ControlCommit::Applied { receipt, delta: Box::new(ControlCommittedDelta { state: persisted, tasks, waits,
                timers, timer_wakes, queue_entries: final_queue, external_triggers, workspace_occupancy: occupancy,
                result_messages, turns, execution_protocol, transition }) })
        })?;
        #[cfg(test)]
        if command.fault == Some(ControlFaultPoint::AfterCommitBeforeAck)
            && matches!(&result, ControlCommit::Applied { .. })
        {
            return Err(super::persistence::mark_persistence_failure(
                anyhow!("injected Control lost commit acknowledgement"),
                super::PersistenceCertainty::Indeterminate,
                super::PersistenceFailurePhase::Commit,
                None,
            ));
        }
        Ok(result)
    }
}

fn load_snapshot_in(connection: &Connection, key: &ControlKey) -> Result<ControlSnapshot> {
    load_snapshot_in_with_original_scope(connection, key, false)
}
fn load_snapshot_in_with_original_scope(
    connection: &Connection,
    key: &ControlKey,
    original_recovery: bool,
) -> Result<ControlSnapshot> {
    let identity: AgentIdentityRecord = payload_optional(
        connection,
        "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
        &key.agent_id,
    )?
    .context("Control requires a durable identity")?;
    ensure!(
        identity.agent_id == key.agent_id
            && identity.incarnation == key.identity_incarnation
            && (identity.status == AgentRegistryStatus::Active
                || (original_recovery && identity.status == AgentRegistryStatus::Deleting)),
        "Control identity scope is retired"
    );
    let (state_payload, revision): (String, i64) = connection.query_row(
        "SELECT payload_json, control_revision FROM agent_states WHERE agent_id = ?1",
        [&key.agent_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let state: AgentState = serde_json::from_str(&state_payload)?;
    ensure!(
        state.id == key.agent_id,
        "Control State payload has a different scope"
    );
    let tasks = payloads(connection, "SELECT payload_json FROM tasks WHERE owner_agent_id = ?1 AND status IN ('queued','running','cancelling') ORDER BY task_id", &key.agent_id)?;
    let waits = load_waits_in(connection, &key.agent_id)?;
    let timers = payloads(connection, "SELECT payload_json FROM timers WHERE agent_id = ?1 AND status = 'active' ORDER BY timer_id", &key.agent_id)?;
    let queue_entries = payloads(connection, "SELECT payload_json FROM queue_entries WHERE agent_id = ?1 AND status IN ('queued','interrupted','dequeued','interjected') ORDER BY message_id", &key.agent_id)?;
    let external_triggers = payloads(connection, "SELECT payload_json FROM external_triggers WHERE target_agent_id = ?1 AND status = 'active' ORDER BY external_trigger_id", &key.agent_id)?;
    let workspace_occupancy = if let Some(id) = state
        .active_workspace_entry
        .as_ref()
        .and_then(|entry| entry.occupancy_id.as_deref())
    {
        let occupancy: WorkspaceOccupancyRecord = payload_optional(
            connection,
            "SELECT payload_json FROM workspace_occupancies WHERE occupancy_id = ?1",
            id,
        )?
        .context("Control workspace occupancy is missing")?;
        ensure!(
            occupancy.holder_agent_id == key.agent_id
                && occupancy.released_at.is_none()
                && state
                    .active_workspace_entry
                    .as_ref()
                    .is_some_and(
                        |entry| entry.execution_root_id == occupancy.execution_root_id
                            && entry.workspace_id == occupancy.workspace_id
                    ),
            "Control occupancy does not match its exact holder/root"
        );
        Some(occupancy)
    } else {
        None
    };
    let execution_protocol =
        execution_protocol_repository::context_execution_protocol_on(connection, &key.agent_id)?;
    let mut attempt_turns = Vec::new();
    let mut turn_ids = BTreeSet::new();
    let mut unstarted_attempt_ids = BTreeSet::new();
    if let Some(protocol) = &execution_protocol {
        for attempt in protocol
            .attempts
            .values()
            .filter(|attempt| attempt.state == ExecutionAttemptState::Open)
        {
            let unstarted = claimed_input_has_not_started(
                connection,
                &key.agent_id,
                &state,
                &queue_entries,
                attempt,
            )?;
            if unstarted {
                unstarted_attempt_ids.insert(attempt.attempt_id.clone());
            }
            if let Some(turn_id) = &attempt.turn_id {
                if !turn_ids.contains(turn_id) {
                    let turn: Option<TurnRecord> = payload_optional(
                        connection,
                        "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
                        turn_id,
                    )?;
                    if let Some(turn) = turn {
                        ensure!(
                            turn.agent_id == key.agent_id && turn.turn_id == *turn_id,
                            "Control target Turn scope mismatch"
                        );
                        turn_ids.insert(turn_id.clone());
                        attempt_turns.push(turn);
                    } else {
                        // Admission freezes a Message's preallocated Turn hint.
                        // It is not evidence that begin_interactive_turn ran.
                        ensure!(unstarted, "Open Control target attempt has no Turn");
                    }
                }
            }
        }
    }
    if let Some(binding) = &state.current_execution_binding {
        if !turn_ids.contains(&binding.turn_id) {
            let turn: TurnRecord = payload_optional(
                connection,
                "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
                &binding.turn_id,
            )?
            .context("Control State execution binding has no canonical Turn")?;
            ensure!(
                turn.agent_id == key.agent_id && turn.turn_id == binding.turn_id,
                "Control State Turn scope mismatch"
            );
            attempt_turns.push(turn);
        }
    }
    attempt_turns.sort_by(|left, right| left.turn_id.cmp(&right.turn_id));
    let reducer_source = qualified_reducer_source(
        connection,
        &key.agent_id,
        &state,
        &queue_entries,
        execution_protocol.as_ref(),
        &attempt_turns,
    )?;
    let reducer_control_source_message_id = reducer_source.as_ref()
        .filter(|message| message.kind == MessageKind::Control
            && matches!(&message.body, MessageBody::Text { text } if text == "start" || text == "stop"))
        .map(|message| message.id.clone());
    let reducer_source_message_id = reducer_source.map(|message| message.id);
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM messages WHERE agent_id = ?1",
        [&key.agent_id],
        |row| row.get(0),
    )?;
    Ok(ControlSnapshot {
        identity,
        state,
        control_revision: u64::try_from(revision)?,
        message_count: usize::try_from(count)?,
        tasks,
        waits,
        timers,
        pending_timer_wakes: load_pending_timer_wakes_in(connection, &key.agent_id)?,
        queue_entries,
        external_triggers,
        workspace_occupancy,
        execution_protocol,
        attempt_turns,
        unstarted_attempt_ids,
        reducer_control_source_message_id,
        reducer_source_message_id,
    })
}

fn qualified_reducer_source(
    connection: &Connection,
    agent_id: &str,
    state: &AgentState,
    queue: &[QueueEntryRecord],
    protocol: Option<&ExecutionProtocolState>,
    turns: &[TurnRecord],
) -> Result<Option<MessageEnvelope>> {
    let Some(binding) = state.current_execution_binding.as_ref() else {
        return Ok(None);
    };
    if state.status != AgentStatus::AwakeRunning
        || state.current_run_id.is_none()
        || state.current_turn_id.as_deref() != Some(binding.turn_id.as_str())
        || binding.activation_id.is_some()
        || binding.owner.is_some()
        || protocol.is_some_and(|protocol| {
            protocol.attempts.values().any(|attempt| {
                attempt.state == ExecutionAttemptState::Open
                    && attempt.source_message_id.as_deref()
                        == Some(binding.source_message_id.as_str())
            })
        })
        || !queue.iter().any(|entry| {
            entry.agent_id == agent_id
                && entry.message_id == binding.source_message_id
                && entry.status == QueueEntryStatus::Dequeued
        })
    {
        return Ok(None);
    }
    let Some(turn) = turns.iter().find(|turn| turn.turn_id == binding.turn_id) else {
        return Ok(None);
    };
    if turn.agent_id != agent_id
        || turn.turn_index != state.turn_index
        || turn.run_id != state.current_run_id
        || turn.terminal.is_some()
        || !turn.input_message_ids.contains(&binding.source_message_id)
    {
        return Ok(None);
    }
    let source: Option<(String, Option<i64>)> = connection
        .query_row(
            "SELECT payload_json,message_seq FROM messages WHERE message_id=?1 AND agent_id=?2",
            params![binding.source_message_id, agent_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((payload, sequence)) = source else {
        return Ok(None);
    };
    let message: MessageEnvelope = serde_json::from_str(&payload)?;
    if message.id != binding.source_message_id
        || message.agent_id != agent_id
        || sequence.is_none()
        || message.message_seq.and_then(|seq| i64::try_from(seq).ok()) != sequence
        || turn.trigger.as_ref() != Some(&crate::types::TurnTriggerSummary::from_message(&message))
    {
        return Ok(None);
    }
    let assignment: Option<(String, String)> = connection
        .query_row(
            "SELECT agent_id,turn_id FROM conversation_input_assignments WHERE message_id=?1",
            [&message.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok((assignment == Some((agent_id.to_owned(), turn.turn_id.clone()))).then_some(message))
}

fn claimed_input_has_not_started(
    connection: &Connection,
    agent_id: &str,
    state: &AgentState,
    queue: &[QueueEntryRecord],
    attempt: &execution_protocol::ExecutionAttempt,
) -> Result<bool> {
    let Some(source_id) = attempt.source_message_id.as_deref() else {
        return Ok(false);
    };
    if attempt.agent_id != agent_id
        || state.status != AgentStatus::AwakeRunning
        || state.current_run_id.is_none()
        || state
            .current_execution_binding
            .as_ref()
            .is_some_and(|binding| {
                binding_matches_attempt(binding, attempt)
                    || attempt.turn_id.as_deref() == Some(binding.turn_id.as_str())
            })
        || attempt
            .turn_id
            .as_ref()
            .is_some_and(|turn_id| state.current_turn_id.as_ref() == Some(turn_id))
        || !queue.iter().any(|entry| {
            entry.agent_id == agent_id
                && entry.message_id == source_id
                && matches!(
                    entry.status,
                    QueueEntryStatus::Dequeued | QueueEntryStatus::Interjected
                )
        })
    {
        return Ok(false);
    }
    let source: Option<(String, Option<i64>)> = connection.query_row(
        "SELECT payload_json, message_seq FROM messages WHERE message_id = ?1 AND agent_id = ?2",
        params![source_id, agent_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((payload, message_seq)) = source else {
        return Ok(false);
    };
    let message: MessageEnvelope = serde_json::from_str(&payload)?;
    if message.id != source_id
        || message.agent_id != agent_id
        || message.message_seq.and_then(|seq| i64::try_from(seq).ok()) != message_seq
        || message.message_seq != Some(attempt.source.generation)
        || message.message_seq != Some(attempt.admitted_fences.source_revision)
        || attempt.turn_id != message.turn_id
    {
        return Ok(false);
    }
    // Unlike Message/attempt.turn_id, this relation means the input really
    // entered a canonical Turn. Never reinterpret a damaged started Turn as
    // the earlier claim-only window (including a foreign-scope assignment).
    let assigned: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM conversation_input_assignments WHERE message_id = ?1)",
        [source_id],
        |row| row.get(0),
    )?;
    Ok(!assigned)
}

fn load_recovery_facts_in(
    connection: &Connection,
    snapshot: &ControlSnapshot,
    receipt: Option<&ControlReceipt>,
    prepared_pending: Option<Vec<MessageEnvelope>>,
) -> Result<ControlRecoveryFacts> {
    let outcome = receipt.map(|receipt| &receipt.outcome);
    let task_ids = merged_ids(
        snapshot.tasks.iter().map(|record| record.id.clone()),
        outcome.map(|outcome| &outcome.interrupted_task_ids),
    );
    let timer_ids = merged_ids(
        snapshot.timers.iter().map(|record| record.id.clone()),
        outcome.map(|outcome| &outcome.cancelled_timer_ids),
    );
    let trigger_ids = merged_ids(
        snapshot
            .external_triggers
            .iter()
            .map(|record| record.external_trigger_id.clone()),
        outcome.map(|outcome| &outcome.revoked_trigger_ids),
    );
    let wait_ids = merged_ids(
        snapshot.waits.iter().map(|record| record.id.clone()),
        outcome.map(|outcome| &outcome.cancelled_wait_ids),
    );
    let turn_ids = merged_ids(
        snapshot
            .attempt_turns
            .iter()
            .map(|record| record.turn_id.clone()),
        outcome.map(|outcome| &outcome.interrupted_turn_ids),
    );
    let tasks = records_by_ids(connection, "tasks", "task_id", &task_ids)?;
    let timers = records_by_ids(connection, "timers", "timer_id", &timer_ids)?;
    let external_triggers = records_by_ids(
        connection,
        "external_triggers",
        "external_trigger_id",
        &trigger_ids,
    )?;
    let waits = wait_ids
        .iter()
        .map(|id| load_wait_by_id(connection, id))
        .collect::<Result<Vec<_>>>()?;
    let turns = records_by_ids(connection, "turn_records", "turn_id", &turn_ids)?;
    let current_queue_messages = if let Some(messages) = prepared_pending {
        let ids = messages
            .iter()
            .map(|message| message.id.as_str())
            .collect::<BTreeSet<_>>();
        let expected = snapshot
            .queue_entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.status,
                    QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
                )
            })
            .map(|entry| entry.message_id.as_str())
            .collect::<BTreeSet<_>>();
        ensure!(
            ids == expected && messages.len() == expected.len(),
            "Control prepared pending queue differs from current snapshot"
        );
        messages
    } else {
        let mut current_queue_messages = Vec::new();
        for entry in snapshot.queue_entries.iter().filter(|entry| {
            matches!(
                entry.status,
                QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
            )
        }) {
            let message: MessageEnvelope = payload_optional(
                connection,
                "SELECT payload_json FROM messages WHERE evidence_id = ?1",
                &entry.message_id,
            )?
            .context("Control recovery pending queue has no canonical Message")?;
            ensure!(
                message.id == entry.message_id
                    && message.agent_id == entry.agent_id
                    && message.message_seq.is_some(),
                "Control recovery queue evidence binding is invalid"
            );
            current_queue_messages.push(message);
        }
        current_queue_messages
    };
    let mut removed = BTreeSet::new();
    if let Some(outcome) = outcome {
        for id in outcome
            .aborted_queue_message_ids
            .iter()
            .chain(&outcome.dropped_timer_queue_message_ids)
        {
            let entry: QueueEntryRecord = payload_optional(
                connection,
                "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
                id,
            )?
            .context("Control receipt queue target is missing")?;
            ensure!(
                entry.agent_id == snapshot.state.id,
                "Control recovery queue scope mismatch"
            );
            if !matches!(
                entry.status,
                QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
            ) {
                removed.insert(id.clone());
            }
        }
    }
    let result_messages = if let Some(outcome) = outcome {
        records_by_ids::<MessageEnvelope>(
            connection,
            "messages",
            "evidence_id",
            &outcome.result_message_ids,
        )?
    } else {
        Vec::new()
    };
    let workspace_occupancy =
        if let Some(id) = outcome.and_then(|outcome| outcome.released_occupancy_id.as_deref()) {
            Some(
                payload_optional(
                    connection,
                    "SELECT payload_json FROM workspace_occupancies WHERE occupancy_id = ?1",
                    id,
                )?
                .context("Control receipt occupancy target is missing")?,
            )
        } else {
            snapshot.workspace_occupancy.clone()
        };
    let wake_ids = merged_ids(
        snapshot
            .pending_timer_wakes
            .iter()
            .map(|wake| wake.message_id.clone()),
        outcome.map(|outcome| &outcome.cancelled_timer_wake_message_ids),
    );
    Ok(ControlRecoveryFacts {
        current_queue_messages,
        waits,
        timer_wakes: load_timer_wakes_by_message_ids_in(connection, &snapshot.state.id, &wake_ids)?,
        workspace_occupancy,
        transition: TransitionCommit {
            applied: true,
            effects: Box::new(PostCommitEffects {
                agent_state: Some(AgentStateMutation {
                    expected: None,
                    record: Box::new(snapshot.state.clone()),
                }),
                tasks,
                timers,
                external_triggers,
                messages: result_messages,
                turns,
                execution_protocol: snapshot.execution_protocol.clone(),
                removed_queue_message_ids: removed.into_iter().collect(),
                notify_scheduler: true,
                ..Default::default()
            }),
            ..Default::default()
        },
    })
}
fn merged_ids(current: impl Iterator<Item = String>, touched: Option<&Vec<String>>) -> Vec<String> {
    current
        .chain(touched.into_iter().flatten().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn load_timer_wakes_by_message_ids_in(
    connection: &Connection,
    agent_id: &str,
    ids: &[String],
) -> Result<Vec<ControlTimerWake>> {
    let mut statement = connection.prepare("SELECT w.timer_id,w.message_id,w.fire_count,w.status,w.created_at,w.updated_at,w.incorporated_at,w.cancelled_at
        FROM timer_wakes w JOIN timers t ON t.timer_id = w.timer_id WHERE t.agent_id = ?1 AND w.message_id = ?2")?;
    ids.iter()
        .map(|id| {
            statement
                .query_row(params![agent_id, id], |row| {
                    Ok(ControlTimerWake {
                        timer_id: row.get(0)?,
                        message_id: row.get(1)?,
                        fire_count: row.get(2)?,
                        status: row.get(3)?,
                        created_at: row.get(4)?,
                        updated_at: row.get(5)?,
                        incorporated_at: row.get(6)?,
                        cancelled_at: row.get(7)?,
                    })
                })
                .with_context(|| format!("Control timer wake target is missing: {id}"))
        })
        .collect()
}

fn load_pending_timer_wakes_in(
    connection: &Connection,
    agent_id: &str,
) -> Result<Vec<ControlTimerWake>> {
    let mut statement = connection.prepare("SELECT w.timer_id,w.message_id,w.fire_count,w.status,w.created_at,w.updated_at,w.incorporated_at,w.cancelled_at
        FROM timer_wakes w JOIN timers t ON t.timer_id = w.timer_id WHERE t.agent_id = ?1
        AND w.status = 'pending' ORDER BY w.timer_id,w.message_id")?;
    let rows = statement.query_map([agent_id], |row| {
        Ok(ControlTimerWake {
            timer_id: row.get(0)?,
            message_id: row.get(1)?,
            fire_count: row.get::<_, u64>(2)?,
            status: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
            incorporated_at: row.get(6)?,
            cancelled_at: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn load_control_receipt_in(
    connection: &Connection,
    key: &ControlKey,
    invocation: &ControlInvocation,
    command_digest: Option<&str>,
) -> Result<Option<ControlReceipt>> {
    let row = connection.query_row("SELECT family_version,action,invocation_digest,command_digest,committed_at,outcome_json,receipt_json
        FROM control_command_receipts WHERE agent_id = ?1 AND identity_incarnation = ?2 AND command_id = ?3",
        params![key.agent_id, i64::try_from(key.identity_incarnation)?, key.command_id], |row| Ok((
            row.get::<_, u32>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?,
            row.get::<_, String>(4)?, row.get::<_, String>(5)?, row.get::<_, String>(6)?))) .optional()?;
    let Some((
        version,
        action,
        invocation_digest,
        stored_digest,
        committed_at,
        outcome_json,
        payload,
    )) = row
    else {
        return Ok(None);
    };
    if command_digest.is_some_and(|digest| digest != stored_digest) {
        return Err(ControlError::CommandConflict.into());
    }
    let expected_invocation_digest = digest(
        &json!({"family_version": CONTROL_FAMILY_VERSION, "key": key, "invocation": invocation}),
    )?;
    if invocation_digest != expected_invocation_digest {
        return Err(ControlError::InvocationConflict.into());
    }
    let receipt: ControlReceipt = serde_json::from_str(&payload)?;
    let outcome: ControlOutcome = serde_json::from_str(&outcome_json)?;
    ensure!(
        version == CONTROL_FAMILY_VERSION
            && receipt.family_version == version
            && receipt.key == *key
            && receipt.invocation == *invocation
            && action == action_name(&invocation.action)
            && receipt.invocation_digest == invocation_digest
            && receipt.command_digest == stored_digest
            && receipt.outcome == outcome
            && receipt.committed_at.to_rfc3339() == committed_at,
        "Control receipt columns and payload disagree"
    );
    Ok(Some(receipt))
}

fn validate_key(key: &ControlKey) -> Result<()> {
    ensure!(
        !key.agent_id.trim().is_empty()
            && !key.command_id.trim().is_empty()
            && key.identity_incarnation > 0,
        "Control requires a nonempty durable command scope"
    );
    Ok(())
}

fn protected_message(invocation: &ControlInvocation) -> Option<&str> {
    match &invocation.source {
        ControlSource::DirectApi => None,
        ControlSource::ControlMessage { message_id } => Some(message_id),
    }
}

fn binding_matches_attempt(
    binding: &crate::types::WorkItemExecutionBinding,
    attempt: &execution_protocol::ExecutionAttempt,
) -> bool {
    attempt.source_message_id.as_deref() == Some(binding.source_message_id.as_str())
        && (binding.activation_id.as_deref() == Some(attempt.attempt_id.as_str())
            || (binding.activation_id.is_none()
                && attempt.binding == ExecutionBinding::Command
                && matches!(
                    binding.owner.as_ref(),
                    Some(crate::types::TurnOwner::Command)
                )))
}

fn validate_protected_attempt(
    invocation: &ControlInvocation,
    snapshot: &ControlSnapshot,
    protected: Option<&str>,
) -> Result<()> {
    let Some(protected) = protected else {
        ensure!(
            protected_message(invocation).is_none()
                || protected_message(invocation)
                    == snapshot.reducer_control_source_message_id.as_deref(),
            "queued Control requires its exact consuming attempt or reducer-only Turn"
        );
        return Ok(());
    };
    let source =
        protected_message(invocation).context("DirectApi cannot protect an execution attempt")?;
    let attempt = snapshot
        .execution_protocol
        .as_ref()
        .and_then(|protocol| protocol.attempts.get(protected))
        .context("protected Control attempt is missing")?;
    ensure!(
        attempt.state == ExecutionAttemptState::Open
            && attempt.binding == ExecutionBinding::Command
            && attempt.source_message_id.as_deref() == Some(source),
        "protected attempt is not this Control Command"
    );
    ensure!(
        snapshot
            .queue_entries
            .iter()
            .any(|entry| entry.message_id == source && entry.status == QueueEntryStatus::Dequeued),
        "protected Control input is not the exact Dequeued message"
    );
    Ok(())
}

fn validate_protected_run(
    invocation: &ControlInvocation,
    snapshot: &ControlSnapshot,
    protected_attempt: Option<&str>,
    protected_run_id: Option<&str>,
) -> Result<()> {
    if let Some(run_id) = protected_run_id {
        ensure!(
            !run_id.is_empty()
                && snapshot.state.current_run_id.as_deref() == Some(run_id)
                && (protected_attempt.is_some()
                    || protected_message(invocation).is_some_and(|source| {
                        snapshot.reducer_control_source_message_id.as_deref() == Some(source)
                    })),
            "protected Control run is not the exact current consuming run"
        );
    }
    Ok(())
}

fn validate_source_tx(tx: &Transaction<'_>, command: &PreparedControl) -> Result<()> {
    validate_protected_attempt(
        &command.invocation,
        &command.expected,
        command.protected_control_attempt.as_deref(),
    )?;
    validate_protected_run(
        &command.invocation,
        &command.expected,
        command.protected_control_attempt.as_deref(),
        command.protected_control_run_id.as_deref(),
    )?;
    if let ControlSource::ControlMessage { message_id } = &command.invocation.source {
        let message: MessageEnvelope = payload_optional(
            tx,
            "SELECT payload_json FROM messages WHERE evidence_id = ?1",
            message_id,
        )?
        .context("Control invocation has no canonical input Message")?;
        ensure!(
            message.agent_id == command.key.agent_id
                && message.kind == MessageKind::Control
                && message.authority_class == command.invocation.authority_class
                && matches!(&message.body, MessageBody::Text { text } if text == action_name(&command.invocation.action)),
            "Control invocation does not match its canonical Message"
        );
    }
    Ok(())
}

fn validate_task_results(
    snapshot: &ControlSnapshot,
    results: &[ControlTaskResult],
    prepared_at: DateTime<Utc>,
) -> Result<()> {
    let expected: BTreeSet<_> = snapshot.tasks.iter().map(|task| task.id.as_str()).collect();
    let actual: BTreeSet<_> = results
        .iter()
        .map(|result| result.task.id.as_str())
        .collect();
    ensure!(
        expected == actual && results.len() == actual.len(),
        "Stop TaskResult set does not exactly cover active tasks"
    );
    let mut message_ids = BTreeSet::new();
    for result in results {
        let before = snapshot
            .tasks
            .iter()
            .find(|task| task.id == result.task.id)
            .expect("validated task set");
        let task = &result.task;
        let message = &result.message;
        ensure!(
            task.agent_id == before.agent_id
                && task.kind == before.kind
                && task.created_at == before.created_at
                && task.work_item_id == before.work_item_id
                && task.recovery == before.recovery
                && task.summary == before.summary
                && task.status == TaskStatus::Interrupted
                && task.updated_at == prepared_at
                && task.parent_message_id.as_deref() == Some(message.id.as_str()),
            "Stop terminal task identity/result mismatch"
        );
        for field in ["rejoin_obligation_id", "rejoin_generation"] {
            ensure!(
                task.detail.as_ref().and_then(|detail| detail.get(field))
                    == before.detail.as_ref().and_then(|detail| detail.get(field)),
                "Stop changed task rejoin generation"
            );
        }
        if let Some(parent) = before
            .detail
            .as_ref()
            .and_then(|detail| detail.get("parent_turn_id"))
        {
            ensure!(
                task.detail
                    .as_ref()
                    .and_then(|detail| detail.get("parent_turn_id"))
                    == Some(parent),
                "Stop changed original task parent Turn"
            );
        } else if let Some(parent) = task
            .detail
            .as_ref()
            .and_then(|detail| detail.get("parent_turn_id"))
        {
            ensure!(
                parent.as_str() == message.turn_id.as_deref(),
                "legacy task parent fallback is not this frozen result Turn"
            );
        }
        ensure!(
            task.detail
                .as_ref()
                .and_then(|detail| detail.get("interrupted_reason"))
                .and_then(Value::as_str)
                == Some("agent_stopped"),
            "Stop terminal task requires agent_stopped evidence"
        );
        ensure!(
            task.detail
                .as_ref()
                .and_then(|detail| detail.get("status_before_stop"))
                .and_then(Value::as_str)
                == serde_json::to_value(&before.status)?.as_str(),
            "Stop changed prior task status evidence"
        );
        ensure!(
            message
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("task_detail"))
                == task.detail.as_ref()
                && message
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("interrupted_reason"))
                    .and_then(Value::as_str)
                    == Some("agent_stopped"),
            "Stop TaskResult detail does not match terminal task evidence"
        );
        task_transition(before, task)?;
        ensure!(
            message_ids.insert(message.id.clone())
                && message.agent_id == before.agent_id
                && message.kind == MessageKind::TaskResult
                && message.task_id.as_deref() == Some(task.id.as_str())
                && message.work_item_id.as_deref() == task.effective_work_item_id()
                && message.authority_class == AuthorityClass::RuntimeInstruction
                && matches!(&message.origin, MessageOrigin::Task { task_id } if task_id == &task.id)
                && message.turn_id.as_deref().is_some_and(|id| !id.is_empty())
                && message.created_at == prepared_at,
            "Stop TaskResult provenance or frozen time is invalid"
        );
    }
    Ok(())
}

fn valid_optional_lease(value: &Value) -> bool {
    value.is_null() || value.as_str().is_some_and(|id| !id.is_empty())
}

fn validate_absent_run_claim(
    snapshot: &ControlSnapshot,
    target: &ControlCancellationTarget,
) -> Result<()> {
    let generation = &target.target_generation;
    let activation = generation["activation_id"].as_str();
    let source = generation["source_message_id"].as_str();
    if activation.is_none() && source.is_none() {
        return Ok(());
    }
    let binding = snapshot
        .state
        .current_execution_binding
        .as_ref()
        .context("absent run claim requires its canonical started Turn binding")?;
    let attempts = snapshot
        .execution_protocol
        .as_ref()
        .into_iter()
        .flat_map(|protocol| protocol.attempts.values())
        .filter(|attempt| {
            attempt.state == ExecutionAttemptState::Open
                && binding_matches_attempt(binding, attempt)
        })
        .collect::<Vec<_>>();
    ensure!(
        attempts.len() == 1
            && activation == Some(attempts[0].attempt_id.as_str())
            && source == Some(binding.source_message_id.as_str())
            && snapshot.attempt_turns.iter().any(|turn| {
                turn.turn_id == binding.turn_id
                    && turn.run_id.as_deref() == Some(target.target_identity.as_str())
                    && turn.terminal.is_none()
            }),
        "absent run claim does not match the exact started Turn and Open attempt"
    );
    Ok(())
}

fn validate_cancellation_targets(
    snapshot: &ControlSnapshot,
    protected_run_id: Option<&str>,
    targets: &[ControlCancellationTarget],
    task_results: &[ControlTaskResult],
) -> Result<()> {
    let mut ids = BTreeSet::new();
    for target in targets {
        ensure!(
            !target.obligation_id.is_empty()
                && ids.insert(target.obligation_id.as_str())
                && target.target_generation.is_object()
                && target.target.is_object(),
            "invalid/duplicate Control cancellation obligation"
        );
        let generation = &target.target_generation;
        ensure!(
            generation
                .get("owner_epoch")
                .and_then(Value::as_u64)
                .is_some_and(|epoch| epoch > 0),
            "cancellation owner epoch is missing"
        );
        match target.target_kind {
            ControlCancellationKind::Run => {
                ensure!(
                    snapshot.state.current_run_id.as_deref()
                        == Some(target.target_identity.as_str())
                        && ["runtime_lease_id", "activation_id", "source_message_id"]
                            .iter()
                            .all(|field| generation.get(*field).is_some_and(valid_optional_lease))
                        && target.target.get("reason") == Some(&json!("agent_stopped")),
                    "run cancellation requires exact identity and explicit original lease/claim facts"
                );
                if generation["runtime_lease_id"].is_null() {
                    validate_absent_run_claim(snapshot, target)?;
                }
            }
            ControlCancellationKind::Task | ControlCancellationKind::Worktree => {
                let task = snapshot
                    .tasks
                    .iter()
                    .find(|task| task.id == target.target_identity)
                    .context("cancellation refers to a non-target Task")?;
                ensure!(
                    generation.get("created_at") == Some(&serde_json::to_value(task.created_at)?)
                        && generation.get("rejoin")
                            == Some(&serde_json::to_value(task.rejoin_fence().ok())?),
                    "task cancellation generation does not match exact original Task"
                );
                // Legacy targets retain their stored digest/outcome. New runtime
                // targets bind both pre-Stop and its validated terminal shape.
                if generation
                    .get(crate::runtime::task_generation::CONTROL_ORIGINAL_REJOIN)
                    .is_some()
                    || generation
                        .get(crate::runtime::task_generation::CONTROL_TERMINAL_REJOIN)
                        .is_some()
                {
                    let terminal = &task_results
                        .iter()
                        .find(|result| result.task.id == task.id)
                        .context("Control target terminal Task missing")?
                        .task;
                    ensure!(
                        crate::runtime::task_generation::control_original_rejoin_matches(
                            task, generation
                        ) && crate::runtime::task_generation::control_terminal_rejoin_matches(
                            terminal, generation
                        ),
                        "Control target raw rejoin generation differs from its original command"
                    );
                }
                ensure!(
                    generation
                        .get("runtime_lease_id")
                        .is_some_and(valid_optional_lease),
                    "task cancellation lease presence is not explicit"
                );
                if target.target_kind == ControlCancellationKind::Task {
                    ensure!(
                        target.target.get("kind") == Some(&serde_json::to_value(&task.kind)?)
                            && target.target.get("recovery")
                                == Some(&serde_json::to_value(&task.recovery)?)
                            && target.target.get("work_item_id")
                                == Some(&serde_json::to_value(task.effective_work_item_id())?),
                        "task cancellation payload differs from original Task"
                    );
                }
                if target.target_kind == ControlCancellationKind::Worktree {
                    ensure!(
                        task.is_worktree_child_agent_task()
                            && generation.get("artifact")
                                == Some(
                                    &task
                                        .detail
                                        .as_ref()
                                        .and_then(|detail| detail.get("worktree"))
                                        .cloned()
                                        .unwrap_or(Value::Null)
                                ),
                        "worktree cancellation does not match exact original artifact"
                    );
                }
            }
            ControlCancellationKind::TimerLoop => {
                let timer = snapshot
                    .timers
                    .iter()
                    .find(|timer| timer.id == target.target_identity)
                    .context("cancellation refers to a non-target Timer")?;
                ensure!(
                    generation.get("created_at") == Some(&serde_json::to_value(timer.created_at)?)
                        && generation.get("fire_count") == Some(&json!(timer.fire_count))
                        && generation.get("next_fire_at")
                            == Some(&serde_json::to_value(timer.next_fire_at)?)
                        && generation
                            .get("runtime_lease_id")
                            .is_some_and(valid_optional_lease)
                        && target.target.get("interval_ms")
                            == Some(&serde_json::to_value(timer.interval_ms)?)
                        && target.target.get("repeat") == Some(&json!(timer.repeat)),
                    "timer cancellation does not match exact original generation"
                );
            }
        }
    }
    for task in &snapshot.tasks {
        ensure!(
            targets
                .iter()
                .filter(|target| target.target_kind == ControlCancellationKind::Task
                    && target.target_identity == task.id)
                .count()
                == 1,
            "Stop requires one obligation per original Task"
        );
        if task.is_worktree_child_agent_task() {
            ensure!(
                targets
                    .iter()
                    .filter(
                        |target| target.target_kind == ControlCancellationKind::Worktree
                            && target.target_identity == task.id
                    )
                    .count()
                    == 1,
                "Stop requires an exact worktree cleanup obligation"
            );
        }
    }
    for timer in &snapshot.timers {
        ensure!(
            targets
                .iter()
                .filter(
                    |target| target.target_kind == ControlCancellationKind::TimerLoop
                        && target.target_identity == timer.id
                )
                .count()
                == 1,
            "Stop requires one obligation per original Timer"
        );
    }
    ensure!(
        targets
            .iter()
            .filter(|target| target.target_kind == ControlCancellationKind::Run)
            .count()
            == usize::from(snapshot.state.current_run_id.is_some() && protected_run_id.is_none()),
        "Stop requires one Run obligation unless the exact consuming run is protected"
    );
    Ok(())
}

const WAIT_COLUMNS: &str = "wait_condition_id,agent_id,work_item_id,status,kind,source,subject_ref,waiting_for,created_at,updated_at,expires_at,resolved_at,cancelled_at,last_turn_id,trigger_message_id,triggered_at,wake_sources_json,continuation_json";
fn load_waits_in(connection: &Connection, agent_id: &str) -> Result<Vec<WaitConditionRecord>> {
    let mut statement = connection.prepare(&format!("SELECT {WAIT_COLUMNS} FROM wait_conditions WHERE agent_id = ?1 AND status IN ('active','triggered') ORDER BY wait_condition_id"))?;
    let rows = statement.query_map([agent_id], |row| {
        decode_wait_condition_row(row).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
fn load_wait_by_id(connection: &Connection, id: &str) -> Result<WaitConditionRecord> {
    connection
        .query_row(
            &format!("SELECT {WAIT_COLUMNS} FROM wait_conditions WHERE wait_condition_id = ?1"),
            [id],
            |row| {
                decode_wait_condition_row(row).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        error.into(),
                    )
                })
            },
        )
        .map_err(Into::into)
}
fn payload_optional<T: DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    id: &str,
) -> Result<Option<T>> {
    connection
        .query_row(sql, [id], |row| row.get::<_, String>(0))
        .optional()?
        .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
        .transpose()
}
fn payloads<T: DeserializeOwned>(connection: &Connection, sql: &str, id: &str) -> Result<Vec<T>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
    rows.map(|payload| serde_json::from_str(&payload?).map_err(Into::into))
        .collect()
}
fn records_by_ids<T: DeserializeOwned>(
    connection: &Connection,
    table: &str,
    column: &str,
    ids: &[String],
) -> Result<Vec<T>> {
    // table/column are private static literals from this module, never request input.
    ids.iter()
        .map(|id| {
            payload_optional(
                connection,
                &format!("SELECT payload_json FROM {table} WHERE {column} = ?1"),
                id,
            )?
            .with_context(|| format!("committed Control record is missing: {table}/{id}"))
        })
        .collect()
}
fn action_name(action: &ControlAction) -> &'static str {
    match action {
        ControlAction::Start => "start",
        ControlAction::Stop => "stop",
    }
}
fn cancellation_kind_name(kind: ControlCancellationKind) -> &'static str {
    match kind {
        ControlCancellationKind::Run => "run",
        ControlCancellationKind::Task => "task",
        ControlCancellationKind::TimerLoop => "timer_loop",
        ControlCancellationKind::Worktree => "worktree",
    }
}
fn index_value(change: &RuntimeIndexChange) -> Value {
    json!({"agent_id": change.agent_id, "source_kind": change.source_kind, "source_id": change.source_id,
        "source_ref": change.source_ref, "operation": change.operation.as_str(), "source_updated_at": change.source_updated_at, "reason": change.reason})
}
fn control_audit(
    scope: &str,
    suffix: &str,
    created_at: DateTime<Utc>,
    kind: &str,
    data: Value,
) -> AuditEvent {
    AuditEvent {
        id: format!("audit:control:{scope}:{suffix}"),
        event_seq: 0,
        event_log_epoch: String::new(),
        created_at,
        kind: kind.into(),
        contract_version: crate::runtime_event::LEGACY_RUNTIME_EVENT_CONTRACT_VERSION,
        payload_schema: crate::runtime_event::LEGACY_PAYLOAD_SCHEMA.into(),
        payload_schema_version: 1,
        data,
    }
}
fn digest(value: &Value) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical(value.clone()))?)
    ))
}
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<_, _> = map
                .into_iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
        other => other,
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlFaultPoint {
    AfterValidation,
    AfterTaskResultWrites,
    AfterWaitTimerWrites,
    AfterQueueTriggerWrites,
    AfterExecutionWorkspaceWrites,
    AfterStateWrite,
    AfterAuditWrites,
    AfterIndexWrites,
    AfterReceiptWrite,
    AfterObligationWrites,
    AfterCommitBeforeAck,
}
#[cfg(test)]
fn inject_fault(command: &PreparedControl, point: ControlFaultPoint) -> Result<()> {
    if command.fault == Some(point) {
        anyhow::bail!("injected Control fault at {point:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlObligationState {
    Pending,
    Completed,
    NeedsAttention,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ControlCancellationObligation {
    pub key: ControlKey,
    pub target: ControlCancellationTarget,
    pub state: ControlObligationState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completion_evidence: Option<Value>,
}

/// Minted only after the actual original Worktree completion CAS in this TX.
/// Metadata observation does not authorize Task writes or physical cleanup.
pub(in crate::runtime_db) struct OriginalControlWorktreeCompletion<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    key: &'tx ControlKey,
    obligation_id: &'tx str,
}
impl OriginalControlWorktreeCompletion<'_, '_> {
    pub(in crate::runtime_db) fn source_in<'a>(
        &'a self,
        tx: &Transaction<'_>,
    ) -> Result<(&'a ControlKey, &'a str)> {
        ensure!(
            std::ptr::eq(self.tx, tx),
            "Control completion observation crossed transaction"
        );
        Ok((self.key, self.obligation_id))
    }
}

/// Same original CAS used by the whole TaskStatus metadata receipt actor.
/// The actor admits the original obligation pool before this decoder runs.
pub(in crate::runtime_db) fn complete_control_obligation_tx(
    tx: &Transaction<'_>,
    key: &ControlKey,
    target: &ControlCancellationTarget,
    completed_at: DateTime<Utc>,
    evidence: &Value,
) -> Result<bool> {
    validate_key(key)?;
    ensure!(
        !evidence.is_null(),
        "Control obligation completion requires evidence"
    );
    let obligation = load_obligations_in(tx, key)?
        .into_iter()
        .find(|obligation| obligation.target.obligation_id == target.obligation_id)
        .context("Control cancellation obligation does not exist")?;
    ensure!(
        obligation.target == *target,
        "Control cancellation target/generation conflict"
    );
    if obligation.state == ControlObligationState::Completed {
        return Ok(false);
    }
    ensure!(
        completed_at >= obligation.updated_at,
        "Control completion time precedes its current evidence"
    );
    let changed = tx.execute("UPDATE control_cancellation_obligations SET state = 'completed', updated_at = ?5, completion_evidence_json = ?6
                WHERE agent_id = ?1 AND identity_incarnation = ?2 AND command_id = ?3 AND obligation_id = ?4
                AND state IN ('pending','needs_attention') AND target_json = ?7", params![key.agent_id,
                i64::try_from(key.identity_incarnation)?, key.command_id, target.obligation_id,
                completed_at.to_rfc3339(), serde_json::to_string(&evidence)?, serde_json::to_string(target)?])?;
    ensure!(changed == 1, "Control obligation completion CAS failed");
    if target.target_kind == ControlCancellationKind::Worktree {
        let observed = OriginalControlWorktreeCompletion {
            tx,
            key,
            obligation_id: &target.obligation_id,
        };
        super::task_snapshot::register_control_worktree_completion_tx(tx, &observed)?;
    }
    Ok(true)
}

impl RuntimeDb {
    /// A completion only settles this immutable original target. It never
    /// overwrites a later Task, Timer, State or worktree record.
    pub(crate) fn complete_control_obligation(
        &self,
        key: &ControlKey,
        target: &ControlCancellationTarget,
        completed_at: DateTime<Utc>,
        evidence: Value,
    ) -> Result<bool> {
        validate_key(key)?;
        ensure!(
            !evidence.is_null(),
            "Control obligation completion requires evidence"
        );
        self.transaction(|tx| {
            complete_control_obligation_tx(tx, key, target, completed_at, &evidence)?;
            Ok(true)
        })
    }

    /// Record a durable handoff without claiming an original physical target
    /// has completed. Repeated attention preserves its first evidence.
    pub(crate) fn mark_control_obligation_needs_attention(
        &self,
        key: &ControlKey,
        target: &ControlCancellationTarget,
        at: DateTime<Utc>,
        evidence: Value,
    ) -> Result<bool> {
        validate_key(key)?;
        ensure!(!evidence.is_null(), "Control attention requires evidence");
        self.transaction(|tx| {
            let obligation = load_obligations_in(tx, key)?.into_iter()
                .find(|obligation| obligation.target.obligation_id == target.obligation_id)
                .context("Control cancellation obligation does not exist")?;
            ensure!(obligation.target == *target, "Control cancellation target/generation conflict");
            match obligation.state {
                ControlObligationState::Completed => return Ok(false),
                ControlObligationState::NeedsAttention => return Ok(true),
                ControlObligationState::Pending => {}
            }
            ensure!(at >= obligation.updated_at, "Control attention time precedes its current evidence");
            let changed = tx.execute("UPDATE control_cancellation_obligations SET state = 'needs_attention', updated_at = ?5, completion_evidence_json = ?6
                WHERE agent_id = ?1 AND identity_incarnation = ?2 AND command_id = ?3 AND obligation_id = ?4
                AND state = 'pending' AND target_json = ?7", params![key.agent_id,
                i64::try_from(key.identity_incarnation)?, key.command_id, target.obligation_id,
                at.to_rfc3339(), serde_json::to_string(&evidence)?, serde_json::to_string(target)?])?;
            ensure!(changed == 1, "Control obligation attention CAS failed");
            Ok(true)
        })
    }

    /// Read only this immutable original target; identity alone cannot prove
    /// completion of a different generation.
    pub(crate) fn control_obligation(
        &self,
        key: &ControlKey,
        target: &ControlCancellationTarget,
    ) -> Result<Option<ControlCancellationObligation>> {
        validate_key(key)?;
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let obligation = load_obligations_in(&tx, key)?
            .into_iter()
            .find(|obligation| obligation.target.obligation_id == target.obligation_id);
        if let Some(obligation) = &obligation {
            ensure!(
                obligation.target == *target,
                "Control cancellation target/generation conflict"
            );
        }
        tx.commit()?;
        Ok(obligation)
    }

    /// Enumerate original obligation scopes, including retired incarnations.
    /// The startup actor reserves these scopes separately from current State.
    pub(crate) fn pending_control_obligation_scopes(&self) -> Result<Vec<(String, u64)>> {
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let scopes = {
            let mut statement = tx.prepare(
                "SELECT DISTINCT agent_id,identity_incarnation FROM control_cancellation_obligations
                 WHERE state IN ('pending','needs_attention') ORDER BY agent_id,identity_incarnation",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let scopes = scopes
            .into_iter()
            .map(|(agent_id, incarnation)| {
                let incarnation = u64::try_from(incarnation)?;
                ensure!(
                    !agent_id.is_empty() && incarnation > 0,
                    "Control obligation scope is invalid"
                );
                Ok((agent_id, incarnation))
            })
            .collect::<Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(scopes)
    }

    /// Only admitted recovery/drain actors use this scope query. A runtime lease
    /// from another instance is not authority to cancel a replacement handle.
    pub(crate) fn pending_control_obligations(
        &self,
        agent_id: &str,
        incarnation: u64,
    ) -> Result<Vec<(ControlKey, ControlCancellationTarget)>> {
        ensure!(
            !agent_id.is_empty() && incarnation > 0,
            "Control obligation scope is invalid"
        );
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let command_ids = {
            let mut statement = tx.prepare("SELECT DISTINCT command_id FROM control_cancellation_obligations
                WHERE agent_id = ?1 AND identity_incarnation = ?2 AND state IN ('pending','needs_attention') ORDER BY command_id")?;
            let rows = statement
                .query_map(params![agent_id, i64::try_from(incarnation)?], |row| {
                    row.get::<_, String>(0)
                })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut pending = Vec::new();
        for command_id in command_ids {
            let key = ControlKey {
                agent_id: agent_id.into(),
                identity_incarnation: incarnation,
                command_id,
            };
            pending.extend(
                load_obligations_in(&tx, &key)?
                    .into_iter()
                    .filter(|obligation| obligation.state != ControlObligationState::Completed)
                    .map(|obligation| (key.clone(), obligation.target)),
            );
        }
        tx.commit()?;
        Ok(pending)
    }
}

fn load_obligations_in(
    connection: &Connection,
    key: &ControlKey,
) -> Result<Vec<ControlCancellationObligation>> {
    let mut statement = connection.prepare("SELECT obligation_id,target_kind,target_identity,target_generation_json,created_at,state,updated_at,target_json,completion_evidence_json
        FROM control_cancellation_obligations WHERE agent_id = ?1 AND identity_incarnation = ?2 AND command_id = ?3 ORDER BY obligation_id")?;
    let rows = statement.query_map(
        params![
            key.agent_id,
            i64::try_from(key.identity_incarnation)?,
            key.command_id
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        },
    )?;
    let mut obligations = Vec::new();
    for row in rows {
        let (id, kind, identity, generation, created_at, state, updated_at, payload, evidence) =
            row?;
        let target: ControlCancellationTarget = serde_json::from_str(&payload)?;
        let generation: Value = serde_json::from_str(&generation)?;
        let state: ControlObligationState = serde_json::from_value(Value::String(state))?;
        let evidence: Option<Value> = evidence.as_deref().map(serde_json::from_str).transpose()?;
        ensure!(
            target.obligation_id == id
                && cancellation_kind_name(target.target_kind) == kind
                && target.target_identity == identity
                && target.target_generation == generation
                && (state != ControlObligationState::Completed || evidence.is_some()),
            "Control obligation columns and payload disagree"
        );
        obligations.push(ControlCancellationObligation {
            key: key.clone(),
            target,
            state,
            created_at: created_at.parse()?,
            updated_at: updated_at.parse()?,
            completion_evidence: evidence,
        });
    }
    Ok(obligations)
}

/// Exact source evidence of still-unfinished physical cancellation obligations.
/// Completed receipts do not become permanent roots merely by existing.
pub(crate) fn control_cancellation_source_refs_in(connection: &Connection) -> Result<Vec<String>> {
    let payloads = {
        let mut statement = connection.prepare(
            "SELECT DISTINCT r.receipt_json
            FROM control_command_receipts r JOIN control_cancellation_obligations o
              ON o.agent_id = r.agent_id AND o.identity_incarnation = r.identity_incarnation
              AND o.command_id = r.command_id
            WHERE o.state IN ('pending','needs_attention')
            ORDER BY r.agent_id,r.identity_incarnation,r.command_id",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut refs = BTreeSet::new();
    for payload in payloads {
        let unchecked: ControlReceipt = serde_json::from_str(&payload)?;
        let receipt = load_control_receipt_in(
            connection,
            &unchecked.key,
            &unchecked.invocation,
            Some(&unchecked.command_digest),
        )?
        .context("pending Control obligation has no valid receipt")?;
        let obligations = load_obligations_in(connection, &receipt.key)?;
        if let ControlSource::ControlMessage { message_id } = &receipt.invocation.source {
            refs.insert(format!("message:{message_id}"));
            collect_message_source_refs_in(connection, message_id, &mut refs)?;
        }
        for id in &receipt.outcome.interrupted_turn_ids {
            refs.insert(format!("turn:{id}"));
        }
        for id in receipt
            .outcome
            .aborted_queue_message_ids
            .iter()
            .chain(&receipt.outcome.dropped_timer_queue_message_ids)
        {
            refs.insert(format!("message:{id}"));
            // Legacy queue fixtures can predate durable Message materialization.
            collect_message_source_refs_in(connection, id, &mut refs)?;
        }
        for obligation in obligations
            .iter()
            .filter(|obligation| obligation.state != ControlObligationState::Completed)
        {
            let target = &obligation.target;
            match target.target_kind {
                ControlCancellationKind::Task | ControlCancellationKind::Worktree => {
                    refs.insert(format!("task:{}", target.target_identity));
                    if let Some(parent) = target
                        .target_generation
                        .get("rejoin")
                        .and_then(|rejoin| rejoin.get("parent_turn_id"))
                        .and_then(Value::as_str)
                    {
                        refs.insert(format!("turn:{parent}"));
                    }
                    if let Some(work_item_id) =
                        target.target.get("work_item_id").and_then(Value::as_str)
                    {
                        refs.insert(format!("work_item:{work_item_id}"));
                    }
                }
                ControlCancellationKind::Run => {
                    if let Some(attempt_id) = target
                        .target_generation
                        .get("activation_id")
                        .and_then(Value::as_str)
                    {
                        let attempt: Option<execution_protocol::ExecutionAttempt> = connection.query_row(
                            "SELECT payload_json FROM execution_protocol_attempts WHERE agent_id = ?1 AND attempt_id = ?2",
                            params![receipt.key.agent_id, attempt_id], |row| row.get::<_, String>(0)).optional()?
                            .map(|payload| serde_json::from_str(&payload)).transpose()?;
                        if let Some(attempt) = attempt {
                            if let Some(turn_id) = attempt.turn_id {
                                refs.insert(format!("turn:{turn_id}"));
                            }
                            if let Some(message_id) = attempt.source_message_id {
                                refs.insert(format!("message:{message_id}"));
                                collect_message_source_refs_in(connection, &message_id, &mut refs)?;
                            }
                        }
                    }
                }
                ControlCancellationKind::TimerLoop => {}
            }
        }
        for id in &receipt.outcome.cancelled_wait_ids {
            let wait = load_wait_by_id(connection, id)?;
            if let Some(turn_id) = wait.turn_id {
                refs.insert(format!("turn:{turn_id}"));
            }
            if let Some(message_id) = wait.trigger_message_id {
                refs.insert(format!("message:{message_id}"));
                collect_message_source_refs_in(connection, &message_id, &mut refs)?;
            }
            if let Some(work_item_id) = wait.work_item_id {
                refs.insert(format!("work_item:{work_item_id}"));
            }
            if let Some(source_ref) = wait.subject_ref {
                refs.insert(source_ref);
            }
        }
    }
    Ok(refs.into_iter().collect())
}

fn collect_message_source_refs_in(
    connection: &Connection,
    message_id: &str,
    refs: &mut BTreeSet<String>,
) -> Result<()> {
    let message: Option<MessageEnvelope> = payload_optional(
        connection,
        "SELECT payload_json FROM messages WHERE evidence_id = ?1",
        message_id,
    )?;
    if let Some(message) = message {
        if let Some(turn_id) = message.turn_id {
            refs.insert(format!("turn:{turn_id}"));
        }
        if let Some(task_id) = message.task_id {
            refs.insert(format!("task:{task_id}"));
        }
        if let Some(work_item_id) = message.work_item_id {
            refs.insert(format!("work_item:{work_item_id}"));
        }
        refs.extend(message.source_refs.into_values());
    }
    Ok(())
}

/// Original in-process run identity; this is evidence lookup, never authority
/// to install a historical State or to clear an unresolved mutation gate.
pub(crate) struct ControlStoppedRun<'a> {
    pub agent_id: &'a str,
    pub identity_incarnation: u64,
    pub owner_epoch: u64,
    pub run_id: &'a str,
    pub runtime_lease_id: &'a str,
    pub activation_id: Option<&'a str>,
    pub source: &'a MessageEnvelope,
}

#[path = "control_stopped_run_budget.rs"]
mod stopped_run_budget;
pub(crate) use stopped_run_budget::stopped_run_proof_material_tx;

#[path = "control_stopped_run.rs"]
mod stopped_run;
pub(crate) use stopped_run::{
    find_committed_control_stop_for_run_tx, prove_committed_control_stop_for_run_tx,
};

impl RuntimeDb {
    /// Preserve the original canonical-settlement check. Physical completion
    /// remains the responsibility of the actual original run lease.
    pub(crate) fn committed_control_stop_for_run(
        &self,
        original: &ControlStoppedRun<'_>,
    ) -> Result<bool> {
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let found = stopped_run::find_committed_control_stop_for_run_in(&tx, original)?.is_some();
        tx.commit()?;
        Ok(found)
    }
}
