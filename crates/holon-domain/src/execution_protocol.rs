//! Scheduler / WorkItem unified execution aggregate.
//!
//! Canonical production scheduling uses this aggregate as the sole execution
//! authority. Retired scheduler payloads remain only as private runtime
//! database migration wire types for already-published databases.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

impl TurnSettlement {
    fn matches_binding(&self, binding: &ExecutionBinding) -> bool {
        use TurnOutcome::{Completed, Failed, Interrupted, Wait};
        use TurnWorkItemEffect as Effect;

        if binding.is_command() {
            return false;
        }
        let bound = binding.work_item_id().is_some();
        match (&self.outcome, &self.work_item_effect, bound) {
            (Completed | Wait { .. }, None | Some(Effect::Handoff { .. }), false)
            | (Failed { .. }, None, false)
            | (
                Interrupted {
                    interruption:
                        TurnInterruption::Interrupted { .. } | TurnInterruption::Aborted { .. },
                },
                None,
                false,
            )
            | (
                Completed,
                Some(
                    Effect::Continue
                    | Effect::Complete { .. }
                    | Effect::Pause { .. }
                    | Effect::Yield { .. },
                ),
                true,
            )
            | (Wait { .. }, Some(Effect::Wait), true)
            | (
                Interrupted {
                    interruption:
                        TurnInterruption::Interrupted { .. } | TurnInterruption::Reentry { .. },
                },
                Some(Effect::ResumeInterrupted),
                true,
            )
            | (
                Interrupted {
                    interruption: TurnInterruption::Aborted { .. },
                },
                Some(Effect::Continue),
                true,
            ) => true,
            (
                Failed {
                    failure: TurnFailure::Policy { policy },
                },
                Some(Effect::Pause { reason }),
                true,
            ) => policy == reason,
            _ => false,
        }
    }
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
    if command.source.work_item_revision < command.expected.source_revision {
        return Err("continuation resume cannot rewind the WorkItem source revision".into());
    }
    // A continuation resumes business state without settling a model Turn.
    let transition = match &command.outcome {
        WorkItemOutcome::Continue => WorkItemTurnTransition::Runnable { recovery_ref: None },
        WorkItemOutcome::Wait { wait } => WorkItemTurnTransition::Waiting {
            wait_id: &wait.wait_id,
        },
        WorkItemOutcome::Pause { reason } => WorkItemTurnTransition::Paused { reason },
        _ => {
            return Err("continuation resume requires a runnable, waiting, or paused state".into())
        }
    };
    let mut next = state.clone();
    let record = next
        .work_items
        .get_mut(&command.work_item_id)
        .expect("continuation resume preserved WorkItem");
    record.state = plan_work_item_transition(record.generation(), transition)?;
    // The resume is the single canonical transition for the suspended
    // WorkItem in its atomic batch: its source fence carries the projected
    // revision that batch writes, so the canonical source revision advances
    // here instead of through a second readiness command that would
    // invalidate this command's expected fence.
    record.source_revision = command.source.work_item_revision;
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
    // Completion is blocked only by the target WorkItem's own open attempt.
    // An attempt owned by another WorkItem (for example the finishing turn of
    // a restart after WaitFor(operator_input)) does not make the target
    // ineligible; that turn is the one completing it.
    let target_in_flight = state.open_attempt().is_some_and(|attempt| {
        attempt.turn_owner().work_item_id() == Some(command.work_item_id.as_str())
    });
    if target_in_flight
        || matches!(
            command.expected.state,
            WorkItemExecutionState::Terminal { .. }
        )
    {
        return Err("WorkItem is not eligible for lifecycle completion".into());
    }
    let mut next = state.clone();
    let record = next
        .work_items
        .get_mut(&command.work_item_id)
        .expect("WorkItem completion preserved WorkItem");
    record.state = plan_work_item_transition(
        record.generation(),
        WorkItemTurnTransition::Terminal {
            completion: &command.completion,
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

impl ExecutionAttempt {
    /// An unattributed model binding in its own agent partition.
    pub fn has_agent_lifecycle_binding(&self) -> bool {
        self.binding.is_agent_lifecycle()
            && self.binding.explicit_agent_id() == Some(self.agent_id.as_str())
    }

    /// The owner categories accepted by existing scheduler recovery paths.
    pub fn has_scheduler_attribution(&self) -> bool {
        self.binding.work_item_id().is_some() || self.has_agent_lifecycle_binding()
    }

    /// Legacy query owner, not an execution or scheduling authority.
    pub fn turn_owner(&self) -> crate::TurnOwner {
        self.binding
            .turn_attribution()
            .map_or(crate::TurnOwner::Command, |attribution| {
                attribution
                    .into_turn_owner(self.binding.explicit_agent_id().unwrap_or(&self.agent_id))
            })
    }
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
    Agent {
        agent_id: String,
        attribution: TurnAttribution,
    },
    Conversation {
        interaction_id: String,
    },
    WorkItem {
        work_item_id: String,
    },
    AgentLifecycle {
        agent_id: String,
    },
    Command,
}

/// Context and history attribution for a model turn. Neither identifier
/// grants scheduling authority or changes the owning agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnAttribution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_id: Option<String>,
}

impl TurnAttribution {
    pub(crate) fn into_turn_owner(self, agent_id: &str) -> crate::TurnOwner {
        if let Some(work_item_id) = self.work_item_id {
            crate::TurnOwner::WorkItem { work_item_id }
        } else if let Some(interaction_id) = self.interaction_id {
            crate::TurnOwner::Conversation { interaction_id }
        } else {
            crate::TurnOwner::AgentLifecycle {
                agent_id: agent_id.to_owned(),
            }
        }
    }
}

impl ExecutionBinding {
    pub fn agent_lifecycle(agent_id: impl Into<String>) -> Self {
        Self::Agent {
            agent_id: agent_id.into(),
            attribution: TurnAttribution::default(),
        }
    }

    pub fn for_work_item(agent_id: impl Into<String>, work_item_id: impl Into<String>) -> Self {
        Self::Agent {
            agent_id: agent_id.into(),
            attribution: TurnAttribution {
                work_item_id: Some(work_item_id.into()),
                interaction_id: None,
            },
        }
    }

    pub fn for_conversation(
        agent_id: impl Into<String>,
        interaction_id: impl Into<String>,
    ) -> Self {
        Self::Agent {
            agent_id: agent_id.into(),
            attribution: TurnAttribution {
                work_item_id: None,
                interaction_id: Some(interaction_id.into()),
            },
        }
    }

    pub fn work_item_id(&self) -> Option<&str> {
        match self {
            Self::Agent { attribution, .. } => attribution.work_item_id.as_deref(),
            Self::WorkItem { work_item_id } => Some(work_item_id),
            _ => None,
        }
    }

    pub fn interaction_id(&self) -> Option<&str> {
        match self {
            Self::Agent { attribution, .. } => attribution.interaction_id.as_deref(),
            Self::Conversation { interaction_id } => Some(interaction_id),
            _ => None,
        }
    }

    pub fn is_command(&self) -> bool {
        matches!(self, Self::Command)
    }

    /// Legacy scoped bindings have no explicit agent ID; the durable attempt
    /// or source still has to be checked against the trusted agent partition.
    pub fn matches_agent_partition(&self, agent_id: &str) -> bool {
        self.explicit_agent_id().is_none_or(|id| id == agent_id)
    }

    /// Compare legacy and canonical bindings using the enclosing agent partition.
    pub fn matches_in_agent(&self, other: &Self, agent_id: &str) -> bool {
        self.matches_agent_partition(agent_id)
            && other.matches_agent_partition(agent_id)
            && self.is_command() == other.is_command()
            && self.work_item_id() == other.work_item_id()
            && self.interaction_id() == other.interaction_id()
    }

    /// Upgrade a binding with the trusted owner supplied by its attempt.
    pub fn canonical_in_agent(&self, agent_id: &str) -> Option<Self> {
        if !self.matches_agent_partition(agent_id) {
            return None;
        }
        Some(match self.turn_attribution() {
            Some(attribution) => Self::Agent {
                agent_id: agent_id.to_owned(),
                attribution,
            },
            None => Self::Command,
        })
    }

    fn explicit_agent_id(&self) -> Option<&str> {
        match self {
            Self::Agent { agent_id, .. } | Self::AgentLifecycle { agent_id } => Some(agent_id),
            _ => None,
        }
    }

    pub fn is_agent_lifecycle(&self) -> bool {
        !self.is_command() && self.work_item_id().is_none() && self.interaction_id().is_none()
    }

    fn is_conversation_only(&self) -> bool {
        self.work_item_id().is_none() && self.interaction_id().is_some()
    }

    pub fn turn_attribution(&self) -> Option<TurnAttribution> {
        (!self.is_command()).then(|| TurnAttribution {
            work_item_id: self.work_item_id().map(str::to_owned),
            interaction_id: self.interaction_id().map(str::to_owned),
        })
    }
}

/// Wire forms found in durable attempts and independently persisted evidence.
/// Legacy work and conversation bindings have no agent identity of their own.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredExecutionBinding {
    Agent {
        agent_id: String,
        attribution: TurnAttribution,
    },
    Conversation {
        interaction_id: String,
    },
    WorkItem {
        work_item_id: String,
    },
    AgentLifecycle {
        agent_id: String,
    },
    Command,
}

impl StoredExecutionBinding {
    /// Decode a binding in the scope of its durable attempt or event.
    /// An explicit foreign agent never inherits the supplied scope.
    pub fn into_execution_binding(self, owner: &str) -> Option<ExecutionBinding> {
        match self {
            Self::Agent {
                agent_id,
                attribution,
            } if agent_id == owner => Some(ExecutionBinding::Agent {
                agent_id,
                attribution,
            }),
            Self::WorkItem { work_item_id } => {
                Some(ExecutionBinding::for_work_item(owner, work_item_id))
            }
            Self::Conversation { interaction_id } => {
                Some(ExecutionBinding::for_conversation(owner, interaction_id))
            }
            Self::AgentLifecycle { agent_id } if agent_id == owner => {
                Some(ExecutionBinding::agent_lifecycle(agent_id))
            }
            Self::Command => Some(ExecutionBinding::Command),
            _ => None,
        }
    }

