//! Original whole-transaction receipts for explicitly frozen transition commands.
//!
//! Prepared commands own their payload exactly once. A replay carries the
//! original outcome only; current installation facts come from a fresh resolver.
use super::*;
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
use crate::runtime_db::provider_admission::{
    acknowledge_frozen_stopped_provider_pause_tx, charge_frozen_stopped_provider_pause_tx,
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
#[path = "prepared_task_stop.rs"]
mod task_stop;
#[path = "prepared_task_stop_phase.rs"]
mod task_stop_phase;
#[path = "prepared_input_task_writes.rs"]
mod task_writes;
pub(crate) use task_stop_phase::worktree_receipt_exists_tx;
#[path = "prepared_current_writes.rs"]
mod current_writes;
#[path = "prepared_live_input.rs"]
mod live_input;
#[path = "prepared_input_native_birth.rs"]
mod native_input;
#[path = "prepared_task_status_metadata.rs"]
mod task_status_metadata;
#[path = "prepared_task_stop_result.rs"]
mod task_stop_result;

/// Every argument of the original queue transaction is frozen before its owner
/// ticket begins. No field is inferred from a callback or the current database.
#[derive(Debug, serde::Serialize)]
pub(crate) struct PreparedQueueTransition {
    pub command: QueueTransitionCommand,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supervised_invocation: Option<PreparedSupervisedInvocationAdmission>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stopped_provider_pause:
        Option<crate::runtime_db::provider_admission::FrozenStoppedProviderPause>,
}
impl PreparedQueueTransition {
    pub(crate) fn new(command: QueueTransitionCommand) -> Self {
        Self {
            command,
            supervised_invocation: None,
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
            stopped_provider_pause: None,
        }
    }

    /// Freeze the original dispatch Stop address that won over a parked
    /// provider pause. The final queue transaction re-verifies it in the
    /// same transaction before the frozen ACK may commit.
    pub(crate) fn with_stopped_provider_pause(
        mut self,
        original: &crate::runtime_db::control_transition::ControlStoppedRun<'_>,
        pause: &crate::runtime_db::provider_admission::ProviderDispatchPause,
    ) -> Result<Self> {
        anyhow::ensure!(
            self.stopped_provider_pause.is_none(),
            "prepared queue transition already froze a stopped provider pause"
        );
        self.stopped_provider_pause = Some(
            crate::runtime_db::provider_admission::FrozenStoppedProviderPause::freeze(
                original, pause,
            )?,
        );
        Ok(self)
    }

    fn is_stopped_provider_pause_ack(&self) -> bool {
        self.stopped_provider_pause
            .as_ref()
            .is_some_and(|frozen| frozen.is_original_ack_shape(self))
    }
}

/// Both owners are reserved before this original cross-agent admission begins.
#[derive(Debug, serde::Serialize)]
pub(crate) struct PreparedSupervisedInvocationAdmission {
    pub parent_scope: InputEvidenceScope,
    pub parent_task: TaskRecord,
    pub child_incarnation: u64,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    original_command_creation: Option<
        std::sync::Arc<crate::runtime_db::command_task_creation::OriginalCommandTaskCreation>,
    >,
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
            original_command_creation: None,
        })
    }

    pub(crate) fn with_original_command_creation(
        mut self,
        creation: std::sync::Arc<
            crate::runtime_db::command_task_creation::OriginalCommandTaskCreation,
        >,
    ) -> Result<Self> {
        anyhow::ensure!(
            serde_json::to_value(&self.command.task)? == serde_json::to_value(creation.task())?
                && self.command.agent_id == creation.scope().agent_id
                && self.command.expected_owner_incarnation == creation.scope().identity_incarnation
                && self.child.is_none()
                && self.stop_intent.is_none()
                && self.original_command_terminal.is_none()
                && self.original_child_terminal.is_none()
                && self.original_child_binding.is_none()
                && self.command.task_result_settlement.is_none()
                && self.command.queue_entry.is_none()
                && self.command.message_evidence.is_empty(),
            "Command creation differs from original creator or combines terminal authority"
        );
        self.original_command_creation = Some(creation);
        Ok(self)
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum InputTransitionCommand {
    Audit {
        agent_id: String,
        events: Vec<AuditEvent>,
        #[serde(skip)]
        fault: Option<TransitionFaultPoint>,
    },
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
    LiveInputAttachment(LiveInputAttachmentCommand),
    LiveInputRecovery(LiveInputRecoveryCommand),
    ContextBudgetStopRecovery {
        agent_id: String,
        protocol: ExecutionProtocolTransition,
        audit_events: Vec<AuditEvent>,
    },
    ProviderResultHandoff(ProviderResultHandoffCommand),
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
            Self::Audit { .. } => InputMutationFamily::Audit,
            Self::Posture { .. } => InputMutationFamily::AgentPosture,
            Self::Queue(_) => InputMutationFamily::Queue,
            Self::QueueHeadNoProgress { .. } => InputMutationFamily::QueueHeadNoProgress,
            Self::Terminal(_) => InputMutationFamily::Terminal,
            Self::Task(_) => InputMutationFamily::Task,
            Self::TaskResult(_) => InputMutationFamily::TaskResult,
            Self::LiveInputAttachment(_) => InputMutationFamily::LiveInputAttachment,
            Self::LiveInputRecovery(_) => InputMutationFamily::LiveInputRecovery,
            Self::ContextBudgetStopRecovery { .. } => {
                InputMutationFamily::ContextBudgetStopRecovery
            }
            Self::ProviderResultHandoff(_) => InputMutationFamily::ProviderResultHandoff,
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
            Self::Audit { agent_id, .. } => agent_id,
            Self::Posture { command, .. } => &command.agent_id,
            Self::Queue(command) => &command.command.agent_id,
            Self::QueueHeadNoProgress { command, .. } => &command.agent_id,
            Self::Terminal(command) => &command.agent_id,
            Self::Task(task) => &task.command.agent_id,
            Self::TaskResult(command) => command.agent_id(),
            Self::LiveInputAttachment(command) => &command.expected_attempt.agent_id,
            Self::LiveInputRecovery(command) => &command.expected_attempt.agent_id,
            Self::ContextBudgetStopRecovery { agent_id, .. } => agent_id,
            Self::ProviderResultHandoff(command) => &command.expected_attempt.agent_id,
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
}
impl PreparedInputTransition {
    /// Only the original typed Queue ACK can carry this read-only observation.
    pub(crate) fn stopped_provider_pause_ack(
        &self,
    ) -> Option<StoppedProviderPauseAck<'_>> {
        match self.command.as_ref() {
            InputTransitionCommand::Queue(queue) => StoppedProviderPauseAck::from_queue(queue),
            _ => None,
        }
    }

    pub(crate) fn with_stopped_provider_pause_memory(
        mut self,
        memory: crate::runtime_db::provider_admission::StoppedProviderPauseMemory,
    ) -> Result<Self> {
        anyhow::ensure!(
            self.memory.is_none() && self.ended_memory.is_none(),
            "stopped ACK cannot carry an ordinary memory command"
        );
        let InputTransitionCommand::Queue(queue) = self.command.as_mut() else {
            anyhow::bail!("stopped memory observation requires its original typed Queue ACK");
        };
        anyhow::ensure!(
            queue.is_stopped_provider_pause_ack(),
            "stopped memory observation requires its original typed Queue ACK"
        );
        queue.stopped_provider_pause.as_mut().expect("typed ACK").with_memory(memory)?;
        self.descriptor = PreparedInputMutation::new(
            self.descriptor.family(),
            self.descriptor.command_nonce().to_owned(),
            self.descriptor.scopes().to_vec(),
            canonical_command_digest(self.command.as_ref())?,
            self.descriptor.prepared_at(),
        )?;
        Ok(self)
    }

    /// One original scheduler recovery transaction, with its own replay receipt.
    pub(crate) fn context_budget_stop_recovery(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        command: crate::domain::execution_protocol::RecheckContextBudgetStop,
        audit_events: Vec<AuditEvent>,
    ) -> Result<Self> {
        anyhow::ensure!(
            !command.command_id.is_empty()
                && !command.work_item_id.is_empty()
                && audit_events.iter().all(|event| !event.id.is_empty()),
            "context budget recovery has an empty original address"
        );
        let agent_id = scope.agent_id.clone();
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::ContextBudgetStopRecovery {
                agent_id,
                protocol: ExecutionProtocolTransition {
                    commands: vec![
                        crate::domain::execution_protocol::ExecutionProtocolCommand::RecheckContextBudgetStop(Box::new(command)),
                    ],
                    ..Default::default()
                },
                audit_events,
            },
        )
    }

    /// A separate original transaction, never a synthetic State mutation.
    pub(crate) fn audit(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        event: AuditEvent,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        crate::diagnostics::resource_io::charge_owned_metadata(
            u64::try_from(std::mem::size_of::<AuditEvent>())?,
            1,
        )?;
        Self::audit_many(scope, command_nonce, prepared_at, vec![event], fault)
    }

    /// Preserve the original atomic append-many boundary and item order.
    pub(crate) fn audit_many(
        scope: InputEvidenceScope,
        command_nonce: String,
        prepared_at: chrono::DateTime<Utc>,
        events: Vec<AuditEvent>,
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Self> {
        let agent_id = scope.agent_id.clone();
        Self::prepare(
            scope,
            command_nonce,
            prepared_at,
            InputTransitionCommand::Audit {
                agent_id,
                events,
                fault,
            },
        )
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
    /// The supervised parent writes its own Task in this original admission.
    pub(crate) fn supervised_parent_scope(&self) -> Option<&InputEvidenceScope> {
        match self.command.as_ref() {
            InputTransitionCommand::Queue(queue) => queue
                .supervised_invocation
                .as_ref()
                .map(|admission| &admission.parent_scope),
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
                !queue.is_stopped_provider_pause_ack()
                    && (queue.command.operation == QueueOperation::Settle
                        || queue.wait_transition.is_some()
                        || queue.wait_registration.is_some()
                        || queue.completion.is_some()
                        || !queue.extra_wait_conditions.is_empty())
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

    /// Bootstrap cleared the physical run, not the original dispatch address.
    /// Only its exact retained Dequeued claim may finish that historical seed.
    pub(crate) fn inactive_queue_memory_turn_id<'a>(
        &self,
        state: &'a AgentState,
    ) -> Option<&'a str> {
        if state.current_run_id.is_some() {
            return None;
        }
        let InputTransitionCommand::Queue(queue) = self.command.as_ref() else {
            return None;
        };
        let QueueMutation::CompareAndSet { expected, record } = &queue.command.mutation else {
            return None;
        };
        let binding = state.current_execution_binding.as_ref()?;
        let activation = binding.activation_id.as_deref()?;
        let interrupts_original = queue.execution_protocol.commands.iter().any(|command| {
            use crate::domain::execution_protocol::ExecutionProtocolCommand as Command;
            match command {
                Command::Interrupt(command) => command.attempt_id == activation,
                Command::Settle(command) => command.outcome.attempt_id == activation,
                _ => false,
            }
        });
        (queue.command.operation == QueueOperation::Settle
            && interrupts_original
            && queue.command.agent_id == state.id
            && expected.agent_id == state.id
            && record.agent_id == state.id
            && expected.status == QueueEntryStatus::Dequeued
            && expected.message_id == binding.source_message_id
            && record.message_id == expected.message_id
            && matches!(
                record.status,
                QueueEntryStatus::Processed
                    | QueueEntryStatus::Interrupted
                    | QueueEntryStatus::Aborted
                    | QueueEntryStatus::Quarantined
            )
            && queue.command.agent_state.is_none()
            && queue.wait_transition.is_none()
            && queue.wait_registration.is_none()
            && queue.completion.is_none()
            && queue.extra_wait_conditions.is_empty()
            && state.current_turn_id.as_deref() == Some(binding.turn_id.as_str()))
        .then_some(binding.turn_id.as_str())
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
            QueueMutation::Upsert(record)
            | QueueMutation::SettleWithProviderRecovery { record, .. }
            | QueueMutation::CompareAndSet { record, .. }
            | QueueMutation::ParkProviderDispatch { record, .. }
            | QueueMutation::ConsumeSettledTaskResultRecovery { record, .. } => record,
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
            // Stop owns the producer's terminal state; its Brief/Audit ACK
            // neither finishes that producer nor creates deferred Memory.
            InputTransitionCommand::Queue(queue) if queue.is_stopped_provider_pause_ack() => {
                Policy::None
            }
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
            self.stopped_provider_pause_ack().is_none(),
            "stopped ACK cannot become an ordinary memory command"
        );
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
    /// whose original deletion result belongs to the same complete receipt and
    /// the supervised parent that binds its own Task to the child admission.
    pub(crate) fn with_scope_group(mut self, scopes: Vec<InputEvidenceScope>) -> Result<Self> {
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
        if let InputTransitionCommand::Queue(queue) = self.command.as_ref() {
            if let Some(admission) = &queue.supervised_invocation {
                anyhow::ensure!(
                    scopes.contains(&admission.parent_scope)
                        && scopes
                            .iter()
                            .filter(|scope| scope.agent_id == admission.parent_scope.agent_id)
                            .count()
                            == 1,
                    "supervised invocation group omits its original parent writer"
                );
            }
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

    /// The supervised parent is an allowed business writer of this original
    /// admission, not a read-only group expansion. A new admission needs its
    /// Active writer incarnation; an exact accepted receipt stays replayable
    /// while that parent retires.
    fn validate_supervised_parent_writer_scope_tx(
        tx: &rusqlite::Transaction<'_>,
        prepared: &PreparedInputTransition,
        scope: &InputEvidenceScope,
    ) -> Result<()> {
        let InputTransitionCommand::Queue(queue) = prepared.command.as_ref() else {
            anyhow::bail!("supervised invocation requires a queue admission");
        };
        let admission = queue
            .supervised_invocation
            .as_ref()
            .context("supervised invocation omitted its frozen parent admission")?;
        anyhow::ensure!(
            scope == &admission.parent_scope && admission.parent_task.agent_id == scope.agent_id,
            "supervised invocation parent scope differs from its frozen owner"
        );
        if lookup_input_mutation_receipt_tx(tx, &prepared.descriptor)?.is_some() {
            crate::runtime_db::input_evidence_snapshot::require_original_input_recovery_scope(
                tx, scope,
            )?;
            return Ok(());
        }
        crate::runtime_db::input_evidence_writes::validate_input_evidence_scope_tx(
            tx,
            scope,
            &admission.parent_task.agent_id,
        )
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
            InputTransitionCommand::Audit { events, .. } => anyhow::ensure!(
                !events.is_empty() && events.iter().all(|event| !event.id.is_empty()),
                "Audit command has an empty original address"
            ),
            InputTransitionCommand::Timer(command) => command.validate(&self.primary_scope)?,
            InputTransitionCommand::ExternalTrigger(command) => {
                command.validate(&self.primary_scope)?
            }
            InputTransitionCommand::TaskResult(command) => command.validate()?,
            InputTransitionCommand::LiveInputAttachment(command) => {
                live_input::validate_attachment(command, &self.primary_scope)?
            }
            InputTransitionCommand::LiveInputRecovery(command) => {
                live_input::validate_recovery(command, &self.primary_scope)?
            }
            InputTransitionCommand::ContextBudgetStopRecovery { protocol, .. } => {
                self.validate_protocol_namespace(protocol)?
            }
            InputTransitionCommand::ProviderResultHandoff(command) => {
                live_input::validate_handoff(command, &self.primary_scope)?
            }
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
                let command = &queue.command;
                if let Some(admission) = &queue.supervised_invocation {
                    anyhow::ensure!(
                        admission.parent_scope.agent_id == admission.parent_task.agent_id
                            && admission.parent_scope.agent_id != self.primary_scope.agent_id
                            && admission.parent_scope.identity_incarnation > 0
                            && admission.child_incarnation
                                == self.primary_scope.identity_incarnation
                            && !admission.parent_task.id.is_empty()
                            && self.descriptor.scopes().contains(&admission.parent_scope),
                        "supervised invocation differs from its frozen owner scopes"
                    );
                }
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
                if let QueueMutation::SettleWithProviderRecovery { recovery, .. } =
                    &queue.command.mutation
                {
                    targets.insert(("queue_entries", "message_id", recovery.id.as_str()));
                }
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
            InputTransitionCommand::Audit { .. }
            | InputTransitionCommand::ContextBudgetStopRecovery { .. }
            | InputTransitionCommand::Posture { .. }
            | InputTransitionCommand::Terminal(_)
            | InputTransitionCommand::TaskResult(_)
            | InputTransitionCommand::LiveInputAttachment(_)
            | InputTransitionCommand::LiveInputRecovery(_)
            | InputTransitionCommand::ProviderResultHandoff(_)
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
                recovery_at: None,
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
    pub(crate) fn commit_prepared_transition_with_domain_requests(
        &self,
        prepared: &PreparedInputTransition,
        work_request: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
        memory_ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        task_request: &crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
        scheduling_request: &crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest,
    ) -> Result<InputMutationExecution<TransitionCommit>> {
        self.commit_prepared_transition_with_domains_and_input_query(
            prepared,
            work_request,
            memory_ledger,
            task_request,
            scheduling_request,
            None,
            None,
        )
    }

    pub(crate) fn commit_prepared_transition_with_input_query(
        &self,
        prepared: &PreparedInputTransition,
        work_request: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
        memory_ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        task_request: &crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
        input_query_source: Option<
            &crate::runtime::resident_input_evidence::OriginalInputQuerySource,
        >,
        current_query_source: Option<
            &std::sync::Arc<crate::runtime_db::current_query::CurrentQuerySource>,
        >,
    ) -> Result<InputMutationExecution<TransitionCommit>> {
        self.commit_prepared_transition_with_domains_and_input_query(
            prepared,
            work_request,
            memory_ledger,
            task_request,
            &timer_writes::default_request(work_request),
            input_query_source,
            current_query_source,
        )
    }

    fn commit_prepared_transition_with_domains_and_input_query(
        &self,
        prepared: &PreparedInputTransition,
        work_request: &crate::runtime_db::work_snapshot::WorkDomainLoadRequest,
        memory_ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        task_request: &crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
        scheduling_request: &crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest,
        input_query_source: Option<
            &crate::runtime::resident_input_evidence::OriginalInputQuerySource,
        >,
        current_query_source: Option<
            &std::sync::Arc<crate::runtime_db::current_query::CurrentQuerySource>,
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
                return task_stop_result::commit_result_tx(tx, prepared, command, work_request, task_request, memory_ledger, input_query_source, current_query_source);
            }
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
                    crate::runtime_db::input_evidence_snapshot::require_original_input_recovery_scope(
                        tx,
                        reserved_scope,
                    )?;
                } else if prepared.task_stop_child_scope() == Some(reserved_scope) {
                    let InputTransitionCommand::Task(task) = prepared.command.as_ref() else { unreachable!() };
                    task_stop::validate_child_readonly_tx(tx, task.stop_intent.as_ref().expect("Stop child scope"))?;
                } else if prepared.task_deletion_scope() == Some(reserved_scope) {
                    // Same original child may already be Deleted at exact replay;
                    // no new write is allowed without its frozen pre-State below.
                    let _ = task_child_identity_tx(tx, reserved_scope)?;
                } else if prepared.original_child_terminal().is_some() {
                    child_terminal::validate_reserved_scope(prepared,reserved_scope)?;
                } else if prepared.supervised_parent_scope() == Some(reserved_scope) {
                    PreparedInputTransition::validate_supervised_parent_writer_scope_tx(tx, prepared, reserved_scope)?;
                } else {
                    crate::runtime_db::input_evidence_writes::validate_readonly_input_reservation_tx(
                        tx,
                        reserved_scope,
                    )?;
                }
            }
            prepared.validate_primary_namespace()?;
            let result_receipt_guard=if matches!(prepared.command.as_ref(),InputTransitionCommand::Task(_) | InputTransitionCommand::TaskResult(_) | InputTransitionCommand::LiveInputAttachment(_) | InputTransitionCommand::LiveInputRecovery(_) | InputTransitionCommand::ProviderResultHandoff(_) | InputTransitionCommand::Timer(_)) {
                Some(super::prepared_task_result::receipt_budget_tx(tx,&prepared.descriptor,scope,task_request,memory_ledger)?)
            } else {None};
            if let Some(receipt) = lookup_input_mutation_receipt_tx(tx, &prepared.descriptor)? {
                return Ok(InputMutationExecution::Replayed { receipt,
                    _material_budget_views:result_receipt_guard.into_iter().collect() });
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
                crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget::prepare_tx(
                    tx,memory,work_request,memory_ledger)).transpose()?;
            let recovery_source_material = match prepared.command.as_ref() {
                InputTransitionCommand::Queue(queue) => match &queue.command.mutation {
                    QueueMutation::ConsumeSettledTaskResultRecovery { source, .. } => {
                        crate::runtime_db::task_snapshot::prepare_settled_recovery_source_material_tx(
                            tx, scope, source, task_request, memory_ledger, memory_budget.as_mut(),
                        )?
                    }
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            };
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
            let scope_read =
                crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope.agent_id)
                    .scope_read()?;
            let facts = scope_read.facts();
            let deleting = facts.identity.as_ref().is_some_and(|identity| {
                identity.status == crate::types::AgentRegistryStatus::Deleting
            });
            if matches!(prepared.command.as_ref(), InputTransitionCommand::Task(task) if task.stop_intent.is_some()) {
                anyhow::ensure!(facts.identity.as_ref().is_some_and(|identity|
                    identity.status == crate::types::AgentRegistryStatus::Active),
                    "new TaskStop requires original Active identity");
            }
            let scheduling_transaction = timer_writes::prepare_tx(tx,prepared,scope,scheduling_request,
                memory_ledger,memory_budget.as_mut())?;
            let supervised_parent_transaction = task_writes::prepare_supervised_parent_transaction_tx(
                tx, prepared, task_request, memory_ledger, memory_budget.as_mut(),
            )?;
            let mut supervised_parent_applied = false;
            let mut task_transaction = if let Some(entry)=child_entry {
                task_writes::prepare_child_task_transaction_tx(tx,prepared,scope,deleting,task_request,memory_ledger,memory_budget.as_mut(),entry)?
            } else {
                task_writes::prepare_task_transaction_tx(tx,prepared,scope,deleting,task_request,memory_ledger,memory_budget.as_mut())?
            };
            let _creation_material = if let InputTransitionCommand::Task(task) = prepared.command.as_ref() {
                if let Some(creation) = &task.original_command_creation {
                    let bytes = creation.original_bytes()?.checked_mul(8)
                        .context("Command creation material overflow")?;
                    if let Some(memory) = &mut memory_budget {
                        memory.additional_material(bytes, 8, 1)?;
                    }
                    Some(crate::runtime_db::command_task_terminal::charge_material(
                        scope, bytes, 8, task_request, memory_ledger)?)
                } else { None }
            } else { None };
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
            let work_proof = prepared.prepare_work_proof_tx(tx, scope, deleting, work_request,
                memory_ledger, memory_budget.as_mut(),scheduling_transaction.as_ref())?;
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
            // Source may write Input before the main command; observe absence
            // before either phase, not after the first canonical writer.
            let mut native_input = crate::runtime_db::input_evidence_snapshot::native_birth::NativeInputWriteProof::prepare_with_scope_read_tx(
                tx, scope, memory_ledger, crate::runtime_db::input_evidence_snapshot::native_birth::original_material_limits(memory_ledger)?,
                memory_budget.as_mut(), Some(&scope_read), |visit| native_input::visit_original(prepared, visit),
            )?;
            if let Some(native) = &mut native_input {
                native.bind_query_source(input_query_source, memory_ledger)?;
            }
            let original_current = if let Some(native) = &mut native_input {
                native.with_input_phase(tx, memory_ledger, memory_budget.as_mut(), |material| {
                    let mut current = crate::runtime_db::current_read_writes::OriginalCurrentWrites::prepare(
                        tx, scope, material, |visit| current_writes::visit_original(prepared, finish_memory.as_ref(), visit),
                    )?;
                    if let Some(current) = &mut current {
                        current.bind_query_source(current_query_source, material)?;
                        current_writes::prepare_generated(prepared, current, material)?;
                    }
                    Ok(current)
                })?
            } else { None };
            let child_source_phase=if prepared.original_child_terminal().is_some() {
                Some(task_transaction.as_mut().context("child Source omitted enclosing Task proof")?
                    .apply_original_child_source_tx(tx,prepared,memory_ledger,memory_budget.as_mut())?)
            } else {None};
            let repository = RuntimeTransitionRepository {
                db: self.db,
                input_scope: Some(scope.clone()),
                cleanup_input: cleanup.as_ref(),
            };
            let (mut value, mut result) = match prepared.command.as_ref() {
                InputTransitionCommand::Audit { agent_id, events, fault } => {
                    inject_fault(*fault, TransitionFaultPoint::AfterValidation)?;
                    super::AuditEventAck::admit_slots(events.len())?;
                    let mut audit_events = Vec::with_capacity(events.len());
                    for event in events {
                        audit_events.push(crate::runtime_db::evidence::append_audit_event_ack_tx(
                            tx, Some(agent_id), event,
                        )?.0);
                    }
                    inject_fault(*fault, TransitionFaultPoint::AfterAuditWrites)?;
                    (TransitionCommit {
                        applied: true,
                        effects: Box::new(PostCommitEffects {
                            audit_events,
                            fault: *fault,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }, InputMutationResult::Transition)
                },
                InputTransitionCommand::ExternalTrigger(_) | InputTransitionCommand::TaskStatusMetadata(_) | InputTransitionCommand::TaskStopProgress(_) | InputTransitionCommand::TaskStopContinuation(_) => unreachable!("focused metadata phase dispatched above"),
                InputTransitionCommand::Posture {
                    command,
                    started_turn,
                } => (
                    repository.commit_agent_posture_tx(tx, command, started_turn.as_ref())?,
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
                    } else { repository.commit_turn_terminal_tx(tx, command)? };
                    (commit, InputMutationResult::Transition)
                },
                InputTransitionCommand::Timer(timer) => timer.execute_tx(tx,self.db,
                    scheduling_transaction.as_ref().context("Timer omitted its Scheduling transaction")?,
                    scope,work_request,task_request,memory_ledger,memory_budget.as_mut())?,
                InputTransitionCommand::LiveInputAttachment(command) => {
                    let tasks = task_transaction.as_ref().context("live attachment omitted Task proof")?;
                    (repository.commit_live_input_attachment_tx(tx, Some(scope), command, Some(tasks.allowed_keys()))?,
                        InputMutationResult::Transition)
                }
                InputTransitionCommand::LiveInputRecovery(command) => {
                    let tasks = task_transaction.as_ref().context("live recovery omitted Task proof")?;
                    (repository.commit_live_input_recovery_tx(tx, Some(scope),
                        &command.expected_attempt, &command.expected_turn, &command.predecessor,
                        command.fault, Some(tasks.allowed_keys()))?, InputMutationResult::Transition)
                }
                InputTransitionCommand::ContextBudgetStopRecovery { agent_id, protocol, audit_events } => {
                    let (value, outcome) = repository.commit_execution_protocol_recovery_plan_tx(
                        tx, agent_id, Some(scope), &protocol.commands, audit_events, None,
                    )?;
                    (value, InputMutationResult::ContextBudgetStopRecovery { outcome })
                }
                InputTransitionCommand::ProviderResultHandoff(command) => {
                    let tasks = task_transaction.as_ref().context("provider handoff omitted Task proof")?;
                    (repository.commit_provider_result_handoff_tx(tx, Some(scope),
                        &command.expected_attempt, &command.expected_turn, command.at, command.fault,
                        Some(tasks.allowed_keys()))?, InputMutationResult::Transition)
                }
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
                    if let Some(creation) = &task.original_command_creation {
                        anyhow::ensure!(creation.scope() == scope, "Command creation scope changed");
                        creation.write_tx(tx)?;
                    }
                    let existing_binding=prepared.original_child_monitor_binding().map(|_|child_binding::existing_write_proof_tx(tx,prepared,task_transaction.as_ref().context("child binding omitted Task proof")?.task_write_proof())).transpose()?;
                    let (mut value, deletion) = repository.commit_task_at_with_task_results_tx(tx, &task.command,
                        &task.execution_protocol, task.synchronize_execution_protocol,
                        prepared.descriptor.prepared_at(), task.child.as_ref().map(|child| child.deletion_id.as_str()),
                        pending,task_transaction.as_ref().and_then(|tasks|tasks.cleanup()),stop_proof.as_ref(),existing_binding.as_ref())?;
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
                    repository.commit_work_item_tx(tx, command, execution_protocol, *synchronize_execution_protocol)?,
                    InputMutationResult::Transition,
                ),
                InputTransitionCommand::Focus { command, execution_protocol, synchronize_execution_protocol, completion_tool_execution } => (
                    repository.commit_work_item_focus_tx(tx, command, execution_protocol,
                        *synchronize_execution_protocol, completion_tool_execution.as_ref())?,
                    InputMutationResult::Transition,
                ),
                InputTransitionCommand::Wait { command, execution_protocol, task_expectation } => (
                    repository.commit_wait_tx(tx, command, execution_protocol, task_expectation.as_ref())?,
                    InputMutationResult::Transition,
                ),
                InputTransitionCommand::Queue(queue) => {
                    let apply = if let Some(admission) = &queue.supervised_invocation {
                        match super::supervised_invocation::prepare_admission_tx(
                            tx, &queue.command, &admission.parent_task, admission.child_incarnation,
                        )? {
                            Some(task) => {
                                upsert_task_tx(tx, &task)?;
                                supervised_parent_applied = true;
                                true
                            }
                            None => false,
                        }
                    } else {
                        true
                    };
                    let mut stopped_pause_ack = None;
                    let stopped_pause_guard = if let Some(frozen) = &queue.stopped_provider_pause {
                        frozen.require_original_ack_shape_tx(tx, queue)?;
                        let guard = charge_frozen_stopped_provider_pause_tx(
                            tx, frozen, scope, task_request, memory_ledger,
                            memory_budget.as_mut(),
                        )?;
                        acknowledge_frozen_stopped_provider_pause_tx(tx, frozen)?;
                        // The frozen ACK never enters the generic queue writer:
                        // repeating the Stop-owned terminal Upsert is Idempotent
                        // there and would drop the frozen Brief/Audit evidence.
                        // Only that original Brief/Audit commits; the Stop-owned
                        // Queue, Turn, attempt and AgentState stay untouched.
                        for brief in &queue.command.brief_evidence {
                            crate::runtime_db::evidence::insert_brief_evidence_tx(tx, brief)?;
                        }
                        stopped_pause_ack = Some(super::finish_transition_tx(
                            tx,
                            Some(scope),
                            true,
                            &queue.command.agent_id,
                            &queue.command.audit_events,
                            &[],
                            queue.command.fault,
                            super::PostCommitEffects {
                                briefs: queue.command.brief_evidence.clone(),
                                ..Default::default()
                            },
                        )?);
                        Some(guard)
                    } else {
                        None
                    };
                    let mut value = if let Some(ack) = stopped_pause_ack {
                        ack
                    } else if apply {
                        repository.commit_queue_transaction_full_with_task_results_tx(
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
                        task_transaction.as_mut().and_then(|tasks| tasks.take_result()),
                        )?
                    } else {
                        TransitionCommit::default()
                    };
                    if let Some(guard) = stopped_pause_guard {
                        value.effects.task_material_budget_views.push(guard);
                    }
                    (value, InputMutationResult::Transition)
                }
                InputTransitionCommand::QueueHeadNoProgress {
                    command,
                    prepared_at,
                    audit_event_id,
                } => {
                    let original = repository.commit_queue_head_no_progress_tx(
                        tx,
                        command,
                        *prepared_at,
                        audit_event_id,
                    )?;
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
            };
            // Business SQL and its exact count/State epilogue are complete.
            // Qualify only the actual final Work/Wait/Frame footprint and actual
            // D3 Result writes, once, before the whole-transaction receipt.
            let actual_work = prepared.actual_work_keys(&value.effects);
            if let Some(budget)=&mut memory_budget { budget.before_finalizer_tx(tx)?; }
            let final_request=memory_budget.as_ref().map(|budget|budget.work_request(work_request))
                .transpose()?.unwrap_or_else(||work_request.clone());
            let mut final_work_view = None;
            if let Some(proof) = &work_proof {
                let actual = &actual_work;
                let result_briefs = value.effects.briefs.iter()
                    .filter(|brief| brief.kind == crate::types::BriefKind::Result)
                    .map(|brief| brief.id.clone()).collect();
                // The Work finalizer checks this ACK against canonical SQL in
                // the same transaction. Without a State write, reuse the
                // original observation instead of decoding it a second time.
                let final_state = value.effects.agent_state.as_ref()
                    .map(|state| state.record.as_ref())
                    .or(facts.state.as_deref())
                    .context("Work finalizer requires actual final State")?;
                let work = if finish_memory.is_some() {
                    let (facts, view) = crate::runtime_db::work_snapshot::register_work_writes_with_view_tx(
                        tx, proof, actual, &result_briefs, final_state, &final_request)?;
                    final_work_view = Some(view);
                    facts
                } else {
                    crate::runtime_db::work_snapshot::register_work_writes_tx(
                        tx, proof, actual, &result_briefs, final_state, &final_request)?
                };
                value.effects.work_input_preparation = Some(WorkInputPreparation {
                    scope: scope.clone(),
                    work_revision: work.delta.revision,
                    owning_roots: work.owning_input_roots,
                    completion_heads: work.delta.coverage.latest_results.iter()
                        .filter_map(|(id, head)| head.clone().map(|head| (id.clone(), head)))
                        .collect(),
                });
                value.effects.work_delta = Some(work.delta);
            }
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
                let final_work = memory_budget.as_mut()
                    .context("memory finish omitted original transaction budget")?
                    .capture_final_work(tx, final_work_view.as_ref()
                        .context("memory finish omitted exact final Work view")?)?;
                let state = crate::runtime_db::context_inputs::ContextDurableRead::new(tx,&scope.agent_id)
                    .scope_facts()?.state.context("memory finish requires final business State")?;
                let memory = crate::runtime_db::dispatch_memory::finish_dispatch_memory_tx(token,&state,
                    final_work,work_request)?;
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
            if task_transaction.is_none() && value.effects.task_delta.is_none()
                && !matches!(prepared.command.as_ref(),InputTransitionCommand::Timer(_)) {
                if let Some((delta, guards)) = crate::runtime_db::task_snapshot::register_task_source_metadata_tx(
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
                scheduling.finish(tx,&mut value,scheduling_request,memory_budget.as_mut())?;
            }
            value.effects.task_material_budget_views.extend(result_receipt_guard);
            value.effects.task_material_budget_views.extend(recovery_source_material);
            if let (Some(current), Some(native)) = (original_current, native_input.as_mut()) {
                value.effects.current_read_writes = Some(native.with_input_phase(
                    tx, memory_ledger, memory_budget.as_mut(), |material| current.finish_with_audit_acks(
                        material, &value.effects.audit_events, |visit| {
                            crate::runtime_db::current_read_writes::visit_effects(&value.effects, visit)?;
                            // Source audits stay durable-only notifications, but
                            // their Current facts belong to this original ACK.
                            if prepared.original_child_terminal().is_some() {
                                child_terminal::visit_source_audits(prepared, visit)?;
                            }
                            Ok(())
                        },
                    ),
                )?);
            }
            if let Some(native) = native_input {
                native.finish_delta_tx(tx, &mut value.effects.input_evidence_delta,
                    memory_ledger, memory_budget.as_mut())?;
            }
            if let Some(tasks) = supervised_parent_transaction {
                if supervised_parent_applied {
                    anyhow::ensure!(value.applied, "supervised parent binding omitted child admission");
                    task_writes::finish_supervised_parent_tx(
                        tx, tasks, &mut value, memory_ledger, memory_budget.as_mut(),
                    )?;
                }
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
            let mut receipt_events = super::AuditEventAck::clone_batch(&value.effects.audit_events)?;
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
                            } else if let InputTransitionCommand::Queue(queue) = prepared.command.as_ref() {
                                let mut keys = super::super::input_recovery_install::InputMutationInstallKeys::default();
                                // An idempotent no-op binding writes no parent Task,
                                // so it must not publish a parent current receipt.
                                if supervised_parent_applied {
                                    if let Some(admission) = &queue.supervised_invocation {
                                        if &admission.parent_scope == reserved_scope {
                                            keys.tasks.insert(admission.parent_task.id.clone());
                                        }
                                    }
                                }
                                keys
                            } else {
                                Default::default()
                            },
                            scope: reserved_scope.clone(),
                            audit_events: if primary {
                                std::mem::take(&mut receipt_events)
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
            if let InputTransitionCommand::Audit { fault, .. } = prepared.command.as_ref() {
                inject_fault(*fault, TransitionFaultPoint::BeforeCommit)?;
            }
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
            InputTransitionCommand::ContextBudgetStopRecovery { .. }
                | InputTransitionCommand::Timer(_)
                | InputTransitionCommand::LiveInputAttachment(_)
                | InputTransitionCommand::LiveInputRecovery(_)
                | InputTransitionCommand::ProviderResultHandoff(_)
        ) {
            anyhow::bail!("new Timer/live input command requires Active identity");
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
            InputTransitionCommand::Audit { .. }
            | InputTransitionCommand::ContextBudgetStopRecovery { .. } => {
                bail!("Deleting identity cannot begin an independent Audit/recovery command");
            }
            InputTransitionCommand::Work { .. }
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
            InputTransitionCommand::LiveInputAttachment(_)
            | InputTransitionCommand::LiveInputRecovery(_)
            | InputTransitionCommand::ProviderResultHandoff(_) => {
                unreachable!("new live input requires Active identity")
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
                            Some((&settle.outcome.attempt_id, Some(&settle.outcome.outcome_id)))
                        }
                        Command::Interrupt(interrupt) => {
                            Some((&interrupt.attempt_id, Some(&interrupt.outcome_id)))
                        }
                        Command::FinalizeInterrupted(finalize) => {
                            Some((&finalize.attempt_id, None))
                        }
                        Command::RegisterWorkItem(_)
                        | Command::Admit(_)
                        | Command::AdvanceWorkItemSourceRevision(_)
                        | Command::RecheckContextBudgetStop(_)
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
                        if let Some(outcome_id) = outcome_id {
                            derived.insert(Key::new(Family::Outcome, outcome_id));
                        }
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
            InputTransitionCommand::ContextBudgetStopRecovery { .. } => unreachable!(),
            InputTransitionCommand::Terminal(command) => Some(&command.agent_state),
            InputTransitionCommand::Queue(queue) => queue.command.agent_state.as_ref(),
            InputTransitionCommand::LiveInputAttachment(_)
            | InputTransitionCommand::LiveInputRecovery(_)
            | InputTransitionCommand::ProviderResultHandoff(_) => unreachable!(),
            InputTransitionCommand::QueueHeadNoProgress { .. } => {
                anyhow::bail!("new queue progress cannot use an existing admission cleanup proof")
            }
            InputTransitionCommand::Audit { .. }
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
            InputTransitionCommand::ContextBudgetStopRecovery { .. } => None,
            InputTransitionCommand::Terminal(command) => Some(&command.turn_record),
            InputTransitionCommand::Queue(queue) => queue.command.turn_record.as_ref(),
            InputTransitionCommand::LiveInputAttachment(_)
            | InputTransitionCommand::LiveInputRecovery(_)
            | InputTransitionCommand::ProviderResultHandoff(_) => None,
            InputTransitionCommand::QueueHeadNoProgress { .. } => {
                anyhow::bail!("new queue progress cannot use an existing admission cleanup proof")
            }
            InputTransitionCommand::Audit { .. }
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
        use crate::runtime::resident_work::{WorkFamily, WorkKey};
        let mut keys = BTreeSet::new();
        let mut waits = Vec::new();
        let mut projection_guards = Vec::new();
        match self.command.as_ref() {
            InputTransitionCommand::ExternalTrigger(_) => return Ok(None),
            InputTransitionCommand::ContextBudgetStopRecovery { protocol, .. } => {
                let [crate::domain::execution_protocol::ExecutionProtocolCommand::RecheckContextBudgetStop(command)] =
                    protocol.commands.as_slice()
                else {
                    anyhow::bail!("context budget recovery requires its original single recheck");
                };
                keys.insert(WorkKey::new(WorkFamily::Work, &command.work_item_id));
            }
            InputTransitionCommand::Timer(timer) => {
                if let Some(cancelled_waits) = timer.cancellation_waits_tx(tx)? {
                    keys.extend(cancelled_waits.iter().filter_map(|wait| {
                        wait.work_item_id
                            .as_ref()
                            .map(|id| WorkKey::new(WorkFamily::Work, id))
                    }));
                    waits.extend(cancelled_waits);
                } else if let Some(plan) = scheduling.filter(|plan| plan.fires_wake()) {
                    let (message, queue) = timer
                        .new_wake()
                        .context("Timer plan has another pending branch")?;
                    let _prediction_material = plan.enter_prediction_material()?;
                    let (wait, guard) = predicted_trigger_wait_tx(
                        tx,
                        scope,
                        message,
                        queue,
                        request,
                        ledger,
                        memory_budget.as_deref_mut(),
                    )?;
                    projection_guards.extend(guard);
                    waits.extend(wait);
                } else if self.memory.is_none() {
                    return Ok(None);
                }
            }
            InputTransitionCommand::Task(task) => {
                keys.extend(
                    task.command
                        .work_items
                        .iter()
                        .map(|work| WorkKey::new(WorkFamily::Work, &work.record().id)),
                );
                waits.extend(task.command.wait_conditions.iter().cloned());
                if let Some(queue) = &task.command.queue_entry {
                    let [message] = task.command.message_evidence.as_slice() else {
                        anyhow::bail!("Task trigger requires exactly one frozen Message");
                    };
                    let (wait, guard) = predicted_trigger_wait_tx(
                        tx,
                        scope,
                        message,
                        queue,
                        request,
                        ledger,
                        memory_budget.as_deref_mut(),
                    )?;
                    projection_guards.extend(guard);
                    waits.extend(wait);
                }
            }
            InputTransitionCommand::Work { command, .. } => {
                keys.insert(WorkKey::new(
                    WorkFamily::Work,
                    &command.mutation.record().id,
                ));
            }
            InputTransitionCommand::Focus { command, .. } => {
                keys.extend(
                    command
                        .work_items
                        .iter()
                        .map(|work| WorkKey::new(WorkFamily::Work, &work.record().id)),
                );
                keys.extend(
                    command
                        .continuations
                        .iter()
                        .map(|frame| WorkKey::new(WorkFamily::Frame, &frame.id)),
                );
                // Focus validates the whole active topology. Observe its old
                // qualified members before the reducer replaces any frame.
                if command.active_continuation_expectations.is_some() {
                    let mut query = tx.prepare("SELECT continuation_id FROM work_item_continuations WHERE agent_id=?1 AND state='active' ORDER BY continuation_id LIMIT 65537")?;
                    for id in query.query_map([&scope.agent_id], |row| row.get::<_, String>(0))? {
                        keys.insert(WorkKey::new(WorkFamily::Frame, id?));
                    }
                }
                waits.extend(command.wait_conditions.iter().cloned());
            }
            InputTransitionCommand::Wait { command, .. } => {
                keys.extend(
                    command
                        .work_items
                        .iter()
                        .map(|work| WorkKey::new(WorkFamily::Work, &work.record().id)),
                );
                waits.extend(command.wait_conditions.iter().cloned());
            }
            InputTransitionCommand::Queue(queue) => {
                if let Some(wait) = &queue.wait_transition {
                    waits.push(wait.record.clone());
                    keys.extend(
                        wait.work_item
                            .iter()
                            .map(|work| WorkKey::new(WorkFamily::Work, &work.record().id)),
                    );
                }
                if let Some(wait) = &queue.wait_registration {
                    keys.extend(
                        wait.work_items
                            .iter()
                            .map(|work| WorkKey::new(WorkFamily::Work, &work.record().id)),
                    );
                    waits.extend(wait.wait_conditions.iter().cloned());
                }
                if let Some(completion) = &queue.completion {
                    keys.extend(
                        completion
                            .work_items
                            .iter()
                            .map(|work| WorkKey::new(WorkFamily::Work, &work.record().id)),
                    );
                    keys.extend(
                        completion
                            .continuations
                            .iter()
                            .map(|frame| WorkKey::new(WorkFamily::Frame, &frame.id)),
                    );
                    waits.extend(completion.wait_conditions.iter().cloned());
                }
                waits.extend(queue.extra_wait_conditions.iter().cloned());
                if queue.command.operation == QueueOperation::Admit
                    && !queue.command.message_evidence.is_empty()
                {
                    let [message] = queue.command.message_evidence.as_slice() else {
                        anyhow::bail!(
                            "TriggerWaitAndEnqueue requires exactly one admitted message"
                        );
                    };
                    let (wait, guard) = predicted_trigger_wait_tx(
                        tx,
                        scope,
                        message,
                        queue_mutation_record(&queue.command.mutation),
                        request,
                        ledger,
                        memory_budget.as_deref_mut(),
                    )?;
                    projection_guards.extend(guard);
                    waits.extend(wait);
                }
            }
            InputTransitionCommand::Terminal(_)
                if matches!(
                    self.memory,
                    Some(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Finish { .. })
                ) => {}
            _ => return Ok(None),
        }
        keys.extend(crate::runtime_db::work_snapshot::work_wait_write_keys_tx(
            tx, &waits,
        )?);
        // Budget preparation observes this fresh view without changing alias or
        // coverage revision. Original proof base remains the published base.
        let mut proof = if cleanup {
            crate::runtime_db::work_snapshot::prepare_original_work_write_tx(tx, scope, &keys)?
        } else {
            crate::runtime_db::work_snapshot::prepare_work_write_tx(tx, scope, &keys)?
        };
        proof.retain_preparation_budget(projection_guards);
        // Wait material and Memory share one allowance before Work preflight.
        let proof_request = memory_budget
            .as_ref()
            .map(|budget| budget.work_request(request))
            .transpose()?
            .unwrap_or_else(|| request.clone());
        crate::runtime_db::work_snapshot::preflight_work_write_snapshot_tx(
            tx,
            &proof,
            &proof_request,
        )?
        .require_current()?;
        Ok(Some(proof))
    }

    fn explicit_frame_ids(&self) -> BTreeSet<String> {
        match self.command.as_ref() {
            InputTransitionCommand::Focus { command, .. } => command
                .continuations
                .iter()
                .map(|frame| frame.id.clone())
                .collect(),
            InputTransitionCommand::Queue(queue) => queue
                .completion
                .iter()
                .flat_map(|completion| {
                    completion
                        .continuations
                        .iter()
                        .map(|frame| frame.id.clone())
                })
                .collect(),
            _ => BTreeSet::new(),
        }
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
    Option<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
)> {
    use crate::runtime::input_evidence_budget::{InputMemoryCategory, OwnedBytesEstimate};
    use crate::runtime_db::work_snapshot::WorkSnapshotError;
    anyhow::ensure!(
        message.id == queue.message_id
            && message.agent_id == queue.agent_id
            && message.agent_id == scope.agent_id
            && queue.status == QueueEntryStatus::Queued,
        "TriggerWaitAndEnqueue admission identity differs"
    );
    let existing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE message_id=?1) OR EXISTS(SELECT 1 FROM queue_entries WHERE message_id=?1)", [&message.id], |row| row.get(0))?;
    // Lengths precede every typed Wait. The original reducer decodes the
    // same pool again; retain this view through that SQL and the Work proof.
    let (count, bytes): (usize, usize) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM wait_conditions WHERE agent_id=?1 AND status IN ('active','triggered')",
        [&message.agent_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let exhausted = || WorkSnapshotError::ResourceExhausted("TriggerWaitAndEnqueue projection");
    let copies = 4usize;
    let records = count.checked_mul(copies).context(exhausted())?;
    let payload_bytes = bytes.checked_mul(copies).context(exhausted())?;
    let metadata_bytes = records.checked_mul(256).context(exhausted())?;
    let total_bytes = payload_bytes
        .checked_add(metadata_bytes)
        .context(exhausted())?;
    anyhow::ensure!(
        records <= request.record_limit && total_bytes <= request.byte_limit,
        exhausted()
    );
    let guard = if let Some(budget) = memory_budget {
        budget.additional_material(bytes, count, copies)?;
        None
    } else if count > 0 {
        Some(ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: payload_bytes as u64,
                metadata_estimate_bytes: metadata_bytes as u64,
            },
        )?)
    } else {
        None
    };
    // The original reducer still materializes its pool on an existing admission.
    if existing {
        return Ok((None, guard));
    }
    let conditions = trigger_wait_conditions_tx(tx, &message.agent_id, Some(count))?;
    let crate::wake_contract::WaitTriggerSelection::Match { condition, .. } =
        crate::wake_contract::select_wait_to_trigger(message, &conditions)?
    else {
        return Ok((None, guard));
    };
    if condition.status != WaitConditionStatus::Active
        || !wait_generation_matches_tx(tx, message, condition)?
    {
        return Ok((None, guard));
    }
    let trigger_id = agent_message_reply_trigger_tx(tx, message)?.unwrap_or(&message.id);
    let claimed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM wait_conditions WHERE agent_id=?1 AND trigger_message_id=?2 AND wait_condition_id!=?3)", params![message.agent_id,trigger_id,condition.id], |row| row.get(0))?;
    if claimed {
        return Ok((None, guard));
    }
    crate::diagnostics::resource_io::admit_cloned_value(condition)?;
    let mut triggered = condition.clone();
    crate::diagnostics::resource_io::admit_cloned_value(&trigger_id)?;
    triggered.mark_triggered(trigger_id, queue.updated_at);
    Ok((Some(triggered), guard))
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
