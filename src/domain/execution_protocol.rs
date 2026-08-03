//! Scheduler / WorkItem unified execution aggregate.
//!
//! This is the additive Phase 2 model. Production scheduling still uses
//! `scheduler_protocol`; this module defines the smaller authority boundary
//! that the canonical reader will adopt in a later cutover.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionProtocolState {
    pub agent_id: String,
    pub attempts: BTreeMap<String, ExecutionAttempt>,
    pub work_items: BTreeMap<String, WorkItemExecutionRecord>,
    pub outcomes: BTreeMap<String, ExecutionOutcomeRecord>,
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
        wait_generation: u64,
        trigger_id: String,
        trigger_generation: u64,
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
    pub parent_turn_id: String,
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
    pub generation: u64,
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
    Wait { wait: WaitReference },
    Paused { reason: String },
    Interrupted { reason: String },
    Failed { policy: String },
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
pub struct InterruptExecution {
    pub attempt_id: String,
    pub outcome_id: String,
    pub reason: String,
    pub interrupted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", content = "payload", rename_all = "snake_case")]
pub enum ExecutionProtocolCommand {
    RegisterWorkItem(Box<RegisterWorkItemExecution>),
    Admit(Box<AdmitExecution>),
    Settle(SettleExecution),
    Interrupt(InterruptExecution),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionTransition {
    pub state: ExecutionProtocolState,
    pub references: Vec<String>,
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

fn validate_source_binding(
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
        (RuntimeRecovery { .. }, Command) => true,
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
    if fence.obligation_id.is_empty() || fence.parent_turn_id.is_empty() || fence.generation == 0 {
        return Err("rejoin fence requires identity, parent turn, and generation".into());
    }
    Ok(())
}

fn validate_provenance(attempt: &ExecutionAttempt) -> Result<(), String> {
    use ExecutionOrigin::*;
    use ExecutionTrust::*;
    let provenance = &attempt.provenance;
    let valid = match provenance.origin {
        Operator => provenance.trust == OperatorInstruction,
        Channel | Webhook => matches!(provenance.trust, IntegrationSignal | ExternalEvidence),
        Callback => matches!(
            provenance.trust,
            RuntimeInstruction | IntegrationSignal | ExternalEvidence
        ),
        Timer | System => matches!(provenance.trust, RuntimeInstruction | IntegrationSignal),
        Task | RuntimeRecovery => provenance.trust == RuntimeInstruction,
    };
    // InternalFollowup identities are runtime-only: the scheduler constructs them
    // from persisted ingress evidence while preserving, never upgrading, trust.
    let runtime_owned_followup = matches!(
        (
            &attempt.source.identity,
            &attempt.binding,
            provenance.origin
        ),
        (
            ExecutionSourceIdentity::InternalFollowup { .. },
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
        work_record.state = settle_work_item(generation, &outcome.outcome)?;
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

fn settle_work_item(
    generation: u64,
    outcome: &ExecutionOutcome,
) -> Result<WorkItemExecutionState, String> {
    let next_generation = generation
        .checked_add(1)
        .ok_or_else(|| "WorkItem scheduling generation overflow".to_string())?;
    match outcome {
        ExecutionOutcome::WorkItem(WorkItemOutcome::Continue) => {
            Ok(WorkItemExecutionState::Runnable {
                generation: next_generation,
                recovery_ref: None,
            })
        }
        ExecutionOutcome::WorkItem(WorkItemOutcome::Interrupted { .. }) => {
            Ok(WorkItemExecutionState::Runnable {
                generation: next_generation,
                recovery_ref: Some("interrupted".into()),
            })
        }
        ExecutionOutcome::WorkItem(WorkItemOutcome::Wait { wait }) => {
            Ok(WorkItemExecutionState::Waiting {
                generation: next_generation,
                wait: wait.clone(),
            })
        }
        ExecutionOutcome::WorkItem(WorkItemOutcome::Pause { reason })
        | ExecutionOutcome::WorkItem(WorkItemOutcome::Failed { policy: reason }) => {
            Ok(WorkItemExecutionState::Paused {
                generation: next_generation,
                reason: reason.clone(),
            })
        }
        ExecutionOutcome::WorkItem(WorkItemOutcome::NeedsRepair { repair_id }) => {
            Ok(WorkItemExecutionState::NeedsRepair {
                generation: next_generation,
                repair_id: repair_id.clone(),
            })
        }
        ExecutionOutcome::WorkItem(WorkItemOutcome::Complete { completion }) => {
            Ok(WorkItemExecutionState::Terminal {
                generation: next_generation,
                completion: completion.clone(),
            })
        }
        ExecutionOutcome::WorkItem(WorkItemOutcome::Yield {
            target_work_item_id,
        }) => Ok(WorkItemExecutionState::Paused {
            generation: next_generation,
            reason: format!("yielded_to:{target_work_item_id}"),
        }),
        _ => Err("WorkItem settlement requires a WorkItem outcome".into()),
    }
}

pub fn assert_invariants(state: &ExecutionProtocolState) -> Result<(), String> {
    let open_attempts = state
        .attempts
        .values()
        .filter(|attempt| attempt.state == ExecutionAttemptState::Open)
        .collect::<Vec<_>>();
    if open_attempts.len() > 1 {
        return Err("execution partition contains more than one open attempt".into());
    }

    for (attempt_id, attempt) in &state.attempts {
        if attempt_id != &attempt.attempt_id || attempt.agent_id != state.agent_id {
            return Err("attempt identity does not match its execution partition".into());
        }
        match attempt.state {
            ExecutionAttemptState::Open => {
                if attempt.terminal_outcome_id.is_some() || attempt.terminal_at.is_some() {
                    return Err("open attempt contains terminal evidence".into());
                }
            }
            _ => {
                let outcome_id = attempt
                    .terminal_outcome_id
                    .as_ref()
                    .ok_or_else(|| "terminal attempt is missing its outcome".to_string())?;
                let outcome = state
                    .outcomes
                    .get(outcome_id)
                    .ok_or_else(|| "terminal attempt references an unknown outcome".to_string())?;
                if outcome.attempt_id != attempt.attempt_id || attempt.terminal_at.is_none() {
                    return Err("terminal attempt outcome evidence is inconsistent".into());
                }
            }
        }
        if let ExecutionBinding::WorkItem { work_item_id } = &attempt.binding {
            if attempt.state == ExecutionAttemptState::Open
                && !matches!(
                    state.work_items.get(work_item_id).map(|record| &record.state),
                    Some(WorkItemExecutionState::InFlight {
                        attempt_id: linked_attempt,
                        ..
                    }) if linked_attempt == attempt_id
                )
            {
                return Err("open WorkItem attempt lacks reciprocal InFlight state".into());
            }
        }
    }

    for work_record in state.work_items.values() {
        if work_record.source_revision == 0 || work_record.generation() == 0 {
            return Err("WorkItem execution record contains a zero revision or generation".into());
        }
        if let WorkItemExecutionState::InFlight { attempt_id, .. } = &work_record.state {
            if !matches!(
                state.attempts.get(attempt_id),
                Some(ExecutionAttempt {
                    state: ExecutionAttemptState::Open,
                    binding: ExecutionBinding::WorkItem { .. },
                    ..
                })
            ) {
                return Err("InFlight WorkItem lacks reciprocal open attempt".into());
            }
        }
    }

    for (outcome_id, outcome) in &state.outcomes {
        if outcome_id != &outcome.outcome_id {
            return Err("outcome identity does not match its map key".into());
        }
        if !matches!(
            state.attempts.get(&outcome.attempt_id),
            Some(attempt) if attempt.terminal_outcome_id.as_deref() == Some(outcome_id)
        ) {
            return Err("outcome lacks reciprocal terminal attempt".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work_item_record(state: WorkItemExecutionState) -> WorkItemExecutionRecord {
        WorkItemExecutionRecord {
            source_revision: state.generation(),
            state,
        }
    }

    fn attempt(id: &str, recovery_of: Option<&str>) -> ExecutionAttempt {
        ExecutionAttempt {
            attempt_id: id.into(),
            agent_id: "agent-a".into(),
            source_message_id: Some(format!("message:{id}")),
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::WorkItemContinuation {
                    work_item_id: "work-a".into(),
                },
                generation: 1,
            },
            binding: ExecutionBinding::WorkItem {
                work_item_id: "work-a".into(),
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
                work_item_generation: Some(if recovery_of.is_some() { 2 } else { 1 }),
                rejoin: None,
                agent_control_revision: 1,
                host_registry_revision: 1,
            },
            state: ExecutionAttemptState::Open,
            run_id: None,
            turn_id: None,
            recovery_of_attempt_id: recovery_of.map(str::to_owned),
            terminal_outcome_id: None,
            admitted_at: "2026-08-01T00:00:00Z".into(),
            terminal_at: None,
        }
    }

    #[test]
    fn work_item_registration_is_additive_and_idempotent() {
        let state = ExecutionProtocolState::empty("agent-a");
        let command = RegisterWorkItemExecution {
            work_item_id: "work-a".into(),
            record: work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        };
        let registered = register_work_item_execution(&state, &command).unwrap();
        let replayed = register_work_item_execution(&registered.state, &command).unwrap();
        assert_eq!(registered.state, replayed.state);

        let conflict = RegisterWorkItemExecution {
            work_item_id: "work-a".into(),
            record: work_item_record(WorkItemExecutionState::Paused {
                generation: 1,
                reason: "changed".into(),
            }),
        };
        assert!(register_work_item_execution(&registered.state, &conflict)
            .unwrap_err()
            .contains("different state"));
    }

    #[test]
    fn interrupted_attempt_releases_lane_and_allows_model_reentry() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        );
        let admitted = admit_execution(
            &state,
            &AdmitExecution {
                attempt: attempt("attempt-1", None),
            },
        )
        .unwrap();
        let interrupted = interrupt_execution(
            &admitted.state,
            &InterruptExecution {
                attempt_id: "attempt-1".into(),
                outcome_id: "outcome-1".into(),
                reason: "runtime_restart".into(),
                interrupted_at: "2026-08-01T00:01:00Z".into(),
            },
        )
        .unwrap();
        assert!(interrupted.state.open_attempt().is_none());
        assert!(matches!(
            interrupted.state.work_items["work-a"].state,
            WorkItemExecutionState::Runnable {
                generation: 2,
                recovery_ref: Some(_)
            }
        ));

        let recovered = admit_execution(
            &interrupted.state,
            &AdmitExecution {
                attempt: attempt("attempt-2", Some("attempt-1")),
            },
        )
        .unwrap();
        assert_eq!(
            recovered
                .state
                .open_attempt()
                .map(|attempt| attempt.attempt_id.as_str()),
            Some("attempt-2")
        );
    }

    #[test]
    fn wait_resume_admits_from_waiting_and_settles_to_runnable() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Waiting {
                generation: 1,
                wait: WaitReference {
                    wait_id: "wait-1".into(),
                    generation: 1,
                },
            }),
        );
        let mut wait_attempt = attempt("attempt-1", None);
        wait_attempt.source.identity = ExecutionSourceIdentity::TriggeredWait {
            wait_id: "wait-1".into(),
            wait_generation: 1,
            trigger_id: "trigger-1".into(),
            trigger_generation: 1,
        };
        let admitted = admit_execution(
            &state,
            &AdmitExecution {
                attempt: wait_attempt,
            },
        )
        .unwrap();
        assert!(matches!(
            admitted.state.work_items["work-a"].state,
            WorkItemExecutionState::InFlight { generation: 1, .. }
        ));
        let settled = settle_execution(
            &admitted.state,
            &SettleExecution {
                outcome: ExecutionOutcomeRecord {
                    outcome_id: "outcome-1".into(),
                    attempt_id: "attempt-1".into(),
                    outcome: ExecutionOutcome::WorkItem(WorkItemOutcome::Continue),
                    created_at: "2026-08-01T00:01:00Z".into(),
                },
            },
        )
        .unwrap();
        assert!(matches!(
            settled.state.work_items["work-a"].state,
            WorkItemExecutionState::Runnable {
                generation: 2,
                recovery_ref: None
            }
        ));
    }

    #[test]
    fn source_binding_matrix_rejects_unsupported_combinations() {
        use ExecutionBinding::*;

        let conversation = Conversation {
            interaction_id: "i1".into(),
        };
        let work_item = WorkItem {
            work_item_id: "w1".into(),
        };
        let agent_lifecycle = AgentLifecycle {
            agent_id: "agent-a".into(),
        };
        let fences = AdmittedFences {
            source_revision: 1,
            work_item_source_revision: None,
            work_item_generation: None,
            rejoin: None,
            agent_control_revision: 1,
            host_registry_revision: 1,
        };
        let source = |identity| ExecutionSource {
            identity,
            generation: 1,
        };

        // Allowed combinations
        let queue = source(ExecutionSourceIdentity::QueueMessage {
            message_id: "m1".into(),
        });
        let continuation = source(ExecutionSourceIdentity::WorkItemContinuation {
            work_item_id: "w1".into(),
        });
        let recovery = source(ExecutionSourceIdentity::RuntimeRecovery {
            recovery_id: "r1".into(),
        });
        assert!(validate_source_binding(&queue, &conversation, &fences).is_ok());
        assert!(validate_source_binding(&queue, &work_item, &fences).is_ok());
        assert!(validate_source_binding(&queue, &agent_lifecycle, &fences).is_ok());
        assert!(validate_source_binding(&continuation, &work_item, &fences).is_ok());
        assert!(validate_source_binding(&recovery, &ExecutionBinding::Command, &fences).is_ok());

        // Rejected combinations
        assert!(validate_source_binding(&queue, &ExecutionBinding::Command, &fences).is_err());
        assert!(validate_source_binding(&continuation, &conversation, &fences).is_err());
        assert!(validate_source_binding(&recovery, &conversation, &fences).is_err());
        assert!(validate_source_binding(&recovery, &work_item, &fences).is_err());
        assert!(validate_source_binding(&recovery, &agent_lifecycle, &fences).is_err());
    }

    #[test]
    fn task_and_child_results_require_exact_live_rejoin_fences() {
        let binding = ExecutionBinding::WorkItem {
            work_item_id: "work-a".into(),
        };
        let task = ExecutionSource {
            identity: ExecutionSourceIdentity::TaskResult {
                task_id: "task-1".into(),
                result_message_id: "message-1".into(),
            },
            generation: 3,
        };
        let child = ExecutionSource {
            identity: ExecutionSourceIdentity::ChildResult {
                child_agent_id: "child-a".into(),
                task_id: "task-2".into(),
                result_message_id: "message-2".into(),
            },
            generation: 4,
        };
        let mut fences = AdmittedFences {
            source_revision: 3,
            work_item_source_revision: Some(1),
            work_item_generation: Some(1),
            rejoin: None,
            agent_control_revision: 1,
            host_registry_revision: 1,
        };
        assert!(validate_source_binding(&task, &binding, &fences)
            .unwrap_err()
            .contains("live rejoin"));

        fences.rejoin = Some(RejoinFence {
            obligation_id: "rejoin-task-1".into(),
            generation: 1,
            parent_turn_id: "turn-parent".into(),
        });
        assert!(validate_source_binding(&task, &binding, &fences).is_ok());

        fences.source_revision = 4;
        fences.rejoin = Some(RejoinFence {
            obligation_id: String::new(),
            generation: 1,
            parent_turn_id: "turn-parent".into(),
        });
        assert!(validate_source_binding(&child, &binding, &fences)
            .unwrap_err()
            .contains("identity"));
    }

    #[test]
    fn admission_rejects_stale_source_fence_and_invalid_provenance() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        );
        let mut stale = attempt("attempt-stale", None);
        stale.admitted_fences.source_revision = 2;
        assert!(admit_execution(&state, &AdmitExecution { attempt: stale })
            .unwrap_err()
            .contains("fences"));

        let mut invalid = attempt("attempt-invalid-provenance", None);
        invalid.provenance.origin = ExecutionOrigin::Operator;
        assert!(
            admit_execution(&state, &AdmitExecution { attempt: invalid })
                .unwrap_err()
                .contains("provenance")
        );
    }

