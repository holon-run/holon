//! Original whole-transaction receipts for explicitly frozen transition commands.
//!
//! Prepared commands own their payload exactly once. A replay carries the
//! original outcome only; current installation facts come from a fresh resolver.
use super::*;
#[path = "prepared_terminal_state_audit.rs"]
mod terminal_state_audit;
use std::sync::Arc;
#[path = "prepared_child_monitor_binding.rs"]
mod child_binding;
#[path = "prepared_child_task_terminal.rs"]
mod child_terminal;
#[path = "prepared_command_task_terminal.rs"]
mod command_terminal;
#[path = "prepared_task_terminal_business.rs"]
mod task_terminal_business;
use crate::runtime_db::input_mutation_receipts::{
    canonical_command_digest, insert_input_mutation_receipt_tx, lookup_input_mutation_receipt_tx,
    InputMutationFamily, InputMutationOutcome, InputMutationReceipt, InputMutationResult,
    InputMutationScopeOutcome, PreparedInputMutation,
};
use crate::runtime_db::repositories;
pub(in crate::runtime_db) use child_binding::ExistingOriginalChildBindingWriteProof;
pub(crate) use child_terminal::OriginalChildStatusSelection;
#[path = "prepared_timer.rs"]
mod timer_writes;
#[path = "prepared_external_trigger.rs"]
mod trigger_writes;
pub(crate) use timer_writes::{PreparedTimerTransition, TimerMutationOutcome};
pub(crate) use trigger_writes::{
    ExternalTriggerMutationOutcome, PreparedExternalTriggerTransition,
};
#[path = "prepared_current_reads.rs"]
mod current_reads;
#[path = "prepared_input_native_birth.rs"]
mod native_birth;
#[path = "prepared_task_stop.rs"]
mod task_stop;
#[path = "prepared_task_stop_phase.rs"]
mod task_stop_phase;
#[path = "prepared_input_task_writes.rs"]
mod task_writes;
#[path = "prepared_workspace_input.rs"]
mod workspace_input;
pub(crate) use task_stop_phase::worktree_receipt_exists_tx;
#[path = "prepared_runtime_audit.rs"]
mod runtime_audit;
#[path = "prepared_task_status_metadata.rs"]
mod task_status_metadata;
#[path = "prepared_task_stop_current_reads.rs"]
mod task_stop_current_reads;
#[path = "prepared_task_stop_result.rs"]
mod task_stop_result;

/// Every argument of the original queue transaction is frozen before its owner
/// ticket begins. No field is inferred from a callback or the current database.
#[derive(Debug, serde::Serialize)]
pub(crate) struct PreparedQueueTransition {
    pub command: QueueTransitionCommand,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_terminal_state_audit: Option<AuditEvent>,
    pub execution_protocol: ExecutionProtocolTransition,
    pub wait_transition: Option<QueueWaitTransition>,
    pub task_expectation: Option<TaskExpectation>,
    pub completion: Option<CompletionTransition>,
    pub wait_registration: Option<WaitTransitionCommand>,
    pub terminal_tool_executions: Vec<ToolExecutionRecord>,
    pub extra_wait_conditions: Vec<WaitConditionRecord>,
    pub delivery: Option<AgentMessageDeliveryRecord>,
    pub timer_wake_claim: Option<TimerWakeClaim>,
    pub task_result_settlement:
        Option<crate::runtime_db::task_result_settlement::TaskResultActivationSettlement>,
}
impl PreparedQueueTransition {
    pub(crate) fn new(command: QueueTransitionCommand) -> Self {
        Self {
            command,
            original_terminal_state_audit: None,
            execution_protocol: Default::default(),
            wait_transition: None,
            task_expectation: None,
            completion: None,
            wait_registration: None,
            terminal_tool_executions: Vec::new(),
            extra_wait_conditions: Vec::new(),
            delivery: None,
            timer_wake_claim: None,
            task_result_settlement: None,
        }
    }
}

/// A borrowed view can only be minted from the frozen original Task bundle.
/// It is not a new mutation command and owns no second command body.
pub(crate) struct OriginalTaskCleanupView<'a> {
    descriptor: &'a PreparedInputMutation,
    task: &'a crate::types::TaskRecord,
    result: &'a crate::runtime_db::task_result_settlement::TaskResultSettlementRecord,
    message: &'a crate::types::MessageEnvelope,
}
impl OriginalTaskCleanupView<'_> {
    pub(crate) fn descriptor(&self) -> &PreparedInputMutation {
        self.descriptor
    }
    pub(crate) fn task(&self) -> &crate::types::TaskRecord {
        self.task
    }
    pub(crate) fn result(
        &self,
    ) -> &crate::runtime_db::task_result_settlement::TaskResultSettlementRecord {
        self.result
    }
    pub(crate) fn message(&self) -> &crate::types::MessageEnvelope {
        self.message
    }
}

/// The child is a business write member, not a read-only group expansion.
#[derive(Debug, serde::Serialize)]
pub(crate) struct PreparedTaskTransition {
    pub command: TaskTransitionCommand,
    pub execution_protocol: ExecutionProtocolTransition,
    pub synchronize_execution_protocol: bool,
    child: Option<PreparedTaskChildDeletion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_intent: Option<crate::runtime_db::task_stop::PreparedTaskStop>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_command_terminal: Option<command_terminal::PreparedOriginalCommandTerminal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_child_terminal: Option<child_terminal::PreparedOriginalChildTerminal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_child_binding: Option<child_binding::PreparedOriginalChildMonitorBinding>,
}
#[derive(Debug, serde::Serialize)]
struct PreparedTaskChildDeletion {
    scope: InputEvidenceScope,
    expected_identity: crate::types::AgentIdentityRecord,
    deletion_id: String,
}
impl PreparedTaskTransition {
    pub(crate) fn new(
        command: TaskTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        synchronize_execution_protocol: bool,
        expected_child: Option<crate::types::AgentIdentityRecord>,
        deletion_id: Option<String>,
    ) -> Result<Self> {
        let child = match (&command.agent_deletion, expected_child, deletion_id) {
            (None, None, None) => None,
            (Some(request), Some(identity), Some(deletion_id)) => {
                anyhow::ensure!(
                    identity.agent_id == request.agent_id
                        && identity.agent_id != command.agent_id
                        && identity.incarnation > 0
                        && matches!(
                            identity.status,
                            crate::types::AgentRegistryStatus::Active
                                | crate::types::AgentRegistryStatus::Deleting
                        )
                        && !deletion_id.trim().is_empty(),
                    "Task child deletion differs from original identity"
                );
                Some(PreparedTaskChildDeletion {
                    scope: InputEvidenceScope {
                        agent_id: identity.agent_id.clone(),
                        identity_incarnation: identity.incarnation,
                    },
                    expected_identity: identity,
                    deletion_id,
                })
            }
            _ => anyhow::bail!("Task deletion requires complete original child facts"),
        };
        Ok(Self {
            command,
            execution_protocol,
            synchronize_execution_protocol,
            child,
            stop_intent: None,
            original_command_terminal: None,
            original_child_terminal: None,
            original_child_binding: None,
        })
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum InputTransitionCommand {
    RuntimeAudit(runtime_audit::PreparedRuntimeAuditCommand),
    Posture {
        command: AgentPostureTransitionCommand,
        started_turn: Option<TurnRecord>,
    },
    Queue(PreparedQueueTransition),
    QueueHeadNoProgress {
        command: QueueHeadNoProgressCommand,
        prepared_at: chrono::DateTime<Utc>,
        audit_event_id: String,
    },
    Terminal(TurnTerminalTransitionCommand),
    Task(PreparedTaskTransition),
    TaskResult(PreparedTaskResultTransition),
    TaskStatusMetadata(task_status_metadata::PreparedTaskStatusMetadataCommand),
    Timer(PreparedTimerTransition),
    ExternalTrigger(PreparedExternalTriggerTransition),
    TaskStopProgress(task_stop_phase::PreparedTaskStopProgressCommand),
    TaskStopContinuation(task_stop_result::PreparedTaskStopResultCommand),
    Work {
        command: WorkItemTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        synchronize_execution_protocol: bool,
    },
    Focus {
        command: WorkItemFocusTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        synchronize_execution_protocol: bool,
        completion_tool_execution: Option<ToolExecutionRecord>,
    },
    Wait {
        command: WaitTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        task_expectation: Option<TaskExpectation>,
    },
}
impl InputTransitionCommand {
    fn family(&self) -> InputMutationFamily {
        match self {
            Self::RuntimeAudit(_) | Self::Posture { .. } => InputMutationFamily::AgentPosture,
            Self::Queue(_) => InputMutationFamily::Queue,
            Self::QueueHeadNoProgress { .. } => InputMutationFamily::QueueHeadNoProgress,
            Self::Terminal(_) => InputMutationFamily::Terminal,
            Self::Task(_) => InputMutationFamily::Task,
            Self::TaskResult(_) => InputMutationFamily::TaskResult,
            Self::Timer(_) => InputMutationFamily::Timer,
            Self::TaskStopProgress(_)
            | Self::TaskStopContinuation(_)
            | Self::TaskStatusMetadata(_) => InputMutationFamily::Task,
            Self::ExternalTrigger(_) => InputMutationFamily::ExternalTrigger,
            Self::Work { .. } => InputMutationFamily::WorkItem,
            Self::Focus { .. } => InputMutationFamily::Focus,
            Self::Wait { .. } => InputMutationFamily::Wait,
        }
    }
    fn agent_id(&self) -> &str {
        match self {
            Self::RuntimeAudit(command) => command.agent_id(),
            Self::Posture { command, .. } => &command.agent_id,
            Self::Queue(command) => &command.command.agent_id,
            Self::QueueHeadNoProgress { command, .. } => &command.agent_id,
            Self::Terminal(command) => &command.agent_id,
            Self::Task(task) => &task.command.agent_id,
            Self::TaskResult(command) => command.agent_id(),
            Self::TaskStatusMetadata(command) => &command.scope().agent_id,
            Self::Timer(command) => command.agent_id(),
            Self::ExternalTrigger(command) => command.agent_id(),
            Self::TaskStopProgress(command) => &command.scope().agent_id,
            Self::TaskStopContinuation(command) => &command.scope().agent_id,
            Self::Work { command, .. } => &command.agent_id,
            Self::Focus { command, .. } => &command.agent_id,
            Self::Wait { command, .. } => &command.agent_id,
        }
    }
}

/// Deliberately not Clone: the owned command is not copied into its receipt or
/// witness. The witness retains only descriptor + original-attempt completion.
#[derive(Debug)]
pub(crate) struct PreparedInputTransition {
    descriptor: PreparedInputMutation,
    primary_scope: InputEvidenceScope,
    command: Box<InputTransitionCommand>,
    memory: Option<crate::runtime_db::dispatch_memory::DispatchMemoryIntent>,
    ended_memory: Option<crate::runtime::ended_dispatch::EndedDispatchAuthorization>,
    original_dispatch_claim: Option<crate::runtime::started_dispatch::OriginalDispatchClaim>,
    auto_base_descriptor: Option<PreparedInputMutation>,
    current_read_source: Option<Arc<crate::runtime::current_read_facts::ResidentCurrentReadFacts>>,
    current_event_roles: Option<crate::runtime::current_event_queries::CurrentEventRoleSnapshot>,
    work_resource_material: Option<crate::runtime::WorkResourceMaterialLoan>,
    original_workspace_material: Option<Arc<crate::runtime::OriginalWorkspaceRequestMaterial>>,
    original_sleep_override_material: Option<Arc<crate::runtime::OriginalSleepOverrideMaterial>>,
    original_task_input_child_material: Option<Arc<crate::runtime::OriginalTaskInputChildMaterial>>,
}
impl PreparedInputTransition {
    pub(crate) fn original_task_input_child_material(
        &self,
    ) -> Option<&Arc<crate::runtime::OriginalTaskInputChildMaterial>> {
        self.original_task_input_child_material.as_ref()
    }
    pub(crate) fn with_original_task_input_child_material(
        mut self,
        material: Arc<crate::runtime::OriginalTaskInputChildMaterial>,
        epoch: u64,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let original = match self.command.as_ref() {
            InputTransitionCommand::Queue(queue) => {
                let command = &queue.command;
                matches!(command.operation, QueueOperation::Admit | QueueOperation::Discard)
                    && command.agent_state.is_some()
                    && command.scheduler_claim_work_item.is_none()
                    && command.transcript_entries.is_empty() && command.turn_record.is_none()
                    && command.brief_evidence.is_empty() && command.message_evidence.len() == 1
                    && command.message_evidence.iter().all(|message| {
                        material.validate_message(message).is_ok()
                            && matches!(&command.mutation, QueueMutation::Upsert(entry)
                                if entry.message_id == message.id && entry.agent_id == message.agent_id
                                && matches!(entry.status, crate::types::QueueEntryStatus::Queued | crate::types::QueueEntryStatus::Aborted))
                    })
            }
            _ => false,
        };
        anyhow::ensure!(
            original
                && self.descriptor.scopes().len() == 1
                && self.original_task_input_child_material.is_none()
                && self.original_workspace_material.is_none()
                && self.original_sleep_override_material.is_none()
                && self.work_resource_material.is_none(),
            "TaskInput material requires its original closed child follow-up admission"
        );
        material.validate_binding(&self.primary_scope, epoch, ledger)?;
        self.original_task_input_child_material = Some(material);
        Ok(self)
    }
    pub(crate) fn original_sleep_override_material(
        &self,
    ) -> Option<&Arc<crate::runtime::OriginalSleepOverrideMaterial>> {
        self.original_sleep_override_material.as_ref()
    }
    pub(crate) fn with_original_sleep_override_material(
        mut self,
        material: Arc<crate::runtime::OriginalSleepOverrideMaterial>,
        epoch: u64,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let original = match self.command.as_ref() {
            InputTransitionCommand::Queue(queue) => {
                let command = &queue.command;
                command.operation == QueueOperation::Admit
                    && command.agent_state.is_some()
                    && command.scheduler_claim_work_item.is_none()
                    && command.transcript_entries.is_empty()
                    && command.turn_record.is_none()
                    && command.brief_evidence.is_empty()
                    && command.message_evidence.len() == 1
                    && command.message_evidence.iter().all(|message| {
                        message.kind == crate::types::MessageKind::SystemTick
                            && message.delivery_surface == Some(crate::types::MessageDeliverySurface::RuntimeSystem)
                            && message.admission_context == Some(crate::types::AdmissionContext::RuntimeOwned)
                            && matches!(&message.origin, crate::types::MessageOrigin::System { subsystem } if subsystem == "work_queue")
                            && matches!(&command.mutation, QueueMutation::Upsert(entry) if entry.message_id == message.id && entry.status == crate::types::QueueEntryStatus::Queued)
                    })
            }
            _ => false,
        };
        anyhow::ensure!(
            original
                && self.original_sleep_override_material.is_none()
                && self.original_workspace_material.is_none()
                && self.memory.is_none()
                && self.work_resource_material.is_none()
                && self.descriptor.scopes().len() == 1,
            "Sleep material requires its original single-scope WorkQueue Tick admission"
        );
        material.validate_binding(&self.primary_scope, epoch, ledger)?;
        self.original_sleep_override_material = Some(material);
        Ok(self)
    }
    /// Accounting only; it grants no business, State, body or filesystem right.
    pub(crate) fn with_original_workspace_request_material(
        mut self,
        material: Arc<crate::runtime::OriginalWorkspaceRequestMaterial>,
        owner_epoch: u64,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        anyhow::ensure!(
            self.original_workspace_material.is_none()
                && matches!(
                    self.command.as_ref(),
                    InputTransitionCommand::Posture {
                        started_turn: None,
                        ..
                    }
                )
                && self.memory.is_none(),
            "Workspace request material requires its original standalone State command"
        );
        material.validate_binding(&self.primary_scope, owner_epoch, ledger)?;
        self.original_workspace_material = Some(material);
        Ok(self)
    }
    /// The original Worktree Task metadata caller supplies accounting only.
    /// Original Task/OCC, Source and State rights remain checked by its kernel.
    pub(crate) fn with_original_task_workspace_request_material(
        mut self,
        material: Arc<crate::runtime::OriginalWorkspaceRequestMaterial>,
        owner_epoch: u64,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let ordinary_metadata = match self.command.as_ref() {
            InputTransitionCommand::Task(task) => {
                task.child.is_none()
                    && task.stop_intent.is_none()
                    && task.original_command_terminal.is_none()
                    && task.original_child_terminal.is_none()
                    && task.original_child_binding.is_none()
                    && task.command.agent_deletion.is_none()
                    && task.command.task_result_settlement.is_none()
                    && task.command.queue_entry.is_none()
                    && task.command.message_evidence.is_empty()
                    && task.command.work_items.is_empty()
                    && task.command.expected_wait_conditions.is_empty()
                    && task.command.wait_conditions.is_empty()
                    && matches!(
                        task.command.task.kind,
                        crate::types::TaskKind::WorktreeSubagentTask
                            | crate::types::TaskKind::ChildAgentTask
                    )
            }
            _ => false,
        };
        anyhow::ensure!(
            self.original_workspace_material.is_none()
                && self.memory.is_none()
                && ordinary_metadata,
            "Workspace Task material requires its original closed metadata command"
        );
        material.validate_binding(&self.primary_scope, owner_epoch, ledger)?;
        self.original_workspace_material = Some(material);
        Ok(self)
    }
    pub(crate) fn original_workspace_request_material(
        &self,
    ) -> Option<&Arc<crate::runtime::OriginalWorkspaceRequestMaterial>> {
        self.original_workspace_material.as_ref()
    }
    /// Read selection from the same original held owner; excluded from business digest.
    pub(crate) fn with_current_event_roles(
        mut self,
        roles: crate::runtime::current_event_queries::CurrentEventRoleSnapshot,
        owner_epoch: u64,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        anyhow::ensure!(
            self.current_event_roles.is_none(),
            "Event roles already supplied"
        );
        roles.validate(&self.primary_scope, owner_epoch, ledger)?;
        self.current_event_roles = Some(roles);
        Ok(self)
    }
    pub(super) fn current_event_request(
        &self,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<(
        crate::runtime::current_read_facts::CurrentReadRequest,
        (usize, usize),
    )> {
        let limits = ledger.limits();
        let mut request = crate::runtime::current_read_facts::CurrentReadRequest::current(
            usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Event role Current allowance overflow")?,
            )?,
            usize::try_from(limits.record_limit)?,
        );
        let prior = match &self.current_event_roles {
            Some(roles) => roles.prepare_request(&self.primary_scope, ledger, &mut request)?,
            None => (0, 0),
        };
        Ok((request, prior))
    }
    pub(crate) fn original_work_material_prior(
        &self,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        self.work_resource_material
            .as_ref()
            .map(|loan| loan.validate_accounting(&self.primary_scope, ledger))
            .transpose()
            .map(|prior| prior.unwrap_or((0, 0)))
    }
    pub(crate) fn original_work_result_material(
        &self,
    ) -> Option<&Arc<crate::runtime::WorkItemMutationMaterial>> {
        self.work_resource_material
            .as_ref()
            .and_then(|loan| loan.original_result_material())
    }
    pub(crate) fn with_work_resource_material(
        mut self,
        loan: crate::runtime::WorkResourceMaterialLoan,
    ) -> Result<Self> {
        anyhow::ensure!(
            self.work_resource_material.is_none()
                && matches!(
                    self.command.as_ref(),
                    InputTransitionCommand::Work { .. }
                        | InputTransitionCommand::Focus { .. }
                        | InputTransitionCommand::Wait { .. }
                        | InputTransitionCommand::Queue(_)
                ),
            "Resource material requires one original Work-bearing command"
        );
        self.work_resource_material = Some(loan);
        Ok(self)
    }
    pub(crate) fn accepts_work_input_selection(&self) -> bool {
        matches!(
            self.command.as_ref(),
            InputTransitionCommand::Task(_)
                | InputTransitionCommand::Timer(_)
                | InputTransitionCommand::Work { .. }
                | InputTransitionCommand::Focus { .. }
                | InputTransitionCommand::Wait { .. }
                | InputTransitionCommand::Queue(_)
        ) || (matches!(self.command.as_ref(), InputTransitionCommand::Terminal(_))
            && matches!(
                self.memory,
                Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Finish { .. })
            ))
    }
    pub(crate) fn with_work_input_selection(
        mut self,
        inputs: &crate::runtime::resident_input_evidence::ResidentInputEvidence,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        limits: (usize, usize),
    ) -> Result<Self> {
        anyhow::ensure!(
            self.accepts_work_input_selection(),
            "original command has no Work owner boundary"
        );
        self.work_resource_material = Some(
            crate::runtime::WorkResourceMaterialLoan::with_input_selection(
                self.work_resource_material.take(),
                inputs,
                &self.primary_scope,
                ledger,
                limits,
            )?,
        );
        Ok(self)
    }
    /// A read witness only: it narrows an original write, never changes its digest.
    pub(crate) fn with_current_read_source(
        mut self,
        source: Arc<crate::runtime::current_read_facts::ResidentCurrentReadFacts>,
    ) -> Result<Self> {
        anyhow::ensure!(
            source.scope() == &self.primary_scope && self.current_read_source.is_none(),
            "CurrentRead source differs from original command scope"
        );
        if !current_reads::accepts_state_source(&self)? {
            // Do not retain an unused old source across a record-writing phase.
            return Ok(self);
        }
        self.current_read_source = Some(source);
        Ok(self)
    }
    pub(crate) fn original_task_cleanup(&self) -> Result<OriginalTaskCleanupView<'_>> {
        let InputTransitionCommand::Task(task) = self.command.as_ref() else {
            anyhow::bail!("original Task cleanup requires a Task command");
        };
        let result = task
            .command
            .task_result_settlement
            .as_ref()
            .context("original Task cleanup has no pending result")?;
        let mut messages = task
            .command
            .message_evidence
            .iter()
            .filter(|message| message.id == result.message_id);
        let message = messages
            .next()
            .context("original Task cleanup result Message missing")?;
        anyhow::ensure!(
            messages.next().is_none(),
            "ambiguous original Task result Message"
        );
        Ok(OriginalTaskCleanupView {
            descriptor: &self.descriptor,
            task: &task.command.task,
            result,
            message,
        })
    }

