//! Scheduler / WorkItem unified execution aggregate.
//!
//! Canonical production scheduling uses this aggregate as the sole execution
//! authority. Retired scheduler payloads remain only as private runtime
//! database migration wire types for already-published databases.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecutionSettlementConflict {
    #[error(
        "execution attempt {attempt_id} cannot settle message {message_id} as processed without a matching terminal Turn"
    )]
    MissingTerminalTurn {
        attempt_id: String,
        message_id: String,
        owner_work_item_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionProtocolState {
    pub agent_id: String,
    pub attempts: BTreeMap<String, ExecutionAttempt>,
    pub work_items: BTreeMap<String, WorkItemExecutionRecord>,
    pub outcomes: BTreeMap<String, ExecutionOutcomeRecord>,
}

pub fn resume_work_item_continuation(
    state: &ExecutionProtocolState,
    command: &ResumeWorkItemContinuation,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    if command.command_id.is_empty()
        || command.work_item_id.is_empty()
        || command.active_work_item_id.is_empty()
        || command.continuation_id.is_empty()
    {
        return Err("continuation resume requires complete identity".into());
    }
    if state.work_items.get(&command.work_item_id) != Some(&command.expected) {
        return Err("continuation resume WorkItem fence is stale".into());
    }
    if !matches!(
        &command.expected.state,
        WorkItemExecutionState::Paused { reason, .. }
            if reason == &format!("yielded_to:{}", command.active_work_item_id)
    ) {
        return Err("continuation resume requires the exact yielded WorkItem state".into());
    }
    if matches!(
        &command.outcome,
        WorkItemOutcome::Complete { .. } | WorkItemOutcome::Yield { .. }
    ) {
        return Err("continuation resume cannot complete or yield the resumed WorkItem".into());
    }
    if continuation_resume_outcome(&command.work_item_id, &command.source)? != command.outcome {
        return Err("continuation resume outcome does not match its source fence".into());
    }
    let mut next = state.clone();
    let record = next
        .work_items
        .get_mut(&command.work_item_id)
        .expect("continuation resume preserved WorkItem");
    record.state = plan_work_item_outcome(record.generation(), &command.outcome)?;
    assert_invariants(&next)?;
    Ok(ExecutionTransition {
        state: next,
        references: vec![
            format!("work_item:{}", command.work_item_id),
            format!("work_item:{}", command.active_work_item_id),
            format!("continuation:{}", command.continuation_id),
        ],
    })
}

pub fn suspend_work_item_continuation(
    state: &ExecutionProtocolState,
    command: &SuspendWorkItemContinuation,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    if command.command_id.is_empty()
        || command.work_item_id.is_empty()
        || command.active_work_item_id.is_empty()
        || command.continuation_id.is_empty()
    {
        return Err("continuation suspension requires complete identity".into());
    }
    if state.work_items.get(&command.work_item_id) != Some(&command.expected) {
        return Err("continuation suspension WorkItem fence is stale".into());
    }
    let WorkItemExecutionState::Runnable { generation, .. } = command.expected.state else {
        return Err("continuation suspension requires a Runnable WorkItem".into());
    };
    let next_generation = generation
        .checked_add(1)
        .ok_or_else(|| "WorkItem scheduling generation overflow".to_string())?;
    let mut next = state.clone();
    let record = next
        .work_items
        .get_mut(&command.work_item_id)
        .expect("continuation suspension preserved WorkItem");
    record.state = WorkItemExecutionState::Paused {
        generation: next_generation,
        reason: format!("yielded_to:{}", command.active_work_item_id),
    };
    assert_invariants(&next)?;
    Ok(ExecutionTransition {
        state: next,
        references: vec![
            format!("work_item:{}", command.work_item_id),
            format!("work_item:{}", command.active_work_item_id),
            format!("continuation:{}", command.continuation_id),
        ],
    })
}

pub fn reconcile_work_item_continuation_yield(
    state: &ExecutionProtocolState,
    command: &ReconcileWorkItemContinuationYield,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    if command.command_id.is_empty()
        || command.work_item_id.is_empty()
        || command.continuation_id.is_empty()
        || command.stale_active_work_item_id.is_empty()
        || command.active_work_item_id.is_empty()
        || command.expected_frame_updated_at.is_empty()
        || command.active_work_item_source_revision == 0
        || command.stale_active_work_item_source_revision == 0
        || command.work_item_id == command.stale_active_work_item_id
        || command.work_item_id == command.active_work_item_id
        || command.stale_active_work_item_id == command.active_work_item_id
    {
        return Err("continuation reconciliation requires complete distinct identity".into());
    }
    if state.work_items.get(&command.work_item_id) != Some(&command.expected) {
        return Err("continuation reconciliation WorkItem fence is stale".into());
    }
    let WorkItemExecutionState::Paused { generation, reason } = &command.expected.state else {
        return Err("continuation reconciliation requires a Paused WorkItem".into());
    };
    if reason != &format!("yielded_to:{}", command.stale_active_work_item_id) {
        return Err("continuation reconciliation requires the exact stale yielded target".into());
    }
    let next_generation = generation
        .checked_add(1)
        .ok_or_else(|| "WorkItem scheduling generation overflow".to_string())?;
    let mut next = state.clone();
    let record = next
        .work_items
        .get_mut(&command.work_item_id)
        .expect("continuation reconciliation preserved WorkItem");
    record.state = WorkItemExecutionState::Paused {
        generation: next_generation,
        reason: format!("yielded_to:{}", command.active_work_item_id),
    };
    assert_invariants(&next)?;
    Ok(ExecutionTransition {
        state: next,
        references: vec![
            format!("work_item:{}", command.work_item_id),
            format!("work_item:{}", command.stale_active_work_item_id),
            format!("work_item:{}", command.active_work_item_id),
            format!("continuation:{}", command.continuation_id),
        ],
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetWorkItemWaiting {
    pub command_id: String,
    pub work_item_id: String,
    pub expected: Option<WorkItemExecutionRecord>,
    pub record: WorkItemExecutionRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompleteWorkItemExecution {
    pub command_id: String,
    pub work_item_id: String,
    pub expected: WorkItemExecutionRecord,
    pub completion: String,
}

pub fn complete_work_item_execution(
    state: &ExecutionProtocolState,
    command: &CompleteWorkItemExecution,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    if command.command_id.is_empty()
        || command.work_item_id.is_empty()
        || command.completion.is_empty()
    {
        return Err("WorkItem completion requires complete identity".into());
    }
    if state.work_items.get(&command.work_item_id) != Some(&command.expected) {
        return Err("WorkItem completion fence is stale".into());
    }
    if matches!(
        command.expected.state,
        WorkItemExecutionState::InFlight { .. } | WorkItemExecutionState::Terminal { .. }
    ) {
        return Err("WorkItem is not eligible for lifecycle completion".into());
    }
    let mut next = state.clone();
    let record = next
        .work_items
        .get_mut(&command.work_item_id)
        .expect("WorkItem completion preserved WorkItem");
    record.state = plan_work_item_outcome(
        record.generation(),
        &WorkItemOutcome::Complete {
            completion: command.completion.clone(),
        },
    )?;
    assert_invariants(&next)?;
    Ok(ExecutionTransition {
        state: next,
        references: vec![
            format!("work_item:{}", command.work_item_id),
            format!("completion:{}", command.completion),
        ],
    })
}

impl ExecutionProtocolState {
    pub fn empty(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            attempts: BTreeMap::new(),
            work_items: BTreeMap::new(),
            outcomes: BTreeMap::new(),
        }
    }

    pub fn open_attempt(&self) -> Option<&ExecutionAttempt> {
        self.attempts
            .values()
            .find(|attempt| attempt.state == ExecutionAttemptState::Open)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionAttempt {
    pub attempt_id: String,
    pub agent_id: String,
    #[serde(default)]
    pub source_message_id: Option<String>,
    /// Selected before claiming the source; absent on legacy admissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_projection: Option<crate::IdleInputBatchItem>,
    pub source: ExecutionSource,
    pub binding: ExecutionBinding,
    pub provenance: ExecutionProvenance,
    pub admitted_fences: AdmittedFences,
    pub state: ExecutionAttemptState,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub recovery_of_attempt_id: Option<String>,
    #[serde(default)]
    pub terminal_outcome_id: Option<String>,
    pub admitted_at: String,
    #[serde(default)]
    pub terminal_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionAttemptState {
    Open,
    Settled,
    Interrupted,
    ProtocolViolation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionSource {
    pub identity: ExecutionSourceIdentity,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionSourceIdentity {
    QueueMessage {
        message_id: String,
    },
    InternalFollowup {
        message_id: String,
    },
    TaskResult {
        task_id: String,
        result_message_id: String,
    },
    ChildResult {
        child_agent_id: String,
        task_id: String,
        result_message_id: String,
    },
    TriggeredWait {
        wait_id: String,
        trigger_message_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ingress: Option<ExecutionWaitIngress>,
    },
    WorkItemContinuation {
        work_item_id: String,
    },
    TargetedContinuation {
        continuation_id: String,
        source_work_item_id: String,
        target_work_item_id: String,
    },
    RuntimeRecovery {
        recovery_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionWaitIngress {
    RuntimeOwnedInternalFollowup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionBinding {
    Conversation { interaction_id: String },
    WorkItem { work_item_id: String },
    AgentLifecycle { agent_id: String },
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionProvenance {
    pub origin: ExecutionOrigin,
    pub trust: ExecutionTrust,
    pub priority: ExecutionPriority,
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub causation_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOrigin {
    Operator,
    Channel,
    Webhook,
    Callback,
    Timer,
    System,
    Task,
    RuntimeRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTrust {
    OperatorInstruction,
    RuntimeInstruction,
    IntegrationSignal,
    ExternalEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPriority {
    Background,
    Normal,
    Next,
    Interject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedFences {
    pub source_revision: u64,
    #[serde(default)]
    pub work_item_source_revision: Option<u64>,
    #[serde(default)]
    pub work_item_generation: Option<u64>,
    #[serde(default)]
    pub rejoin: Option<RejoinFence>,
    pub agent_control_revision: u64,
    pub host_registry_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejoinFence {
    pub obligation_id: String,
    pub generation: u64,
    #[serde(flatten)]
    pub source: TaskRejoinSource,
}

/// Creation provenance is independent of the future result execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum TaskRejoinSource {
    ParentTurn { parent_turn_id: String },
    CommandCreation { command_creation_id: String },
}

impl<'de> Deserialize<'de> for TaskRejoinSource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        fn present_identity<'de, D: serde::Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<String>, D::Error> {
            String::deserialize(deserializer).map(Some)
        }

        #[derive(Deserialize)]
        struct Source {
            #[serde(default, deserialize_with = "present_identity")]
            parent_turn_id: Option<String>,
            #[serde(default, deserialize_with = "present_identity")]
            command_creation_id: Option<String>,
        }
        let source = Source::deserialize(deserializer)?;
        match (source.parent_turn_id, source.command_creation_id) {
            (Some(parent_turn_id), None) if !parent_turn_id.is_empty() => {
                Ok(Self::ParentTurn { parent_turn_id })
            }
            (None, Some(command_creation_id)) if !command_creation_id.is_empty() => {
                Ok(Self::CommandCreation {
                    command_creation_id,
                })
            }
            _ => Err(serde::de::Error::custom(
                "rejoin requires exactly one creation source",
            )),
        }
    }
}

impl TaskRejoinSource {
    pub fn parent_turn_id(&self) -> Option<&str> {
        match self {
            Self::ParentTurn { parent_turn_id } => Some(parent_turn_id),
            Self::CommandCreation { .. } => None,
        }
    }

    pub fn command_creation_id(&self) -> Option<&str> {
        match self {
            Self::CommandCreation {
                command_creation_id,
            } => Some(command_creation_id),
            Self::ParentTurn { .. } => None,
        }
    }

    pub fn validates_identity(&self, task_id: &str) -> bool {
        match self {
            Self::ParentTurn { parent_turn_id } => !parent_turn_id.is_empty(),
            Self::CommandCreation {
                command_creation_id,
            } => !task_id.is_empty() && command_creation_id == task_id,
        }
    }

    pub fn result_source_ref(&self) -> (&'static str, &str) {
        match self {
            Self::ParentTurn { parent_turn_id } => ("task_result_parent_turn_id", parent_turn_id),
            Self::CommandCreation {
                command_creation_id,
            } => ("task_result_command_creation_id", command_creation_id),
        }
    }

    pub fn matches_result_source_refs(
        &self,
        refs: &std::collections::BTreeMap<String, String>,
    ) -> bool {
        let (key, id) = self.result_source_ref();
        let other = if self.parent_turn_id().is_some() {
            "task_result_command_creation_id"
        } else {
            "task_result_parent_turn_id"
        };
        refs.get(key).is_some_and(|value| value == id) && !refs.contains_key(other)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemExecutionRecord {
    pub source_revision: u64,
    pub state: WorkItemExecutionState,
}

impl WorkItemExecutionRecord {
    pub fn generation(&self) -> u64 {
        self.state.generation()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkItemExecutionState {
    Runnable {
        generation: u64,
        #[serde(default)]
        recovery_ref: Option<String>,
    },
    InFlight {
        generation: u64,
        attempt_id: String,
    },
    Waiting {
        generation: u64,
        wait: WaitReference,
    },
    Paused {
        generation: u64,
        reason: String,
    },
    NeedsRepair {
        generation: u64,
        repair_id: String,
    },
    Terminal {
        generation: u64,
        completion: String,
    },
}

impl WorkItemExecutionState {
    pub fn generation(&self) -> u64 {
        match self {
            Self::Runnable { generation, .. }
            | Self::InFlight { generation, .. }
            | Self::Waiting { generation, .. }
            | Self::Paused { generation, .. }
            | Self::NeedsRepair { generation, .. }
            | Self::Terminal { generation, .. } => *generation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitReference {
    pub wait_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "owner", content = "outcome", rename_all = "snake_case")]
pub enum ExecutionOutcome {
    Conversation(ConversationOutcome),
    WorkItem(WorkItemOutcome),
    Command(CommandResult),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConversationOutcome {
    Replied,
    HandoffToWorkItemContinue {
        work_item_id: String,
    },
    Wait {
        wait: WaitReference,
    },
    HandoffToWorkItemWait {
        work_item_id: String,
        wait: WaitReference,
    },
    Paused {
        reason: String,
    },
    Interrupted {
        reason: String,
    },
    Failed {
        policy: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkItemOutcome {
    Continue,
    Wait { wait: WaitReference },
    Complete { completion: String },
    Pause { reason: String },
    Yield { target_work_item_id: String },
    Failed { policy: String },
    Interrupted { reason: String },
    NeedsRepair { repair_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandResult {
    Applied { references: Vec<String> },
    Unsupported { reason: String },
    Quarantined { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionOutcomeRecord {
    pub outcome_id: String,
    pub attempt_id: String,
    pub outcome: ExecutionOutcome,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmitExecution {
    pub attempt: ExecutionAttempt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettleExecution {
    pub outcome: ExecutionOutcomeRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterWorkItemExecution {
    pub work_item_id: String,
    pub record: WorkItemExecutionRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvanceWorkItemSourceRevision {
    pub command_id: String,
    pub work_item_id: String,
    pub expected_source_revision: u64,
    pub source_revision: u64,
}

/// #3816 controlled recovery for a registration stranded behind its durable
/// revision. Unlike [`AdvanceWorkItemSourceRevision`], this transition is a
/// repair: the durable revision is already at `source_revision` and the
/// caller must have verified the per-revision attribution evidence before
/// issuing it. It never rewrites admitted fences, generations, wait state,
/// or attempt history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileWorkItemSourceRevision {
    pub command_id: String,
    pub work_item_id: String,
    pub expected_source_revision: u64,
    pub source_revision: u64,
    pub evidence_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetWorkItemReadiness {
    pub command_id: String,
    pub work_item_id: String,
    pub expected: WorkItemExecutionRecord,
    pub record: WorkItemExecutionRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudgetStop {
    pub source_revision: u64,
    pub source_turn_id: String,
    pub configured_estimated_tokens: usize,
}

impl ContextBudgetStop {
    const PREFIX: &str = "context_pinned_minimum_over_budget:";

    pub fn policy(&self) -> Result<String, serde_json::Error> {
        Ok(format!("{}{}", Self::PREFIX, serde_json::to_string(self)?))
    }

    pub fn from_policy(policy: &str) -> Option<Self> {
        serde_json::from_str(policy.strip_prefix(Self::PREFIX)?).ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecheckContextBudgetStop {
    pub command_id: String,
    pub work_item_id: String,
    pub expected: WorkItemExecutionRecord,
    pub configured_estimated_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeWorkItemContinuation {
    pub command_id: String,
    pub work_item_id: String,
    pub active_work_item_id: String,
    pub continuation_id: String,
    pub expected: WorkItemExecutionRecord,
    pub source: WorkItemContinuationResumeSource,
    pub outcome: WorkItemOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemContinuationResumeSource {
    pub work_item_revision: u64,
    pub blocked_by: Option<String>,
    pub active_wait_ids: Vec<String>,
}

pub fn continuation_resume_outcome(
    work_item_id: &str,
    source: &WorkItemContinuationResumeSource,
) -> Result<WorkItemOutcome, String> {
    if work_item_id.is_empty()
        || source.work_item_revision == 0
        || source.active_wait_ids.iter().any(String::is_empty)
        || source
            .active_wait_ids
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
    {
        return Err("continuation resume source fence is invalid".into());
    }
    Ok(match source.active_wait_ids.as_slice() {
        [wait_id] => WorkItemOutcome::Wait {
            wait: WaitReference {
                wait_id: wait_id.clone(),
            },
        },
        [] if source.blocked_by.is_some() => WorkItemOutcome::Pause {
            reason: source
                .blocked_by
                .clone()
                .expect("continuation resume blocker checked above"),
        },
        [] => WorkItemOutcome::Continue,
        _ => WorkItemOutcome::NeedsRepair {
            repair_id: format!("work_item_waits_ambiguous:{work_item_id}"),
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspendWorkItemContinuation {
    pub command_id: String,
    pub work_item_id: String,
    pub active_work_item_id: String,
    pub continuation_id: String,
    pub expected: WorkItemExecutionRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileWorkItemContinuationYield {
    pub command_id: String,
    pub work_item_id: String,
    pub continuation_id: String,
    pub stale_active_work_item_id: String,
    pub active_work_item_id: String,
    pub expected: WorkItemExecutionRecord,
    pub expected_frame_updated_at: String,
    pub active_work_item_source_revision: u64,
    pub stale_active_work_item_source_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterruptExecution {
    pub attempt_id: String,
    pub outcome_id: String,
    pub reason: String,
    pub interrupted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalizeInterruptedExecution {
    pub command_id: String,
    pub attempt_id: String,
    pub reason: String,
    pub finalized_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoverInterruptedTaskResultClaim {
    pub command_id: String,
    pub attempt_id: String,
    pub outcome_id: String,
    pub work_item_id: String,
    pub task_id: String,
    pub result_message_id: String,
    pub wait_id: String,
    pub rejoin: RejoinFence,
    pub expected_source_revision: u64,
    pub source_revision: u64,
    pub interrupted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoverUnadvancedTaskResultClaim {
    pub command_id: String,
    pub attempt_id: String,
    pub outcome_id: String,
    pub work_item_id: String,
    pub task_id: String,
    pub result_message_id: String,
    pub wait_id: String,
    pub rejoin: RejoinFence,
    pub expected_source_revision: u64,
    pub interrupted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", content = "payload", rename_all = "snake_case")]
pub enum ExecutionProtocolCommand {
    RegisterWorkItem(Box<RegisterWorkItemExecution>),
    AdvanceWorkItemSourceRevision(AdvanceWorkItemSourceRevision),
    ReconcileWorkItemSourceRevision(ReconcileWorkItemSourceRevision),
    SetWorkItemReadiness(Box<SetWorkItemReadiness>),
    RecheckContextBudgetStop(Box<RecheckContextBudgetStop>),
    SuspendWorkItemContinuation(Box<SuspendWorkItemContinuation>),
    ResumeWorkItemContinuation(Box<ResumeWorkItemContinuation>),
    ReconcileWorkItemContinuationYield(Box<ReconcileWorkItemContinuationYield>),
    SetWorkItemWaiting(Box<SetWorkItemWaiting>),
    CompleteWorkItem(Box<CompleteWorkItemExecution>),
    Admit(Box<AdmitExecution>),
    Settle(SettleExecution),
    Interrupt(InterruptExecution),
    FinalizeInterrupted(FinalizeInterruptedExecution),
    RecoverInterruptedTaskResultClaim(Box<RecoverInterruptedTaskResultClaim>),
    RecoverUnadvancedTaskResultClaim(Box<RecoverUnadvancedTaskResultClaim>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionTransition {
    pub state: ExecutionProtocolState,
    pub references: Vec<String>,
}

pub fn set_work_item_waiting(
    state: &ExecutionProtocolState,
    command: &SetWorkItemWaiting,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    if command.command_id.is_empty()
        || command.work_item_id.is_empty()
        || command.record.source_revision == 0
        || command.record.generation() == 0
        || !matches!(command.record.state, WorkItemExecutionState::Waiting { .. })
    {
        return Err("WorkItem waiting transition requires identity and Waiting state".into());
    }
    if state.work_items.get(&command.work_item_id) != command.expected.as_ref() {
        return Err("WorkItem waiting transition fence is stale".into());
    }
    if command.expected.as_ref().is_some_and(|record| {
        matches!(
            record.state,
            WorkItemExecutionState::InFlight { .. } | WorkItemExecutionState::Terminal { .. }
        ) || command.record.generation() <= record.generation()
    }) {
        return Err("WorkItem is not eligible for lifecycle wait handoff".into());
    }
    let mut next = state.clone();
    next.work_items
        .insert(command.work_item_id.clone(), command.record.clone());
    assert_invariants(&next)?;
    Ok(ExecutionTransition {
        state: next,
        references: vec![format!("work_item:{}", command.work_item_id)],
    })
}

pub fn advance_work_item_source_revision(
    state: &ExecutionProtocolState,
    command: &AdvanceWorkItemSourceRevision,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    if command.command_id.is_empty()
        || command.work_item_id.is_empty()
        || command.expected_source_revision == 0
        || command.source_revision <= command.expected_source_revision
    {
        return Err(
            "WorkItem source revision advance requires identity and a monotonic revision".into(),
        );
    }
    let mut next = state.clone();
    let record = next
        .work_items
        .get_mut(&command.work_item_id)
        .ok_or_else(|| "WorkItem source revision advance requires registered state".to_string())?;
    if record.source_revision != command.expected_source_revision {
        return Err("WorkItem source revision advance fence is stale".into());
    }
    if matches!(record.state, WorkItemExecutionState::Terminal { .. }) {
        return Err("terminal WorkItem source revision cannot advance".into());
    }
    record.source_revision = command.source_revision;
    assert_invariants(&next)?;
    Ok(ExecutionTransition {
        state: next,
        references: vec![format!("work_item:{}", command.work_item_id)],
    })
}

pub fn reconcile_work_item_source_revision(
    state: &ExecutionProtocolState,
    command: &ReconcileWorkItemSourceRevision,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    if command.command_id.is_empty()
        || command.work_item_id.is_empty()
        || command.evidence_fingerprint.is_empty()
        || command.expected_source_revision == 0
        || command.source_revision <= command.expected_source_revision
    {
        return Err(
            "WorkItem source revision reconcile requires identity, evidence, and a \
             durable-ahead revision range"
                .into(),
        );
    }
    let mut next = state.clone();
    let record = next
        .work_items
        .get_mut(&command.work_item_id)
        .ok_or_else(|| {
            "WorkItem source revision reconcile requires registered state".to_string()
        })?;
    if record.source_revision != command.expected_source_revision {
        return Err("WorkItem source revision reconcile fence is stale".into());
    }
    if matches!(
        record.state,
        WorkItemExecutionState::Terminal { .. } | WorkItemExecutionState::InFlight { .. }
    ) {
        return Err(
            "WorkItem source revision reconcile requires a non-executing registration".into(),
        );
    }
    record.source_revision = command.source_revision;
    assert_invariants(&next)?;
    Ok(ExecutionTransition {
        state: next,
        references: vec![format!("work_item:{}", command.work_item_id)],
    })
}

pub fn recheck_context_budget_stop(
    state: &ExecutionProtocolState,
    command: &RecheckContextBudgetStop,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    if command.command_id.is_empty()
        || command.work_item_id.is_empty()
        || state.work_items.get(&command.work_item_id) != Some(&command.expected)
    {
        return Err("context budget recheck identity or WorkItem fence is stale".into());
    }
    let WorkItemExecutionState::Paused { reason, .. } = &command.expected.state else {
        return Err("context budget recheck requires a paused WorkItem".into());
    };
    let stop = ContextBudgetStop::from_policy(reason)
        .ok_or_else(|| "context budget recheck requires a typed budget stop".to_string())?;
    if stop.source_revision == command.expected.source_revision
        && stop.configured_estimated_tokens == command.configured_estimated_tokens
    {
        return Err("context budget recheck requires a changed input or budget".into());
    }
    let generation = command
        .expected
        .generation()
        .checked_add(1)
        .ok_or_else(|| "WorkItem scheduling generation overflow".to_string())?;
    let mut next = state.clone();
    next.work_items.insert(
        command.work_item_id.clone(),
        WorkItemExecutionRecord {
            source_revision: command.expected.source_revision,
            state: WorkItemExecutionState::Runnable {
                generation,
                recovery_ref: None,
            },
        },
    );
    assert_invariants(&next)?;
    Ok(ExecutionTransition {
        state: next,
        references: vec![format!("work_item:{}", command.work_item_id)],
    })
}

pub fn set_work_item_readiness(
    state: &ExecutionProtocolState,
    command: &SetWorkItemReadiness,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    if command.command_id.is_empty()
        || command.work_item_id.is_empty()
        || command.expected.source_revision == 0
        || command.record.source_revision < command.expected.source_revision
    {
        return Err(
            "WorkItem readiness transition requires identity and monotonic revision".into(),
        );
    }
    if state.work_items.get(&command.work_item_id) != Some(&command.expected) {
        return Err("WorkItem readiness transition fence is stale".into());
    }
    let generation = command.expected.generation();
    let valid = match (&command.expected.state, &command.record.state) {
        (
            WorkItemExecutionState::Runnable { .. },
            WorkItemExecutionState::Runnable {
                generation: next, ..
            },
        )
        | (
            WorkItemExecutionState::Paused { .. },
            WorkItemExecutionState::Paused {
                generation: next, ..
            },
        ) => {
            *next == generation && command.record.source_revision > command.expected.source_revision
        }
        (
            WorkItemExecutionState::Runnable { .. },
            WorkItemExecutionState::Paused {
                generation: next, ..
            },
        )
        | (
            WorkItemExecutionState::Paused { .. },
            WorkItemExecutionState::Runnable {
                generation: next, ..
            },
        )
        | (
            WorkItemExecutionState::Waiting { .. },
            WorkItemExecutionState::Runnable {
                generation: next, ..
            },
        ) => {
            *next
                == generation
                    .checked_add(1)
                    .ok_or_else(|| "WorkItem scheduling generation overflow".to_string())?
        }
        _ => false,
    };
    if !valid {
        return Err("WorkItem readiness transition is not allowed".into());
    }
    let mut next = state.clone();
    next.work_items
        .insert(command.work_item_id.clone(), command.record.clone());
    assert_invariants(&next)?;
    Ok(ExecutionTransition {
        state: next,
        references: vec![format!("work_item:{}", command.work_item_id)],
    })
}

pub fn register_work_item_execution(
    state: &ExecutionProtocolState,
    command: &RegisterWorkItemExecution,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    if command.work_item_id.is_empty()
        || command.record.source_revision == 0
        || command.record.generation() == 0
    {
        return Err(
            "WorkItem registration requires identity, source revision, and generation".into(),
        );
    }
    if matches!(
        command.record.state,
        WorkItemExecutionState::InFlight { .. }
    ) {
        return Err("WorkItem registration cannot import an InFlight state".into());
    }
    if let Some(existing) = state.work_items.get(&command.work_item_id) {
        if existing == &command.record {
            return Ok(ExecutionTransition {
                state: state.clone(),
                references: vec![format!("work_item:{}", command.work_item_id)],
            });
        }
        return Err("WorkItem execution identity already exists with different state".into());
    }
    let mut next = state.clone();
    next.work_items
        .insert(command.work_item_id.clone(), command.record.clone());
    assert_invariants(&next)?;
    Ok(ExecutionTransition {
        state: next,
        references: vec![format!("work_item:{}", command.work_item_id)],
    })
}

#[doc(hidden)]
pub fn validate_source_binding(
    source: &ExecutionSource,
    binding: &ExecutionBinding,
    fences: &AdmittedFences,
) -> Result<(), String> {
    use ExecutionBinding::*;
    use ExecutionSourceIdentity::*;
    let allowed = match (&source.identity, binding) {
        (
            QueueMessage { .. } | InternalFollowup { .. },
            Conversation { .. } | WorkItem { .. } | AgentLifecycle { .. },
        ) => true,
        (
            RuntimeRecovery { .. },
            WorkItem { .. } | Command | Conversation { .. } | AgentLifecycle { .. },
        ) => true,
        (TaskResult { .. }, Conversation { .. } | WorkItem { .. } | Command) => true,
        (ChildResult { .. }, Conversation { .. } | WorkItem { .. } | Command) => true,
        (TriggeredWait { .. }, Conversation { .. } | WorkItem { .. } | AgentLifecycle { .. }) => {
            true
        }
        (
            WorkItemContinuation {
                work_item_id: source_work_item_id,
            },
            WorkItem { work_item_id },
        ) => source_work_item_id == work_item_id,
        (
            TargetedContinuation {
                target_work_item_id,
                ..
            },
            WorkItem { work_item_id },
        ) => target_work_item_id == work_item_id,
        (TargetedContinuation { .. }, Conversation { .. } | Command) => true,
        _ => false,
    };
    if !allowed {
        return Err(format!(
            "unsupported source-binding combination: {:?} → {:?}",
            source.identity, binding
        ));
    }
    match &source.identity {
        TaskResult { .. } | ChildResult { .. } => validate_rejoin_fence(fences.rejoin.as_ref())?,
        _ if fences.rejoin.is_some() => {
            return Err("only task or child results may carry a rejoin fence".into());
        }
        _ => {}
    }
    Ok(())
}

fn validate_rejoin_fence(fence: Option<&RejoinFence>) -> Result<(), String> {
    let fence =
        fence.ok_or_else(|| "task or child result requires a live rejoin fence".to_string())?;
    if fence.obligation_id.is_empty()
        || !fence.source.validates_identity(&fence.obligation_id)
        || fence.generation == 0
    {
        return Err("rejoin fence requires identity, creation source, and generation".into());
    }
    Ok(())
}

#[doc(hidden)]
pub fn validate_provenance(attempt: &ExecutionAttempt) -> Result<(), String> {
    use ExecutionOrigin::*;
    use ExecutionTrust::*;
    let provenance = &attempt.provenance;
    let valid = match provenance.origin {
        // An authenticated operator ingress establishes activation authority,
        // while the message's content trust remains independently classified.
        Operator => matches!(
            provenance.trust,
            OperatorInstruction | IntegrationSignal | ExternalEvidence
        ),
        Channel | Webhook => matches!(provenance.trust, IntegrationSignal | ExternalEvidence),
        Callback => matches!(
            provenance.trust,
            RuntimeInstruction | IntegrationSignal | ExternalEvidence
        ),
        Timer | System => matches!(provenance.trust, RuntimeInstruction | IntegrationSignal),
        Task | RuntimeRecovery => provenance.trust == RuntimeInstruction,
    };
    // Runtime-owned followups retain their ingress identity through exact waits,
    // preserving, never upgrading, the original message trust.
    let runtime_owned_followup = matches!(
        (
            &attempt.source.identity,
            &attempt.binding,
            provenance.origin
        ),
        (
            ExecutionSourceIdentity::InternalFollowup { .. }
                | ExecutionSourceIdentity::TriggeredWait {
                    ingress: Some(ExecutionWaitIngress::RuntimeOwnedInternalFollowup),
                    ..
                },
            ExecutionBinding::WorkItem { .. } | ExecutionBinding::AgentLifecycle { .. },
            System | Task
        )
    );
    if !valid && !runtime_owned_followup {
        return Err("execution provenance origin and trust are incompatible".into());
    }
    Ok(())
}

pub fn admit_execution(
    state: &ExecutionProtocolState,
    command: &AdmitExecution,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    let attempt = &command.attempt;
    validate_source_binding(&attempt.source, &attempt.binding, &attempt.admitted_fences)?;
    validate_provenance(attempt)?;
    if attempt.input_projection.as_ref().is_some_and(|projection| {
        attempt.source_message_id.as_deref() != Some(projection.message_id.as_str())
    }) {
        return Err("input projection must belong to the admitted source message".into());
    }

    if attempt.agent_id != state.agent_id {
        return Err("attempt agent does not match execution partition".into());
    }
    if attempt.source.generation == 0
        || attempt.source.generation != attempt.admitted_fences.source_revision
        || attempt.admitted_fences.agent_control_revision == 0
        || attempt.admitted_fences.host_registry_revision == 0
    {
        return Err("admission source and control fences must match nonzero revisions".into());
    }
    if let ExecutionBinding::AgentLifecycle { agent_id } = &attempt.binding {
        if agent_id != &attempt.agent_id {
            return Err("agent lifecycle binding does not match attempt partition".into());
        }
    }
    if attempt.state != ExecutionAttemptState::Open
        || attempt.terminal_outcome_id.is_some()
        || attempt.terminal_at.is_some()
    {
        return Err("admission requires a new open attempt".into());
    }
    if state.attempts.contains_key(&attempt.attempt_id) {
        return Err("attempt identity already exists".into());
    }
    if state.open_attempt().is_some() {
        return Err("agent already owns an open execution attempt".into());
    }
    if let Some(recovery_of) = &attempt.recovery_of_attempt_id {
        let prior = state
            .attempts
            .get(recovery_of)
            .ok_or_else(|| "recovery attempt references an unknown prior attempt".to_string())?;
        if prior.state == ExecutionAttemptState::Open {
            return Err("recovery attempt cannot reference an open attempt".into());
        }
        if matches!(
            attempt.source.identity,
            ExecutionSourceIdentity::RuntimeRecovery { .. }
        ) && (prior.agent_id != attempt.agent_id || prior.binding != attempt.binding)
        {
            return Err("runtime recovery must inherit its predecessor binding".into());
        }
    } else if matches!(
        attempt.source.identity,
        ExecutionSourceIdentity::RuntimeRecovery { .. }
    ) && matches!(
        attempt.binding,
        ExecutionBinding::Conversation { .. } | ExecutionBinding::AgentLifecycle { .. }
    ) {
        return Err("unbound runtime recovery requires a validated predecessor".into());
    }

    let mut next = state.clone();
    if let ExecutionBinding::WorkItem { work_item_id } = &attempt.binding {
        let expected_source_revision = attempt
            .admitted_fences
            .work_item_source_revision
            .ok_or_else(|| "WorkItem admission requires a source revision fence".to_string())?;
        let expected_generation = attempt
            .admitted_fences
            .work_item_generation
            .ok_or_else(|| "WorkItem admission requires a generation fence".to_string())?;
        let work_record = next
            .work_items
            .get_mut(work_item_id)
            .ok_or_else(|| "WorkItem admission requires registered execution state".to_string())?;
        if work_record.source_revision != expected_source_revision {
            return Err("WorkItem admission source revision fence is stale".into());
        }
        match &mut work_record.state {
            WorkItemExecutionState::Runnable { generation, .. }
            | WorkItemExecutionState::Waiting { generation, .. }
                if *generation == expected_generation =>
            {
                work_record.state = WorkItemExecutionState::InFlight {
                    generation: expected_generation,
                    attempt_id: attempt.attempt_id.clone(),
                };
            }
            _ => return Err("WorkItem is not eligible at the admitted generation".into()),
        }
    } else if attempt.admitted_fences.work_item_source_revision.is_some()
        || attempt.admitted_fences.work_item_generation.is_some()
    {
        return Err("non-WorkItem admission cannot carry WorkItem fences".into());
    }

    next.attempts
        .insert(attempt.attempt_id.clone(), attempt.clone());
    assert_invariants(&next)?;
    Ok(ExecutionTransition {
        state: next,
        references: vec![format!("attempt:{}", attempt.attempt_id)],
    })
}

pub fn settle_execution(
    state: &ExecutionProtocolState,
    command: &SettleExecution,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    let outcome = &command.outcome;
    if state.outcomes.contains_key(&outcome.outcome_id) {
        return Err("outcome identity already exists".into());
    }
    let mut next = state.clone();
    let attempt = next
        .attempts
        .get_mut(&outcome.attempt_id)
        .ok_or_else(|| "settlement references an unknown attempt".to_string())?;
    if attempt.state != ExecutionAttemptState::Open {
        return Err("only an open attempt may settle".into());
    }
    validate_outcome_binding(&attempt.binding, &outcome.outcome)?;

    if let ExecutionBinding::WorkItem { work_item_id } = &attempt.binding {
        let work_record = next
            .work_items
            .get_mut(work_item_id)
            .ok_or_else(|| "attempt references an unknown WorkItem state".to_string())?;
        let generation = match &work_record.state {
            WorkItemExecutionState::InFlight {
                generation,
                attempt_id,
            } if attempt_id == &attempt.attempt_id => *generation,
            _ => return Err("WorkItem does not reference the settling attempt".into()),
        };
        let ExecutionOutcome::WorkItem(work_item_outcome) = &outcome.outcome else {
            unreachable!("WorkItem binding validated above");
        };
        work_record.state = plan_work_item_outcome(generation, work_item_outcome)?;
    }

    attempt.state = ExecutionAttemptState::Settled;
    attempt.terminal_outcome_id = Some(outcome.outcome_id.clone());
    attempt.terminal_at = Some(outcome.created_at.clone());
    next.outcomes
        .insert(outcome.outcome_id.clone(), outcome.clone());
    assert_invariants(&next)?;
    Ok(ExecutionTransition {
        state: next,
        references: vec![
            format!("attempt:{}", outcome.attempt_id),
            format!("outcome:{}", outcome.outcome_id),
        ],
    })
}

pub fn interrupt_execution(
    state: &ExecutionProtocolState,
    command: &InterruptExecution,
) -> Result<ExecutionTransition, String> {
    let outcome = ExecutionOutcomeRecord {
        outcome_id: command.outcome_id.clone(),
        attempt_id: command.attempt_id.clone(),
        outcome: match state
            .attempts
            .get(&command.attempt_id)
            .map(|attempt| &attempt.binding)
        {
            Some(ExecutionBinding::WorkItem { .. }) => {
                ExecutionOutcome::WorkItem(WorkItemOutcome::Interrupted {
                    reason: command.reason.clone(),
                })
            }
            Some(ExecutionBinding::Conversation { .. })
            | Some(ExecutionBinding::AgentLifecycle { .. }) => {
                ExecutionOutcome::Conversation(ConversationOutcome::Interrupted {
                    reason: command.reason.clone(),
                })
            }
            Some(ExecutionBinding::Command) => ExecutionOutcome::Command(CommandResult::Applied {
                references: vec![format!("interrupted:{}", command.attempt_id)],
            }),
            None => return Err("interruption references an unknown attempt".into()),
        },
        created_at: command.interrupted_at.clone(),
    };
    let mut transition = settle_execution(state, &SettleExecution { outcome })?;
    let attempt = transition
        .state
        .attempts
        .get_mut(&command.attempt_id)
        .expect("settlement preserved attempt");
    attempt.state = ExecutionAttemptState::Interrupted;
    assert_invariants(&transition.state)?;
    Ok(transition)
}

/// Release an interrupted attempt's replay anchor without rewriting its
/// outcome history. Used when the attached turn already reached a different
/// terminal state, so replay-anchored admission can never succeed again.
pub fn finalize_interrupted_execution(
    state: &ExecutionProtocolState,
    command: &FinalizeInterruptedExecution,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    if command.command_id.is_empty()
        || command.attempt_id.is_empty()
        || command.reason.is_empty()
        || command.finalized_at.is_empty()
    {
        return Err("interrupted execution finalization requires complete identity".into());
    }
    let mut next = state.clone();
    let attempt = next.attempts.get_mut(&command.attempt_id).ok_or_else(|| {
        "interrupted execution finalization references an unknown attempt".to_string()
    })?;
    if attempt.state != ExecutionAttemptState::Interrupted {
        return Err("only an interrupted attempt may finalize".into());
    }
    // Terminal outcome evidence stays as recorded at interruption time.
    attempt.state = ExecutionAttemptState::Settled;
    assert_invariants(&next)?;
    Ok(ExecutionTransition {
        state: next,
        references: vec![
            format!("attempt:{}", command.attempt_id),
            format!("finalized:{}", command.command_id),
        ],
    })
}

pub fn recover_interrupted_task_result_claim(
    state: &ExecutionProtocolState,
    command: &RecoverInterruptedTaskResultClaim,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    if command.command_id.is_empty()
        || command.attempt_id.is_empty()
        || command.outcome_id.is_empty()
        || command.work_item_id.is_empty()
        || command.task_id.is_empty()
        || command.result_message_id.is_empty()
        || command.wait_id.is_empty()
        || command.expected_source_revision == 0
        || command.source_revision <= command.expected_source_revision
    {
        return Err("TaskResult claim recovery requires complete monotonic identity".into());
    }
    let attempt = state
        .attempts
        .get(&command.attempt_id)
        .ok_or_else(|| "TaskResult claim recovery references an unknown attempt".to_string())?;
    if attempt.state != ExecutionAttemptState::Open
        || attempt.source_message_id.as_deref() != Some(command.result_message_id.as_str())
        || attempt.source.identity
            != (ExecutionSourceIdentity::TaskResult {
                task_id: command.task_id.clone(),
                result_message_id: command.result_message_id.clone(),
            })
        || attempt.binding
            != (ExecutionBinding::WorkItem {
                work_item_id: command.work_item_id.clone(),
            })
        || attempt.admitted_fences.work_item_source_revision
            != Some(command.expected_source_revision)
        || attempt.admitted_fences.rejoin.as_ref() != Some(&command.rejoin)
    {
        return Err("TaskResult claim recovery attempt fence is stale".into());
    }
    let work = state
        .work_items
        .get(&command.work_item_id)
        .ok_or_else(|| "TaskResult claim recovery WorkItem authority is missing".to_string())?;
    if !matches!(
        work.source_revision,
        revision
            if revision == command.expected_source_revision
                || revision == command.source_revision
    ) || !matches!(
        &work.state,
        WorkItemExecutionState::InFlight { attempt_id, generation }
            if attempt_id == &command.attempt_id
                && Some(*generation) == attempt.admitted_fences.work_item_generation
    ) {
        return Err("TaskResult claim recovery WorkItem fence is stale".into());
    }
    let mut transition = interrupt_execution(
        state,
        &InterruptExecution {
            attempt_id: command.attempt_id.clone(),
            outcome_id: command.outcome_id.clone(),
            reason: "runtime_interrupted".into(),
            interrupted_at: command.interrupted_at.clone(),
        },
    )?;
    let recovered_work = transition
        .state
        .work_items
        .get_mut(&command.work_item_id)
        .expect("interruption preserved WorkItem authority");
    recovered_work.source_revision = command.source_revision;
    recovered_work.state = WorkItemExecutionState::Waiting {
        generation: recovered_work.generation(),
        wait: WaitReference {
            wait_id: command.wait_id.clone(),
        },
    };
    assert_invariants(&transition.state)?;
    transition.references.extend([
        format!("task:{}", command.task_id),
        format!("message:{}", command.result_message_id),
        format!("wait:{}", command.wait_id),
        format!("work_item:{}", command.work_item_id),
    ]);
    Ok(transition)
}

pub fn recover_unadvanced_task_result_claim(
    state: &ExecutionProtocolState,
    command: &RecoverUnadvancedTaskResultClaim,
) -> Result<ExecutionTransition, String> {
    assert_invariants(state)?;
    if command.command_id.is_empty()
        || command.attempt_id.is_empty()
        || command.outcome_id.is_empty()
        || command.work_item_id.is_empty()
        || command.task_id.is_empty()
        || command.result_message_id.is_empty()
        || command.wait_id.is_empty()
        || command.expected_source_revision == 0
    {
        return Err("Unadvanced TaskResult claim recovery requires complete identity".into());
    }
    let attempt = state.attempts.get(&command.attempt_id).ok_or_else(|| {
        "Unadvanced TaskResult claim recovery references an unknown attempt".to_string()
    })?;
    if attempt.state != ExecutionAttemptState::Open
        || attempt.source_message_id.as_deref() != Some(command.result_message_id.as_str())
        || attempt.source.identity
            != (ExecutionSourceIdentity::TaskResult {
                task_id: command.task_id.clone(),
                result_message_id: command.result_message_id.clone(),
            })
        || attempt.binding
            != (ExecutionBinding::WorkItem {
                work_item_id: command.work_item_id.clone(),
            })
        || attempt.admitted_fences.work_item_source_revision
            != Some(command.expected_source_revision)
        || attempt.admitted_fences.rejoin.as_ref() != Some(&command.rejoin)
    {
        return Err("Unadvanced TaskResult claim recovery attempt fence is stale".into());
    }
    let work = state.work_items.get(&command.work_item_id).ok_or_else(|| {
        "Unadvanced TaskResult claim recovery WorkItem authority is missing".to_string()
    })?;
    if work.source_revision != command.expected_source_revision
        || !matches!(
            &work.state,
            WorkItemExecutionState::InFlight { attempt_id, generation }
                if attempt_id == &command.attempt_id
                    && Some(*generation) == attempt.admitted_fences.work_item_generation
        )
    {
        return Err("Unadvanced TaskResult claim recovery WorkItem fence is stale".into());
    }
    let mut transition = interrupt_execution(
        state,
        &InterruptExecution {
            attempt_id: command.attempt_id.clone(),
            outcome_id: command.outcome_id.clone(),
            reason: "runtime_interrupted".into(),
            interrupted_at: command.interrupted_at.clone(),
        },
    )?;
    let recovered_work = transition
        .state
        .work_items
        .get_mut(&command.work_item_id)
        .expect("interruption preserved WorkItem authority");
    recovered_work.state = WorkItemExecutionState::Waiting {
        generation: recovered_work.generation(),
        wait: WaitReference {
            wait_id: command.wait_id.clone(),
        },
    };
    assert_invariants(&transition.state)?;
    transition.references.extend([
        format!("task:{}", command.task_id),
        format!("message:{}", command.result_message_id),
        format!("wait:{}", command.wait_id),
        format!("work_item:{}", command.work_item_id),
    ]);
    Ok(transition)
}

fn validate_outcome_binding(
    binding: &ExecutionBinding,
    outcome: &ExecutionOutcome,
) -> Result<(), String> {
    match (binding, outcome) {
        (ExecutionBinding::WorkItem { .. }, ExecutionOutcome::WorkItem(_))
        | (
            ExecutionBinding::Conversation { .. } | ExecutionBinding::AgentLifecycle { .. },
            ExecutionOutcome::Conversation(_),
        )
        | (ExecutionBinding::Command, ExecutionOutcome::Command(_)) => Ok(()),
        _ => Err("execution outcome is incompatible with attempt binding".into()),
    }
}

fn plan_work_item_outcome(
    generation: u64,
    outcome: &WorkItemOutcome,
) -> Result<WorkItemExecutionState, String> {
    let next_generation = generation
        .checked_add(1)
        .ok_or_else(|| "WorkItem scheduling generation overflow".to_string())?;
    match outcome {
        WorkItemOutcome::Continue => Ok(WorkItemExecutionState::Runnable {
            generation: next_generation,
            recovery_ref: None,
        }),
        WorkItemOutcome::Interrupted { .. } => Ok(WorkItemExecutionState::Runnable {
            generation: next_generation,
            recovery_ref: Some("interrupted".into()),
        }),
        WorkItemOutcome::Wait { wait } => Ok(WorkItemExecutionState::Waiting {
            generation: next_generation,
            wait: wait.clone(),
        }),
        WorkItemOutcome::Pause { reason } | WorkItemOutcome::Failed { policy: reason } => {
            Ok(WorkItemExecutionState::Paused {
                generation: next_generation,
                reason: reason.clone(),
            })
        }
        WorkItemOutcome::NeedsRepair { repair_id } => Ok(WorkItemExecutionState::NeedsRepair {
            generation: next_generation,
            repair_id: repair_id.clone(),
        }),
        WorkItemOutcome::Complete { completion } => Ok(WorkItemExecutionState::Terminal {
            generation: next_generation,
            completion: completion.clone(),
        }),
        WorkItemOutcome::Yield {
            target_work_item_id,
        } => Ok(WorkItemExecutionState::Paused {
            generation: next_generation,
            reason: format!("yielded_to:{target_work_item_id}"),
        }),
    }
}

pub fn assert_invariants(state: &ExecutionProtocolState) -> Result<(), String> {
    if state
        .attempts
        .values()
        .filter(|a| a.state == ExecutionAttemptState::Open)
        .count()
        > 1
    {
        return Err("execution partition contains more than one open attempt".into());
    }
    for (id, attempt) in &state.attempts {
        validate_attempt_invariants(
            &state.agent_id,
            id,
            attempt,
            &state.work_items,
            attempt
                .terminal_outcome_id
                .as_ref()
                .and_then(|id| state.outcomes.get(id)),
        )?;
    }
    for record in state.work_items.values() {
        let attempt = match &record.state {
            WorkItemExecutionState::InFlight { attempt_id, .. } => state.attempts.get(attempt_id),
            _ => None,
        };
        validate_work_invariants(record, attempt)?;
    }
    for (id, outcome) in &state.outcomes {
        validate_outcome_invariants(id, outcome, state.attempts.get(&outcome.attempt_id))?;
    }
    Ok(())
}

// Shared by aggregate validation and bounded closure-history traversal.
pub fn validate_attempt_invariants(
    agent_id: &str,
    id: &str,
    attempt: &ExecutionAttempt,
    works: &BTreeMap<String, WorkItemExecutionRecord>,
    outcome: Option<&ExecutionOutcomeRecord>,
) -> Result<(), String> {
    if id != attempt.attempt_id || attempt.agent_id != agent_id {
        return Err("attempt identity does not match its execution partition".into());
    }
    if attempt.state == ExecutionAttemptState::Open {
        if attempt.terminal_outcome_id.is_some() || attempt.terminal_at.is_some() {
            return Err("open attempt contains terminal evidence".into());
        }
    } else {
        attempt
            .terminal_outcome_id
            .as_ref()
            .ok_or("terminal attempt is missing its outcome")?;
        let outcome = outcome.ok_or("terminal attempt references an unknown outcome")?;
        if outcome.attempt_id != attempt.attempt_id || attempt.terminal_at.is_none() {
            return Err("terminal attempt outcome evidence is inconsistent".into());
        }
    }
    if let ExecutionBinding::WorkItem { work_item_id } = &attempt.binding {
        if attempt.state == ExecutionAttemptState::Open
            && !matches!(
                works.get(work_item_id).map(|record| &record.state),
                Some(WorkItemExecutionState::InFlight { attempt_id, .. }) if attempt_id == id
            )
        {
            return Err("open WorkItem attempt lacks reciprocal InFlight state".into());
        }
    }
    Ok(())
}

pub fn validate_work_invariants(
    record: &WorkItemExecutionRecord,
    attempt: Option<&ExecutionAttempt>,
) -> Result<(), String> {
    if record.source_revision == 0 || record.generation() == 0 {
        return Err("WorkItem execution record contains a zero revision or generation".into());
    }
    if matches!(&record.state, WorkItemExecutionState::InFlight { .. })
        && !matches!(
            attempt,
            Some(ExecutionAttempt {
                state: ExecutionAttemptState::Open,
                binding: ExecutionBinding::WorkItem { .. },
                ..
            })
        )
    {
        return Err("InFlight WorkItem lacks reciprocal open attempt".into());
    }
    Ok(())
}

pub fn validate_outcome_invariants(
    id: &str,
    outcome: &ExecutionOutcomeRecord,
    attempt: Option<&ExecutionAttempt>,
) -> Result<(), String> {
    if id != outcome.outcome_id {
        return Err("outcome identity does not match its map key".into());
    }
    if !matches!(attempt, Some(a) if a.terminal_outcome_id.as_deref() == Some(id)) {
        return Err("outcome lacks reciprocal terminal attempt".into());
    }
    Ok(())
}