    #[test]
    fn admission_fences_source_and_work_item_revisions_independently() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        );
        let mut independent = attempt("attempt-independent-fences", None);
        independent.source.generation = 7;
        independent.admitted_fences.source_revision = 7;
        let transition = admit_execution(
            &state,
            &AdmitExecution {
                attempt: independent,
            },
        )
        .unwrap();
        assert!(matches!(
            transition.state.work_items["work-a"].state,
            WorkItemExecutionState::InFlight { generation: 1, .. }
        ));
    }

    #[test]
    fn source_identity_must_match_bound_work_item() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        );
        let mut mismatched = attempt("attempt-mismatch", None);
        mismatched.source.identity = ExecutionSourceIdentity::WorkItemContinuation {
            work_item_id: "work-other".into(),
        };
        assert!(admit_execution(
            &state,
            &AdmitExecution {
                attempt: mismatched,
            },
        )
        .unwrap_err()
        .contains("unsupported source-binding"));
    }

    #[test]
    fn runtime_recovery_command_settles_without_workitem() {
        let state = ExecutionProtocolState::empty("agent-a");
        let recovery_attempt = ExecutionAttempt {
            attempt_id: "recovery-1".into(),
            agent_id: "agent-a".into(),
            source_message_id: None,
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::RuntimeRecovery {
                    recovery_id: "bootstrap-recovery".into(),
                },
                generation: 1,
            },
            binding: ExecutionBinding::Command,
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::RuntimeRecovery,
                trust: ExecutionTrust::RuntimeInstruction,
                priority: ExecutionPriority::Background,
                correlation_id: None,
                causation_id: None,
            },
            admitted_fences: AdmittedFences {
                source_revision: 1,
                work_item_source_revision: None,
                work_item_generation: None,
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
        };
        let admitted = admit_execution(
            &state,
            &AdmitExecution {
                attempt: recovery_attempt,
            },
        )
        .unwrap();
        assert!(admitted.state.work_items.is_empty());
        let settled = settle_execution(
            &admitted.state,
            &SettleExecution {
                outcome: ExecutionOutcomeRecord {
                    outcome_id: "recovery-outcome-1".into(),
                    attempt_id: "recovery-1".into(),
                    outcome: ExecutionOutcome::Command(CommandResult::Applied {
                        references: vec!["recovered:attempt-0".into()],
                    }),
                    created_at: "2026-08-01T00:01:00Z".into(),
                },
            },
        )
        .unwrap();
        assert!(settled.state.open_attempt().is_none());
        assert!(matches!(
            settled.state.attempts["recovery-1"].state,
            ExecutionAttemptState::Settled
        ));
    }

    #[test]
    fn command_binding_settles_with_unsupported_and_quarantined() {
        fn make_command_attempt(id: &str) -> ExecutionAttempt {
            ExecutionAttempt {
                attempt_id: id.into(),
                agent_id: "agent-a".into(),
                source_message_id: None,
                source: ExecutionSource {
                    identity: ExecutionSourceIdentity::RuntimeRecovery {
                        recovery_id: "recovery".into(),
                    },
                    generation: 1,
                },
                binding: ExecutionBinding::Command,
                provenance: ExecutionProvenance {
                    origin: ExecutionOrigin::RuntimeRecovery,
                    trust: ExecutionTrust::RuntimeInstruction,
                    priority: ExecutionPriority::Background,
                    correlation_id: None,
                    causation_id: None,
                },
                admitted_fences: AdmittedFences {
                    source_revision: 1,
                    work_item_source_revision: None,
                    work_item_generation: None,
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
            }
        }

        let state = ExecutionProtocolState::empty("agent-a");
        for (outcome_id, result) in [
            (
                "out-unsupported",
                CommandResult::Unsupported {
                    reason: "no matching candidate".into(),
                },
            ),
            (
                "out-quarantined",
                CommandResult::Quarantined {
                    reason: "untrusted source".into(),
                },
            ),
        ] {
            let admitted = admit_execution(
                &state,
                &AdmitExecution {
                    attempt: make_command_attempt("cmd-1"),
                },
            )
            .unwrap();
            let settled = settle_execution(
                &admitted.state,
                &SettleExecution {
                    outcome: ExecutionOutcomeRecord {
                        outcome_id: outcome_id.into(),
                        attempt_id: "cmd-1".into(),
                        outcome: ExecutionOutcome::Command(result),
                        created_at: "2026-08-01T00:01:00Z".into(),
                    },
                },
            )
            .unwrap();
            assert!(settled.state.open_attempt().is_none());
            assert!(matches!(
                settled.state.attempts["cmd-1"].state,
                ExecutionAttemptState::Settled
            ));
        }
    }

    #[test]
    fn settlement_rejects_owner_incompatible_outcome() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        );
        let admitted = admit_execution(
            &state,
            &AdmitExecution {
                attempt: attempt("attempt-1", None),
            },
        )
        .unwrap();
        let error = settle_execution(
            &admitted.state,
            &SettleExecution {
                outcome: ExecutionOutcomeRecord {
                    outcome_id: "outcome-1".into(),
                    attempt_id: "attempt-1".into(),
                    outcome: ExecutionOutcome::Conversation(ConversationOutcome::Replied),
                    created_at: "2026-08-01T00:01:00Z".into(),
                },
            },
        )
        .unwrap_err();
        assert!(error.contains("incompatible"));
    }
}