    pub(crate) fn posture(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: AgentPostureTransitionCommand,
        started_turn: Option<TurnRecord>,
    ) -> Result<Self> {
        anyhow::ensure!(
            command.agent_id == command.agent_state.record.id,
            "agent posture transition identity mismatch"
        );
        if let Some(turn) = started_turn.as_ref() {
            anyhow::ensure!(
                turn.agent_id == command.agent_id
                    && Some(&turn.turn_id) == command.agent_state.record.current_turn_id.as_ref()
                    && turn.turn_index == command.agent_state.record.turn_index
                    && turn.terminal.is_none(),
                "started Turn and posture differ"
            );
        }
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::Posture {
                command,
                started_turn,
            },
        )
    }
    pub(crate) fn queue(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: PreparedQueueTransition,
    ) -> Result<Self> {
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::Queue(command),
        )
    }
    /// One bounded defer/quarantine attempt. Its clock and event identity are
    /// frozen before SQL; replay returns its original optional disposition.
    pub(crate) fn queue_head_no_progress(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: QueueHeadNoProgressCommand,
    ) -> Result<Self> {
        let audit_event_id = format!("queue-progress:{command_nonce}");
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::QueueHeadNoProgress {
                command,
                prepared_at,
                audit_event_id,
            },
        )
    }
    pub(crate) fn terminal(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: TurnTerminalTransitionCommand,
    ) -> Result<Self> {
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::Terminal(command),
        )
    }
    pub(crate) fn work(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: WorkItemTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        synchronize_execution_protocol: bool,
    ) -> Result<Self> {
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::Work {
                command,
                execution_protocol,
                synchronize_execution_protocol,
            },
        )
    }
    pub(crate) fn focus(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: WorkItemFocusTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        synchronize_execution_protocol: bool,
        completion_tool_execution: Option<ToolExecutionRecord>,
    ) -> Result<Self> {
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::Focus {
                command,
                execution_protocol,
                synchronize_execution_protocol,
                completion_tool_execution,
            },
        )
    }
    pub(crate) fn wait(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: WaitTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        task_expectation: Option<TaskExpectation>,
    ) -> Result<Self> {
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::Wait {
                command,
                execution_protocol,
                task_expectation,
            },
        )
    }
    pub(crate) fn task(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: PreparedTaskTransition,
    ) -> Result<Self> {
        let child_scope = command
            .child
            .as_ref()
            .map(|child| child.scope.clone())
            .or_else(|| {
                command
                    .stop_intent
                    .as_ref()
                    .and_then(|intent| intent.child.as_ref())
                    .map(|child| child.scope.clone())
            });
        let mut prepared = Self::prepare(
            scope.clone(),
            command_nonce,
            prepared_at,
            InputTransitionCommand::Task(command),
        )?;
        if let Some(child_scope) = child_scope {
            prepared = prepared.with_scope_group(vec![scope, child_scope])?;
        }
        prepared.validate_primary_namespace()?;
        Ok(prepared)
    }
    pub(crate) fn task_result(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: PreparedTaskResultTransition,
    ) -> Result<Self> {
        command.validate()?;
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::TaskResult(command),
        )
    }
    pub(crate) fn timer(
        scope: InputEvidenceScope,
        nonce: String,
        at: chrono::DateTime<Utc>,
        command: PreparedTimerTransition,
    ) -> Result<Self> {
        command.validate(&scope)?;
        Self::prepare(scope, nonce, at, InputTransitionCommand::Timer(command))
    }
    pub(crate) fn initial_task_stop_intent(
        &self,
    ) -> Option<&crate::runtime_db::task_stop::PreparedTaskStop> {
        match self.command.as_ref() {
            InputTransitionCommand::Task(task) => task.stop_intent.as_ref(),
            _ => None,
        }
    }
    pub(crate) fn task_stop_phase_scope(&self) -> Option<&InputEvidenceScope> {
        match self.command.as_ref() {
            InputTransitionCommand::TaskStopProgress(command) => Some(command.scope()),
            InputTransitionCommand::TaskStopContinuation(command) => Some(command.scope()),
            _ => None,
        }
    }
    pub(crate) fn is_task_stop_phase(&self) -> bool {
        matches!(
            self.command.as_ref(),
            InputTransitionCommand::TaskStopProgress(_)
                | InputTransitionCommand::TaskStopContinuation(_)
        )
    }
    pub(crate) fn task_deletion_scope(&self) -> Option<&InputEvidenceScope> {
        match self.command.as_ref() {
            InputTransitionCommand::Task(task) => task.child.as_ref().map(|child| &child.scope),
            _ => None,
        }
    }
    fn task_stop_child_scope(&self) -> Option<&InputEvidenceScope> {
        match self.command.as_ref() {
            InputTransitionCommand::Task(task) => task
                .stop_intent
                .as_ref()
                .and_then(|intent| intent.child.as_ref())
                .map(|child| &child.scope),
            _ => None,
        }
    }
    fn prepare(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: InputTransitionCommand,
    ) -> Result<Self> {
        anyhow::ensure!(
            scope.agent_id == command.agent_id(),
            "prepared transition differs from its original scope"
        );
        let descriptor = PreparedInputMutation::new(
            command.family(),
            command_nonce,
            vec![scope.clone()],
            canonical_command_digest(&command)?,
            prepared_at,
        )?;
        Ok(Self {
            descriptor,
            primary_scope: scope,
            command: Box::new(command),
            memory: None,
            ended_memory: None,
            original_dispatch_claim: None,
            auto_base_descriptor: None,
            current_read_source: None,
            current_event_roles: None,
            work_resource_material: None,
            original_workspace_material: None,
            original_sleep_override_material: None,
            original_task_input_child_material: None,
        })
    }
    pub(crate) fn with_original_dispatch_claim(
        mut self,
        claim: crate::runtime::started_dispatch::OriginalDispatchClaim,
    ) -> Result<Self> {
        anyhow::ensure!(
            matches!(
                self.command.as_ref(),
                InputTransitionCommand::Posture {
                    started_turn: Some(_),
                    ..
                }
            ) && matches!(
                self.memory.as_ref(),
                Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Started { .. })
            ),
            "original claim requires an actual StartedTurn memory command"
        );
        self.original_dispatch_claim = Some(claim);
        Ok(self)
    }
    pub(crate) fn descriptor(&self) -> &PreparedInputMutation {
        &self.descriptor
    }

    pub(crate) fn owned_payload_bytes(&self) -> Result<u64> {
        Ok(crate::runtime::input_evidence_budget::serialized_bytes(&(
            self.command.as_ref(),
            &self.memory,
            &self.ended_memory,
            &self.auto_base_descriptor,
        ))?)
    }

    pub(crate) fn dispatch_memory_candidate(&self) -> bool {
        if self.memory.is_some() {
            return false;
        }
        match self.command.as_ref() {
            InputTransitionCommand::Work { .. }
            | InputTransitionCommand::Focus { .. }
            | InputTransitionCommand::Wait { .. }
            | InputTransitionCommand::Terminal(_)
            | InputTransitionCommand::TaskStopContinuation(_) => true,
            // Stop's first ACK has no Work/Wait/State/Result footprint. Its later
            // terminal Result command follows the ordinary memory path.
            InputTransitionCommand::Task(task) => {
                task.stop_intent.is_none()
                    && task
                        .original_command_terminal
                        .as_ref()
                        .is_none_or(|original| !original.intent.no_op)
                    && task
                        .original_child_terminal
                        .as_ref()
                        .is_none_or(|original| !original.intent.no_op)
            }
            InputTransitionCommand::Timer(timer) => timer.new_wake().is_some(),
            InputTransitionCommand::Queue(queue) => {
                queue.command.operation == QueueOperation::Settle
                    || queue.wait_transition.is_some()
                    || queue.wait_registration.is_some()
                    || queue.completion.is_some()
                    || !queue.extra_wait_conditions.is_empty()
            }
            _ => false,
        }
    }
    pub(crate) fn actual_memory_terminal_turn_id(&self) -> Option<&str> {
        match self.command.as_ref() {
            InputTransitionCommand::Terminal(command) => Some(&command.turn_record.turn_id),
            InputTransitionCommand::Queue(queue) => queue
                .command
                .turn_record
                .as_ref()
                .filter(|turn| turn.terminal.is_some())
                .map(|turn| turn.turn_id.as_str()),
            _ => None,
        }
    }

    /// Only failed setup of a claimed source, before any actual StartedTurn.
    /// The caller must still prove the exact live claim and absent Turn in SQL.
    pub(crate) fn claim_only_queue_terminal_source(&self, state: &AgentState) -> Option<&str> {
        if state.current_turn_id.is_some()
            || state.current_execution_binding.is_some()
            || state.current_run_id.is_none()
        {
            return None;
        }
        let InputTransitionCommand::Queue(queue) = self.command.as_ref() else {
            return None;
        };
        let command = &queue.command;
        let turn = command.turn_record.as_ref()?;
        let expected = command.agent_state.as_ref()?.expected.as_deref()?;
        let record = match &command.mutation {
            QueueMutation::Upsert(record) | QueueMutation::CompareAndSet { record, .. } => record,
            QueueMutation::Consume(_) => return None,
        };
        (command.operation == QueueOperation::Settle
            && command.agent_id == state.id
            && expected == state
            && turn.agent_id == state.id
            && turn.run_id == state.current_run_id
            && turn.turn_index == state.turn_index
            && turn
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.kind == crate::types::TurnTerminalKind::Aborted)
            && matches!(
                record.status,
                QueueEntryStatus::Aborted | QueueEntryStatus::Interrupted
            ))
        .then_some(record.message_id.as_str())
    }

    /// Cheap typed classification. This does not discover a seed, grant an
    /// admission, or turn a raw Message hint into an actual dispatch.
    pub(crate) fn dispatch_memory_policy(
        &self,
        binding: &crate::runtime_db::dispatch_memory::DispatchMemoryBinding,
    ) -> crate::runtime_db::dispatch_memory::DispatchMemoryCommandPolicy {
        use crate::runtime_db::dispatch_memory::DispatchMemoryCommandPolicy as Policy;
        if self.memory.is_some() || binding.scope() != self.primary_scope {
            return Policy::None;
        }
        match self.command.as_ref() {
            InputTransitionCommand::Terminal(command)
                if command.turn_record.turn_id == binding.key().actual_dispatch_turn_id =>
            {
                Policy::Finish
            }
            InputTransitionCommand::Queue(queue)
                if queue.command.operation == QueueOperation::Settle
                    && queue_mutation_record(&queue.command.mutation).message_id
                        == binding.invocation().message_id =>
            {
                Policy::Finish
            }
            InputTransitionCommand::Timer(timer) if timer.new_wake().is_some() => Policy::Deferred,
            InputTransitionCommand::Work { .. }
            | InputTransitionCommand::Focus { .. }
            | InputTransitionCommand::Wait { .. }
            | InputTransitionCommand::Task(_)
            | InputTransitionCommand::TaskStopContinuation(_) => Policy::Deferred,
            InputTransitionCommand::Queue(queue)
                if queue.wait_transition.is_some()
                    || queue.wait_registration.is_some()
                    || queue.completion.is_some()
                    || !queue.extra_wait_conditions.is_empty() =>
            {
                Policy::Deferred
            }
            _ => Policy::None,
        }
    }

    /// Explicit actual-dispatch protocol. Its frozen IDs/time/baseline are part
    /// of this original whole command; None keeps the old digest exactly.
    pub(crate) fn with_dispatch_memory(
        mut self,
        memory: crate::runtime_db::dispatch_memory::DispatchMemoryIntent,
    ) -> Result<Self> {
        use crate::runtime_db::dispatch_memory::DispatchMemoryIntent as Memory;
        anyhow::ensure!(
            memory.binding_scope() == self.primary_scope,
            "dispatch memory differs from its original primary scope"
        );
        match (&memory, self.command.as_ref()) {
            (
                Memory::Started { start },
                InputTransitionCommand::Posture {
                    started_turn: Some(turn),
                    ..
                },
            ) => {
                anyhow::ensure!(
                    start.key().actual_dispatch_turn_id == turn.turn_id
                        && start.invocation().turn_index == turn.turn_index,
                    "memory Start differs from actual StartedTurn"
                );
            }
            (
                Memory::Deferred { .. },
                InputTransitionCommand::Work { .. }
                | InputTransitionCommand::Focus { .. }
                | InputTransitionCommand::Wait { .. }
                | InputTransitionCommand::Task(_)
                | InputTransitionCommand::TaskStopContinuation(_)
                | InputTransitionCommand::Timer(_)
                | InputTransitionCommand::Queue(_),
            ) => {}
            (Memory::Finish { binding, .. }, InputTransitionCommand::Terminal(command)) => {
                anyhow::ensure!(
                    command.turn_record.turn_id == binding.key().actual_dispatch_turn_id,
                    "terminal memory finish differs from original actual Turn"
                );
            }
            (Memory::Finish { binding, .. }, InputTransitionCommand::Queue(queue)) => {
                anyhow::ensure!(
                    queue.command.operation == QueueOperation::Settle
                        && queue_mutation_record(&queue.command.mutation).message_id
                            == binding.invocation().message_id,
                    "bound memory finish requires the original Queue settlement"
                );
            }
            _ => anyhow::bail!("dispatch memory intent differs from its typed business command"),
        }
        let digest = canonical_command_digest(&(
            "input_dispatch_memory/v1",
            self.command.as_ref(),
            &memory,
        ))?;
        self.descriptor = PreparedInputMutation::new(
            self.descriptor.family(),
            self.descriptor.command_nonce().to_owned(),
            self.descriptor.scopes().to_vec(),
            digest,
            self.descriptor.prepared_at(),
        )?;
        self.memory = Some(memory);
        Ok(self)
    }

    /// The business portion repeats only the existing canonical terminal and
    /// fresh State. Its hidden capability permits this original Finish alone.
    pub(crate) fn with_ended_dispatch_memory(
        self,
        memory: crate::runtime_db::dispatch_memory::DispatchMemoryIntent,
        authorization: crate::runtime::ended_dispatch::EndedDispatchAuthorization,
    ) -> Result<Self> {
        anyhow::ensure!(
            matches!(
                &memory,
                crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Finish { .. }
            ),
            "ended dispatch requires its original Finish"
        );
        let InputTransitionCommand::Terminal(command) = self.command.as_ref() else {
            anyhow::bail!("ended dispatch requires an existing terminal no-op");
        };
        anyhow::ensure!(
            command.agent_state.expected.as_deref() == Some(command.agent_state.record.as_ref())
                && command.turn_record.terminal.is_some()
                && command.terminal_tool_executions.is_empty()
                && command.audit_events.is_empty(),
            "ended memory cannot alter business State, terminal or tools"
        );
        let mut result = self.with_dispatch_memory(memory)?;
        let digest = canonical_command_digest(&(
            "input_ended_dispatch_memory/v1",
            result.command.as_ref(),
            &result.memory,
            &authorization,
        ))?;
        result.descriptor = PreparedInputMutation::new(
            result.descriptor.family(),
            result.descriptor.command_nonce().to_owned(),
            result.descriptor.scopes().to_vec(),
            digest,
            result.descriptor.prepared_at(),
        )?;
        result.ended_memory = Some(authorization);
        Ok(result)
    }

    fn validate_memory_ending_tx(&self, tx: &rusqlite::Transaction<'_>) -> Result<()> {
        let Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Finish {
            binding, ..
        }) = &self.memory
        else {
            anyhow::ensure!(
                self.ended_memory.is_none(),
                "ended capability without Finish"
            );
            return Ok(());
        };
        if let Some(authorization) = &self.ended_memory {
            authorization.validate_tx(tx, binding)?;
            let InputTransitionCommand::Terminal(command) = self.command.as_ref() else {
                anyhow::bail!("ended dispatch lost its terminal command");
            };
            let actual = crate::runtime_db::context_inputs::ContextDurableRead::new(
                tx,
                &self.primary_scope.agent_id,
            )
            .turn_by_id(&command.turn_record.turn_id)?
            .context("ended original terminal missing")?;
            anyhow::ensure!(
                actual == command.turn_record,
                "ended memory cannot replace the original terminal"
            );
        } else {
            // An administrative terminal alone cannot grant physical ending.
            let terminal_kind: Option<String> = tx.query_row(
                "SELECT json_extract(payload_json,'$.terminal.kind') FROM turn_records WHERE turn_id=?1",
                [&binding.key().actual_dispatch_turn_id], |row| row.get(0))?;
            anyhow::ensure!(
                terminal_kind.as_deref() != Some("interrupted"),
                "administratively interrupted dispatch requires original ended capability"
            );
        }
        Ok(())
    }

    /// Automatic decoration preserves the original plain business descriptor.
    /// Its exact frozen intent is written in the same original transaction.
    pub(crate) fn with_automatic_dispatch_memory(
        self,
        memory: crate::runtime_db::dispatch_memory::DispatchMemoryIntent,
    ) -> Result<Self> {
        anyhow::ensure!(
            self.memory.is_none() && self.auto_base_descriptor.is_none(),
            "automatic memory cannot replace explicit memory intent"
        );
        anyhow::ensure!(
            !matches!(
                &memory,
                crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Started { .. }
            ),
            "actual StartedTurn memory must remain explicit"
        );
        let base = self.descriptor.clone();
        let mut prepared = self.with_dispatch_memory(memory)?;
        prepared.auto_base_descriptor = Some(base);
        Ok(prepared)
    }

    /// Rehash the original typed business body plus the preserved small intent.
    /// A valid stored receipt alone cannot authorize a changed body or intent.
    pub(crate) fn restore_automatic_dispatch_memory(
        self,
        original: crate::runtime_db::dispatch_memory::OriginalAutomaticMemory,
    ) -> Result<Self> {
        let (base, descriptor, memory) = original.into_parts();
        anyhow::ensure!(
            self.descriptor == base,
            crate::runtime_db::input_mutation_receipts::ReceiptError::CommandConflict
        );
        let prepared = self.with_automatic_dispatch_memory(memory)?;
        anyhow::ensure!(
            prepared.descriptor == descriptor,
            crate::runtime_db::input_mutation_receipts::ReceiptError::CommandConflict
        );
        Ok(prepared)
    }

    /// Freeze every owner held for this original transaction into its receipt.
    /// Additional owners have no business delta, except the explicit Task child
    /// whose original deletion result belongs to the same complete receipt.
    pub(crate) fn with_scope_group(mut self, scopes: Vec<InputEvidenceScope>) -> Result<Self> {
        anyhow::ensure!(
            !matches!(
                self.command.as_ref(),
                InputTransitionCommand::RuntimeAudit(_)
            ) || scopes.as_slice() == std::slice::from_ref(&self.primary_scope),
            "Runtime Audit requires only its original scope"
        );

        anyhow::ensure!(
            self.task_status_metadata_scope().is_none()
                || scopes.as_slice() == std::slice::from_ref(&self.primary_scope),
            "Control metadata group must contain only its original scope"
        );
        anyhow::ensure!(
            self.original_command_terminal().is_none()
                || scopes.as_slice() == std::slice::from_ref(&self.primary_scope),
            "Command producer requires only its original owner"
        );
        anyhow::ensure!(
            self.task_status_metadata_scope().is_none()
                || scopes.as_slice() == std::slice::from_ref(&self.primary_scope),
            "Control metadata group must contain only its original scope"
        );
        anyhow::ensure!(
            !matches!(
                self.command.as_ref(),
                InputTransitionCommand::ExternalTrigger(_)
            ) || scopes.as_slice() == std::slice::from_ref(&self.primary_scope),
            "Trigger producer requires only its original scope"
        );
        anyhow::ensure!(
            !self.is_task_stop_phase()
                || scopes.as_slice() == std::slice::from_ref(&self.primary_scope),
            "Stop phase group must contain only its original parent"
        );
        anyhow::ensure!(
            scopes.contains(&self.primary_scope),
            "prepared transition group omits its original primary scope"
        );
        if let Some(child) = self.task_deletion_scope() {
            anyhow::ensure!(
                scopes.contains(child),
                "Task group omits its original child writer"
            );
            anyhow::ensure!(
                scopes
                    .iter()
                    .filter(|scope| scope.agent_id == child.agent_id)
                    .count()
                    == 1,
                "Task group has conflicting child incarnations"
            );
        }
        if let Some(child) = self.task_stop_child_scope() {
            anyhow::ensure!(
                scopes.contains(child),
                "TaskStop group omits original child witness"
            );
            anyhow::ensure!(
                scopes
                    .iter()
                    .filter(|scope| scope.agent_id == child.agent_id)
                    .count()
                    == 1,
                "TaskStop group has conflicting child incarnations"
            );
        }
        if let Some(base) = &mut self.auto_base_descriptor {
            *base = PreparedInputMutation::new(
                base.family(),
                base.command_nonce().to_owned(),
                scopes.clone(),
                base.command_digest().to_owned(),
                base.prepared_at(),
            )?;
        }
        self.descriptor = PreparedInputMutation::new(
            self.descriptor.family(),
            self.descriptor.command_nonce().to_owned(),
            scopes,
            self.descriptor.command_digest().to_owned(),
            self.descriptor.prepared_at(),
        )?;
        Ok(self)
    }

    // Extra owners are read-only; the explicit Task child alone has metadata writes.
    fn validate_primary_namespace(&self) -> Result<()> {
        let agent = |actual: &str, target: &str| -> Result<()> {
            anyhow::ensure!(
                actual == self.primary_scope.agent_id,
                "prepared input {target} target differs from primary namespace"
            );
            Ok(())
        };
        let state = |mutation: &AgentStateMutation| -> Result<()> {
            agent(&mutation.record.id, "State record")?;
            if let Some(expected) = &mutation.expected {
                agent(&expected.id, "State expected")?;
            }
            Ok(())
        };
        let indexes = |changes: &[RuntimeIndexChange]| -> Result<()> {
            for change in changes {
                agent(&change.agent_id, "index intent")?;
            }
            Ok(())
        };
        let work_items = |mutations: &[WorkItemMutation]| -> Result<()> {
            for mutation in mutations {
                agent(&mutation.record().agent_id, "WorkItem")?;
            }
            Ok(())
        };
        let waits = |records: &[WaitConditionRecord]| -> Result<()> {
            for record in records {
                agent(&record.agent_id, "WaitCondition")?;
            }
            Ok(())
        };
        agent(self.command.agent_id(), "command")?;
        match self.command.as_ref() {
            InputTransitionCommand::RuntimeAudit(command) => {
                command.validate(&self.primary_scope)?
            }
            InputTransitionCommand::Timer(command) => command.validate(&self.primary_scope)?,
            InputTransitionCommand::ExternalTrigger(command) => {
                command.validate(&self.primary_scope)?
            }
            InputTransitionCommand::TaskResult(command) => command.validate()?,
            InputTransitionCommand::TaskStatusMetadata(command) => anyhow::ensure!(
                command.scope() == &self.primary_scope,
                "Control metadata namespace differs"
            ),
            InputTransitionCommand::TaskStopProgress(command) => anyhow::ensure!(
                command.scope() == &self.primary_scope,
                "Stop phase namespace differs"
            ),
            InputTransitionCommand::TaskStopContinuation(command) => anyhow::ensure!(
                command.scope() == &self.primary_scope,
                "Stop Result namespace differs"
            ),
            InputTransitionCommand::Task(task) => {
                let command = &task.command;
                if let Some(intent) = &task.stop_intent {
                    task_stop::validate_command(task)?;
                    anyhow::ensure!(
                        intent.task.scope == self.primary_scope,
                        "TaskStop intent differs from original primary scope"
                    );
                }
                agent(&command.task.agent_id, "Task")?;
                work_items(&command.work_items)?;
                waits(&command.wait_conditions)?;
                for expected in &command.expected_wait_conditions {
                    agent(&expected.agent_id, "Task Wait expected")?;
                }
                if let Some(mutation) = &command.agent_state {
                    state(mutation)?;
                }
                if let Some(queue) = &command.queue_entry {
                    agent(&queue.agent_id, "Task Queue")?;
                }
                for message in &command.message_evidence {
                    agent(&message.agent_id, "Task Message")?;
                }
                if let Some(settlement) = &command.task_result_settlement {
                    agent(&settlement.agent_id, "Task settlement")?;
                    anyhow::ensure!(
                        settlement.task_id == command.task.id,
                        "Task settlement differs from original Task"
                    );
                }
                if let Some(request) = &command.agent_deletion {
                    let child = task.child.as_ref().context("Task child facts missing")?;
                    anyhow::ensure!(
                        child.scope.agent_id == request.agent_id
                            && child.expected_identity.agent_id == request.agent_id,
                        "Task deletion target differs from original child"
                    );
                    use crate::runtime_db::repositories::AgentDeletionAdmission;
                    match &request.admission {
                        AgentDeletionAdmission::TerminalEphemeralChild {
                            parent_agent_id,
                            task_id,
                        } => {
                            agent(parent_agent_id, "child deletion parent")?;
                            anyhow::ensure!(
                                task_id == &command.task.id,
                                "child deletion belongs to a different parent Task"
                            );
                        }
                        AgentDeletionAdmission::SupervisingParent {
                            parent_agent_id,
                            parent_incarnation,
                            expected_incarnation,
                        } => {
                            agent(parent_agent_id, "child deletion parent")?;
                            anyhow::ensure!(
                                *parent_incarnation == self.primary_scope.identity_incarnation
                                    && *expected_incarnation == child.scope.identity_incarnation,
                                "child deletion admission incarnation differs from original group"
                            );
                        }
                        AgentDeletionAdmission::ExpectedRevision(revision) => anyhow::ensure!(
                            *revision == child.expected_identity.revision,
                            "child deletion expected revision differs from original identity"
                        ),
                    }
                }
                indexes(&command.index_changes)?;
                self.validate_protocol_namespace(&task.execution_protocol)?;
            }
            InputTransitionCommand::Work {
                command,
                execution_protocol,
                ..
            } => {
                work_items(std::slice::from_ref(&command.mutation))?;
                if let Some(mutation) = &command.agent_state {
                    state(mutation)?;
                }
                for brief in &command.brief_evidence {
                    agent(&brief.agent_id, "Brief")?;
                }
                indexes(&command.index_changes)?;
                self.validate_protocol_namespace(execution_protocol)?;
            }
            InputTransitionCommand::Focus {
                command,
                execution_protocol,
                completion_tool_execution,
                ..
            } => {
                work_items(&command.work_items)?;
                waits(&command.wait_conditions)?;
                state(&command.agent_state)?;
                for frame in command
                    .active_continuation_expectations
                    .iter()
                    .flatten()
                    .chain(&command.continuation_expectations)
                    .chain(&command.continuations)
                {
                    agent(&frame.agent_id, "continuation")?;
                }
                for brief in &command.brief_evidence {
                    agent(&brief.agent_id, "Brief")?;
                }
                if let Some(tool) = completion_tool_execution {
                    agent(&tool.agent_id, "completion Tool")?;
                }
                indexes(&command.index_changes)?;
                self.validate_protocol_namespace(execution_protocol)?;
            }
            InputTransitionCommand::Wait {
                command,
                execution_protocol,
                task_expectation,
            } => {
                work_items(&command.work_items)?;
                waits(&command.wait_conditions)?;
                for expected in &command.expected_wait_conditions {
                    agent(&expected.agent_id, "Wait expected")?;
                }
                if let Some(mutation) = &command.agent_state {
                    state(mutation)?;
                }
                if let Some(expected) = task_expectation {
                    agent(&expected.agent_id, "Task expected")?;
                }
                if let Some(admission) = &command.task_result_admission {
                    agent(&admission.message.agent_id, "Wait Message")?;
                    agent(&admission.record.agent_id, "Wait Queue record")?;
                    if let Some(expected) = &admission.expected {
                        agent(&expected.agent_id, "Wait Queue expected")?;
                    }
                }
                indexes(&command.index_changes)?;
                self.validate_protocol_namespace(execution_protocol)?;
            }
            InputTransitionCommand::Posture {
                command,
                started_turn,
            } => {
                state(&command.agent_state)?;
                if let Some(turn) = started_turn {
                    agent(&turn.agent_id, "started Turn")?;
                }
            }
            InputTransitionCommand::Terminal(command) => {
                state(&command.agent_state)?;
                agent(&command.turn_record.agent_id, "terminal Turn")?;
                for tool in &command.terminal_tool_executions {
                    agent(&tool.agent_id, "terminal Tool")?;
                }
            }
            InputTransitionCommand::QueueHeadNoProgress { command, .. } => {
                agent(&command.expected.agent_id, "Queue expected")?;
                agent(&command.quarantined.agent_id, "Queue record")?;
                state(&command.agent_state)?;
            }
            InputTransitionCommand::Queue(queue) => {
                terminal_state_audit::validate_queue(queue)?;
                let command = &queue.command;
                agent(
                    &queue_mutation_record(&command.mutation).agent_id,
                    "Queue record",
                )?;
                if let QueueMutation::CompareAndSet { expected, .. } = &command.mutation {
                    agent(&expected.agent_id, "Queue expected")?;
                }
                if let Some(mutation) = &command.agent_state {
                    state(mutation)?;
                }
                if let Some(expectation) = &command.scheduler_claim_work_item {
                    let record = match expectation {
                        SchedulerClaimWorkItemExpectation::Runnable(record)
                        | SchedulerClaimWorkItemExpectation::BlockedRecheck { record, .. } => {
                            record
                        }
                    };
                    agent(&record.agent_id, "scheduler WorkItem")?;
                }
                for message in &command.message_evidence {
                    agent(&message.agent_id, "Message")?;
                }
                for entry in &command.transcript_entries {
                    agent(&entry.agent_id, "Transcript")?;
                }
                for brief in &command.brief_evidence {
                    agent(&brief.agent_id, "Brief")?;
                }
                if let Some(turn) = &command.turn_record {
                    agent(&turn.agent_id, "Turn")?;
                }
                for tool in &queue.terminal_tool_executions {
                    agent(&tool.agent_id, "terminal Tool")?;
                }
                if let Some(protocol) = &queue.execution_protocol.bootstrap {
                    agent(&protocol.agent_id, "execution bootstrap")?;
                }
                for command in &queue.execution_protocol.commands {
                    if let crate::domain::execution_protocol::ExecutionProtocolCommand::Admit(
                        command,
                    ) = command
                    {
                        agent(&command.attempt.agent_id, "execution Attempt")?;
                    }
                }
                if let Some(expectation) = &queue.task_expectation {
                    agent(&expectation.agent_id, "Task expectation")?;
                }
                if let Some(wait) = &queue.wait_transition {
                    agent(&wait.expected.agent_id, "Wait expected")?;
                    agent(&wait.record.agent_id, "WaitCondition")?;
                    if let Some(work) = &wait.work_item {
                        work_items(std::slice::from_ref(work))?;
                    }
                    indexes(&wait.index_changes)?;
                }
                if let Some(wait) = &queue.wait_registration {
                    agent(&wait.agent_id, "Wait registration")?;
                    work_items(&wait.work_items)?;
                    waits(&wait.wait_conditions)?;
                    for expected in &wait.expected_wait_conditions {
                        agent(&expected.agent_id, "Wait expected")?;
                    }
                    if let Some(mutation) = &wait.agent_state {
                        state(mutation)?;
                    }
                    if let Some(admission) = &wait.task_result_admission {
                        agent(&admission.message.agent_id, "Wait Message")?;
                        agent(&admission.record.agent_id, "Wait Queue record")?;
                        if let Some(expected) = &admission.expected {
                            agent(&expected.agent_id, "Wait Queue expected")?;
                        }
                    }
                    indexes(&wait.index_changes)?;
                }
                waits(&queue.extra_wait_conditions)?;
                if let Some(completion) = &queue.completion {
                    work_items(&completion.work_items)?;
                    waits(&completion.wait_conditions)?;
                    for frame in completion
                        .continuation_expectations
                        .iter()
                        .chain(&completion.continuations)
                    {
                        agent(&frame.agent_id, "continuation")?;
                    }
                    agent(&completion.tool_execution.agent_id, "completion Tool")?;
                    indexes(&completion.index_changes)?;
                }
                if let Some(delivery) = &queue.delivery {
                    agent(&delivery.target_agent_id, "delivery")?;
                }
            }
        }
        Ok(())
    }

    fn validate_protocol_namespace(&self, protocol: &ExecutionProtocolTransition) -> Result<()> {
        if let Some(bootstrap) = &protocol.bootstrap {
            anyhow::ensure!(
                bootstrap.agent_id == self.primary_scope.agent_id,
                "execution bootstrap differs from primary namespace"
            );
        }
        for command in &protocol.commands {
            if let crate::domain::execution_protocol::ExecutionProtocolCommand::Admit(command) =
                command
            {
                anyhow::ensure!(
                    command.attempt.agent_id == self.primary_scope.agent_id,
                    "execution Attempt differs from primary namespace"
                );
            }
        }
        Ok(())
    }

    // ID-only mutable targets are checked in the original transaction after
    // receipt replay, so later pruning cannot invalidate an exact original ACK.
    fn validate_primary_targets_tx(&self, tx: &Transaction<'_>) -> Result<()> {
        // These global-ID writers permit owner changes in their original SQL.
        // Check only the finite actual footprint, once per existing target.
        let mut targets = BTreeSet::new();
        if let InputTransitionCommand::Task(task) = self.command.as_ref() {
            let owner: Option<String> = tx
                .query_row(
                    "SELECT owner_agent_id FROM tasks WHERE task_id=?1",
                    [&task.command.task.id],
                    |row| row.get(0),
                )
                .optional()?;
            anyhow::ensure!(
                owner.is_none_or(|owner| owner == self.primary_scope.agent_id),
                "prepared Task stored target differs from primary namespace"
            );
            if let Some(intent) = &task.stop_intent {
                task_stop::validate_child_readonly_tx(tx, intent)?;
            }
            if let Some(child) = &task.child {
                let actual = task_child_identity_tx(tx, &child.scope)?;
                anyhow::ensure!(
                    actual == child.expected_identity,
                    "Task child identity changed before original business SQL"
                );
            }
        }
        match self.command.as_ref() {
            InputTransitionCommand::Task(task) => {
                let command = &task.command;
                if let Some(queue) = &command.queue_entry {
                    targets.insert(("queue_entries", "message_id", queue.message_id.as_str()));
                }
                for work in &command.work_items {
                    targets.insert(("work_items", "work_item_id", work.record().id.as_str()));
                }
                for wait in &command.wait_conditions {
                    targets.insert(("wait_conditions", "wait_condition_id", wait.id.as_str()));
                }
            }
            InputTransitionCommand::Queue(queue) => {
                targets.insert((
                    "queue_entries",
                    "message_id",
                    queue_mutation_record(&queue.command.mutation)
                        .message_id
                        .as_str(),
                ));
                if let Some(admission) = queue
                    .wait_registration
                    .as_ref()
                    .and_then(|wait| wait.task_result_admission.as_ref())
                {
                    targets.insert((
                        "queue_entries",
                        "message_id",
                        admission.record.message_id.as_str(),
                    ));
                }
                for work in queue
                    .wait_transition
                    .iter()
                    .filter_map(|wait| wait.work_item.as_ref())
                    .chain(
                        queue
                            .wait_registration
                            .iter()
                            .flat_map(|wait| &wait.work_items),
                    )
                    .chain(
                        queue
                            .completion
                            .iter()
                            .flat_map(|completion| &completion.work_items),
                    )
                {
                    targets.insert(("work_items", "work_item_id", work.record().id.as_str()));
                }
                for wait in queue
                    .wait_transition
                    .iter()
                    .map(|wait| &wait.record)
                    .chain(
                        queue
                            .wait_registration
                            .iter()
                            .flat_map(|wait| &wait.wait_conditions),
                    )
                    .chain(queue.extra_wait_conditions.iter())
                    .chain(
                        queue
                            .completion
                            .iter()
                            .flat_map(|completion| &completion.wait_conditions),
                    )
                {
                    targets.insert(("wait_conditions", "wait_condition_id", wait.id.as_str()));
                }
            }
            InputTransitionCommand::QueueHeadNoProgress { command, .. } => {
                targets.insert((
                    "queue_entries",
                    "message_id",
                    command.quarantined.message_id.as_str(),
                ));
            }
            InputTransitionCommand::Work { command, .. } => {
                targets.insert((
                    "work_items",
                    "work_item_id",
                    command.mutation.record().id.as_str(),
                ));
            }
            InputTransitionCommand::Focus { command, .. } => {
                for work in &command.work_items {
                    targets.insert(("work_items", "work_item_id", work.record().id.as_str()));
                }
                for wait in &command.wait_conditions {
                    targets.insert(("wait_conditions", "wait_condition_id", wait.id.as_str()));
                }
            }
            InputTransitionCommand::Wait { command, .. } => {
                for work in &command.work_items {
                    targets.insert(("work_items", "work_item_id", work.record().id.as_str()));
                }
                for wait in &command.wait_conditions {
                    targets.insert(("wait_conditions", "wait_condition_id", wait.id.as_str()));
                }
                if let Some(admission) = &command.task_result_admission {
                    targets.insert((
                        "queue_entries",
                        "message_id",
                        admission.record.message_id.as_str(),
                    ));
                }
                if let Some(claim) = &command.timer_wake {
                    targets.insert(("timers", "timer_id", claim.timer_id.as_str()));
                }
            }
            // Turn, Tool, Continuation and canonical evidence already reject
            // existing foreign owners in their original validators.
            InputTransitionCommand::RuntimeAudit(_)
            | InputTransitionCommand::Posture { .. }
            | InputTransitionCommand::Terminal(_)
            | InputTransitionCommand::TaskResult(_)
            | InputTransitionCommand::TaskStatusMetadata(_)
            | InputTransitionCommand::Timer(_)
            | InputTransitionCommand::ExternalTrigger(_)
            | InputTransitionCommand::TaskStopProgress(_)
            | InputTransitionCommand::TaskStopContinuation(_) => {}
        }
        for (table, column, id) in targets {
            let owner: Option<String> = tx
                .query_row(
                    &format!("SELECT agent_id FROM {table} WHERE {column}=?1"),
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(owner) = owner {
                anyhow::ensure!(
                    owner == self.primary_scope.agent_id,
                    "prepared input stored {table} target differs from primary namespace"
                );
            }
        }
        let InputTransitionCommand::Queue(queue) = self.command.as_ref() else {
            return Ok(());
        };
        let check = |actual: &str, target: &str| -> Result<()> {
            anyhow::ensure!(
                actual == self.primary_scope.agent_id,
                "prepared input {target} target differs from primary namespace"
            );
            Ok(())
        };
        let record = queue_mutation_record(&queue.command.mutation);
        if let Some(delivery) = delivery_by_message_id_tx(tx, &record.message_id)? {
            check(&delivery.target_agent_id, "stored delivery")?;
        }
        if let Some(candidate) = &queue.delivery {
            if let Some(delivery) =
                crate::runtime_db::agent_message_delivery::delivery_by_idempotency_scope_tx(
                    tx,
                    &candidate.idempotency_scope,
                    &candidate.idempotency_key_digest,
                )?
            {
                check(&delivery.target_agent_id, "stored delivery")?;
            }
        }
        if let Some(claim) = &queue.timer_wake_claim {
            let owner: Option<String> = tx
                .query_row(
                    "SELECT agent_id FROM timers WHERE timer_id=?1",
                    [&claim.timer_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(owner) = owner {
                check(&owner, "Timer wake")?;
            }
        }
        Ok(())
    }

    /// Potential pending admissions, borrowed from the one frozen command.
    /// Existing queued bodies must be frozen by the caller before preparation;
    /// this method never reads storage or fabricates a SQL-produced body.
    pub(crate) fn pending_messages(&self) -> Result<Vec<&MessageEnvelope>> {
        fn add<'a>(
            primary_scope: &InputEvidenceScope,
            pending: &mut Vec<&'a MessageEnvelope>,
            record: &QueueEntryRecord,
            message: &'a MessageEnvelope,
        ) -> Result<()> {
            anyhow::ensure!(
                record.agent_id == primary_scope.agent_id
                    && message.agent_id == primary_scope.agent_id
                    && record.message_id == message.id,
                "pending admission differs from its original primary scope"
            );
            if let Some(previous) = pending.iter().find(|previous| previous.id == message.id) {
                anyhow::ensure!(
                    *previous == message,
                    "pending admission repeats an ID with a different frozen body"
                );
            } else {
                pending.push(message);
            }
            Ok(())
        }
        let mut pending = Vec::new();
        if let InputTransitionCommand::Timer(timer) = self.command.as_ref() {
            if let Some((message, queue)) = timer.new_wake() {
                add(&self.primary_scope, &mut pending, queue, message)?;
            }
            return Ok(pending);
        }
        if let InputTransitionCommand::TaskStopContinuation(command) = self.command.as_ref() {
            anyhow::ensure!(
                command.message().agent_id == self.primary_scope.agent_id,
                "Stop Result pending scope differs"
            );
            pending.push(command.message());
            return Ok(pending);
        }
        if let InputTransitionCommand::Task(task) = self.command.as_ref() {
            if let Some(queue) = &task.command.queue_entry {
                let [message] = task.command.message_evidence.as_slice() else {
                    anyhow::bail!("Task queue admission requires exactly one frozen Message");
                };
                if matches!(
                    queue.status,
                    QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
                ) {
                    add(&self.primary_scope, &mut pending, queue, message)?;
                }
            }
            return Ok(pending);
        }
        if let InputTransitionCommand::Wait { command, .. } = self.command.as_ref() {
            if let Some(admission) = &command.task_result_admission {
                if matches!(
                    admission.record.status,
                    QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
                ) {
                    add(
                        &self.primary_scope,
                        &mut pending,
                        &admission.record,
                        &admission.message,
                    )?;
                }
            }
            return Ok(pending);
        }
        let InputTransitionCommand::Queue(queue) = self.command.as_ref() else {
            return Ok(pending);
        };
        let record = queue_mutation_record(&queue.command.mutation);
        if matches!(
            record.status,
            QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
        ) {
            let mut matching = queue
                .command
                .message_evidence
                .iter()
                .filter(|message| message.id == record.message_id);
            let message = matching
                .next()
                .context("pending queue mutation requires its frozen message body")?;
            add(&self.primary_scope, &mut pending, record, message)?;
            for duplicate in matching {
                add(&self.primary_scope, &mut pending, record, duplicate)?;
            }
        }
        if let Some(admission) = queue
            .wait_registration
            .as_ref()
            .and_then(|wait| wait.task_result_admission.as_ref())
        {
            anyhow::ensure!(
                queue
                    .wait_registration
                    .as_ref()
                    .is_some_and(|wait| wait.agent_id == self.primary_scope.agent_id),
                "pending wait registration differs from its original primary scope"
            );
            if matches!(
                admission.record.status,
                QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
            ) {
                add(
                    &self.primary_scope,
                    &mut pending,
                    &admission.record,
                    &admission.message,
                )?;
            }
        }
        Ok(pending)
    }
}

/// Business no-op is still Committed: receipt insertion is a real transaction.
/// Replayed never contains an earlier candidate, State, queue, or input delta.
#[derive(Debug)]
pub(crate) enum InputMutationExecution<T> {
    Committed {
        value: T,
        receipt: InputMutationReceipt,
    },
    Replayed {
        receipt: InputMutationReceipt,
        _material_budget_views:
            Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    },
}

impl RuntimeTransitionRepository<'_> {
    #[cfg(test)]
    pub(crate) fn commit_prepared_transition(
        &self,
        prepared: &PreparedInputTransition,
    ) -> Result<InputMutationExecution<TransitionCommit>> {
        let scope = self
            .input_scope
            .as_ref()
            .context("prepared input transition requires an explicit reserved scope")?;
        anyhow::ensure!(
            prepared.primary_scope == *scope
                && prepared.descriptor.scopes().contains(scope)
                && prepared.command.agent_id() == scope.agent_id,
            "prepared input transition scope differs from its repository"
        );
        self.commit_prepared_transition_with_work_request(
            prepared,
            &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                record_limit: 65_536,
                byte_limit: 64 * 1024 * 1024,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn commit_prepared_transition_with_work_request(
        &self,
        prepared: &PreparedInputTransition,
        work_request: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
    ) -> Result<InputMutationExecution<TransitionCommit>> {
        let ledger = std::sync::Arc::new(
            crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
                crate::config::StateMemoryLimits::default(),
            )?,
        );
        self.commit_prepared_transition_with_work_request_and_memory_budget(
            prepared,
            work_request,
            &ledger,
            &crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                query: Default::default(),
                capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                record_limit: work_request.record_limit,
                byte_limit: work_request.byte_limit,
            },
        )
    }

    /// Real Host ledger observes transaction-owned memory. Candidate observation
    /// does not consume History limits; this TX uses one combined acquisition cap.
    pub(crate) fn commit_prepared_transition_with_work_request_and_memory_budget(
        &self,
        prepared: &PreparedInputTransition,
        work_request: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
        memory_ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        task_request: &crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
    ) -> Result<InputMutationExecution<TransitionCommit>> {
        self.commit_prepared_transition_with_domain_requests(
            prepared,
            work_request,
            memory_ledger,
            task_request,
            &timer_writes::default_request(work_request),
        )
    }
    pub(crate) fn commit_prepared_transition_with_input_query_source(
        &self,
        prepared: &PreparedInputTransition,
        work_request: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
        memory_ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        task_request: &crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
        input_source: &std::sync::Arc<crate::runtime::mutation_owner::OriginalInputQuerySource>,
    ) -> Result<InputMutationExecution<TransitionCommit>> {
        self.commit_prepared_transition_with_domain_requests_and_input_source(
            prepared,
            work_request,
            memory_ledger,
            task_request,
            &timer_writes::default_request(work_request),
            Some(input_source),
        )
    }
    pub(crate) fn commit_prepared_transition_with_domain_requests(
        &self,
        prepared: &PreparedInputTransition,
        work_request: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
        memory_ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        task_request: &crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
        scheduling_request: &crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest,
    ) -> Result<InputMutationExecution<TransitionCommit>> {
        self.commit_prepared_transition_with_domain_requests_and_input_source(
            prepared,
            work_request,
            memory_ledger,
            task_request,
            scheduling_request,
            None,
        )
    }
    fn commit_prepared_transition_with_domain_requests_and_input_source(
        &self,
        prepared: &PreparedInputTransition,
        work_request: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
        memory_ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        task_request: &crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
        scheduling_request: &crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest,
        input_source: Option<
            &std::sync::Arc<crate::runtime::mutation_owner::OriginalInputQuerySource>,
        >,
    ) -> Result<InputMutationExecution<TransitionCommit>> {
        let scope = self
            .input_scope
            .as_ref()
            .context("prepared input transition requires an explicit reserved scope")?;
        anyhow::ensure!(
            prepared.primary_scope == *scope
                && prepared.descriptor.scopes().contains(scope)
                && prepared.command.agent_id() == scope.agent_id,
            "prepared input transition scope differs from its repository"
        );
        self.db.transaction(|tx| {
            if let InputTransitionCommand::RuntimeAudit(command)=prepared.command.as_ref() {
                return runtime_audit::commit_tx(tx,prepared,command,memory_ledger);
            }
            if let InputTransitionCommand::ExternalTrigger(command)=prepared.command.as_ref() {
                return trigger_writes::commit_tx(tx,prepared,command,scheduling_request,memory_ledger);
            }
            if let InputTransitionCommand::TaskStatusMetadata(command)=prepared.command.as_ref() {
                return task_status_metadata::commit_tx(tx,prepared,command,task_request,memory_ledger);
            }
            if prepared.original_child_monitor_binding().is_some() {
                if let Some(execution)=child_binding::replay_tx(tx,prepared,task_request,memory_ledger)? { return Ok(execution); }
            }
            if prepared.original_child_terminal().is_some() {
                if let Some(execution)=child_terminal::replay_or_sparse_tx(tx,prepared,task_request,memory_ledger)? {return Ok(execution)}
            }
            if prepared.original_command_terminal().is_some() {
                if let Some(execution) = command_terminal::replay_or_sparse_tx(
                    tx, prepared, task_request, memory_ledger,
                )? { return Ok(execution); }
            }
            if let InputTransitionCommand::TaskStopProgress(command) = prepared.command.as_ref() {
                return task_stop_phase::commit_progress_tx(tx, prepared, command, task_request, memory_ledger);
            }
            if let InputTransitionCommand::TaskStopContinuation(command) = prepared.command.as_ref() {
                return task_stop_result::commit_result_tx(tx, prepared, command, work_request, task_request, memory_ledger, input_source);
            }
            let mut first_input=workspace_input::OriginalInputWriterMaterial::prepare(prepared,scope,memory_ledger,work_request,task_request,input_source)?;
            let mut original_scope_facts=None;
            let child_entry=if prepared.original_child_terminal().is_some() {
                Some(child_terminal::preflight_current_entry_tx(tx,prepared,task_request,memory_ledger)?)
            } else if prepared.original_child_monitor_binding().is_some() {
                Some(child_binding::preflight_current_entry_tx(tx, prepared, task_request, memory_ledger)?)
            } else {None};
            // Current authorization precedes replay; OCC and canonical aliases
            // follow it. A later current version never invalidates exact replay.
            for reserved_scope in prepared.descriptor.scopes() {
                // An already accepted exact receipt may be replayed during
                // retirement. A missing receipt still needs typed admission.
                if reserved_scope == scope {
                    if let Some(first)=first_input.as_mut() {
                        let facts=first.input.scope_facts_tx(tx)?;
                        anyhow::ensure!(facts.identity.as_ref().is_some_and(|identity|
                            identity.agent_id==scope.agent_id && identity.incarnation==scope.identity_incarnation
                            && matches!(identity.status,crate::types::AgentRegistryStatus::Active|crate::types::AgentRegistryStatus::Deleting)),
                            "original input recovery requires its current non-deleted incarnation");
                        original_scope_facts=Some(facts);
                    } else {
                        crate::runtime_db::input_evidence_snapshot::require_original_input_recovery_scope(tx,reserved_scope)?;
                    }
                } else if prepared.task_stop_child_scope() == Some(reserved_scope) {
                    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else { unreachable!() };
                    task_stop::validate_child_readonly_tx(tx, task.stop_intent.as_ref().expect("Stop child scope"))?;
                } else if prepared.task_deletion_scope() == Some(reserved_scope) {
                    // Same original child may already be Deleted at exact replay;
                    // no new write is allowed without its frozen pre-State below.
                    let _ = task_child_identity_tx(tx, reserved_scope)?;
                } else if prepared.original_child_terminal().is_some() {
                    child_terminal::validate_reserved_scope(prepared,reserved_scope)?;
                } else {
                    crate::runtime_db::input_evidence_writes::validate_readonly_input_reservation_tx(
                        tx,
                        reserved_scope,
                    )?;
                }
            }
            prepared.validate_primary_namespace()?;
            let result_receipt_guard=if first_input.is_none() && matches!(prepared.command.as_ref(),InputTransitionCommand::Task(_) | InputTransitionCommand::TaskResult(_) | InputTransitionCommand::Timer(_)) {
                Some(super::prepared_task_result::receipt_budget_tx(tx,&prepared.descriptor,scope,task_request,memory_ledger)?)
            } else {None};
            if let Some(first)=first_input.as_mut() {first.input.original_receipt_material_tx(tx,&prepared.descriptor)?;}
            if let Some(receipt) = lookup_input_mutation_receipt_tx(tx, &prepared.descriptor)? {
                let mut views:Vec<_>=result_receipt_guard.into_iter().collect();
                if let Some(first)=first_input {views.extend(first.input.into_guards());}
                return Ok(InputMutationExecution::Replayed { receipt,_material_budget_views:views });
            }
            if let Some(child)=prepared.original_task_input_child_material() {
                let identity=original_scope_facts.as_ref()
                    .and_then(|facts|facts.identity.as_ref())
                    .context("TaskInput child lacks its actual scoped identity")?;
                // The closed follow-up already owns its request. Recheck the
                // actual same-TX parent/task association before Memory or SQL;
                // an exact receipt above keeps its original replay authority.
                child.validate_identity(identity)?;
            }
            // Exact replay above is legal after later dispatches. A new claim
            // or StartedTurn must first finish the prior actual phase.
            if matches!(prepared.command.as_ref(),InputTransitionCommand::Queue(queue)
                if queue.command.operation == QueueOperation::Claim)
                || matches!(prepared.command.as_ref(),InputTransitionCommand::Posture { started_turn:Some(_),.. }) {
                crate::runtime_db::dispatch_memory::validate_new_dispatch_tx(tx,scope)?;
            }
            if let InputTransitionCommand::Posture { started_turn: Some(turn), .. } = prepared.command.as_ref() {
                if let Some(source) = turn.trigger.as_ref().and_then(|trigger| trigger.message_id.as_deref()) {
                    // Only booleans are acquired here; malformed/unbounded assignment
                    // addresses cannot allocate before the transaction material budget.
                    let assignment: Option<(bool,bool)> = rusqlite::OptionalExtension::optional(tx.query_row(
                        "SELECT COALESCE(t.agent_id=?1 AND json_extract(t.payload_json,'$.agent_id')=?1,0),t.turn_id IS NOT ?3 AND json_type(t.payload_json,'$.terminal') IS NOT 'object' FROM conversation_input_assignments a LEFT JOIN turn_records t ON t.turn_id=a.turn_id WHERE a.agent_id=?1 AND a.message_id=?2",
                        rusqlite::params![scope.agent_id,source,turn.turn_id],|r|Ok((r.get(0)?,r.get(1)?)),
                    ))?;
                    if let Some((owned,historical_open)) = assignment {
                        anyhow::ensure!(owned,"historical assignment Turn owner differs");
                        anyhow::ensure!(!historical_open || prepared.original_dispatch_claim.is_some(),
                            crate::runtime::started_dispatch::StartedDispatchConflict("historical assignment requires an original live dispatch claim"));
                    }
                }
            }
            prepared.validate_primary_targets_tx(tx)?;
            let mut memory_budget=prepared.memory.as_ref().map(|memory|
                crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget::prepare_with_original_input_tx(
                    tx,memory,work_request,memory_ledger,prepared.work_resource_material.as_ref(),
                    first_input.as_ref().map(|first|&first.input))).transpose()?;
            if let Some(authorization) = &prepared.ended_memory {
                let (bytes, records) = authorization.proof_material_tx(tx)?;
                memory_budget.as_mut().context("ended memory omitted budget")?
                    .additional_material(bytes, records, 12)?;
            }
            if let Some(claim) = &prepared.original_dispatch_claim {
                let (bytes, records) = claim.proof_material_tx(tx)?;
                memory_budget.as_mut().context("original claim omitted Start memory budget")?
                    .additional_material(bytes, records, 4)?;
                let InputTransitionCommand::Posture { command, started_turn: Some(turn) } = prepared.command.as_ref() else {
                    anyhow::bail!("original claim has another business command");
                };
                claim.validate_posture_tx(tx, command, turn)?;
            }
            prepared.validate_memory_ending_tx(tx)?;
            let facts=match original_scope_facts.take() {
                Some(facts)=>facts,
                None=>crate::runtime_db::context_inputs::ContextDurableRead::new(tx,&scope.agent_id).scope_facts()?,
            };
            let deleting = facts.identity.as_ref().is_some_and(|identity| {
                identity.status == crate::types::AgentRegistryStatus::Deleting
            });
            if matches!(prepared.command.as_ref(), InputTransitionCommand::Task(task) if task.stop_intent.is_some()) {
                anyhow::ensure!(facts.identity.as_ref().is_some_and(|identity|
                    identity.status == crate::types::AgentRegistryStatus::Active),
                    "new TaskStop requires original Active identity");
            }
            // NoMemory family phases have no global Memory counter. Their
            // first decoder borrows only the same Input phase's true remainder.
            let shared_nomemory=first_input.is_some() && memory_budget.is_none();
            let scheduling_policy=if shared_nomemory && matches!(prepared.command.as_ref(),
                InputTransitionCommand::Timer(_)|InputTransitionCommand::Queue(_)) {
                let first=first_input.as_mut().unwrap();
                // Scheduling freezes its original target key before creating
                // its family proof. Admit those exact borrowed IDs first.
                let id=match prepared.command.as_ref() {
                    InputTransitionCommand::Timer(timer)=>Some(timer.record().id.as_str()),
                    InputTransitionCommand::Queue(queue)=>queue.timer_wake_claim.as_ref().map(|claim|claim.timer_id.as_str()),
                    _=>None,
                };
                if let Some(id)=id {
                    first.input.observe_projected(u64::try_from(id.len().checked_add(256)
                        .and_then(|n|n.checked_mul(8)).context("Scheduling original key material overflow")?)?,8)?;
                }
                Some(workspace_input::scheduling_request(&mut first.input,scheduling_request,(0,0))?)
            } else {None};
            let scheduling_transaction = timer_writes::prepare_tx(tx,prepared,scope,scheduling_policy.as_ref().unwrap_or(scheduling_request),
                memory_ledger,memory_budget.as_mut())?;
            if shared_nomemory {
                if let Some(scheduling)=&scheduling_transaction {
                    first_input.as_mut().unwrap().input.observe_original_writer_prior(scheduling.input_writer_material(tx,scope,memory_ledger)?)?;
                }
            }
            let task_policy=if shared_nomemory && !matches!(prepared.command.as_ref(),
                InputTransitionCommand::Posture {..}|InputTransitionCommand::Terminal(_)|InputTransitionCommand::Timer(_)|InputTransitionCommand::QueueHeadNoProgress {..}) {
                Some(workspace_input::task_request(&mut first_input.as_mut().unwrap().input,task_request)?)
            } else {None};
            let mut task_transaction = if let Some(entry)=child_entry {
                task_writes::prepare_child_task_transaction_tx(tx,prepared,scope,deleting,task_request,memory_ledger,memory_budget.as_mut(),entry)?
            } else {
                task_writes::prepare_task_transaction_tx(tx,prepared,scope,deleting,task_policy.as_ref().unwrap_or(task_request),memory_ledger,memory_budget.as_mut())?
            };
            if shared_nomemory {
                if let Some(tasks)=&task_transaction {
                    first_input.as_mut().unwrap().input.observe_original_writer_prior(tasks.input_writer_material(scope,memory_ledger)?)?;
                }
            }
            if prepared.original_command_terminal().is_some() {
                let InputTransitionCommand::Task(command)=prepared.command.as_ref() else {unreachable!()};
                task_transaction.as_mut().context("Command terminal omitted W1 proof")?
                    .prepare_original_terminal_material_tx(tx,&command.command,memory_ledger,memory_budget.as_mut())?;
                command_terminal::validate_current_task_proof(prepared,
                    task_transaction.as_ref().context("Command terminal omitted W1 proof")?.task_write_proof())?;
            }
            if prepared.original_child_terminal().is_some() {
                let InputTransitionCommand::Task(command)=prepared.command.as_ref() else{unreachable!()};
                let tasks=task_transaction.as_mut().context("child terminal omitted W1 proof")?;
                tasks.prepare_original_terminal_material_tx(tx,&command.command,memory_ledger,memory_budget.as_mut())?;
                child_terminal::validate_current_task_proof(prepared,tasks.task_write_proof())?;
            }
            if prepared.original_child_monitor_binding().is_some() {
                child_binding::validate_task_proof(prepared, task_transaction.as_ref()
                    .context("child binding omitted original Task proof")?.task_write_proof())?;
            }
            let stop_proof = if let InputTransitionCommand::Task(task) = prepared.command.as_ref() {
                if let Some(intent) = &task.stop_intent {
                    let tasks = task_transaction.as_mut().context("TaskStop omitted original Task proof")?;
                    tasks.prepare_stop_material_tx(tx, &prepared.descriptor, intent, &task.command.task,
                        memory_ledger, memory_budget.as_mut())?;
                    Some(crate::runtime_db::task_stop::prepare_task_stop_tx(tx, &prepared.descriptor, intent,
                        tasks.task_write_proof(), task_request, memory_ledger)?)
                } else { None }
            } else { None };
            let mut inserted_stop = None;
            let resource_prior=prepared.work_resource_material.as_ref()
                .map(|loan|loan.validate_accounting(scope,memory_ledger)).transpose()?.unwrap_or((0,0));
            let work_policy=if shared_nomemory && matches!(prepared.command.as_ref(),
                InputTransitionCommand::Task(_)|InputTransitionCommand::Timer(_)|InputTransitionCommand::Work {..}
                |InputTransitionCommand::Focus {..}|InputTransitionCommand::Wait {..}|InputTransitionCommand::Queue(_)) {
                Some(workspace_input::work_request(&mut first_input.as_mut().unwrap().input,scope,work_request,resource_prior)?)
            } else {None};
            let work_proof = prepared.prepare_work_proof_tx(tx, scope, deleting,work_policy.as_ref().unwrap_or(work_request),
                memory_ledger, memory_budget.as_mut(),scheduling_transaction.as_ref())?;
            if shared_nomemory {
                if let Some(work)=&work_proof {
                    first_input.as_mut().unwrap().input.observe_original_writer_prior(
                        workspace_input::increment(work.input_writer_material(scope,memory_ledger)?,resource_prior)?)?;
                }
            }
            anyhow::ensure!(!prepared.work_resource_material.as_ref().is_some_and(|loan|loan.has_original_resource()) || work_proof.is_some(),
                "original Resource loan omitted its actual Work proof");
            let producer_input = match child_terminal::prepare_input_view_tx(tx,prepared,
                task_transaction.as_ref().map(|task|task.task_write_proof()))? {
                Some(view)=>Some(view),None=>command_terminal::prepare_input_view_tx(tx,prepared,
                    task_transaction.as_ref().map(|task|task.task_write_proof()))?,
            };
            let cleanup = if deleting {
                Some(prepared.cleanup_proof_tx(tx, scope, work_proof.as_ref(),
                    task_transaction.as_ref().and_then(|task|task.cleanup()),producer_input.as_ref())?)
            } else { None };
            // Freeze the original memory baseline before any business write.
            // Its own phase cannot substitute for this whole command's receipt.
            let detached_memory = match &prepared.memory {
                Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Deferred { binding, frozen }) => {
                    let proof = work_proof.as_ref().context("detached memory omitted Work preflight proof")?;
                    let token = crate::runtime_db::dispatch_memory::prepare_detached_memory_tx(tx, proof,
                        &prepared.descriptor, frozen)?.context("detached memory has no actual StartedTurn")?;
                    crate::runtime_db::dispatch_memory::validate_detached_binding(&token, binding)?;
                    Some(token)
                }
                _ => None,
            };
            let finish_memory = match &prepared.memory {
                Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Finish { binding, frozen }) =>
                    Some(crate::runtime_db::dispatch_memory::prepare_dispatch_memory_finish_tx(tx,binding,frozen)?),
                _ => None,
            };
            let mut current_read_transaction = if let Some(state) = facts.state.as_ref().filter(|_| facts.identity.is_some()) {
                Some(match first_input.as_ref() {
                    Some(first)=>current_reads::prepare_with_material_limits_tx(tx,prepared,scope,state,memory_ledger,
                        finish_memory.as_ref(),Some(first.current_limits(memory_budget.as_ref())?))?,
                    None=>current_reads::prepare_tx(tx,prepared,scope,state,memory_ledger,finish_memory.as_ref())?,
                })
            } else { None };
            let task_tool_before = if input_source.is_some() {
                task_transaction.as_ref().map(|tasks|tasks.input_writer_material(scope,memory_ledger))
                    .transpose()?.unwrap_or((0,0))
            } else {(0,0)};
            let has_first_input=first_input.is_some();
            let mut input_birth = native_birth::prepare_tx(tx, prepared, scope, memory_ledger, work_request, input_source,
                work_proof.as_ref(), task_transaction.as_ref(), scheduling_transaction.as_ref(),
                current_read_transaction.as_ref(), memory_budget.as_mut(),first_input.take())?;
            let child_source_phase=if prepared.original_child_terminal().is_some() {
                Some(task_transaction.as_mut().context("child Source omitted enclosing Task proof")?
                    .apply_original_child_source_tx(tx,prepared,memory_ledger,memory_budget.as_mut())?)
            } else {None};
            let (current_tool_usage, task_tool_increment) = if input_source.is_some() {
                let current = current_read_transaction.as_ref()
                    .map(|current|current.input_writer_material(tx)?.validate_accounting(scope,memory_ledger))
                    .transpose()?.unwrap_or((0,0));
                let task = task_transaction.as_ref().map(|tasks|tasks.input_writer_material(scope,memory_ledger))
                    .transpose()?.unwrap_or((0,0));
                (current, (task.0.checked_sub(task_tool_before.0).context("original Task writer bytes retired")?,
                    task.1.checked_sub(task_tool_before.1).context("original Task writer nodes retired")?))
            } else {((0,0),(0,0))};
            if shared_nomemory {
                if let Some(current)=current_read_transaction.as_mut() {
                    input_birth.as_mut().context("original Current lost shared Input phase")?
                        .with_input_phase(tx,memory_ledger,None,|material|current.narrow_original_remaining(tx,material.remaining_material_limits()?))?;
                }
            }
            let mut current_admitted_after_business=current_tool_usage;
            let (mut value, mut result) = if let InputTransitionCommand::Timer(timer) = prepared.command.as_ref() {
                timer.execute_with_current_read_tx(tx,self.db,scope,work_request,task_request,memory_ledger,
                    memory_budget.as_mut(),current_read_transaction.as_mut())?
            } else {
                let mut execute = |input_tool_writer: Option<&dyn crate::runtime_db::input_evidence_snapshot::tool_writer::OriginalToolInputWriter>| -> Result<_> {
            let repository = RuntimeTransitionRepository {
                db: self.db,
                input_scope: Some(scope.clone()),
                cleanup_input: cleanup.as_ref(),
                input_tool_writer,
            };
            Ok(match prepared.command.as_ref() {
                InputTransitionCommand::RuntimeAudit(_) | InputTransitionCommand::ExternalTrigger(_) | InputTransitionCommand::TaskStatusMetadata(_) | InputTransitionCommand::TaskStopProgress(_) | InputTransitionCommand::TaskStopContinuation(_) => unreachable!("focused metadata phase dispatched above"),
                InputTransitionCommand::Posture {
                    command,
                    started_turn,
                } => (
                    repository.commit_agent_posture_with_current_read_tx(tx, command, started_turn.as_ref(), current_read_transaction.as_mut())?,
                    InputMutationResult::Transition,
                ),
                InputTransitionCommand::Terminal(command) => {
                    let commit = if prepared.ended_memory.is_some() {
                        validate_agent_state_mutation_tx(tx, Some(&command.agent_state))?;
                        inject_fault(command.fault, TransitionFaultPoint::AfterValidation)?;
                        TransitionCommit {
                            effects: Box::new(PostCommitEffects { fault: command.fault, ..Default::default() }),
                            ..Default::default()
                        }
                    } else { repository.commit_turn_terminal_with_current_read_tx(tx, command, current_read_transaction.as_mut())? };
                    (commit, InputMutationResult::Transition)
                },
                InputTransitionCommand::Timer(_) => unreachable!("Timer executes before the short Tool writer borrow"),
                InputTransitionCommand::TaskResult(_) => {
                    let ack=task_transaction.as_mut().context("TaskResult omitted its original proof")?
                        .execute_result()?;
                    let mut effects=PostCommitEffects::default();
                    effects.task_write_keys.extend(ack.actual_result_ids.iter().map(|id|
                        crate::runtime::resident_task::TaskKey::new(crate::runtime::resident_task::TaskFamily::ResultSettlement,id)));
                    (TransitionCommit {applied:!ack.actual_result_ids.is_empty(),effects: Box::new(effects),..Default::default()},
                        InputMutationResult::TaskResult {outcome:Box::new(ack.outcome)})
                }
                InputTransitionCommand::Task(task) => {
                    let pending=task_transaction.as_mut().and_then(|tasks|tasks.take_result());
                    let existing_binding=prepared.original_child_monitor_binding().map(|_|child_binding::existing_write_proof_tx(tx,prepared,task_transaction.as_ref().context("child binding omitted Task proof")?.task_write_proof())).transpose()?;
                    let (mut value, deletion) = repository.commit_task_at_with_task_results_with_current_read_tx(tx, &task.command,
                        &task.execution_protocol, task.synchronize_execution_protocol,
                        prepared.descriptor.prepared_at(), task.child.as_ref().map(|child| child.deletion_id.as_str()),
                        pending,task_transaction.as_ref().and_then(|tasks|tasks.cleanup()),stop_proof.as_ref(),existing_binding.as_ref(), current_read_transaction.as_mut())?;
                    if let Some(deletion) = &deletion {
                        let child = task.child.as_ref().context("Task deletion omitted original scope")?;
                        let actual_identity = task_child_identity_tx(tx, &child.scope)?;
                        let payload: String = tx.query_row(
                            "SELECT payload_json FROM agent_deletion_jobs WHERE agent_id=?1",
                            [&child.scope.agent_id], |row| row.get(0))?;
                        let actual_job: crate::types::AgentDeletionJob = serde_json::from_str(&payload)?;
                        anyhow::ensure!(actual_identity == deletion.identity && actual_job == deletion.job,
                            "Task deletion ACK differs from actual canonical metadata");
                    }
                    let stop = if let Some(proof) = &stop_proof {
                        let (record, observation) = crate::runtime_db::task_stop::insert_task_stop_with_observation_ack_tx(
                            proof, &task.command.task)?;
                        value.applied = true;
                        inserted_stop = Some(observation);
                        Some(record.receipt_binding())
                    } else { None };
                    let result = if let Some(original) = &task.original_command_terminal {
                        anyhow::ensure!(deletion.is_none() && stop.is_none(), "Command producer combined another lifecycle facet");
                        command_terminal::current_ack(original, &value)?
                    } else {
                        InputMutationResult::Task { task_id: task.command.task.id.clone(), deletion: deletion.map(Box::new), stop }
                    };
                    (value, result)
                }
                InputTransitionCommand::Work { command, execution_protocol, synchronize_execution_protocol } => (
                    repository.commit_work_item_with_current_read_tx(tx, command, execution_protocol, *synchronize_execution_protocol, current_read_transaction.as_mut())?,
                    InputMutationResult::Transition,
                ),
                InputTransitionCommand::Focus { command, execution_protocol, synchronize_execution_protocol, completion_tool_execution } => (
                    repository.commit_work_item_focus_with_current_read_tx(tx, command, execution_protocol,
                        *synchronize_execution_protocol, completion_tool_execution.as_ref(), current_read_transaction.as_mut())?,
                    InputMutationResult::Transition,
                ),
                InputTransitionCommand::Wait { command, execution_protocol, task_expectation } => (
                    repository.commit_wait_with_current_read_tx(tx, command, execution_protocol, task_expectation.as_ref(), current_read_transaction.as_mut())?,
                    InputMutationResult::Transition,
                ),
                InputTransitionCommand::Queue(queue) => (
                    repository.commit_queue_transaction_full_with_task_results_with_current_read_tx(
                        tx,
                        &queue.command,
                        &queue.execution_protocol,
                        queue.wait_transition.as_ref(),
                        queue.task_expectation.as_ref(),
                        queue.completion.as_ref(),
                        queue.wait_registration.as_ref(),
                        &queue.terminal_tool_executions,
                        &queue.extra_wait_conditions,
                        queue.delivery.as_ref(),
                        DeliverySynchronization::Required,
                        queue.timer_wake_claim.as_ref(),
                        queue.task_result_settlement.as_ref(),
                        task_transaction.as_mut().and_then(|tasks| tasks.take_result()), current_read_transaction.as_mut())?,
                    InputMutationResult::Transition,
                ),
                InputTransitionCommand::QueueHeadNoProgress {
                    command,
                    prepared_at,
                    audit_event_id,
                } => {
                    let original = repository.commit_queue_head_no_progress_with_current_read_tx(
                        tx,
                        command,
                        *prepared_at,
                        audit_event_id, current_read_transaction.as_mut())?;
                    match original {
                        Some(original) => (
                            original.commit,
                            InputMutationResult::QueueHeadNoProgress {
                                outcome: Some(original.outcome),
                            },
                        ),
                        None => (
                            TransitionCommit::default(),
                            InputMutationResult::QueueHeadNoProgress { outcome: None },
                        ),
                    }
                }
            })
                };
                if input_source.is_some() {
                    if let Some(proof) = input_birth.as_mut() {
                        let (outcome,noops,admitted_current) = proof.with_input_phase(tx,memory_ledger,memory_budget.as_mut(),|material| {
                            material.observe_original_writer_prior(task_tool_increment)?;
                            let writer = crate::runtime_db::input_evidence_snapshot::tool_writer::InputToolWriter::new(tx,material,current_tool_usage);
                            native_birth::reserve_original_tools(prepared,&writer)?;
                            let outcome = execute(Some(&writer))?;
                            let current=writer.admitted_current_material();
                            Ok((outcome,writer.take_original_noop_readbacks(),current))
                        })?;
                        proof.retain_original_tool_noops(tx,noops)?;
                        current_admitted_after_business=admitted_current;
                        outcome
                    } else { execute(None)? }
                } else { execute(None)? }
            };
            // Execution consumed/dropped the standalone Result selection and
            // predicate. Its actual outcome remains in result through receipt.
            if let InputMutationResult::TaskResult {outcome} = &result {
                let tasks=task_transaction.as_mut().context("Result lost its original Task phase")?;
                if has_first_input {
                    input_birth.as_mut().context("Result lost its original Input phase")?
                        .with_input_phase(tx,memory_ledger,memory_budget.as_mut(),|material| {
                            if let Some(released)=tasks.retire_result_execution(scope,memory_ledger,
                                outcome,&value.effects.task_write_keys)? {
                                material.release_consumed_task_result_prior(released)?;
                            }
                            Ok(())
                        })?;
                } else if let Some(released)=tasks.retire_result_execution(scope,memory_ledger,
                    outcome,&value.effects.task_write_keys)? {
                    if let Some(memory)=memory_budget.as_mut() {memory.exchange_original_result_material(released)?;}
                }
            }
            if shared_nomemory {
                if let Some(current)=current_read_transaction.as_ref() {
                    let observed=current.input_writer_material(tx)?;
                    input_birth.as_mut().context("original Current lost shared Input phase")?
                        .with_input_phase(tx,memory_ledger,None,|material|material.observe_current_prior(
                            &observed.after_admitted(current_admitted_after_business)?))?;
                    current_admitted_after_business=observed.validate_accounting(scope,memory_ledger)?;
                }
            }
            // Business SQL and its exact count/State epilogue are complete.
            // Qualify only the actual final Work/Wait/Frame footprint and actual
            // D3 Result writes, once, before the whole-transaction receipt.
            if let Some(budget)=&mut memory_budget { budget.before_finalizer_tx(tx)?; }
            let final_request=if shared_nomemory {
                if let Some(work)=&work_proof {
                    input_birth.as_mut().context("original Work lost shared Input phase")?
                        .with_input_phase(tx,memory_ledger,None,|material|workspace_input::work_request(
                            material,scope,work_request,work.input_writer_material(scope,memory_ledger)?))?
                } else {work_request.clone()}
            } else {memory_budget.as_ref().map(|budget|budget.work_request(work_request))
                .transpose()?.unwrap_or_else(||work_request.clone())};
            let mut final_material=if let Some(proof)=&work_proof {
                crate::runtime_db::work_snapshot::WorkWriteMaterial::for_readback(
                    proof,&final_request,memory_budget.as_mut(),
                )?
            } else {None};
            if let Some(material)=final_material.as_mut() {
                material.reserve_actual_ids(value.effects.work_items.iter().map(|work|work.id.as_str())
                    .chain(value.effects.wait_condition_writes.wait_ids.iter().map(String::as_str))
                    .chain(prepared.explicit_frames().iter().map(|frame|frame.id.as_str())))?;
                material.reserve_result_briefs(&value.effects.briefs)?;
            }
            let actual_work = prepared.actual_work_keys(&value.effects);
            let mut final_work_view = None;
            let mut final_work_input_usage=None;
            if let Some(proof) = &work_proof {
                let actual = &actual_work;
                let result_briefs = value.effects.briefs.iter()
                    .filter(|brief| brief.kind == crate::types::BriefKind::Result)
                    .map(|brief| brief.id.clone()).collect();
                let final_state=if let Some(material)=final_material.as_mut() {
                    material.final_state_tx(tx,proof)?
                } else {
                    crate::runtime_db::context_inputs::ContextDurableRead::new(tx,&scope.agent_id)
                        .scope_facts()?.state.context("Work finalizer requires actual final State")?
                };
                let work=if let Some(material)=final_material.take() {
                    let (facts,view)=material.register_tx(tx,proof,actual,&result_briefs,&final_state,&final_request,finish_memory.is_some())?;
                    final_work_view=view;
                    facts
                } else if finish_memory.is_some() {
                    let (facts,view)=crate::runtime_db::work_snapshot::register_work_writes_with_view_tx(tx,proof,actual,&result_briefs,&final_state,&final_request)?;
                    final_work_view=Some(view);facts
                } else {
                    crate::runtime_db::work_snapshot::register_work_writes_tx(tx,proof,actual,&result_briefs,&final_state,&final_request)?
                };
                final_work_input_usage=work.material_usage;
                if shared_nomemory {
                    let before=proof.input_writer_material(scope,memory_ledger)?;
                    let actual=work.material_usage.context("original Work omitted actual final material")?;
                    input_birth.as_mut().context("original Work lost shared Input phase")?
                        .with_input_phase(tx,memory_ledger,None,|material|material.observe_original_writer_prior(
                            workspace_input::increment(actual,before)?))?;
                }
                value.effects.work_input_preparation = Some(WorkInputPreparation {
                    scope: scope.clone(),
                    work_revision: work.delta.revision,
                    owning_roots: work.owning_input_roots,
                    completion_heads: work.delta.coverage.latest_results.iter()
                        .filter_map(|(id, head)| head.clone().map(|head| (id.clone(), head)))
                        .collect(),
                });
                // Preserve the actual readback material through Known/cache
                // installation while runtime facts strip authoritative deltas.
                // Sharing this Arc neither clones bodies nor registers twice.
                if let Some(guards) = &work.delta._material_views {
                    anyhow::ensure!(
                        value.effects.work_material_budget_views.as_ref()
                            .is_none_or(|old| std::sync::Arc::ptr_eq(old, guards)),
                        "one Work finalizer supplied different readback material"
                    );
                    value.effects.work_material_budget_views = Some(guards.clone());
                }
                value.effects.work_delta = Some(work.delta);
            }
            drop(final_material);
            let mut memory_refs = BTreeSet::new();
            if let Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Started { start }) = &prepared.memory {
                crate::runtime_db::dispatch_memory::install_dispatch_memory_start_tx(tx, start, &prepared.descriptor)?;
                memory_refs.extend(start.input_roots());
            }
            if let Some(token) = detached_memory {
                // A business no-op needs no new deferred responsibility, but its
                // original whole receipt is still committed below.
                if !actual_work.is_empty() {
                    let mut refs = prepared.memory.as_ref().expect("prepared memory").input_roots();
                    if let Some(p) = &value.effects.work_input_preparation {
                        refs.extend(p.owning_roots.iter().cloned());
                    }
                    use crate::runtime::resident_input_evidence::{InputEvidenceFamily as Family, InputEvidenceKey as Key};
                    refs.extend(value.effects.messages.iter().map(|v| Key::new(Family::Message,&v.id)));
                    refs.extend(value.effects.turns.iter().map(|v| Key::new(Family::Turn,&v.turn_id)));
                    refs.extend(value.effects.tool_executions.iter().map(|v| Key::new(Family::ToolExecution,&v.id)));
                    refs.extend(value.effects.briefs.iter().map(|v| Key::new(Family::Brief,&v.id)));
                    refs.extend(value.effects.transcript_entries.iter().map(|v| Key::new(Family::Transcript,&v.id)));
                    if let Some(budget)=&mut memory_budget {
                        budget.detached_member(&actual_work,&refs,&prepared.descriptor)?;
                    }
                    let record = crate::runtime_db::dispatch_memory::record_detached_memory_tx(token,actual_work,refs)?;
                    memory_refs.extend(record.input_roots());
                }
            }
            if let Some(token) = finish_memory {
                if let Some(budget)=&mut memory_budget {
                    budget.capture_final_work(final_work_view.as_ref()
                        .context("memory finish omitted exact final Work view")?)?;
                }
                let state = crate::runtime_db::context_inputs::ContextDurableRead::new(tx,&scope.agent_id)
                    .scope_facts()?.state.context("memory finish requires final business State")?;
                let memory = crate::runtime_db::dispatch_memory::finish_dispatch_memory_tx(token,&state,
                    final_work_view.as_ref().context("memory finish omitted exact final Work view")?,work_request,current_read_transaction.as_mut())?;
                if let Some(final_state) = memory.effects.agent_state {
                    if let Some(outer_state) = &mut value.effects.agent_state {
                        outer_state.record = final_state.record;
                    } else { value.effects.agent_state = Some(final_state); }
                }
                value.effects.context_episodes.extend(memory.effects.context_episodes);
                value.effects.audit_events.extend(memory.effects.audit_events);
                value.effects.notify_memory_index |= memory.effects.notify_memory_index;
                value.effects.notify_scheduler |= memory.effects.notify_scheduler;
                value.applied |= memory.applied;
            }
            if shared_nomemory {
                if let Some(current)=current_read_transaction.as_mut() {
                    input_birth.as_mut().context("original Current lost shared Input phase")?
                        .with_input_phase(tx,memory_ledger,None,|material|current.narrow_original_remaining(tx,material.remaining_material_limits()?))?;
                }
            }
            terminal_state_audit::finish_tx(tx,prepared,&mut value,current_read_transaction.as_mut())?;
            if shared_nomemory {
                if let Some(current)=current_read_transaction.as_ref() {
                    let observed=current.input_writer_material(tx)?;
                    input_birth.as_mut().context("original Current lost shared Input phase")?
                        .with_input_phase(tx,memory_ledger,None,|material|material.observe_current_prior(
                            &observed.after_admitted(current_admitted_after_business)?))?;
                }
            }
            if value.effects.work_delta.is_none() {
                if let Some(delta) = crate::runtime_db::work_snapshot::refresh_work_delivery_sources_tx(
                    tx, scope, &value.effects.input_evidence_delta, work_request, memory_ledger,
                    memory_budget.as_mut(),
                )? {
                    value.effects.work_delta = Some(delta);
                    value.effects.work_delta_requires_prepared_domain = true;
                }
            }
            if task_transaction.is_none() && value.effects.task_delta.is_none()
                && !matches!(prepared.command.as_ref(),InputTransitionCommand::Timer(_)) {
                if has_first_input {
                    let delta=input_birth.as_mut().context("Task metadata lost shared Input phase")?
                        .with_input_phase(tx,memory_ledger,memory_budget.as_mut(),|material|
                            material.with_task_material_limits(scope,memory_ledger,
                                Some((task_request.byte_limit,task_request.record_limit)),|task|
                                    crate::runtime_db::task_snapshot::register_task_source_metadata_using_material_tx(
                                        tx,scope,&value.effects.input_evidence_delta,memory_ledger,task)))?;
                    if let Some(delta)=delta {
                        value.effects.task_delta=Some(delta);
                        value.effects.task_delta_requires_prepared_domain=true;
                    }
                } else if let Some((delta, guards)) = crate::runtime_db::task_snapshot::register_task_source_metadata_tx(
                    tx, scope, &value.effects.input_evidence_delta, task_request, memory_ledger, memory_budget.as_mut(),
                )? {
                    value.effects.task_delta = Some(delta);
                    value.effects.task_delta_requires_prepared_domain = true;
                    value.effects.task_material_budget_views.extend(guards);
                }
            }
            let mut child_usage=None;
            if let Some(tasks) = task_transaction {
                if prepared.original_child_terminal().is_some() {
                    child_usage=Some(tasks.finish_original_child(tx,&mut value,memory_ledger,memory_budget.as_mut())?);
                } else if shared_nomemory {
                    let before=tasks.input_writer_material(scope,memory_ledger)?;
                    input_birth.as_mut().context("original Task lost shared Input phase")?
                        .with_input_phase(tx,memory_ledger,None,|material| {
                            let remaining=material.remaining_material_limits()?;
                            let actual=tasks.finish_using_original_material(tx,&mut value,memory_ledger,None,
                                inserted_stop.as_ref(),Some(remaining))?;
                            #[cfg(test)]
                            if actual.records>16384 {
                                eprintln!("original Task final actual phase: before={before:?} after=({}, {}) input_before={:?} remaining={remaining:?}",
                                    actual.bytes,actual.records,material.usage());
                            }
                            material.observe_original_writer_prior(workspace_input::increment((actual.bytes,actual.records),before)?)
                        })?;
                } else { match inserted_stop.as_ref() {
                    Some(observation) => tasks.finish_with_stop_observation(tx, &mut value, memory_ledger,
                        memory_budget.as_mut(), observation)?,
                    None => tasks.finish(tx,&mut value,memory_ledger,memory_budget.as_mut())?,
                }}
            }
            if let Some(phase)=child_source_phase {
                let deletion=match result {InputMutationResult::Task{deletion,..}=>deletion,_=>anyhow::bail!("child Task ACK placeholder changed")};
                result=child_terminal::finish_related_tx(tx,prepared,phase,&mut value,deletion,
                    child_usage.context("child Task omitted cumulative final material")?,task_request,memory_ledger,memory_budget.as_mut())?;
                // The original Task may be a no-op while this compound phase
                // still writes its source, related heads and mandatory receipt.
                let InputTransitionCommand::Task(task)=prepared.command.as_ref() else{unreachable!()};
                value.effects.fault=task.command.fault;
            }
            if let Some(scheduling)=scheduling_transaction {
                if shared_nomemory {
                    let before=scheduling.input_writer_material(tx,scope,memory_ledger)?;
                    input_birth.as_mut().context("original Scheduling lost shared Input phase")?
                        .with_input_phase(tx,memory_ledger,None,|material| {
                            let request=workspace_input::scheduling_request(material,scheduling_request,before)?;
                            let original_state=facts.state.as_ref().context("original Scheduling original State missing")?;
                            let actual=scheduling.finish_using_original_material(tx,&mut value,&request,original_state,None)?;
                            material.observe_original_writer_prior(workspace_input::increment(actual,before)?)
                        })?;
                } else {
                    let original_state=facts.state.as_ref().context("original Scheduling State missing")?;
                    scheduling.finish_using_original_material(tx,&mut value,scheduling_request,original_state,memory_budget.as_mut())?;
                }
            }
            value.effects.task_material_budget_views.extend(result_receipt_guard);
            if let Some(mut current) = current_read_transaction {
                if has_first_input {
                    let proof=input_birth.as_mut().context("first Input phase lost its actual Native proof")?;
                    proof.with_input_phase(tx,memory_ledger,memory_budget.as_mut(),|material| {
                        let before=current.input_writer_material(tx)?.validate_accounting(scope,memory_ledger)?;
                        #[cfg(test)]
                        if value.effects.task_delta.as_ref().is_some_and(|delta|delta.records.len()>64) {
                            eprintln!("original Current final actual phase: current_before={before:?} input={:?} remaining={:?} task_selected={}",
                                material.usage(),material.remaining_material_limits(),
                                value.effects.task_delta.as_ref().map_or(0,|delta|delta.records.len()));
                        }
                        current.narrow_original_remaining(tx,material.remaining_material_limits()?)?;
                        current_reads::finish(current,&mut value,facts.state.as_ref().context("CurrentRead original State missing")?)?;
                        let actual=value.effects.current_read_facts.as_ref().context("Workspace Current finalizer omitted actual facts")?;
                        let observed=actual.input_writer_material(scope,memory_ledger)?;
                        material.observe_current_prior(&observed.after_admitted(before)?)
                    })?;
                } else {current_reads::finish(current,&mut value,facts.state.as_ref().context("CurrentRead original State missing")?)?;}
            }
            value.effects._original_workspace_material=prepared.original_workspace_request_material().cloned();
            value.effects._original_sleep_override_material=prepared.original_sleep_override_material().cloned();
            value.effects._original_task_input_child_material=prepared.original_task_input_child_material().cloned();


            if let Some(proof) = input_birth {
                // Work's later owning-input preparation has its own live usage;
                // do not label that unsupported compound frontier complete yet.
                if has_first_input && memory_budget.is_none()
                    && value.effects.work_input_preparation.is_none()
                    && value.effects.work_delta.is_none() {
                    let final_state=value.effects.agent_state.as_ref().map(|state|state.record.as_ref())
                        .or(facts.state.as_ref()).context("Input publication final State is unavailable")?;
                    proof.finish_delta_with_publication_tx(tx,&mut value.effects.input_evidence_delta,
                        memory_ledger,memory_budget.as_mut(),final_state)?;
                } else {
                    proof.finish_delta_tx(tx, &mut value.effects.input_evidence_delta)?;
                }
            }

            if let (Some(preparation),Some(work),Some(selection))=(
                value.effects.work_input_preparation.as_ref(),value.effects.work_delta.as_ref(),
                prepared.work_resource_material.as_ref()
                    .map(|loan|loan.original_input_request(scope,memory_ledger)).transpose()?.flatten(),
            ) {
                let (inputs,report,guards)=crate::runtime_db::work_snapshot::prepare_committed_work_inputs_tx(
                    tx,scope,selection,work,&preparation.owning_roots,final_work_input_usage.context("original Work ACK omitted actual material usage")?,
                    work_request,memory_ledger,memory_budget.as_mut(),prepared.work_resource_material.as_ref()
                        .and_then(|loan|loan.original_result_material()).map(|result|(result,prepared.descriptor())))?;
                value.effects.original_work_report=report.map(Arc::new);
                value.effects.prepared_work_inputs=Some(Arc::new(inputs));
                value.effects.task_material_budget_views.extend(guards);
            }

            if prepared.work_resource_material.as_ref().and_then(|loan|loan.original_result_material())
                .is_some_and(|result|result.expects_completed_report()) {
                anyhow::ensure!(value.effects.original_work_report.is_some(),
                    "original Completed Work result requires its same-TX qualified Input report");
            }

            // The original body has already materialized SQL sequences, linked
            // Brief events, persisted index intent, exact count and aliases.
            let mut install_keys =
                super::super::input_recovery_install::InputMutationInstallKeys::from_effects(
                    scope,
                    &value.effects,
                )?;
            install_keys.wait_conditions.extend(value.effects.wait_condition_writes.wait_ids.iter().cloned());
            install_keys.continuations.extend(prepared.explicit_frame_ids());
            if let Some(preparation) = &value.effects.work_input_preparation {
                anyhow::ensure!(preparation.scope == *scope
                    && value.effects.work_delta.as_ref().is_some_and(|delta|
                        delta.scope == preparation.scope && delta.revision == preparation.work_revision),
                    "Work input preparation differs from its committed scope/revision");
                install_keys.input_roots.extend(preparation.owning_roots.iter().cloned());
                // Head selection is not an ordinary mandatory-body lookup.
                // Fresh recovery follows the current Work head and proves its
                // optional edges separately from independent owning roots.
                install_keys.completion_brief_ids.extend(preparation.completion_heads.values()
                    .map(|head| head.brief_id.clone()));
            }
            install_keys.input_roots.extend(memory_refs);
            install_keys.validate()?;
            let outcome = InputMutationOutcome {
                result,
                applied: value.applied,
                delivery_receipt: value.delivery_receipt.clone(),
                scopes: prepared
                    .descriptor
                    .scopes()
                    .iter()
                    .map(|reserved_scope| {
                        let primary = reserved_scope == scope;
                        InputMutationScopeOutcome {
                            install_keys: if primary {
                                install_keys.clone()
                            } else if prepared.original_child_terminal().is_some() {
                                child_terminal::related_install_keys(prepared,reserved_scope)
                            } else {
                                Default::default()
                            },
                            scope: reserved_scope.clone(),
                            audit_events: if primary {
                                value.effects.audit_events.clone()
                            } else {
                                Vec::new()
                            },
                            notify_memory_index: primary && value.effects.notify_memory_index,
                            notify_scheduler: primary && value.effects.notify_scheduler,
                        }
                    })
                    .collect(),
            };
            if let Some(base) = &prepared.auto_base_descriptor {
                crate::runtime_db::dispatch_memory::insert_original_auto_memory_tx(tx, base,
                    &prepared.descriptor, prepared.memory.as_ref().context("automatic memory omitted intent")?)?;
            }
            let receipt =
                insert_input_mutation_receipt_tx(tx, &prepared.descriptor, &outcome, Utc::now())?;
            // A successful no-op still wrote its receipt in this transaction.
            if prepared.original_child_terminal().is_some() {
                let InputTransitionCommand::Task(task)=prepared.command.as_ref() else{unreachable!()};
                inject_fault(task.command.fault,TransitionFaultPoint::BeforeCommit)?;
            }
            if let InputTransitionCommand::QueueHeadNoProgress { command, .. } =
                prepared.command.as_ref()
            {
                inject_fault(command.fault, TransitionFaultPoint::BeforeCommit)?;
            }
            if let InputTransitionCommand::Terminal(command) = prepared.command.as_ref() {
                if prepared.ended_memory.is_some() {
                    inject_fault(command.fault, TransitionFaultPoint::BeforeCommit)?;
                }
            }
            if let InputTransitionCommand::Timer(timer)=prepared.command.as_ref() {
                inject_fault(timer.fault(),TransitionFaultPoint::BeforeCommit)?;
            }
            Ok(InputMutationExecution::Committed { value, receipt })
        })
    }
}