    fn scoped_attribution(&self, owner: &str) -> Option<(Option<&str>, Option<&str>)> {
        match self {
            Self::Agent {
                agent_id,
                attribution,
            } if agent_id == owner => Some((
                attribution.work_item_id.as_deref(),
                attribution.interaction_id.as_deref(),
            )),
            Self::WorkItem { work_item_id } => Some((Some(work_item_id), None)),
            Self::Conversation { interaction_id } => Some((None, Some(interaction_id))),
            Self::AgentLifecycle { agent_id } if agent_id == owner => Some((None, None)),
            _ => None,
        }
    }

    /// Compare bindings within their trusted durable row/attempt owner. Legacy
    /// WorkItem and Conversation bindings get their missing owner from this scope.
    pub fn matches_in_agent(&self, other: &Self, owner: &str) -> bool {
        match (self, other) {
            (Self::Command, Self::Command) => true,
            _ => self
                .scoped_attribution(owner)
                .is_some_and(|attribution| other.scoped_attribution(owner) == Some(attribution)),
        }
    }

    pub fn matches_attempt(&self, attempt: &ExecutionAttempt) -> bool {
        self.matches_in_agent(&(&attempt.binding).into(), &attempt.agent_id)
    }
}

impl From<&ExecutionBinding> for StoredExecutionBinding {
    fn from(binding: &ExecutionBinding) -> Self {
        match binding {
            ExecutionBinding::Agent {
                agent_id,
                attribution,
            } => Self::Agent {
                agent_id: agent_id.clone(),
                attribution: attribution.clone(),
            },
            ExecutionBinding::Conversation { interaction_id } => Self::Conversation {
                interaction_id: interaction_id.clone(),
            },
            ExecutionBinding::WorkItem { work_item_id } => Self::WorkItem {
                work_item_id: work_item_id.clone(),
            },
            ExecutionBinding::AgentLifecycle { agent_id } => Self::AgentLifecycle {
                agent_id: agent_id.clone(),
            },
            ExecutionBinding::Command => Self::Command,
        }
    }
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
    Waiting {
        generation: u64,
        wait: WaitReference,
    },
    Paused {
        generation: u64,
        reason: String,
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
            | Self::Waiting { generation, .. }
            | Self::Paused { generation, .. }
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
    Turn(TurnSettlement),
    // Historical model outcome encodings. New settlements store Turn instead.
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
    Aborted {
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
    Wait {
        wait: WaitReference,
    },
    Complete {
        completion: String,
    },
    Pause {
        reason: String,
    },
    Yield {
        target_work_item_id: String,
    },
    Failed {
        policy: String,
    },
    Interrupted {
        reason: String,
    },
    Aborted {
        reason: String,
    },
    /// The attempt stopped before any provider IO or side effect; the
    /// WorkItem re-enters Runnable with `recovery_ref` recording why.
    Reentry {
        recovery_ref: String,
    },
}

/// The model Turn's result, regardless of its WorkItem or interaction attribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Wait { wait: WaitReference },
    Failed { failure: TurnFailure },
    Interrupted { interruption: TurnInterruption },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnFailure {
    Policy { policy: String },
    Paused { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnInterruption {
    Interrupted { reason: String },
    Aborted { reason: String },
    Reentry { recovery_ref: String },
}

/// A model terminal and its business effect are independent. In particular a
/// lifecycle Turn can end without changing focus, or hand off to a WorkItem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSettlement {
    pub outcome: TurnOutcome,
    pub work_item_effect: Option<TurnWorkItemEffect>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnWorkItemEffect {
    Continue,
    Wait,
    Complete { completion: String },
    Pause { reason: String },
    Yield { target_work_item_id: String },
    ResumeInterrupted,
    Handoff { work_item_id: String },
}

impl ExecutionOutcome {
    /// Read a model settlement in either the current or historical encoding.
    pub fn model_turn(&self) -> Option<TurnSettlement> {
        use TurnOutcome::{Completed, Failed, Interrupted, Wait};
        use TurnWorkItemEffect as Effect;

        let (outcome, work_item_effect) = match self {
            Self::Turn(turn) => return Some(turn.clone()),
            Self::Command(_) => return None,
            Self::Conversation(legacy) => match legacy {
                ConversationOutcome::Replied => (Completed, None),
                ConversationOutcome::HandoffToWorkItemContinue { work_item_id } => (
                    Completed,
                    Some(Effect::Handoff {
                        work_item_id: work_item_id.clone(),
                    }),
                ),
                ConversationOutcome::Wait { wait } => (Wait { wait: wait.clone() }, None),
                ConversationOutcome::HandoffToWorkItemWait { work_item_id, wait } => (
                    Wait { wait: wait.clone() },
                    Some(Effect::Handoff {
                        work_item_id: work_item_id.clone(),
                    }),
                ),
                ConversationOutcome::Paused { reason } => (
                    Failed {
                        failure: TurnFailure::Paused {
                            reason: reason.clone(),
                        },
                    },
                    None,
                ),
                ConversationOutcome::Failed { policy } => (
                    Failed {
                        failure: TurnFailure::Policy {
                            policy: policy.clone(),
                        },
                    },
                    None,
                ),
                ConversationOutcome::Interrupted { reason } => (
                    Interrupted {
                        interruption: TurnInterruption::Interrupted {
                            reason: reason.clone(),
                        },
                    },
                    None,
                ),
                ConversationOutcome::Aborted { reason } => (
                    Interrupted {
                        interruption: TurnInterruption::Aborted {
                            reason: reason.clone(),
                        },
                    },
                    None,
                ),
            },
            Self::WorkItem(legacy) => {
                let (outcome, effect) = match legacy {
                    WorkItemOutcome::Continue => (Completed, Effect::Continue),
                    WorkItemOutcome::Wait { wait } => (Wait { wait: wait.clone() }, Effect::Wait),
                    WorkItemOutcome::Complete { completion } => (
                        Completed,
                        Effect::Complete {
                            completion: completion.clone(),
                        },
                    ),
                    WorkItemOutcome::Pause { reason } => (
                        Completed,
                        Effect::Pause {
                            reason: reason.clone(),
                        },
                    ),
                    WorkItemOutcome::Yield {
                        target_work_item_id,
                    } => (
                        Completed,
                        Effect::Yield {
                            target_work_item_id: target_work_item_id.clone(),
                        },
                    ),
                    WorkItemOutcome::Failed { policy } => (
                        Failed {
                            failure: TurnFailure::Policy {
                                policy: policy.clone(),
                            },
                        },
                        Effect::Pause {
                            reason: policy.clone(),
                        },
                    ),
                    WorkItemOutcome::Interrupted { reason } => (
                        Interrupted {
                            interruption: TurnInterruption::Interrupted {
                                reason: reason.clone(),
                            },
                        },
                        Effect::ResumeInterrupted,
                    ),
                    WorkItemOutcome::Aborted { reason } => (
                        Interrupted {
                            interruption: TurnInterruption::Aborted {
                                reason: reason.clone(),
                            },
                        },
                        Effect::Continue,
                    ),
                    WorkItemOutcome::Reentry { recovery_ref } => (
                        Interrupted {
                            interruption: TurnInterruption::Reentry {
                                recovery_ref: recovery_ref.clone(),
                            },
                        },
                        Effect::ResumeInterrupted,
                    ),
                };
                (outcome, Some(effect))
            }
        };
        Some(TurnSettlement {
            outcome,
            work_item_effect,
        })
    }

    pub fn same_terminal_as(&self, other: &Self) -> bool {
        self == other
            || self
                .model_turn()
                .is_some_and(|turn| other.model_turn().as_ref() == Some(&turn))
    }

    pub fn matches_binding(&self, binding: &ExecutionBinding) -> bool {
        match self {
            Self::Turn(turn) => turn.matches_binding(binding),
            Self::WorkItem(_) => binding.work_item_id().is_some(),
            Self::Conversation(_) => !binding.is_command() && binding.work_item_id().is_none(),
            Self::Command(_) => binding.is_command(),
        }
    }
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

impl ExecutionOutcomeRecord {
    pub fn same_terminal_as(&self, other: &Self) -> bool {
        self.outcome_id == other.outcome_id
            && self.attempt_id == other.attempt_id
            && self.created_at == other.created_at
            && self.outcome.same_terminal_as(&other.outcome)
    }
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
    /// Retained for historical command decoding; not used to derive an outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
    pub active_wait_ids: Vec<String>,
    /// The most recently admitted active wait, by (created_at, id) order.
    /// Disambiguates which wait owns the resume when several are active.
    #[serde(default)]
    pub latest_admitted_wait_id: Option<String>,
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
        // Multiple active waits are adjudicated deterministically: the latest
        // admission owns the resume; any other answer is a fence violation.
        _ if source
            .latest_admitted_wait_id
            .as_ref()
            .is_some_and(|latest| source.active_wait_ids.contains(latest)) =>
        {
            WorkItemOutcome::Wait {
                wait: WaitReference {
                    wait_id: source
                        .latest_admitted_wait_id
                        .clone()
                        .expect("latest admitted wait checked above"),
                },
            }
        }
        [] => WorkItemOutcome::Continue,
        _ => WorkItemOutcome::Pause {
            reason: format!("work_item_waits_ambiguous:{work_item_id}"),
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

impl ExecutionProtocolCommand {
    pub fn identity(&self) -> (&'static str, &str) {
        match self {
            Self::RegisterWorkItem(command) => {
                ("register_work_item_execution", &command.work_item_id)
            }
            Self::AdvanceWorkItemSourceRevision(command) => {
                ("advance_work_item_source_revision", &command.command_id)
            }
            Self::ReconcileWorkItemSourceRevision(command) => {
                ("reconcile_work_item_source_revision", &command.command_id)
            }
            Self::SetWorkItemReadiness(command) => ("set_work_item_readiness", &command.command_id),
            Self::RecheckContextBudgetStop(command) => {
                ("recheck_context_budget_stop", &command.command_id)
            }
            Self::SuspendWorkItemContinuation(command) => {
                ("suspend_work_item_continuation", &command.command_id)
            }
            Self::ResumeWorkItemContinuation(command) => {
                ("resume_work_item_continuation", &command.command_id)
            }
            Self::ReconcileWorkItemContinuationYield(command) => (
                "reconcile_work_item_continuation_yield",
                &command.command_id,
            ),
            Self::SetWorkItemWaiting(command) => ("set_work_item_waiting", &command.command_id),
            Self::CompleteWorkItem(command) => {
                ("complete_work_item_execution", &command.command_id)
            }
            Self::Admit(command) => ("admit_execution", &command.attempt.attempt_id),
            Self::Settle(command) => ("settle_execution", &command.outcome.outcome_id),
            Self::Interrupt(command) => ("interrupt_execution", &command.outcome_id),
            Self::FinalizeInterrupted(command) => {
                ("finalize_interrupted_execution", &command.command_id)
            }
            Self::RecoverInterruptedTaskResultClaim(command) => {
                ("recover_interrupted_task_result_claim", &command.command_id)
            }
            Self::RecoverUnadvancedTaskResultClaim(command) => {
                ("recover_unadvanced_task_result_claim", &command.command_id)
            }
        }
    }

    pub fn reduce(&self, state: &ExecutionProtocolState) -> Result<ExecutionTransition, String> {
        match self {
            Self::RegisterWorkItem(command) => register_work_item_execution(state, command),
            Self::AdvanceWorkItemSourceRevision(command) => {
                advance_work_item_source_revision(state, command)
            }
            Self::ReconcileWorkItemSourceRevision(command) => {
                reconcile_work_item_source_revision(state, command)
            }
            Self::SetWorkItemReadiness(command) => set_work_item_readiness(state, command),
            Self::RecheckContextBudgetStop(command) => recheck_context_budget_stop(state, command),
            Self::SuspendWorkItemContinuation(command) => {
                suspend_work_item_continuation(state, command)
            }
            Self::ResumeWorkItemContinuation(command) => {
                resume_work_item_continuation(state, command)
            }
            Self::ReconcileWorkItemContinuationYield(command) => {
                reconcile_work_item_continuation_yield(state, command)
            }
            Self::SetWorkItemWaiting(command) => set_work_item_waiting(state, command),
            Self::CompleteWorkItem(command) => complete_work_item_execution(state, command),
            Self::Admit(command) => admit_execution(state, command),
            Self::Settle(command) => settle_execution(state, command),
            Self::Interrupt(command) => interrupt_execution(state, command),
            Self::FinalizeInterrupted(command) => finalize_interrupted_execution(state, command),
            Self::RecoverInterruptedTaskResultClaim(command) => {
                recover_interrupted_task_result_claim(state, command)
            }
            Self::RecoverUnadvancedTaskResultClaim(command) => {
                recover_unadvanced_task_result_claim(state, command)
            }
        }
    }
}

/// Hash the exact serialized payload recorded in the command receipt.
pub fn command_payload_hash(payload: &impl Serialize) -> Result<String, serde_json::Error> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(payload)?)
    ))
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
    if state
        .open_attempt()
        .is_some_and(|attempt| attempt.binding.work_item_id() == Some(&command.work_item_id))
        || command.expected.as_ref().is_some_and(|record| {
            matches!(record.state, WorkItemExecutionState::Terminal { .. })
                || command.record.generation() <= record.generation()
        })
    {
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
    if state.open_attempt().is_some()
        || matches!(record.state, WorkItemExecutionState::Terminal { .. })
    {
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
            }
            | WorkItemExecutionState::Paused {
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
    use ExecutionSourceIdentity::*;
    let allowed = match &source.identity {
        QueueMessage { .. } | InternalFollowup { .. } | TriggeredWait { .. } => {
            !binding.is_command()
        }
        RuntimeRecovery { .. } => true,
        TaskResult { .. } | ChildResult { .. } => !binding.is_agent_lifecycle(),
        WorkItemContinuation { work_item_id } => binding.work_item_id() == Some(work_item_id),
        TargetedContinuation {
            target_work_item_id,
            ..
        } => {
            binding.work_item_id() == Some(target_work_item_id)
                || binding.is_conversation_only()
                || binding.is_command()
        }
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
        (&attempt.source.identity, provenance.origin),
        (
            ExecutionSourceIdentity::InternalFollowup { .. }
                | ExecutionSourceIdentity::TriggeredWait {
                    ingress: Some(ExecutionWaitIngress::RuntimeOwnedInternalFollowup),
                    ..
                },
            System | Task
        )
    ) && (attempt.binding.work_item_id().is_some()
        || attempt.binding.is_agent_lifecycle());
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
    if !attempt.binding.matches_agent_partition(&attempt.agent_id) {
        return Err("agent lifecycle binding does not match attempt partition".into());
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
        ) && (prior.agent_id != attempt.agent_id
            || !prior
                .binding
                .matches_in_agent(&attempt.binding, &attempt.agent_id))
        {
            return Err("runtime recovery must inherit its predecessor binding".into());
        }
    } else if matches!(
        attempt.source.identity,
        ExecutionSourceIdentity::RuntimeRecovery { .. }
    ) && !attempt.binding.is_command()
        && attempt.binding.work_item_id().is_none()
    {
        return Err("unbound runtime recovery requires a validated predecessor".into());
    }

    let mut next = state.clone();
    if let Some(work_item_id) = attempt.binding.work_item_id() {
        let expected_source_revision = attempt
            .admitted_fences
            .work_item_source_revision
            .ok_or_else(|| "WorkItem admission requires a source revision fence".to_string())?;
        let expected_generation = attempt
            .admitted_fences
            .work_item_generation
            .ok_or_else(|| "WorkItem admission requires a generation fence".to_string())?;
        let work_record = state
            .work_items
            .get(work_item_id)
            .ok_or_else(|| "WorkItem admission requires registered execution state".to_string())?;
        if work_record.source_revision != expected_source_revision {
            return Err("WorkItem admission source revision fence is stale".into());
        }
        if !eligible_work_item_at_generation(work_record, Some(expected_generation)) {
            return Err("WorkItem is not eligible at the admitted generation".into());
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
    if !outcome.outcome.matches_binding(&attempt.binding) {
        return Err("execution outcome is incompatible with attempt binding".into());
    }
    let turn = outcome.outcome.model_turn();

    if let Some(work_item_id) = attempt.binding.work_item_id() {
        let work_record = next
            .work_items
            .get_mut(work_item_id)
            .ok_or_else(|| "attempt references an unknown WorkItem state".to_string())?;
        if !eligible_work_item_at_generation(
            work_record,
            attempt.admitted_fences.work_item_generation,
        ) {
            return Err("WorkItem generation does not match the settling attempt".into());
        }
        let turn = turn.as_ref().expect("WorkItem binding validated above");
        let transition =
            plan_bound_turn_transition(turn, &work_record.state, &attempt.source.identity)?;
        work_record.state = plan_work_item_transition(work_record.generation(), transition)?;
    }

    attempt.state = ExecutionAttemptState::Settled;
    attempt.terminal_outcome_id = Some(outcome.outcome_id.clone());
    attempt.terminal_at = Some(outcome.created_at.clone());
    let mut settled = outcome.clone();
    if let Some(turn) = turn {
        settled.outcome = ExecutionOutcome::Turn(turn);
    }
    next.outcomes.insert(settled.outcome_id.clone(), settled);
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
        outcome: {
            let attempt = state
                .attempts
                .get(&command.attempt_id)
                .ok_or("interruption references an unknown attempt")?;
            if attempt.binding.is_command() {
                ExecutionOutcome::Command(CommandResult::Applied {
                    references: vec![format!("interrupted:{}", command.attempt_id)],
                })
            } else if attempt.binding.work_item_id().is_some() {
                ExecutionOutcome::WorkItem(WorkItemOutcome::Interrupted {
                    reason: command.reason.clone(),
                })
            } else {
                ExecutionOutcome::Conversation(ConversationOutcome::Interrupted {
                    reason: command.reason.clone(),
                })
            }
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
        || attempt.binding.work_item_id() != Some(&command.work_item_id)
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
    ) || !eligible_work_item_at_generation(work, attempt.admitted_fences.work_item_generation)
    {
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
        || attempt.binding.work_item_id() != Some(&command.work_item_id)
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
        || !eligible_work_item_at_generation(work, attempt.admitted_fences.work_item_generation)
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

/// The business state change, independent of how the model turn ended.
enum WorkItemTurnTransition<'a> {
    Runnable { recovery_ref: Option<&'a str> },
    Waiting { wait_id: &'a str },
    Paused { reason: &'a str },
    Terminal { completion: &'a str },
    Yield { target_work_item_id: &'a str },
}

fn plan_bound_turn_transition<'a>(
    turn: &'a TurnSettlement,
    state: &WorkItemExecutionState,
    source: &'a ExecutionSourceIdentity,
) -> Result<WorkItemTurnTransition<'a>, String> {
    use TurnWorkItemEffect as Effect;
    use WorkItemTurnTransition as Transition;

    let transition = match (&turn.work_item_effect, &turn.outcome) {
        (Some(Effect::Continue), _) => Transition::Runnable { recovery_ref: None },
        (Some(Effect::Wait), TurnOutcome::Wait { wait }) => Transition::Waiting {
            wait_id: &wait.wait_id,
        },
        (Some(Effect::Pause { reason }), _) => Transition::Paused { reason },
        (Some(Effect::Complete { completion }), _) => Transition::Terminal { completion },
        (
            Some(Effect::Yield {
                target_work_item_id,
            }),
            _,
        ) => Transition::Yield {
            target_work_item_id,
        },
        (
            Some(Effect::ResumeInterrupted),
            TurnOutcome::Interrupted {
                interruption: TurnInterruption::Interrupted { .. },
            },
        ) => match (state, source) {
            (
                WorkItemExecutionState::Waiting { .. },
                ExecutionSourceIdentity::TriggeredWait { wait_id, .. },
            ) => Transition::Waiting { wait_id },
            _ => Transition::Runnable {
                recovery_ref: Some("interrupted"),
            },
        },
        (
            Some(Effect::ResumeInterrupted),
            TurnOutcome::Interrupted {
                interruption: TurnInterruption::Reentry { recovery_ref },
            },
        ) => Transition::Runnable {
            recovery_ref: Some(recovery_ref),
        },
        _ => return Err("model Turn has an incompatible bound WorkItem effect".into()),
    };
    Ok(transition)
}

fn plan_work_item_transition(
    generation: u64,
    transition: WorkItemTurnTransition<'_>,
) -> Result<WorkItemExecutionState, String> {
    let next_generation = generation
        .checked_add(1)
        .ok_or_else(|| "WorkItem scheduling generation overflow".to_string())?;
    Ok(match transition {
        WorkItemTurnTransition::Runnable { recovery_ref } => WorkItemExecutionState::Runnable {
            generation: next_generation,
            recovery_ref: recovery_ref.map(str::to_owned),
        },
        WorkItemTurnTransition::Waiting { wait_id } => WorkItemExecutionState::Waiting {
            generation: next_generation,
            wait: WaitReference {
                wait_id: wait_id.to_owned(),
            },
        },
        WorkItemTurnTransition::Paused { reason } => WorkItemExecutionState::Paused {
            generation: next_generation,
            reason: reason.to_owned(),
        },
        WorkItemTurnTransition::Terminal { completion } => WorkItemExecutionState::Terminal {
            generation: next_generation,
            completion: completion.to_owned(),
        },
        WorkItemTurnTransition::Yield {
            target_work_item_id,
        } => WorkItemExecutionState::Paused {
            generation: next_generation,
            reason: format!("yielded_to:{target_work_item_id}"),
        },
    })
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
        validate_work_invariants(record)?;
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
    if let Some(work_item_id) = attempt.binding.work_item_id() {
        if attempt.state == ExecutionAttemptState::Open
            && !works.get(work_item_id).is_some_and(|record| {
                eligible_work_item_at_generation(
                    record,
                    attempt.admitted_fences.work_item_generation,
                )
            })
        {
            return Err("open WorkItem attempt lacks an eligible WorkItem generation".into());
        }
    }
    Ok(())
}

pub fn eligible_work_item_at_generation(
    record: &WorkItemExecutionRecord,
    generation: Option<u64>,
) -> bool {
    generation == Some(record.generation())
        && matches!(
            record.state,
            WorkItemExecutionState::Runnable { .. } | WorkItemExecutionState::Waiting { .. }
        )
}

pub fn validate_work_invariants(record: &WorkItemExecutionRecord) -> Result<(), String> {
    if record.source_revision == 0 || record.generation() == 0 {
        return Err("WorkItem execution record contains a zero revision or generation".into());
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

#[cfg(test)]
mod abort_tests;