#[cfg(test)]
#[path = "prepared_input_tests.rs"]
mod tests;

impl PreparedInputTransition {
    fn cleanup_proof_tx(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        work_proof: Option<&crate::runtime_db::work_snapshot::WorkWriteProof>,
        task_cleanup: Option<
            &crate::runtime_db::task_snapshot::OriginalTaskResultCleanupProof<'_, '_, '_>,
        >,
        producer_input: Option<
            &crate::runtime_db::command_task_terminal::OriginalCommandTaskInputView<'_, '_, '_>,
        >,
    ) -> Result<crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof> {
        if matches!(
            self.command.as_ref(),
            InputTransitionCommand::Timer(_) | InputTransitionCommand::RuntimeAudit(_)
        ) {
            anyhow::bail!("ordinary Timer/Audit command requires Active identity");
        }
        if let Some(proof) =
            self.work_cleanup_proof_tx(tx, scope, work_proof, task_cleanup, producer_input)?
        {
            return Ok(proof);
        }
        if matches!(self.command.as_ref(), InputTransitionCommand::TaskResult(_)) {
            return crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_tx(
                tx, scope, BTreeSet::new(), BTreeSet::new());
        }
        use crate::domain::execution_protocol::{
            ExecutionBinding, ExecutionProtocolCommand as Command,
        };
        use crate::runtime::resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key,
        };
        let mut roots = BTreeSet::new();
        let mut derived = BTreeSet::new();
        let mut source_ids = BTreeSet::new();
        let mut turn_ids = BTreeSet::new();
        let mut interrupted_return = false;
        let validate_state = |mutation: &AgentStateMutation| -> Result<()> {
            anyhow::ensure!(
                mutation.expected.is_some(),
                "cleanup State requires its original OCC baseline"
            );
            validate_agent_state_mutation_tx(tx, Some(mutation))
        };
        let (transcript, briefs, tools) = match self.command.as_ref() {
            InputTransitionCommand::RuntimeAudit(_)
            | InputTransitionCommand::Work { .. }
            | InputTransitionCommand::Focus { .. }
            | InputTransitionCommand::Wait { .. }
            | InputTransitionCommand::Task(_)
            | InputTransitionCommand::TaskResult(_)
            | InputTransitionCommand::TaskStatusMetadata(_)
            | InputTransitionCommand::TaskStopProgress(_)
            | InputTransitionCommand::TaskStopContinuation(_)
            | InputTransitionCommand::Timer(_)
            | InputTransitionCommand::ExternalTrigger(_) => {
                unreachable!("handled by original Work cleanup proof")
            }
            InputTransitionCommand::QueueHeadNoProgress { .. } => {
                bail!("Deleting identity cannot begin a queue-head progress attempt");
            }
            InputTransitionCommand::Posture {
                command,
                started_turn,
            } => {
                anyhow::ensure!(
                    started_turn.is_none(),
                    "Deleting identity cannot start a Turn"
                );
                validate_state(&command.agent_state)?;
                let expected = command
                    .agent_state
                    .expected
                    .as_deref()
                    .context("cleanup State baseline missing")?;
                let next = command.agent_state.record.as_ref();
                anyhow::ensure!(
                    next.turn_index == expected.turn_index
                        && (next.current_run_id.is_none()
                            || next.current_run_id == expected.current_run_id)
                        && (next.current_turn_id.is_none()
                            || next.current_turn_id == expected.current_turn_id)
                        && !matches!(next.status, crate::types::AgentStatus::AwakeRunning)
                        && next.pending <= expected.pending,
                    "Deleting posture cannot create a run, Turn or pending admission"
                );
                return crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_tx(tx, scope, roots, derived);
            }
            InputTransitionCommand::Terminal(command) => {
                validate_state(&command.agent_state)?;
                extend_cleanup_turn_tx(
                    tx,
                    scope,
                    &command.turn_record,
                    &mut roots,
                    &mut source_ids,
                    &mut turn_ids,
                )?;
                anyhow::ensure!(
                    !source_ids.is_empty(),
                    "cleanup terminal Turn has no canonical admission source"
                );
                (
                    &[][..],
                    &[][..],
                    command.terminal_tool_executions.as_slice(),
                )
            }
            InputTransitionCommand::Queue(queue) => {
                anyhow::ensure!(
                    matches!(
                        queue.command.operation,
                        QueueOperation::Settle
                            | QueueOperation::Discard
                            | QueueOperation::RepairDrop
                    ),
                    "Deleting identity cannot admit, claim, interject or requeue work"
                );
                anyhow::ensure!(
                    queue.delivery.is_none()
                        && queue.timer_wake_claim.is_none()
                        && queue.command.scheduler_claim_work_item.is_none()
                        && queue
                            .wait_registration
                            .as_ref()
                            .is_none_or(|wait| wait.task_result_admission.is_none()),
                    "cleanup cannot admit a new delivery, claim or task result"
                );
                if let Some(state) = queue.command.agent_state.as_ref() {
                    validate_state(state)?;
                }
                let record = queue_mutation_record(&queue.command.mutation);
                anyhow::ensure!(
                    record.status != QueueEntryStatus::Queued,
                    "cleanup cannot requeue an admission"
                );
                let old: QueueEntryRecord = committed_input_payload_tx(
                    tx,
                    "queue_entries",
                    "message_id",
                    &record.message_id,
                    &scope.agent_id,
                )?;
                anyhow::ensure!(
                    old.agent_id == scope.agent_id,
                    "cleanup queue admission belongs to another agent"
                );
                interrupted_return = record.status == QueueEntryStatus::Interrupted
                    && old.status == QueueEntryStatus::Dequeued;
                source_ids.insert(record.message_id.clone());
                if let Some(turn) = queue.command.turn_record.as_ref() {
                    extend_cleanup_turn_tx(
                        tx,
                        scope,
                        turn,
                        &mut roots,
                        &mut source_ids,
                        &mut turn_ids,
                    )?;
                }
                anyhow::ensure!(
                    queue.command.turn_record.is_none()
                        || source_ids.contains(&record.message_id)
                            && queue.command.turn_record.as_ref().is_some_and(|turn| {
                                turn.input_message_ids.contains(&record.message_id)
                                    || turn.trigger.as_ref().is_some_and(|trigger| {
                                        trigger.message_id.as_deref()
                                            == Some(record.message_id.as_str())
                                    })
                            }),
                    "cleanup terminal Turn differs from its queue admission"
                );
                for message in &queue.command.message_evidence {
                    anyhow::ensure!(
                        message.id == record.message_id && message.agent_id == scope.agent_id,
                        "cleanup cannot append a new Message"
                    );
                }
                for command in &queue.execution_protocol.commands {
                    let attempt_and_outcome = match command {
                        Command::Settle(settle) => {
                            Some((&settle.outcome.attempt_id, &settle.outcome.outcome_id))
                        }
                        Command::Interrupt(interrupt) => {
                            Some((&interrupt.attempt_id, &interrupt.outcome_id))
                        }
                        Command::RegisterWorkItem(_)
                        | Command::Admit(_)
                        | Command::AdvanceWorkItemSourceRevision(_)
                        | Command::RecoverInterruptedTaskResultClaim(_)
                        | Command::RecoverUnadvancedTaskResultClaim(_) => {
                            anyhow::bail!(
                                "cleanup cannot create or recover an execution admission"
                            );
                        }
                        Command::SetWorkItemReadiness(command) => {
                            roots.insert(Key::new(Family::WorkExecution, &command.work_item_id));
                            None
                        }
                        Command::SuspendWorkItemContinuation(command) => {
                            roots.insert(Key::new(Family::WorkExecution, &command.work_item_id));
                            None
                        }
                        Command::ResumeWorkItemContinuation(command) => {
                            roots.insert(Key::new(Family::WorkExecution, &command.work_item_id));
                            None
                        }
                        Command::ReconcileWorkItemContinuationYield(command) => {
                            roots.insert(Key::new(Family::WorkExecution, &command.work_item_id));
                            None
                        }
                        Command::SetWorkItemWaiting(command) => {
                            roots.insert(Key::new(Family::WorkExecution, &command.work_item_id));
                            None
                        }
                        Command::CompleteWorkItem(command) => {
                            roots.insert(Key::new(Family::WorkExecution, &command.work_item_id));
                            None
                        }
                    };
                    if let Some((attempt_id, outcome_id)) = attempt_and_outcome {
                        let attempt: crate::domain::execution_protocol::ExecutionAttempt =
                            committed_input_payload_tx(
                                tx,
                                "execution_protocol_attempts",
                                "attempt_id",
                                attempt_id,
                                &scope.agent_id,
                            )?;
                        let source = attempt.source_message_id.as_deref().or_else(|| {
                            use crate::domain::execution_protocol::ExecutionSourceIdentity as Source;
                            match &attempt.source.identity {
                                Source::QueueMessage { message_id } | Source::InternalFollowup { message_id } => Some(message_id.as_str()),
                                Source::TaskResult { result_message_id, .. } | Source::ChildResult { result_message_id, .. } => Some(result_message_id.as_str()),
                                Source::TriggeredWait { trigger_message_id, .. } => Some(trigger_message_id.as_str()),
                                _ => None,
                            }
                        });
                        anyhow::ensure!(
                            source == Some(record.message_id.as_str()),
                            "cleanup Attempt differs from its original queue admission"
                        );
                        roots.insert(Key::new(Family::Attempt, attempt_id));
                        if let ExecutionBinding::WorkItem { work_item_id } = &attempt.binding {
                            roots.insert(Key::new(Family::WorkExecution, work_item_id));
                        }
                        derived.insert(Key::new(Family::Outcome, outcome_id));
                    }
                }
                if let Some(completion) = queue.completion.as_ref() {
                    anyhow::ensure!(
                        completion
                            .tool_execution
                            .turn_id
                            .as_ref()
                            .is_some_and(|id| turn_ids.contains(id)),
                        "cleanup completion Tool differs from its terminal Turn"
                    );
                    derived.insert(Key::new(
                        Family::ToolExecution,
                        &completion.tool_execution.id,
                    ));
                }
                (
                    queue.command.transcript_entries.as_slice(),
                    queue.command.brief_evidence.as_slice(),
                    queue.terminal_tool_executions.as_slice(),
                )
            }
        };
        let mutation = match self.command.as_ref() {
            InputTransitionCommand::Terminal(command) => Some(&command.agent_state),
            InputTransitionCommand::Queue(queue) => queue.command.agent_state.as_ref(),
            InputTransitionCommand::QueueHeadNoProgress { .. } => {
                anyhow::bail!("new queue progress cannot use an existing admission cleanup proof")
            }
            InputTransitionCommand::RuntimeAudit(_)
            | InputTransitionCommand::Posture { .. }
            | InputTransitionCommand::Work { .. }
            | InputTransitionCommand::Focus { .. }
            | InputTransitionCommand::Wait { .. }
            | InputTransitionCommand::Task(_)
            | InputTransitionCommand::TaskResult(_)
            | InputTransitionCommand::TaskStatusMetadata(_)
            | InputTransitionCommand::TaskStopProgress(_)
            | InputTransitionCommand::TaskStopContinuation(_)
            | InputTransitionCommand::Timer(_)
            | InputTransitionCommand::ExternalTrigger(_) => unreachable!(),
        };
        if let Some(mutation) = mutation {
            let old = mutation
                .expected
                .as_deref()
                .context("cleanup State baseline missing")?;
            let next = mutation.record.as_ref();
            anyhow::ensure!(
                next.turn_index == old.turn_index
                    && (next.current_run_id.is_none() || next.current_run_id == old.current_run_id)
                    && (next.current_turn_id.is_none()
                        || next.current_turn_id == old.current_turn_id
                        || next
                            .current_turn_id
                            .as_ref()
                            .is_some_and(|id| turn_ids.contains(id))),
                "cleanup cannot create a run or a new Turn"
            );
            anyhow::ensure!(
                next.pending <= old.pending.saturating_add(usize::from(interrupted_return)),
                "cleanup cannot add an unrelated pending admission"
            );
        }
        // Root IDs come from the already stored Turn/queue, never a Message's
        // preallocated turn hint or caller-provided arbitrary source refs.
        roots.extend(source_ids.iter().map(|id| Key::new(Family::Message, id)));
        for entry in transcript {
            anyhow::ensure!(
                entry.agent_id == scope.agent_id
                    && entry
                        .related_message_id
                        .as_ref()
                        .is_some_and(|id| source_ids.contains(id)),
                "cleanup Transcript is unrelated to its original admission"
            );
            derived.insert(Key::new(Family::Transcript, &entry.id));
        }
        for brief in briefs {
            anyhow::ensure!(
                brief.agent_id == scope.agent_id
                    && (brief
                        .related_message_id
                        .as_ref()
                        .is_some_and(|id| source_ids.contains(id))
                        || brief
                            .turn_id
                            .as_ref()
                            .is_some_and(|id| turn_ids.contains(id))),
                "cleanup Brief is unrelated to its original admission"
            );
            derived.insert(Key::new(Family::Brief, &brief.id));
        }
        for tool in tools {
            anyhow::ensure!(
                tool.agent_id == scope.agent_id
                    && tool
                        .turn_id
                        .as_ref()
                        .is_some_and(|id| turn_ids.contains(id)),
                "cleanup Tool is unrelated to its original terminal Turn"
            );
            derived.insert(Key::new(Family::ToolExecution, &tool.id));
        }
        let terminal = match self.command.as_ref() {
            InputTransitionCommand::Terminal(command) => Some(&command.turn_record),
            InputTransitionCommand::Queue(queue) => queue.command.turn_record.as_ref(),
            InputTransitionCommand::QueueHeadNoProgress { .. } => {
                anyhow::bail!("new queue progress cannot use an existing admission cleanup proof")
            }
            InputTransitionCommand::RuntimeAudit(_)
            | InputTransitionCommand::Posture { .. }
            | InputTransitionCommand::Work { .. }
            | InputTransitionCommand::Focus { .. }
            | InputTransitionCommand::Wait { .. }
            | InputTransitionCommand::Task(_)
            | InputTransitionCommand::TaskResult(_)
            | InputTransitionCommand::TaskStatusMetadata(_)
            | InputTransitionCommand::TaskStopProgress(_)
            | InputTransitionCommand::TaskStopContinuation(_)
            | InputTransitionCommand::Timer(_)
            | InputTransitionCommand::ExternalTrigger(_) => None,
        };
        if let Some(turn) = terminal {
            for id in &turn.tool_execution_ids {
                let key = Key::new(Family::ToolExecution, id);
                if !derived.contains(&key) {
                    roots.insert(key);
                }
            }
            for id in &turn.produced_brief_ids {
                let key = Key::new(Family::Brief, id);
                if !derived.contains(&key) {
                    roots.insert(key);
                }
            }
        }
        crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_tx(
            tx, scope, roots, derived,
        )
    }
}
fn extend_cleanup_turn_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    turn: &TurnRecord,
    roots: &mut BTreeSet<crate::runtime::resident_input_evidence::InputEvidenceKey>,
    source_ids: &mut BTreeSet<String>,
    turn_ids: &mut BTreeSet<String>,
) -> Result<()> {
    use crate::runtime::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey as Key,
    };
    anyhow::ensure!(
        turn.terminal.is_some(),
        "cleanup cannot start or extend an unfinished Turn"
    );
    let old: TurnRecord = committed_input_payload_tx(
        tx,
        "turn_records",
        "turn_id",
        &turn.turn_id,
        &scope.agent_id,
    )?;
    for message_id in &turn.input_message_ids {
        if !old.input_message_ids.contains(message_id) {
            let assigned: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM conversation_input_assignments WHERE agent_id=?1 AND message_id=?2 AND turn_id=?3)",
                params![scope.agent_id, message_id, old.turn_id], |row| row.get(0))?;
            anyhow::ensure!(
                assigned,
                "cleanup cannot introduce a new Turn input assignment"
            );
            source_ids.insert(message_id.clone());
        }
    }
    roots.insert(Key::new(Family::Turn, &old.turn_id));
    turn_ids.insert(old.turn_id.clone());
    source_ids.extend(old.input_message_ids.iter().cloned());
    source_ids.extend(
        old.trigger
            .as_ref()
            .and_then(|trigger| trigger.message_id.clone()),
    );
    let mut statement = tx.prepare("SELECT evidence_id FROM tool_executions WHERE agent_id=?1 AND turn_id=?2 AND json_extract(payload_json,'$.status')='deferred' ORDER BY evidence_id LIMIT 65537")?;
    let ids = statement
        .query_map(params![scope.agent_id, old.turn_id], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(
        ids.len() <= 65_536,
        "cleanup deferred tools exceed fixed preparation limit"
    );
    roots.extend(
        ids.into_iter()
            .map(|id| Key::new(Family::ToolExecution, id)),
    );
    Ok(())
}

impl PreparedInputTransition {
    /// Observe every explicit Work/wait/frame target and implicit Wait owner
    /// cancellation before the original reducer writes any canonical row.
    fn prepare_work_proof_tx(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        cleanup: bool,
        request: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        mut memory_budget: Option<
            &mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
        >,
        scheduling: Option<&timer_writes::SchedulingTransaction<'_, '_>>,
    ) -> Result<Option<crate::runtime_db::work_snapshot::WorkWriteProof>> {
        use crate::runtime::resident_work::WorkFamily;
        if !matches!(
            self.command.as_ref(),
            InputTransitionCommand::Task(_)
                | InputTransitionCommand::Timer(_)
                | InputTransitionCommand::Work { .. }
                | InputTransitionCommand::Focus { .. }
                | InputTransitionCommand::Wait { .. }
                | InputTransitionCommand::Queue(_)
        ) && !(matches!(self.command.as_ref(), InputTransitionCommand::Terminal(_))
            && matches!(
                self.memory,
                Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Finish { .. })
            ))
        {
            return Ok(None);
        }
        let mut material =
            crate::runtime_db::work_snapshot::WorkWriteMaterial::with_resource_material(
                scope,
                request,
                ledger,
                memory_budget.as_deref_mut(),
                self.work_resource_material.as_ref(),
            )?;
        let mut keys = BTreeSet::new();
        let mut waits = Vec::new();
        let mut predicted_wait = None;
        match self.command.as_ref() {
            InputTransitionCommand::ExternalTrigger(_) => return Ok(None),
            InputTransitionCommand::Timer(timer) => {
                if scheduling.is_some_and(|plan| plan.fires_wake()) {
                    let (message, queue) = timer
                        .new_wake()
                        .context("Timer plan has another pending branch")?;
                    predicted_wait = predicted_trigger_wait_with_material_tx(
                        tx,
                        scope,
                        message,
                        queue,
                        &mut material,
                    )?;
                } else if self.memory.is_none() {
                    return Ok(None);
                }
            }
            InputTransitionCommand::Task(task) => {
                material.wait_slots(task.command.work_items.len())?;
                keys.extend(
                    task.command
                        .work_items
                        .iter()
                        .map(|work| {
                            material.admit_work(work.record())?;
                            material.key(WorkFamily::Work, &work.record().id)
                        })
                        .collect::<Result<Vec<_>>>()?,
                );
                material.wait_slots(task.command.wait_conditions.len())?;
                for wait in &task.command.wait_conditions {
                    material.admit_wait(wait)?;
                }
                waits.extend(task.command.wait_conditions.iter());
                if let Some(queue) = &task.command.queue_entry {
                    let [message] = task.command.message_evidence.as_slice() else {
                        anyhow::bail!("Task trigger requires exactly one frozen Message");
                    };
                    let wait = predicted_trigger_wait_with_material_tx(
                        tx,
                        scope,
                        message,
                        queue,
                        &mut material,
                    )?;
                    predicted_wait = wait;
                }
            }
            InputTransitionCommand::Work { command, .. } => {
                material.admit_work(command.mutation.record())?;
                keys.insert(material.key(WorkFamily::Work, &command.mutation.record().id)?);
            }
            InputTransitionCommand::Focus { command, .. } => {
                material.wait_slots(command.work_items.len())?;
                keys.extend(
                    command
                        .work_items
                        .iter()
                        .map(|work| {
                            material.admit_work(work.record())?;
                            material.key(WorkFamily::Work, &work.record().id)
                        })
                        .collect::<Result<Vec<_>>>()?,
                );
                material.wait_slots(command.continuations.len())?;
                keys.extend(
                    command
                        .continuations
                        .iter()
                        .map(|frame| {
                            material.admit_frame(frame)?;
                            material.key(WorkFamily::Frame, &frame.id)
                        })
                        .collect::<Result<Vec<_>>>()?,
                );
                // Focus validates the whole active topology. Observe its old
                // qualified members before the reducer replaces any frame.
                if command.active_continuation_expectations.is_some() {
                    material.active_frame_keys_tx(tx, &mut keys)?;
                }
                material.wait_slots(command.wait_conditions.len())?;
                for wait in &command.wait_conditions {
                    material.admit_wait(wait)?;
                }
                waits.extend(command.wait_conditions.iter());
            }
            InputTransitionCommand::Wait { command, .. } => {
                material.wait_slots(command.work_items.len())?;
                keys.extend(
                    command
                        .work_items
                        .iter()
                        .map(|work| {
                            material.admit_work(work.record())?;
                            material.key(WorkFamily::Work, &work.record().id)
                        })
                        .collect::<Result<Vec<_>>>()?,
                );
                material.wait_slots(command.wait_conditions.len())?;
                for wait in &command.wait_conditions {
                    material.admit_wait(wait)?;
                }
                waits.extend(command.wait_conditions.iter());
            }
            InputTransitionCommand::Queue(queue) => {
                if let Some(wait) = &queue.wait_transition {
                    material.wait_slots(1)?;
                    material.admit_wait(&wait.record)?;
                    waits.push(&wait.record);
                    keys.extend(
                        wait.work_item
                            .iter()
                            .map(|work| {
                                material.admit_work(work.record())?;
                                material.key(WorkFamily::Work, &work.record().id)
                            })
                            .collect::<Result<Vec<_>>>()?,
                    );
                }
                if let Some(wait) = &queue.wait_registration {
                    material.wait_slots(wait.work_items.len())?;
                    keys.extend(
                        wait.work_items
                            .iter()
                            .map(|work| {
                                material.admit_work(work.record())?;
                                material.key(WorkFamily::Work, &work.record().id)
                            })
                            .collect::<Result<Vec<_>>>()?,
                    );
                    material.wait_slots(wait.wait_conditions.len())?;
                    for wait in &wait.wait_conditions {
                        material.admit_wait(wait)?;
                    }
                    waits.extend(wait.wait_conditions.iter());
                }
                if let Some(completion) = &queue.completion {
                    material.wait_slots(completion.work_items.len())?;
                    keys.extend(
                        completion
                            .work_items
                            .iter()
                            .map(|work| {
                                material.admit_work(work.record())?;
                                material.key(WorkFamily::Work, &work.record().id)
                            })
                            .collect::<Result<Vec<_>>>()?,
                    );
                    material.wait_slots(completion.continuations.len())?;
                    keys.extend(
                        completion
                            .continuations
                            .iter()
                            .map(|frame| {
                                material.admit_frame(frame)?;
                                material.key(WorkFamily::Frame, &frame.id)
                            })
                            .collect::<Result<Vec<_>>>()?,
                    );
                    material.wait_slots(completion.wait_conditions.len())?;
                    for wait in &completion.wait_conditions {
                        material.admit_wait(wait)?;
                    }
                    waits.extend(completion.wait_conditions.iter());
                }
                material.wait_slots(queue.extra_wait_conditions.len())?;
                for wait in &queue.extra_wait_conditions {
                    material.admit_wait(wait)?;
                }
                waits.extend(queue.extra_wait_conditions.iter());
                if queue.command.operation == QueueOperation::Admit
                    && !queue.command.message_evidence.is_empty()
                {
                    let [message] = queue.command.message_evidence.as_slice() else {
                        anyhow::bail!(
                            "TriggerWaitAndEnqueue requires exactly one admitted message"
                        );
                    };
                    let wait = predicted_trigger_wait_with_material_tx(
                        tx,
                        scope,
                        message,
                        queue_mutation_record(&queue.command.mutation),
                        &mut material,
                    )?;
                    predicted_wait = wait;
                }
            }
            InputTransitionCommand::Terminal(_)
                if matches!(
                    self.memory,
                    Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Finish { .. })
                ) => {}
            _ => return Ok(None),
        }
        if let Some(wait) = &predicted_wait {
            material.admit_wait(wait)?;
        }
        material.wait_keys_tx(
            tx,
            waits.iter().copied().chain(predicted_wait.as_ref()),
            &mut keys,
        )?;
        let proof = material.prepare_tx(tx, scope, &keys, cleanup)?;
        // Wait material and Memory share one allowance before Work preflight.
        let proof_request = memory_budget
            .as_ref()
            .map(|budget| budget.work_request(request))
            .transpose()?
            .unwrap_or_else(|| request.clone());
        crate::runtime_db::work_snapshot::preflight_work_write_snapshot_with_shared_phase_tx(
            tx,
            &proof,
            &proof_request,
            memory_budget.as_deref_mut(),
        )?;
        Ok(Some(proof))
    }

    fn explicit_frames(&self) -> &[crate::types::WorkItemContinuationFrame] {
        match self.command.as_ref() {
            InputTransitionCommand::Focus { command, .. } => &command.continuations,
            InputTransitionCommand::Queue(queue) => queue
                .completion
                .as_ref()
                .map(|completion| completion.continuations.as_slice())
                .unwrap_or_default(),
            _ => &[],
        }
    }
    fn explicit_frame_ids(&self) -> BTreeSet<String> {
        self.explicit_frames()
            .iter()
            .map(|frame| frame.id.clone())
            .collect()
    }

    fn actual_work_keys(
        &self,
        effects: &PostCommitEffects,
    ) -> BTreeSet<crate::runtime::resident_work::WorkKey> {
        use crate::runtime::resident_work::{WorkFamily, WorkKey};
        let mut keys: BTreeSet<_> = effects
            .work_items
            .iter()
            .map(|work| WorkKey::new(WorkFamily::Work, &work.id))
            .collect();
        keys.extend(
            effects
                .wait_condition_writes
                .wait_ids
                .iter()
                .map(|id| WorkKey::new(WorkFamily::Wait, id)),
        );
        // Equal frame upserts are finite actual targets too; finalizer compares
        // the pre-SQL body and will not manufacture a revision for equality.
        keys.extend(
            self.explicit_frame_ids()
                .into_iter()
                .map(|id| WorkKey::new(WorkFamily::Frame, id)),
        );
        keys
    }

    fn work_cleanup_proof_tx(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        work_proof: Option<&crate::runtime_db::work_snapshot::WorkWriteProof>,
        task_cleanup: Option<
            &crate::runtime_db::task_snapshot::OriginalTaskResultCleanupProof<'_, '_, '_>,
        >,
        producer_input: Option<
            &crate::runtime_db::command_task_terminal::OriginalCommandTaskInputView<'_, '_, '_>,
        >,
    ) -> Result<Option<crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof>>
    {
        use crate::domain::execution_protocol::ExecutionProtocolCommand as Command;
        use crate::runtime::resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key,
        };
        if self.original_child_monitor_binding().is_some() {
            return child_binding::existing_input_projection_tx(tx, self, scope).map(Some);
        }
        let (works, waits, state, briefs, protocol, completion_tool) = match self.command.as_ref() {
            InputTransitionCommand::Task(task) => {
                let command = &task.command;
                let payload: String = tx.query_row(
                    "SELECT payload_json FROM tasks WHERE task_id=?1 AND owner_agent_id=?2",
                    params![command.task.id, scope.agent_id],
                    |row| row.get(0),
                )?;
                let old: TaskRecord = serde_json::from_str(&payload)?;
                anyhow::ensure!(
                    old.agent_id == scope.agent_id
                        && repositories::is_terminal_task_status(&command.task.status),
                    "Deleting Task cleanup cannot create an active Task"
                );
                if let Some(cap) = task_cleanup {
                    let (cap_scope, descriptor, message, result) =
                        cap.original_task_input_tx(tx)?;
                    anyhow::ensure!(
                        cap_scope == scope
                            && descriptor == self.descriptor()
                            && command.task_result_settlement.as_ref() == Some(result)
                            && command.message_evidence.as_slice() == std::slice::from_ref(message),
                        "Task cleanup Message/Result differs from original command"
                    );
                    let body_exists: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM messages WHERE evidence_id=?1)",
                        [&message.id],
                        |row| row.get(0),
                    )?;
                    if let Some(queue) = &command.queue_entry {
                        anyhow::ensure!(
                            queue.message_id == message.id
                                && queue.agent_id == scope.agent_id
                                && queue.priority == message.priority
                                && queue.status == QueueEntryStatus::Queued,
                            "Task cleanup queue differs from its original Message"
                        );
                        let previous: Option<String> = tx
                            .query_row(
                                "SELECT status FROM queue_entries WHERE message_id=?1",
                                [&message.id],
                                |row| row.get(0),
                            )
                            .optional()?;
                        anyhow::ensure!(
                            previous
                                .as_deref()
                                .is_none_or(|status| matches!(status, "queued" | "interrupted")),
                            "Task cleanup cannot requeue a claimed or terminal input"
                        );
                    } else {
                        anyhow::ensure!(
                            body_exists,
                            "first Task Result Message requires its original queue responsibility"
                        );
                    }
                } else if let Some(view) = producer_input {
                    view.validate_tx(tx)?;
                    anyhow::ensure!(
                        view.scope() == scope
                            && command.task_result_settlement.is_none()
                            && command.message_evidence.as_slice()
                                == std::slice::from_ref(view.message())
                            && command.task == *view.task(),
                        "Command cleanup differs from original accepted producer"
                    );
                    let message = view.message();
                    if let Some(queue) = &command.queue_entry {
                        anyhow::ensure!(
                            queue.message_id == message.id
                                && queue.agent_id == scope.agent_id
                                && queue.priority == message.priority
                                && queue.status == QueueEntryStatus::Queued,
                            "Command cleanup Queue differs from original Message"
                        );
                        let old: Option<String> = tx
                            .query_row(
                                "SELECT status FROM queue_entries WHERE message_id=?1",
                                [&message.id],
                                |r| r.get(0),
                            )
                            .optional()?;
                        anyhow::ensure!(
                            old.as_deref()
                                .is_none_or(|status| matches!(status, "queued" | "interrupted")),
                            "Command cleanup cannot requeue claimed or terminal source"
                        );
                    } else {
                        let exists: bool = tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM messages WHERE evidence_id=?1)",
                            [&message.id],
                            |r| r.get(0),
                        )?;
                        anyhow::ensure!(
                            exists,
                            "first Command terminal Message requires original queue responsibility"
                        );
                    }
                } else {
                    anyhow::ensure!(
                        command.queue_entry.is_none() && command.task_result_settlement.is_none(),
                        "Deleting Task cleanup requires its original Result capability"
                    );
                }
                (
                    command.work_items.as_slice(),
                    command.wait_conditions.as_slice(),
                    command.agent_state.as_ref(),
                    &[][..],
                    &task.execution_protocol,
                    None,
                )
            }
            InputTransitionCommand::Work {
                command,
                execution_protocol,
                ..
            } => (
                std::slice::from_ref(&command.mutation),
                &[][..],
                command.agent_state.as_ref(),
                command.brief_evidence.as_slice(),
                execution_protocol,
                None,
            ),
            InputTransitionCommand::Focus {
                command,
                execution_protocol,
                completion_tool_execution,
                ..
            } => (
                command.work_items.as_slice(),
                command.wait_conditions.as_slice(),
                Some(&command.agent_state),
                command.brief_evidence.as_slice(),
                execution_protocol,
                completion_tool_execution.as_ref(),
            ),
            InputTransitionCommand::Wait {
                command,
                execution_protocol,
                ..
            } => {
                anyhow::ensure!(
                    command.task_result_admission.is_none() && command.timer_wake.is_none(),
                    "Deleting Wait cleanup cannot admit a task result or claim a timer wake"
                );
                (
                    command.work_items.as_slice(),
                    command.wait_conditions.as_slice(),
                    command.agent_state.as_ref(),
                    &[][..],
                    execution_protocol,
                    None,
                )
            }
            _ => return Ok(None),
        };
        let proof = work_proof.context("Work cleanup omitted its pre-SQL qualification proof")?;
        let mut original_work_ids = BTreeSet::new();
        for mutation in works {
            let WorkItemMutation::Update {
                record,
                expected_revision,
            } = mutation
            else {
                anyhow::bail!("Deleting cleanup cannot insert new Work");
            };
            let old: WorkItemRecord = committed_input_payload_tx(
                tx,
                "work_items",
                "work_item_id",
                &record.id,
                &scope.agent_id,
            )?;
            anyhow::ensure!(
                old.revision == *expected_revision
                    && (old.state == WorkItemState::Open || record.state != WorkItemState::Open),
                "Deleting cleanup cannot revive completed Work"
            );
            original_work_ids.insert(record.id.clone());
        }
        for wait in waits {
            anyhow::ensure!(
                matches!(
                    wait.status,
                    WaitConditionStatus::Resolved | WaitConditionStatus::Cancelled
                ),
                "Deleting cleanup cannot create or trigger a Wait"
            );
            let old: WaitConditionRecord = committed_input_payload_tx(
                tx,
                "wait_conditions",
                "wait_condition_id",
                &wait.id,
                &scope.agent_id,
            )?;
            anyhow::ensure!(
                old.work_item_id == wait.work_item_id && old.kind == wait.kind,
                "Deleting cleanup cannot rebind an existing Wait"
            );
        }
        if let InputTransitionCommand::Task(task) = self.command.as_ref() {
            if (task_cleanup.is_some() || producer_input.is_some())
                && task.command.queue_entry.is_some()
                && state.is_none()
            {
                let queue = task.command.queue_entry.as_ref().unwrap();
                let existing:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id=?1 AND message_id=?2 AND status IN ('queued','interrupted'))",params![scope.agent_id,queue.message_id],|row|row.get(0))?;
                anyhow::ensure!(
                    existing,
                    "Task cleanup queue requires original State pending accounting"
                );
            }
        }
        if let Some(state) = state {
            let old = state
                .expected
                .as_deref()
                .context("Work cleanup State requires its original OCC baseline")?;
            validate_agent_state_mutation_tx(tx, Some(state))?;
            let next = state.record.as_ref();
            if task_cleanup.is_some() || producer_input.is_some() {
                anyhow::ensure!(
                    (next.current_turn_work_item_id.is_none()
                        || next.current_turn_work_item_id == old.current_turn_work_item_id)
                        && (next.current_turn_operator_binding_id.is_none()
                            || next.current_turn_operator_binding_id
                                == old.current_turn_operator_binding_id)
                        && (next.current_turn_operator_reply_route_id.is_none()
                            || next.current_turn_operator_reply_route_id
                                == old.current_turn_operator_reply_route_id)
                        && (next.status != crate::types::AgentStatus::AwakeRunning
                            || next.status == old.status),
                    "Deleting Task cleanup cannot create execution or operator bindings"
                );
            }
            let task_queue = match self.command.as_ref() {
                InputTransitionCommand::Task(task)
                    if task_cleanup.is_some() || producer_input.is_some() =>
                {
                    task.command.queue_entry.as_ref()
                }
                _ => None,
            };
            let pending_limit = if let Some(queue) = task_queue {
                let (count,already_pending):(usize,bool)=tx.query_row(
                    "SELECT (SELECT COUNT(*) FROM queue_entries WHERE agent_id=?1 AND status IN ('queued','interrupted')),EXISTS(SELECT 1 FROM queue_entries WHERE agent_id=?1 AND message_id=?2 AND status IN ('queued','interrupted'))",
                    params![scope.agent_id,queue.message_id],|row|Ok((row.get(0)?,row.get(1)?)))?;
                let expected = count
                    .checked_add(usize::from(!already_pending))
                    .context("Task queue count overflow")?;
                anyhow::ensure!(
                    old.pending == count && next.pending == expected,
                    "Task cleanup pending differs from its one actual queue membership"
                );
                expected
            } else {
                old.pending
            };
            anyhow::ensure!(
                next.turn_index == old.turn_index
                    && (next.current_run_id.is_none() || next.current_run_id == old.current_run_id)
                    && (next.current_turn_id.is_none()
                        || next.current_turn_id == old.current_turn_id)
                    && (next.current_work_item_id.is_none()
                        || next.current_work_item_id == old.current_work_item_id)
                    && (!matches!(self.command.as_ref(), InputTransitionCommand::Task(_))
                        || next.current_execution_binding.is_none()
                        || next.current_execution_binding == old.current_execution_binding)
                    && next.pending <= pending_limit,
                "Deleting Work cleanup cannot start a run, Turn, focus or admission"
            );
        }
        if let InputTransitionCommand::Focus { command, .. } = self.command.as_ref() {
            for frame in &command.continuations {
                let old: WorkItemContinuationFrame = committed_input_payload_tx(
                    tx,
                    "work_item_continuations",
                    "continuation_id",
                    &frame.id,
                    &scope.agent_id,
                )?;
                anyhow::ensure!(
                    frame.state != WorkItemContinuationState::Active || *frame == old,
                    "Deleting cleanup cannot activate a continuation"
                );
            }
        }
        let mut roots = BTreeSet::new();
        if let InputTransitionCommand::Task(task) = self.command.as_ref() {
            if task_cleanup.is_none() && producer_input.is_none() {
                roots.extend(
                    task.command
                        .message_evidence
                        .iter()
                        .map(|message| Key::new(Family::Message, &message.id)),
                );
            }
        }
        anyhow::ensure!(
            protocol.bootstrap.is_none(),
            "Deleting cleanup cannot bootstrap execution admission"
        );
        for command in &protocol.commands {
            let work_id = match command {
                Command::SetWorkItemReadiness(x) => &x.work_item_id,
                Command::SuspendWorkItemContinuation(x) => &x.work_item_id,
                Command::ResumeWorkItemContinuation(x) => &x.work_item_id,
                Command::ReconcileWorkItemContinuationYield(x) => &x.work_item_id,
                Command::SetWorkItemWaiting(x) => &x.work_item_id,
                Command::CompleteWorkItem(x) => &x.work_item_id,
                // Queue's original cleanup proof owns terminal Attempt/outcome
                // derivation. It is never silently delegated to a Work proof.
                _ => anyhow::bail!(
                    "Deleting Work cleanup requires an existing Work execution command"
                ),
            };
            roots.insert(Key::new(Family::WorkExecution, work_id));
        }
        let mut derived = BTreeMap::new();
        for brief in briefs {
            let work = brief
                .work_item_id
                .as_ref()
                .context("Work cleanup Brief has no original Work binding")?;
            anyhow::ensure!(
                original_work_ids.contains(work),
                "Work cleanup Brief is outside the original Work footprint"
            );
            derived.insert(Key::new(Family::Brief, &brief.id), work.clone());
        }
        if let Some(tool) = completion_tool {
            let work = tool
                .work_item_id
                .as_ref()
                .context("Work cleanup Tool has no original Work binding")?;
            anyhow::ensure!(
                original_work_ids.contains(work),
                "Work cleanup Tool is outside the original Work footprint"
            );
            derived.insert(Key::new(Family::ToolExecution, &tool.id), work.clone());
        }
        let input_proof = if derived.is_empty() {
            crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_tx(
                tx,
                scope,
                roots,
                BTreeSet::new(),
            )?
        } else {
            crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_from_work_tx(
                tx, proof, roots, derived,
            )?
        };
        Ok(Some(if let Some(cap) = task_cleanup {
            crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_from_task_result_cleanup_tx(tx,input_proof,cap)?
        } else if let Some(view) = producer_input {
            crate::runtime_db::input_evidence_snapshot::ExistingAdmissionInputProof::prepare_from_original_command_message_tx(tx,input_proof,view)?
        } else {
            input_proof
        }))
    }
}

/// Read-only possible-write footprint of the original TriggerWaitAndEnqueue
/// reducer. Original generation/delivery predicates remain authoritative; this
/// never triggers, qualifies or grants a new Wait.
pub(in crate::runtime_db) fn predicted_trigger_wait_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    message: &MessageEnvelope,
    queue: &QueueEntryRecord,
    request: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<(
    Option<WaitConditionRecord>,
    Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
)> {
    let mut material = crate::runtime_db::work_snapshot::WorkWriteMaterial::new(
        scope,
        request,
        ledger,
        memory_budget,
    )?;
    let wait = predicted_trigger_wait_with_material_tx(tx, scope, message, queue, &mut material)?;
    Ok((wait, material.into_guards()))
}

fn predicted_trigger_wait_with_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    message: &MessageEnvelope,
    queue: &QueueEntryRecord,
    material: &mut crate::runtime_db::work_snapshot::WorkWriteMaterial<'_>,
) -> Result<Option<WaitConditionRecord>> {
    anyhow::ensure!(
        message.id == queue.message_id
            && message.agent_id == queue.agent_id
            && message.agent_id == scope.agent_id
            && queue.status == QueueEntryStatus::Queued,
        "TriggerWaitAndEnqueue admission identity differs"
    );
    let existing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE message_id=?1) OR EXISTS(SELECT 1 FROM queue_entries WHERE message_id=?1)", [&message.id], |row| row.get(0))?;
    material.trigger_wait_pool_tx(tx, scope)?;
    // The original reducer still materializes its pool on an existing admission.
    if existing {
        return Ok(None);
    }
    let count:usize=tx.query_row(
        "SELECT COUNT(*) FROM wait_conditions WHERE agent_id=?1 AND status IN ('active','triggered')",
        [&scope.agent_id],|row|row.get(0))?;
    let conditions = {
        let mut query = tx.prepare("SELECT payload_json FROM wait_conditions WHERE agent_id=?1 AND status IN ('active','triggered') ORDER BY updated_at DESC,created_at DESC,wait_condition_id DESC LIMIT ?2")?;
        let rows = query
            .query_map(params![message.agent_id, i64::try_from(count)?], |row| {
                row.get::<_, String>(0)
            })?
            .map(|row| Ok(serde_json::from_str::<WaitConditionRecord>(&row?)?))
            .collect::<Result<Vec<_>>>()?;
        rows
    };
    let crate::wake_contract::WaitTriggerSelection::Match { condition, .. } =
        crate::wake_contract::select_wait_to_trigger(message, &conditions)
    else {
        return Ok(None);
    };
    if condition.status != WaitConditionStatus::Active
        || !wait_generation_matches_tx(tx, message, condition)?
    {
        return Ok(None);
    }
    let trigger_id =
        agent_message_reply_trigger_tx(tx, message)?.unwrap_or_else(|| message.id.clone());
    let claimed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM wait_conditions WHERE agent_id=?1 AND trigger_message_id=?2 AND wait_condition_id!=?3)", params![message.agent_id,trigger_id,condition.id], |row| row.get(0))?;
    if claimed {
        return Ok(None);
    }
    let mut triggered = condition.clone();
    triggered.mark_triggered(&trigger_id, queue.updated_at);
    Ok(Some(triggered))
}

/// Read one exact child identity in this original view. This does not activate
/// it, load a successor State or grant the parent a cross-agent evidence scope.
fn task_child_identity_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<crate::types::AgentIdentityRecord> {
    let (status, payload): (String, String) = tx.query_row(
        "SELECT status,payload_json FROM agent_identities WHERE agent_id=?1",
        [&scope.agent_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let identity: crate::types::AgentIdentityRecord = serde_json::from_str(&payload)?;
    anyhow::ensure!(
        identity.agent_id == scope.agent_id
            && identity.incarnation == scope.identity_incarnation
            && identity.incarnation > 0
            && serde_json::to_value(&identity.status)?.as_str() == Some(status.as_str()),
        "Task child identity differs from its original current scope"
    );
    Ok(identity)
}
