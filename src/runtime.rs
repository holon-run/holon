mod bootstrap;
mod callback;
mod clock;
mod closure;
mod command_task;
mod continuation;
mod delivery;
mod failure;
mod first_run_intro;
mod lifecycle;
mod memory_refresh;
mod message_dispatch;
mod operator;
mod operator_dispatch;
mod provider_turn;
mod repair;
mod scheduler;
mod scheduler_acceptance;
mod scheduler_executor;
mod subagent;
mod task_state_reducer;
mod task_supervisor;
mod tasks;
#[cfg(test)]
mod test_util;
mod turn;
mod unsettled_claim;
mod waiting;
pub(crate) mod workspace;
pub(crate) mod workspace_control;
mod worktree;

pub use first_run_intro::maybe_enqueue_first_run_intro;
pub(crate) use lifecycle::LightweightAgentStateProjection;
pub(crate) use repair::is_wake_only_message;
pub use repair::{
    SchedulerRepairInspection, SchedulerRepairOperation, SchedulerRepairRequest,
    SchedulerRepairResult,
};
pub use scheduler_acceptance::{
    seed_scheduler_restart_fixture, seed_scheduler_terminal_recovery_fixture,
    SchedulerIngressAdmissionRestartFixture, SchedulerTerminalRecoveryFixture,
};
pub use tasks::{
    PickedWorkItem, WorkItemContinuationSummary, WorkItemFocusTransition,
    WorkItemFocusTransitionWarning,
};
pub(crate) use waiting::{WaitForRegistrationOutcome, WaitForScope, WaitForWakeKind};
pub(crate) use worktree::format_worktree_task_summary;

#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use arc_swap::ArcSwap;
use bootstrap::ConfigSnapshot;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, Notify, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

#[cfg(test)]
use crate::provider::{ConversationMessage, ProviderTurnRequest};
#[cfg(test)]
use crate::runtime_error::{RuntimeError, RuntimeErrorDomain};
use crate::{
    agent_memory::load_agent_memory,
    agent_template::discover_agent_templates_catalog,
    agents_md::load_agents_md,
    brief,
    config::{ModelRouteRef, RuntimeModelCatalog},
    context::{sync_agent_message_count, ContextConfig},
    host::RuntimeHostBridge,
    ingress::WakeDisposition,
    memory::{refresh_episode_memory, refresh_working_memory},
    prompt::{
        build_effective_prompt_with_apply_patch_surface,
        build_effective_prompt_with_apply_patch_surface_and_default_external_ingress,
        EffectivePrompt,
    },
    provider::{
        provider_attempt_timeline, AgentProvider, ModelBlock, ProviderBuiltinWebSearchCapability,
        ProviderNativeWebSearchKind, ProviderNativeWebSearchRequest,
    },
    queue::RuntimeQueue,
    runtime_db::{
        transitions::{
            PostCommitWarning, TransitionApplyResult, TransitionCommit, TransitionFaultPoint,
        },
        RuntimeDb, RuntimeStateTransitionConflict,
    },
    runtime_error::describe_runtime_error,
    runtime_event::RuntimeEventKind,
    skills::{
        effective_skill_root_registrations, find_skill_by_entrypoint, find_skill_by_script_path,
        skills_runtime_view_from_catalog, SkillVisibility,
    },
    storage::{to_json_value, AppStorage, PollActivityMarker},
    system::{
        EffectiveExecution, ExecutionScopeKind, ExecutionSnapshot, LocalSystem,
        WorkspaceAccessMode, WorkspaceProjectionKind, WorkspaceView,
    },
    tool::{ToolRegistry, ToolResult},
    types::LoadedAgentMemory,
    types::{
        ActiveWorkspaceEntry, AdmissionContext, AgentIdentityView, AgentKind,
        AgentModelOverrideAuditEvent, AgentModelSource, AgentModelState, AgentState,
        AgentStateChangedEvent, AgentStatus, AgentSummary, AuditEvent, AuthorityClass,
        BriefCreatedAuditEvent, BriefRecord, CallbackDeliveryMode, CallbackDeliveryPayload,
        CallbackDeliveryResult, CallbackIngressDisposition, ClosureDecision,
        ContinuationResolution, ControlAction, ExecCommandBatchItemStatus, ExecCommandBatchResult,
        ExecutionAdmissionProvenance, ExternalTriggerCapability, ExternalTriggerRecord,
        ExternalTriggerScope, ExternalTriggerStatus, ExternalTriggerSummary, LoadedAgentsMd,
        MessageBody, MessageDeliverySurface, MessageEnvelope, MessageKind,
        MessageLifecycleAuditEvent, MessageOrigin, PendingWakeHint, Priority, QueueEntryRecord,
        QueueEntryStatus, RuntimeFailurePhase, RuntimeFailureSummary, RuntimePosture,
        SkillActivationSource, SkillActivationState, SkillCatalogEntry, SkillLoadReason,
        SkillsRuntimeView, TaskKind, TaskLifecycleAuditEvent, TaskRecord, TaskRecoverySpec,
        TaskStatus, TimerRecord, TimerStatus, ToolExecutionRecord, TranscriptEntry,
        TranscriptEntryKind, TurnRecord, TurnTerminalKind, ViewImageObservation,
        WaitConditionRecord, WaitConditionStatus, WaitingReason, WorkItemExecutionBinding,
        WorkItemLifecycleAuditEvent, WorkItemRecord, WorkItemState, WorkspaceEntry,
        AGENT_HOME_WORKSPACE_ID,
    },
    web::{WebConfig, WebProviderKind},
};
use command_task::ManagedTaskHandle;
use continuation::{resolve_continuation, ContinuationTrigger};
#[cfg(test)]
use subagent::sanitize_subagent_result;
use turn::LoopControlOptions;

pub(crate) const ENQUEUE_AGENT_STATE_MAX_ATTEMPTS: usize = 3;
const AUTHORITY_BLOCKED_RETRY_SECONDS: i64 = 30;

#[derive(Debug, Clone)]
pub(super) struct WorkItemCompletionReportPromotion {
    pub(super) record: crate::types::WorkItemRecord,
    pub(super) brief_id: String,
    pub(super) continuation_resumed: Option<WorkItemContinuationSummary>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedWorkItemCompletion {
    pub(crate) record: crate::types::WorkItemRecord,
    pub(crate) brief: crate::types::BriefRecord,
    pub(crate) expected_execution_protocol_state:
        Option<crate::domain::execution_protocol::ExecutionProtocolState>,
    pub(crate) expected_agent_state: crate::types::AgentState,
    pub(crate) committed_agent_state: crate::types::AgentState,
    pub(crate) wait_conditions: Vec<crate::types::WaitConditionRecord>,
    pub(crate) continuations: Vec<crate::types::WorkItemContinuationFrame>,
    pub(crate) continuation_resumed: Option<WorkItemContinuationSummary>,
    pub(crate) audit_events: Vec<crate::types::AuditEvent>,
    pub(crate) index_changes: Vec<crate::runtime_db::RuntimeIndexChange>,
    pub(crate) tool_execution: Option<crate::types::ToolExecutionRecord>,
    pub(crate) transcript_entries: Vec<crate::types::TranscriptEntry>,
}

fn rebase_prepared_completion_agent_state(
    prepared: &PreparedWorkItemCompletion,
    baseline: &AgentState,
) -> Result<AgentState> {
    // The expected_agent_state was captured from guard.state (in-memory) at
    // completion preparation time, while baseline is guard.last_persisted_state
    // at commit time. Between these two reads, persist_state() may be called
    // (e.g. by promote_turn_active_skills or concurrent wake_hint coalescing),
    // updating last_persisted_state to the current guard.state. If guard.state
    // changed in any tracked field during that window, the strict equality
    // check would bail even though the completion is still valid.
    //
    // The committed_agent_state already contains the correct final values for
    // status, current_run_id, current_work_item_id, current_turn_work_item_id,
    // and current_execution_binding — they are unconditionally applied on top
    // of the baseline below. The DB-level OCC check (expected =
    // last_persisted_state) in the commit transaction catches real concurrent
    // writes. The work item revision is validated in the execution protocol
    // commit. Removing this redundant TOCTOU guard avoids false-positive
    // bail-outs from benign persist_state() calls.
    anyhow::ensure!(
        baseline.id == prepared.expected_agent_state.id,
        "completion commit agent identity changed before settlement"
    );
    let committed = &prepared.committed_agent_state;
    let mut state = baseline.clone();
    state.status = committed.status.clone();
    state.current_run_id = committed.current_run_id.clone();
    state.last_brief_at = committed.last_brief_at;
    state.current_work_item_id = committed.current_work_item_id.clone();
    state.current_turn_work_item_id = committed.current_turn_work_item_id.clone();
    state.current_execution_binding = committed.current_execution_binding.clone();
    Ok(state)
}

#[derive(Debug, Clone)]
pub(super) enum WorkItemCompletionReportPromotionOutcome {
    /// Completion changed the WorkItem state, but did not create a new
    /// user-facing report for terminal delivery.
    Unchanged(crate::types::WorkItemRecord),
    /// Completion promoted the assistant's same-round report into the
    /// WorkItem's canonical result brief.
    Promoted(WorkItemCompletionReportPromotion),
}

#[derive(Debug, Clone)]
pub(crate) enum WorkItemCompletionAuthority {
    AgentExecution(WorkItemExecutionBinding),
    Control,
}

impl WorkItemCompletionReportPromotionOutcome {
    pub(super) fn into_record(self) -> crate::types::WorkItemRecord {
        match self {
            Self::Unchanged(record) => record,
            Self::Promoted(promotion) => promotion.record,
        }
    }
}

#[derive(Debug, Clone)]
struct WorktreeSubagentResult {
    text: String,
    worktree_path: PathBuf,
    worktree_branch: String,
    changed_files: Vec<String>,
    failed: bool,
}

#[derive(Debug, Clone)]
pub struct ManagedWorktreeSeed {
    pub original_cwd: PathBuf,
    pub original_branch: String,
    pub worktree_path: PathBuf,
    pub worktree_branch: String,
}

#[derive(Debug, Clone)]
pub enum InitialWorkspaceBinding {
    Detached,
    Anchor(PathBuf),
    Entry(WorkspaceEntry),
}

impl From<PathBuf> for InitialWorkspaceBinding {
    fn from(value: PathBuf) -> Self {
        Self::Anchor(value)
    }
}

impl From<WorkspaceEntry> for InitialWorkspaceBinding {
    fn from(value: WorkspaceEntry) -> Self {
        Self::Entry(value)
    }
}

impl From<Option<WorkspaceEntry>> for InitialWorkspaceBinding {
    fn from(value: Option<WorkspaceEntry>) -> Self {
        match value {
            Some(value) => Self::Entry(value),
            None => Self::Detached,
        }
    }
}

pub(crate) fn agent_model_state_for_catalog(
    model_catalog: &RuntimeModelCatalog,
    base_context_config: &ContextConfig,
    state: &AgentState,
) -> AgentModelState {
    let effective_model = model_catalog.effective_model(state.model_override.as_ref());
    let active_model = state
        .last_requested_model
        .as_ref()
        .filter(|requested| *requested == &effective_model)
        .and_then(|_| state.last_active_model.clone())
        .unwrap_or_else(|| effective_model.clone());
    let fallback_active = active_model != effective_model;
    let effective_chain = model_catalog.provider_chain(state.model_override.as_ref());
    let resolved_policy =
        model_catalog.resolved_model_policy(base_context_config, state.model_override.as_ref());
    AgentModelState {
        source: if state.model_override.is_some() {
            AgentModelSource::AgentOverride
        } else {
            AgentModelSource::RuntimeDefault
        },
        runtime_default_model: model_catalog.default_model.clone(),
        effective_model: effective_model.clone(),
        requested_model: Some(effective_model),
        active_model: Some(active_model),
        fallback_active,
        effective_fallback_models: effective_chain.into_iter().skip(1).collect(),
        override_model: state.model_override.clone(),
        override_reasoning_effort: state.model_override_reasoning_effort.clone(),
        resolved_policy,
    }
}

pub(crate) fn lightweight_agent_list_waiting_reason(agent: &AgentState) -> Option<WaitingReason> {
    match agent.status {
        AgentStatus::AwaitingTask => Some(WaitingReason::AwaitingTaskResult),
        _ => None,
    }
}

#[derive(Clone)]
pub struct RuntimeHandle {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    agent: Mutex<RuntimeAgent>,
    projection_cache: Mutex<AgentRuntimeProjectionCache>,
    object_query_cache: Arc<crate::object_query_cache::ObjectQueryCache>,
    notify: Notify,
    storage: AppStorage,
    runtime_db: RuntimeDb,
    clock: Arc<dyn clock::Clock>,
    base_provider: Arc<dyn AgentProvider>,
    provider: RwLock<Arc<dyn AgentProvider>>,
    turn_fallback_model: RwLock<Option<ModelRouteRef>>,
    context_config: RwLock<ContextConfig>,
    config_snapshot: ArcSwap<ConfigSnapshot>,
    builtin_web_search_probe_cache:
        Mutex<HashMap<BuiltinWebSearchProbeKey, BuiltinWebSearchProbeCacheEntry>>,
    view_image_observation_cache:
        Mutex<HashMap<ViewImageObservationCacheKey, ViewImageObservation>>,
    model_discovery_refreshes: Mutex<HashSet<crate::config::ProviderId>>,
    callback_base_url: String,
    tools: ToolRegistry,
    system: Arc<LocalSystem>,
    default_agent_id: String,
    host_bridge: Option<RuntimeHostBridge>,
    task_handles: Mutex<HashMap<String, ManagedTaskHandle>>,
    recovered_tasks: Mutex<Option<Vec<TaskRecord>>>,
    recovered_timers: Mutex<Option<Vec<TimerRecord>>>,
    bootstrap_result: StdMutex<Option<std::result::Result<(), String>>>,
    bootstrap_notify: Notify,
    suppress_next_continue_active_tick: Mutex<bool>,
    shutdown_requested: AtomicBool,
    transition_faults: StdMutex<std::collections::VecDeque<TransitionFaultPoint>>,
    #[cfg(test)]
    completion_binding_replacement: StdMutex<Option<WorkItemExecutionBinding>>,
    #[cfg(test)]
    task_transition_conflicts_remaining: AtomicUsize,
    #[cfg(test)]
    terminal_task_transition_conflicts_remaining: AtomicUsize,
    #[cfg(test)]
    fail_after_next_runtime_claim: AtomicBool,
    #[cfg(test)]
    fail_non_retryable_after_next_runtime_claim: AtomicBool,
    #[cfg(test)]
    claim_work_item_plan_status_before_commit:
        StdMutex<Option<(String, crate::types::WorkItemPlanStatus)>>,
    transition_warnings: StdMutex<Vec<PostCommitWarning>>,
}

const SCHEDULER_ACCEPTANCE_FIXTURES_ENV: &str = "HOLON_SCHEDULER_ACCEPTANCE_FIXTURES";

fn boolean_env(name: &str) -> Result<Option<bool>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let value = match value.to_string_lossy().trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => return Err(anyhow!("{name} expects a boolean")),
    };
    Ok(Some(value))
}

pub fn require_scheduler_acceptance_fixtures_enabled() -> Result<()> {
    if boolean_env(SCHEDULER_ACCEPTANCE_FIXTURES_ENV)? != Some(true) {
        return Err(anyhow!(
            "scheduler acceptance fixtures require {SCHEDULER_ACCEPTANCE_FIXTURES_ENV}=true"
        ));
    }
    Ok(())
}

#[cfg(test)]
fn scheduler_acceptance_fixtures_enabled_from_values(
    acceptance_fixtures: Option<&str>,
) -> Result<bool> {
    fn parse(name: &str, value: Option<&str>) -> Result<Option<bool>> {
        let Some(value) = value else {
            return Ok(None);
        };
        match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(Some(true)),
            "0" | "false" | "no" | "off" => Ok(Some(false)),
            _ => Err(anyhow!("{name} expects a boolean")),
        }
    }
    Ok(parse(SCHEDULER_ACCEPTANCE_FIXTURES_ENV, acceptance_fixtures)? == Some(true))
}

#[cfg(test)]
mod scheduler_acceptance_gate_tests {
    use super::*;

    #[test]
    fn scheduler_acceptance_gate_requires_explicit_fixture_opt_in() {
        assert!(scheduler_acceptance_fixtures_enabled_from_values(Some("true")).unwrap());
        assert!(!scheduler_acceptance_fixtures_enabled_from_values(None).unwrap());
    }

    #[test]
    fn scheduler_acceptance_gate_rejects_invalid_fixture_configuration() {
        assert!(
            scheduler_acceptance_fixtures_enabled_from_values(Some("sometimes"))
                .unwrap_err()
                .to_string()
                .contains(SCHEDULER_ACCEPTANCE_FIXTURES_ENV)
        );
    }
}

fn work_item_continuation_resume_source(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    agent_id: &str,
    parent_work_item_id: &str,
    projected_parent: Option<&WorkItemRecord>,
    projected_wait_conditions: &[WaitConditionRecord],
) -> Result<(
    crate::domain::execution_protocol::WorkItemContinuationResumeSource,
    crate::domain::execution_protocol::WorkItemOutcome,
)> {
    use crate::domain::execution_protocol::{
        continuation_resume_outcome, WorkItemContinuationResumeSource,
    };

    let durable_parent = runtime_db
        .work_items()
        .latest(parent_work_item_id)?
        .ok_or_else(|| anyhow!("continuation parent WorkItem is missing"))?;
    let parent = projected_parent
        .filter(|parent| parent.id == parent_work_item_id)
        .unwrap_or(&durable_parent);
    anyhow::ensure!(
        parent.agent_id == agent_id && parent.state == WorkItemState::Open,
        "continuation parent WorkItem is not open for this agent"
    );
    let mut parent_waits = storage
        .raw_active_wait_conditions_for_agent(agent_id)?
        .into_iter()
        .filter(|wait| wait.work_item_id.as_deref() == Some(parent_work_item_id))
        .map(|wait| (wait.id.clone(), wait))
        .collect::<std::collections::BTreeMap<_, _>>();
    for wait in projected_wait_conditions {
        parent_waits.remove(&wait.id);
        if wait.agent_id == agent_id
            && wait.work_item_id.as_deref() == Some(parent_work_item_id)
            && wait.status == WaitConditionStatus::Active
        {
            parent_waits.insert(wait.id.clone(), wait.clone());
        }
    }
    let source = WorkItemContinuationResumeSource {
        work_item_revision: parent.revision,
        blocked_by: parent.blocked_by.clone(),
        active_wait_ids: parent_waits.into_keys().collect(),
    };
    let outcome = continuation_resume_outcome(parent_work_item_id, &source)
        .map_err(|error| anyhow!(error))?;
    Ok((source, outcome))
}

fn execution_protocol_completion_transition_from_prepared(
    record: &QueueEntryRecord,
    terminal_turn: &TurnRecord,
    prepared: &PreparedWorkItemCompletion,
) -> Result<crate::runtime_db::transitions::ExecutionProtocolTransition> {
    use crate::domain::execution_protocol::{
        CompleteWorkItemExecution, ConversationOutcome, ExecutionAttemptState, ExecutionBinding,
        ExecutionOutcome, ExecutionOutcomeRecord, ExecutionProtocolCommand, SettleExecution,
        WorkItemExecutionState, WorkItemOutcome,
    };

    anyhow::ensure!(
        record.status == QueueEntryStatus::Processed,
        "completion commit requires a processed source queue claim"
    );
    let state = prepared
        .expected_execution_protocol_state
        .as_ref()
        .ok_or_else(|| anyhow!("completion commit requires canonical execution state"))?;
    let intent = prepared
        .record
        .completion_intent
        .as_ref()
        .ok_or_else(|| anyhow!("completion commit intent is missing"))?;
    let activation_id = intent
        .source_activation_id
        .as_deref()
        .ok_or_else(|| anyhow!("completion commit activation identity is missing"))?;
    let attempt = state
        .attempts
        .get(activation_id)
        .ok_or_else(|| anyhow!("completion commit execution attempt is missing"))?;
    anyhow::ensure!(
        attempt.state == ExecutionAttemptState::Open,
        "completion commit requires an open execution attempt"
    );
    let authoritative = state
        .work_items
        .get(&prepared.record.id)
        .ok_or_else(|| anyhow!("completion commit WorkItem execution state is missing"))?;
    anyhow::ensure!(
        intent.work_item_id == prepared.record.id
            && intent.source_activation_id.as_deref() == Some(attempt.attempt_id.as_str())
            && intent.source_message_id.as_deref() == Some(record.message_id.as_str())
            && intent.source_turn_id.as_deref() == Some(terminal_turn.turn_id.as_str())
            && intent.result_brief_id.as_deref() == Some(prepared.brief.id.as_str())
            && prepared.record.result_brief_id.as_deref() == Some(prepared.brief.id.as_str()),
        "completion commit intent does not match the admitted execution"
    );
    anyhow::ensure!(
        prepared.brief.kind.is_success()
            && prepared.brief.work_item_id.as_deref() == Some(prepared.record.id.as_str())
            && prepared.brief.turn_id.as_deref() == Some(terminal_turn.turn_id.as_str())
            && prepared.brief.related_message_id.as_deref() == Some(record.message_id.as_str())
            && !prepared.brief.text.trim().is_empty(),
        "completion commit brief evidence is invalid"
    );

    let commands = match &attempt.binding {
        ExecutionBinding::WorkItem { work_item_id } => {
            anyhow::ensure!(
                work_item_id == &prepared.record.id,
                "completion commit WorkItem binding mismatch"
            );
            anyhow::ensure!(
                matches!(
                    &authoritative.state,
                    WorkItemExecutionState::InFlight {
                        attempt_id,
                        generation,
                    } if attempt_id == &attempt.attempt_id
                        && Some(*generation) == attempt.admitted_fences.work_item_generation
                ) && Some(intent.expected_work_revision)
                    == attempt.admitted_fences.work_item_source_revision,
                "completion commit execution attempt fence is stale"
            );
            vec![ExecutionProtocolCommand::Settle(SettleExecution {
                outcome: ExecutionOutcomeRecord {
                    outcome_id: format!("outcome:complete:{}", attempt.attempt_id),
                    attempt_id: attempt.attempt_id.clone(),
                    outcome: ExecutionOutcome::WorkItem(WorkItemOutcome::Complete {
                        completion: prepared.brief.id.clone(),
                    }),
                    created_at: record.updated_at.to_rfc3339(),
                },
            })]
        }
        ExecutionBinding::AgentLifecycle { agent_id } => {
            anyhow::ensure!(
                agent_id == &record.agent_id
                    && authoritative.source_revision == intent.expected_work_revision
                    && !matches!(
                        authoritative.state,
                        WorkItemExecutionState::InFlight { .. }
                            | WorkItemExecutionState::Terminal { .. }
                    ),
                "completion commit lifecycle WorkItem fence is stale"
            );
            vec![
                ExecutionProtocolCommand::Settle(SettleExecution {
                    outcome: ExecutionOutcomeRecord {
                        outcome_id: format!("outcome:complete:{}", attempt.attempt_id),
                        attempt_id: attempt.attempt_id.clone(),
                        outcome: ExecutionOutcome::Conversation(ConversationOutcome::Replied),
                        created_at: record.updated_at.to_rfc3339(),
                    },
                }),
                ExecutionProtocolCommand::CompleteWorkItem(Box::new(CompleteWorkItemExecution {
                    command_id: format!("completion:work_item:{}", attempt.attempt_id),
                    work_item_id: prepared.record.id.clone(),
                    expected: authoritative.clone(),
                    completion: prepared.brief.id.clone(),
                })),
            ]
        }
        ExecutionBinding::Conversation { .. } | ExecutionBinding::Command => {
            bail!("completion commit requires a WorkItem or agent-lifecycle execution")
        }
    };

    Ok(
        crate::runtime_db::transitions::ExecutionProtocolTransition {
            bootstrap: None,
            commands,
        },
    )
}

fn canonical_matching_terminal_turn(
    runtime_db: &RuntimeDb,
    record: &QueueEntryRecord,
    message: &MessageEnvelope,
    owner_work_item_id: Option<&str>,
    terminal_turn: Option<&TurnRecord>,
) -> Result<Option<TurnRecord>> {
    let matches_activation = |turn: &TurnRecord| {
        let matches_turn_identity = message
            .turn_id
            .as_deref()
            .is_none_or(|turn_id| turn_id == turn.turn_id)
            || turn.replay.as_ref().is_some_and(|replay| {
                replay.source_message_id == record.message_id
                    && message.turn_id.as_deref() == Some(replay.source_turn_id.as_str())
            });
        turn.terminal.is_some()
            && turn
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                == Some(record.message_id.as_str())
            && matches_turn_identity
            && turn.current_work_item_id.as_deref() == owner_work_item_id
    };
    if let Some(turn) = terminal_turn.filter(|turn| matches_activation(turn)) {
        return Ok(Some(turn.clone()));
    }
    Ok(runtime_db
        .turn_records()
        .recent_for_agent(&record.agent_id, usize::MAX)?
        .into_iter()
        .find(matches_activation))
}

fn execution_protocol_settlement_transition_from_facts(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    record: &QueueEntryRecord,
    terminal_turn: Option<&TurnRecord>,
) -> Result<crate::runtime_db::transitions::ExecutionProtocolTransition> {
    use crate::domain::execution_protocol::{
        CommandResult, ConversationOutcome, ExecutionAttemptState, ExecutionBinding,
        ExecutionOutcome, ExecutionOutcomeRecord, ExecutionProtocolCommand, InterruptExecution,
        SettleExecution, WaitReference, WorkItemOutcome,
    };

    let Some(state) = runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized(&record.agent_id)?
    else {
        return Ok(Default::default());
    };
    let Some(attempt) = execution_attempt_for_message(&state, &record.message_id) else {
        return Ok(Default::default());
    };
    if attempt.state != ExecutionAttemptState::Open {
        return Ok(Default::default());
    }
    let interrupted = |reason: &str| crate::runtime_db::transitions::ExecutionProtocolTransition {
        bootstrap: None,
        commands: vec![ExecutionProtocolCommand::Interrupt(InterruptExecution {
            attempt_id: attempt.attempt_id.clone(),
            outcome_id: format!("outcome:interrupted:{}", attempt.attempt_id),
            reason: reason.to_string(),
            interrupted_at: record.updated_at.to_rfc3339(),
        })],
    };
    if record.status != QueueEntryStatus::Processed {
        return Ok(interrupted("runtime_interrupted"));
    }
    let Some(message) = storage.read_message_by_id(&record.message_id)? else {
        return Ok(interrupted("source_message_missing"));
    };
    let owner_work_item_id = match &attempt.binding {
        ExecutionBinding::WorkItem { work_item_id } => Some(work_item_id.as_str()),
        ExecutionBinding::Conversation { .. }
        | ExecutionBinding::AgentLifecycle { .. }
        | ExecutionBinding::Command => None,
    };
    let matching_terminal_turn = canonical_matching_terminal_turn(
        runtime_db,
        record,
        &message,
        owner_work_item_id,
        terminal_turn,
    )?;
    let Some(terminal_turn) = matching_terminal_turn else {
        return Err(
            crate::domain::execution_protocol::ExecutionSettlementConflict::MissingTerminalTurn {
                attempt_id: attempt.attempt_id.clone(),
                message_id: record.message_id.clone(),
                owner_work_item_id: owner_work_item_id.map(ToString::to_string),
            }
            .into(),
        );
    };

    let outcome = match &attempt.binding {
        ExecutionBinding::WorkItem { work_item_id } => {
            let Some(authoritative_work) = state.work_items.get(work_item_id) else {
                return Ok(interrupted("work_item_execution_missing"));
            };
            let crate::domain::execution_protocol::WorkItemExecutionState::InFlight {
                generation,
                attempt_id,
            } = &authoritative_work.state
            else {
                return Ok(interrupted("work_item_execution_not_in_flight"));
            };
            if attempt_id != &attempt.attempt_id
                || attempt.admitted_fences.work_item_generation != Some(*generation)
            {
                return Ok(interrupted("work_item_execution_attempt_mismatch"));
            }
            let matching_continuations = runtime_db
                .work_item_continuations()
                .active_for_agent(&record.agent_id)?
                .into_iter()
                .filter(|frame| {
                    frame.suspended_work_item_id == *work_item_id
                        && frame.turn_id.as_deref() == Some(terminal_turn.turn_id.as_str())
                })
                .collect::<Vec<_>>();
            match matching_continuations.as_slice() {
                [frame] => {
                    let Some(target) = storage.latest_work_item(&frame.active_work_item_id)? else {
                        return Ok(interrupted("yield_target_missing"));
                    };
                    let Some(target_execution) = state.work_items.get(&target.id) else {
                        return Ok(interrupted("yield_target_execution_missing"));
                    };
                    if target.agent_id != record.agent_id
                        || target.state != crate::types::WorkItemState::Open
                        || target_execution.source_revision != target.revision
                        || !matches!(
                            target_execution.state,
                            crate::domain::execution_protocol::WorkItemExecutionState::Runnable { .. }
                        )
                    {
                        return Ok(interrupted("yield_target_not_runnable"));
                    }
                    ExecutionOutcome::WorkItem(WorkItemOutcome::Yield {
                        target_work_item_id: target.id,
                    })
                }
                [] => {
                    let Some(work_item) = runtime_db.work_items().latest(work_item_id)? else {
                        return Ok(interrupted("work_item_missing"));
                    };
                    let completion_intent = work_item.completion_intent.as_ref();
                    if let Some(intent) = completion_intent {
                        if intent.work_item_id != *work_item_id
                            || intent.source_activation_id.as_deref()
                                != Some(attempt.attempt_id.as_str())
                            || intent.source_message_id.as_deref()
                                != Some(record.message_id.as_str())
                            || intent.source_turn_id.as_deref()
                                != Some(terminal_turn.turn_id.as_str())
                            || Some(intent.expected_work_revision)
                                != attempt.admitted_fences.work_item_source_revision
                        {
                            return Ok(interrupted("completion_intent_mismatch"));
                        }
                        let Some(brief_id) = intent.result_brief_id.as_deref().filter(|brief_id| {
                            work_item.result_brief_id.as_deref() == Some(*brief_id)
                        }) else {
                            return Ok(interrupted("completion_brief_binding_missing"));
                        };
                        let Some(brief) = storage.read_brief_by_id(brief_id)?.filter(|brief| {
                            brief.kind.is_success()
                                && brief.work_item_id.as_deref() == Some(work_item_id.as_str())
                                && brief.turn_id.as_deref() == Some(terminal_turn.turn_id.as_str())
                                && brief.related_message_id.as_deref()
                                    == Some(record.message_id.as_str())
                                && !brief.text.trim().is_empty()
                        }) else {
                            return Ok(interrupted("completion_brief_evidence_missing"));
                        };
                        ExecutionOutcome::WorkItem(WorkItemOutcome::Complete {
                            completion: brief.id,
                        })
                    } else {
                        if work_item.state != crate::types::WorkItemState::Open
                            || work_item.result_brief_id.is_some()
                        {
                            return Ok(interrupted("completion_intent_missing"));
                        }
                        let recovery_wait_id = if matches!(
                            attempt.source.identity,
                            crate::domain::execution_protocol::ExecutionSourceIdentity::RuntimeRecovery {
                                ..
                            }
                        ) {
                            let unresolved_waits = storage
                                .raw_unresolved_wait_conditions_for_agent(&record.agent_id)?
                                .into_iter()
                                .filter(|wait| {
                                    wait.work_item_id.as_deref() == Some(work_item_id.as_str())
                                })
                                .collect::<Vec<_>>();
                            match unresolved_waits.as_slice() {
                                [] => None,
                                [wait] => Some(wait.id.clone()),
                                _ => return Ok(interrupted("wait_outcome_ambiguous")),
                            }
                        } else {
                            None
                        };
                        if let Some(wait_id) = recovery_wait_id {
                            ExecutionOutcome::WorkItem(WorkItemOutcome::Wait {
                                wait: WaitReference { wait_id },
                            })
                        } else {
                            // Settlement inspects raw durable wait facts so read-model readiness
                            // cannot become reverse authority over the canonical outcome.
                            let unresolved_waits = storage
                                .raw_unresolved_wait_conditions_for_agent(&record.agent_id)?
                                .into_iter()
                                .filter(|wait| {
                                    wait.work_item_id.as_deref() == Some(work_item_id.as_str())
                                })
                                .collect::<Vec<_>>();
                            let current_turn_waits = unresolved_waits
                                .iter()
                                .filter(|wait| {
                                    wait.status == crate::types::WaitConditionStatus::Active
                                        && wait.turn_id.as_deref()
                                            == Some(terminal_turn.turn_id.as_str())
                                })
                                .collect::<Vec<_>>();
                            match (unresolved_waits.as_slice(), current_turn_waits.as_slice()) {
                                ([], []) => {
                                    if authoritative_work.source_revision != work_item.revision {
                                        return Ok(interrupted(
                                            "work_item_execution_revision_mismatch",
                                        ));
                                    }
                                    ExecutionOutcome::WorkItem(WorkItemOutcome::Continue)
                                }
                                ([_], [wait]) => {
                                    ExecutionOutcome::WorkItem(WorkItemOutcome::Wait {
                                        wait: WaitReference {
                                            wait_id: wait.id.clone(),
                                        },
                                    })
                                }
                                ([_], []) => {
                                    return Ok(interrupted("wait_outcome_turn_mismatch"));
                                }
                                _ => return Ok(interrupted("wait_outcome_ambiguous")),
                            }
                        }
                    }
                }
                _ => return Ok(interrupted("yield_continuation_ambiguous")),
            }
        }
        ExecutionBinding::Conversation { .. } | ExecutionBinding::AgentLifecycle { .. } => {
            let active_waits = storage
                .latest_wait_conditions()?
                .into_iter()
                .filter(|wait| {
                    wait.agent_id == record.agent_id
                        && wait.work_item_id.is_none()
                        && wait.turn_id.as_deref() == Some(terminal_turn.turn_id.as_str())
                        && wait.status == crate::types::WaitConditionStatus::Active
                })
                .collect::<Vec<_>>();
            match active_waits.as_slice() {
                [] => ExecutionOutcome::Conversation(ConversationOutcome::Replied),
                [wait] => ExecutionOutcome::Conversation(ConversationOutcome::Wait {
                    wait: WaitReference {
                        wait_id: wait.id.clone(),
                    },
                }),
                _ => return Ok(interrupted("ambiguous_lifecycle_waits")),
            }
        }
        ExecutionBinding::Command => ExecutionOutcome::Command(CommandResult::Applied {
            references: vec![
                format!("message:{}", record.message_id),
                format!("turn:{}", terminal_turn.turn_id),
            ],
        }),
    };
    Ok(
        crate::runtime_db::transitions::ExecutionProtocolTransition {
            bootstrap: None,
            commands: vec![ExecutionProtocolCommand::Settle(SettleExecution {
                outcome: ExecutionOutcomeRecord {
                    outcome_id: format!("outcome:message:{}", record.message_id),
                    attempt_id: attempt.attempt_id.clone(),
                    outcome,
                    created_at: record.updated_at.to_rfc3339(),
                },
            })],
        },
    )
}

fn execution_settlement_conflict(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        source
            .downcast_ref::<crate::domain::execution_protocol::ExecutionSettlementConflict>()
            .is_some()
    })
}

fn execution_attempt_for_message<'a>(
    state: &'a crate::domain::execution_protocol::ExecutionProtocolState,
    message_id: &str,
) -> Option<&'a crate::domain::execution_protocol::ExecutionAttempt> {
    state
        .attempts
        .values()
        .filter(|attempt| attempt.source_message_id.as_deref() == Some(message_id))
        .max_by(|left, right| {
            left.admitted_at
                .cmp(&right.admitted_at)
                .then_with(|| left.attempt_id.cmp(&right.attempt_id))
        })
}

fn exact_task_result_wait_with_status(
    storage: &AppStorage,
    message: &MessageEnvelope,
    task_id: &str,
    work_item_id: &str,
    status_matches: impl Fn(&WaitConditionStatus) -> bool,
) -> Result<Option<WaitConditionRecord>> {
    let matching_waits = storage
        .latest_wait_conditions()?
        .into_iter()
        .filter(|wait| {
            wait.agent_id == message.agent_id
                && wait.work_item_id.as_deref() == Some(work_item_id)
                && status_matches(&wait.status)
                && wait.kind == crate::types::WaitConditionKind::Task
                && wait.trigger_message_id() == Some(message.id.as_str())
                && wait.wake_sources.iter().any(|source| {
                    matches!(
                        source,
                        crate::types::WakeSource::TaskResult { task_id: expected }
                            if expected == task_id
                    )
                })
        })
        .collect::<Vec<_>>();
    let [wait] = matching_waits.as_slice() else {
        return Ok(None);
    };
    Ok(Some(wait.clone()))
}

fn exact_triggered_or_resolved_task_result_wait(
    storage: &AppStorage,
    message: &MessageEnvelope,
    task_id: &str,
    work_item_id: &str,
) -> Result<Option<WaitConditionRecord>> {
    exact_task_result_wait_with_status(storage, message, task_id, work_item_id, |status| {
        matches!(
            status,
            WaitConditionStatus::Triggered | WaitConditionStatus::Resolved
        )
    })
}

fn exact_task_result_claim_wait(
    storage: &AppStorage,
    message: &MessageEnvelope,
    task_id: &str,
    work_item_id: &str,
) -> Result<Option<WaitConditionRecord>> {
    exact_task_result_wait_with_status(storage, message, task_id, work_item_id, |status| {
        matches!(
            status,
            WaitConditionStatus::Resolved | WaitConditionStatus::Cancelled
        )
    })
}

enum TaskResultClaimRecovery {
    Replayable {
        transition: crate::runtime_db::transitions::ExecutionProtocolTransition,
        reason: &'static str,
    },
    Revoked {
        wait: WaitConditionRecord,
        reason: &'static str,
    },
    RequiresInactiveRuntime,
    Ineligible {
        reason: &'static str,
    },
}

#[derive(Clone, Copy)]
enum TaskResultClaimRecoveryAuthority {
    Diagnostic,
    RuntimeTerminatedBootstrap,
}

fn exact_task_result_claim_recovery(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    message: &MessageEnvelope,
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
    work_item_id: &str,
    interrupted_at: chrono::DateTime<Utc>,
    authority: TaskResultClaimRecoveryAuthority,
) -> Result<TaskResultClaimRecovery> {
    use crate::domain::execution_protocol::{
        ExecutionSourceIdentity, RecoverInterruptedTaskResultClaim,
        RecoverUnadvancedTaskResultClaim,
    };

    let ExecutionSourceIdentity::TaskResult {
        task_id,
        result_message_id,
    } = &attempt.source.identity
    else {
        return Ok(TaskResultClaimRecovery::Ineligible {
            reason: "execution_source_not_task_result",
        });
    };
    if result_message_id != &message.id
        || message.kind != MessageKind::TaskResult
        || message.authority_class != AuthorityClass::RuntimeInstruction
        || message.admission_context != Some(AdmissionContext::RuntimeOwned)
        || message.delivery_surface != Some(MessageDeliverySurface::TaskRejoin)
        || message.task_id.as_deref() != Some(task_id.as_str())
        || message.work_item_id.as_deref() != Some(work_item_id)
        || !matches!(&message.origin, MessageOrigin::Task { task_id: origin } if origin == task_id)
    {
        return Ok(TaskResultClaimRecovery::Ineligible {
            reason: "task_result_message_identity_mismatch",
        });
    }
    let Some(task) = storage.latest_task_record(task_id)? else {
        return Ok(TaskResultClaimRecovery::Ineligible {
            reason: "task_result_task_missing",
        });
    };
    let Ok(rejoin) = task.rejoin_fence() else {
        return Ok(TaskResultClaimRecovery::Ineligible {
            reason: "task_result_rejoin_fence_missing",
        });
    };
    if task.agent_id != message.agent_id
        || task.work_item_id.as_deref() != Some(work_item_id)
        || task.parent_message_id.as_deref() != Some(message.id.as_str())
        || attempt.admitted_fences.rejoin.as_ref() != Some(&rejoin)
    {
        return Ok(TaskResultClaimRecovery::Ineligible {
            reason: "task_result_rejoin_identity_mismatch",
        });
    }
    let Some(wait) = exact_task_result_claim_wait(storage, message, task_id, work_item_id)? else {
        return Ok(TaskResultClaimRecovery::Ineligible {
            reason: "task_result_wait_missing_or_ambiguous",
        });
    };
    if wait.status == WaitConditionStatus::Cancelled {
        return Ok(TaskResultClaimRecovery::Revoked {
            wait,
            reason: "task_result_wait_cancelled",
        });
    }
    let Some(work_item) = runtime_db.work_items().latest(work_item_id)? else {
        return Ok(TaskResultClaimRecovery::Ineligible {
            reason: "task_result_work_item_missing",
        });
    };
    let Some(expected_source_revision) = attempt.admitted_fences.work_item_source_revision else {
        return Ok(TaskResultClaimRecovery::Ineligible {
            reason: "task_result_work_item_revision_fence_missing",
        });
    };
    if work_item.state != WorkItemState::Open || work_item.blocked_by.is_some() {
        return Ok(TaskResultClaimRecovery::Ineligible {
            reason: "task_result_work_item_not_runnable",
        });
    }
    let (command, reason) = match work_item.revision.cmp(&expected_source_revision) {
        std::cmp::Ordering::Greater => (
            crate::domain::execution_protocol::ExecutionProtocolCommand::
                RecoverInterruptedTaskResultClaim(Box::new(
                    RecoverInterruptedTaskResultClaim {
                        command_id: format!(
                            "recover_task_result_claim:{}:{}",
                            attempt.attempt_id, work_item.revision
                        ),
                        attempt_id: attempt.attempt_id.clone(),
                        outcome_id: format!("outcome:interrupted:{}", attempt.attempt_id),
                        work_item_id: work_item_id.to_string(),
                        task_id: task_id.clone(),
                        result_message_id: message.id.clone(),
                        wait_id: wait.id,
                        rejoin,
                        expected_source_revision,
                        source_revision: work_item.revision,
                        interrupted_at: interrupted_at.to_rfc3339(),
                    },
                )),
            "stale_task_result_claim_revision",
        ),
        std::cmp::Ordering::Equal => {
            if matches!(authority, TaskResultClaimRecoveryAuthority::Diagnostic) {
                return Ok(TaskResultClaimRecovery::RequiresInactiveRuntime);
            }
            (
                crate::domain::execution_protocol::ExecutionProtocolCommand::
                    RecoverUnadvancedTaskResultClaim(Box::new(
                        RecoverUnadvancedTaskResultClaim {
                            command_id: format!(
                                "recover_unadvanced_task_result_claim:{}:{}",
                                attempt.attempt_id, work_item.revision
                            ),
                            attempt_id: attempt.attempt_id.clone(),
                            outcome_id: format!("outcome:interrupted:{}", attempt.attempt_id),
                            work_item_id: work_item_id.to_string(),
                            task_id: task_id.clone(),
                            result_message_id: message.id.clone(),
                            wait_id: wait.id,
                            rejoin,
                            expected_source_revision,
                            interrupted_at: interrupted_at.to_rfc3339(),
                        },
                    )),
                "unadvanced_task_result_claim",
            )
        }
        std::cmp::Ordering::Less => {
            return Ok(TaskResultClaimRecovery::Ineligible {
                reason: "task_result_work_item_revision_regressed",
            });
        }
    };
    Ok(TaskResultClaimRecovery::Replayable {
        transition: crate::runtime_db::transitions::ExecutionProtocolTransition {
            bootstrap: None,
            commands: vec![command],
        },
        reason,
    })
}

fn scheduler_task_result_claim_recovery_candidates(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    agent_id: &str,
    execution_state: Option<&crate::domain::execution_protocol::ExecutionProtocolState>,
) -> Result<Vec<SchedulerTaskResultClaimRecoveryCandidate>> {
    let queue_entries = runtime_db
        .queue_entries()
        .recent(Some(agent_id), usize::MAX)?;
    execution_state
        .into_iter()
        .flat_map(|state| state.attempts.values())
        .filter(|attempt| {
            attempt.state == crate::domain::execution_protocol::ExecutionAttemptState::Open
        })
        .filter_map(|attempt| {
            let crate::domain::execution_protocol::ExecutionBinding::WorkItem { work_item_id } =
                &attempt.binding
            else {
                return None;
            };
            let message_id = attempt.source_message_id.as_deref()?;
            let entry = queue_entries.iter().find(|entry| {
                entry.message_id == message_id && entry.status == QueueEntryStatus::Dequeued
            })?;
            Some((attempt, work_item_id, entry))
        })
        .map(|(attempt, work_item_id, entry)| {
            let mut candidate = SchedulerTaskResultClaimRecoveryCandidate {
                message_id: entry.message_id.clone(),
                activation_id: attempt.attempt_id.clone(),
                work_item_id: work_item_id.clone(),
                queue_status: entry.status.clone(),
                health: SchedulerUnsettledClaimHealth::Unhealthy,
                lane_blocked: true,
                // Queue entries currently expose the latest durable update, not a
                // distinct claimed-at timestamp, so this is a diagnostic approximation.
                claim_age_seconds: Utc::now()
                    .signed_duration_since(entry.updated_at)
                    .num_seconds()
                    .max(0) as u64,
                expected_queue_entry: None,
                eligible: false,
                reason: "task_result_claim_evidence_incomplete".into(),
                recovery_decision: "no_op".into(),
                recovery_generation: u32::from(attempt.recovery_of_attempt_id.is_some()),
                evidence: vec![
                    format!("work_item:{work_item_id}"),
                    format!("open_execution_attempt:{}", attempt.attempt_id),
                    format!(
                        "admitted_work_item_revision:{}",
                        attempt
                            .admitted_fences
                            .work_item_source_revision
                            .unwrap_or_default()
                    ),
                ],
                proposed_queue_entry: None,
                proposed_command: None,
            };
            let Some(message) = storage.read_message_by_id(&entry.message_id)? else {
                candidate.reason = "message_missing".into();
                return Ok(candidate);
            };
            let recovery = exact_task_result_claim_recovery(
                storage,
                runtime_db,
                &message,
                attempt,
                work_item_id,
                Utc::now(),
                TaskResultClaimRecoveryAuthority::Diagnostic,
            )?;
            let replay_fence = match &recovery {
                TaskResultClaimRecovery::Replayable { .. }
                | TaskResultClaimRecovery::RequiresInactiveRuntime => {
                    unsettled_claim::ReplayFence::ExactReplayable
                }
                TaskResultClaimRecovery::Revoked { wait, reason } => {
                    candidate.reason = (*reason).into();
                    candidate
                        .evidence
                        .push(format!("cancelled_wait:{}", wait.id));
                    unsettled_claim::ReplayFence::Revoked
                }
                TaskResultClaimRecovery::Ineligible { reason } => {
                    candidate.reason = (*reason).into();
                    unsettled_claim::ReplayFence::Ambiguous
                }
            };
            if replay_fence == unsettled_claim::ReplayFence::Ambiguous
                && attempt.recovery_of_attempt_id.is_none()
            {
                return Ok(candidate);
            }
            let decision =
                unsettled_claim::plan_unsettled_claim(&unsettled_claim::UnsettledClaimFacts {
                    queue_status: entry.status.clone(),
                    attempt_state: attempt.state,
                    terminal_turn_completed: None,
                    replay_fence,
                    recovery_of_attempt_id: attempt.recovery_of_attempt_id.clone(),
                });
            if let unsettled_claim::UnsettledClaimDecision::InterruptAndQuarantine { reason } =
                decision
            {
                let mut proposed_entry = entry.clone();
                proposed_entry.status = QueueEntryStatus::Quarantined;
                proposed_entry.updated_at = Utc::now();
                candidate.eligible = true;
                candidate.health = SchedulerUnsettledClaimHealth::Recoverable;
                candidate.reason = reason.into();
                candidate.recovery_decision = "interrupt_and_quarantine".into();
                candidate.evidence.push("recovery_generation=1".to_string());
                candidate.proposed_queue_entry = Some(proposed_entry);
                candidate.expected_queue_entry = Some(entry.clone());
                candidate.proposed_command = Some(
                    crate::domain::execution_protocol::ExecutionProtocolCommand::Interrupt(
                        crate::domain::execution_protocol::InterruptExecution {
                            attempt_id: attempt.attempt_id.clone(),
                            outcome_id: format!("outcome:interrupted:{}", attempt.attempt_id),
                            reason: reason.into(),
                            interrupted_at: Utc::now().to_rfc3339(),
                        },
                    ),
                );
                return Ok(candidate);
            }
            if let unsettled_claim::UnsettledClaimDecision::QuarantineSettled { reason } = decision
            {
                let mut proposed_entry = entry.clone();
                proposed_entry.status = QueueEntryStatus::Quarantined;
                proposed_entry.updated_at = Utc::now();
                candidate.eligible = true;
                candidate.health = SchedulerUnsettledClaimHealth::Recoverable;
                candidate.reason = reason.into();
                candidate.recovery_decision = "quarantine_settled".into();
                candidate.evidence.push("recovery_generation=1".to_string());
                candidate.proposed_queue_entry = Some(proposed_entry);
                candidate.expected_queue_entry = Some(entry.clone());
                candidate.proposed_command = None;
                return Ok(candidate);
            }
            let (transition, reason) = match recovery {
                TaskResultClaimRecovery::Replayable { transition, reason } => (transition, reason),
                TaskResultClaimRecovery::RequiresInactiveRuntime => {
                    candidate.reason = "requires_inactive_runtime_recovery".into();
                    return Ok(candidate);
                }
                TaskResultClaimRecovery::Revoked { .. }
                | TaskResultClaimRecovery::Ineligible { .. } => return Ok(candidate),
            };
            let [command] = transition.commands.as_slice() else {
                return Ok(candidate);
            };
            let mut proposed_entry = entry.clone();
            proposed_entry.status = QueueEntryStatus::Queued;
            proposed_entry.updated_at = Utc::now();
            candidate.eligible = true;
            candidate.health = SchedulerUnsettledClaimHealth::Recoverable;
            candidate.reason = reason.into();
            candidate.recovery_decision = "interrupt_and_requeue".into();
            candidate.evidence.push(format!(
                "durable_work_item_revision:{}",
                runtime_db
                    .work_items()
                    .latest(work_item_id)?
                    .map(|work| work.revision)
                    .unwrap_or_default()
            ));
            candidate.proposed_queue_entry = Some(proposed_entry);
            candidate.expected_queue_entry = Some(entry.clone());
            candidate.proposed_command = Some(command.clone());
            Ok(candidate)
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerRecoveryReport {
    pub agent_id: String,
    pub partition_initialized: bool,
    pub execution_partition_initialized: bool,
    pub authority_inventory: Vec<SchedulerAuthorityInventoryEntry>,
    pub retired_rollout_metadata: SchedulerRetiredRolloutMetadata,
    pub candidates: Vec<SchedulerRecoveryCandidate>,
    pub task_result_claim_recoveries: Vec<SchedulerTaskResultClaimRecoveryCandidate>,
    pub continuation_reconciliations: Vec<SchedulerContinuationReconciliationCandidate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerTaskResultClaimRecoveryCandidate {
    pub message_id: String,
    pub activation_id: String,
    pub work_item_id: String,
    pub queue_status: QueueEntryStatus,
    pub health: SchedulerUnsettledClaimHealth,
    pub lane_blocked: bool,
    pub claim_age_seconds: u64,
    pub expected_queue_entry: Option<QueueEntryRecord>,
    pub eligible: bool,
    pub reason: String,
    pub recovery_decision: String,
    pub recovery_generation: u32,
    pub evidence: Vec<String>,
    pub proposed_queue_entry: Option<QueueEntryRecord>,
    pub proposed_command: Option<crate::domain::execution_protocol::ExecutionProtocolCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerUnsettledClaimHealth {
    Recoverable,
    Unhealthy,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerAuthorityInventoryEntry {
    pub storage: String,
    pub role: String,
    pub canonical_reader: bool,
    pub target: String,
    pub row_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerRetiredRolloutMetadata {
    pub retirement_marked: bool,
    pub compatibility_data_present: bool,
    pub protocol_mode: String,
    pub config_revision: u64,
    pub preflight_count: u64,
    pub manifest_count: u64,
    pub scenario_count: u64,
    pub authoritative_scenario_count: u64,
    pub stale_authoritative_scenario_count: u64,
    pub hard_blocker_count: u64,
    pub command_result_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerContinuationReconciliationCandidate {
    pub continuation_id: String,
    pub work_item_id: String,
    pub stale_active_work_item_id: Option<String>,
    pub active_work_item_id: String,
    pub eligible: bool,
    pub reason: String,
    pub evidence: Vec<String>,
    pub proposed_command: Option<crate::domain::execution_protocol::ExecutionProtocolCommand>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerRecoveryCandidate {
    pub kind: SchedulerRecoveryCandidateKind,
    pub message_id: String,
    pub activation_id: String,
    pub work_item_id: Option<String>,
    pub queue_status: QueueEntryStatus,
    pub terminal_turn_id: Option<String>,
    pub eligible: bool,
    pub reason: String,
    pub target_queue_status: Option<QueueEntryStatus>,
    pub evidence: Vec<String>,
    pub expected_queue_entry: Option<QueueEntryRecord>,
    pub proposed_queue_entry: Option<QueueEntryRecord>,
    pub proposed_commands: Vec<crate::domain::execution_protocol::ExecutionProtocolCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerRecoveryCandidateKind {
    UnsettledExecution,
    DequeuedClaim,
}

fn scheduler_continuation_reconciliation_candidates(
    runtime_db: &RuntimeDb,
    agent_id: &str,
    execution_state: Option<&crate::domain::execution_protocol::ExecutionProtocolState>,
    active_continuations: &[crate::types::WorkItemContinuationFrame],
) -> Result<Vec<SchedulerContinuationReconciliationCandidate>> {
    use crate::domain::execution_protocol::{
        ExecutionProtocolCommand, ReconcileWorkItemContinuationYield, WorkItemExecutionState,
    };

    let mut candidates = Vec::new();
    for frame in active_continuations {
        let mut candidate = SchedulerContinuationReconciliationCandidate {
            continuation_id: frame.id.clone(),
            work_item_id: frame.suspended_work_item_id.clone(),
            stale_active_work_item_id: None,
            active_work_item_id: frame.active_work_item_id.clone(),
            eligible: false,
            reason: "execution_partition_missing".into(),
            evidence: vec![format!("continuation:{}", frame.id)],
            proposed_command: None,
        };
        let Some(state) = execution_state else {
            candidates.push(candidate);
            continue;
        };
        if frame.id.is_empty()
            || frame.suspended_work_item_id.is_empty()
            || frame.active_work_item_id.is_empty()
        {
            candidate.reason = "identity_incomplete".into();
            candidates.push(candidate);
            continue;
        }
        let parent_edges = active_continuations
            .iter()
            .filter(|other| other.suspended_work_item_id == frame.suspended_work_item_id)
            .count();
        let child_edges = active_continuations
            .iter()
            .filter(|other| other.active_work_item_id == frame.active_work_item_id)
            .count();
        if parent_edges != 1 || child_edges != 1 {
            candidate.reason = "competing_active_continuation".into();
            candidates.push(candidate);
            continue;
        }
        let mut cursor = frame.active_work_item_id.as_str();
        let mut visited = HashSet::new();
        let mut cycle = false;
        while let Some(next) = active_continuations
            .iter()
            .find(|other| other.suspended_work_item_id == cursor)
        {
            if !visited.insert(next.id.as_str())
                || next.active_work_item_id == frame.suspended_work_item_id
            {
                cycle = true;
                break;
            }
            cursor = next.active_work_item_id.as_str();
        }
        if cycle {
            candidate.reason = "active_continuation_cycle".into();
            candidates.push(candidate);
            continue;
        }
        let Some(parent_execution) = state.work_items.get(&frame.suspended_work_item_id) else {
            candidate.reason = "parent_execution_missing".into();
            candidates.push(candidate);
            continue;
        };
        let WorkItemExecutionState::Paused { generation, reason } = &parent_execution.state else {
            candidate.reason = "parent_not_paused".into();
            candidates.push(candidate);
            continue;
        };
        let Some(stale_target) = reason.strip_prefix("yielded_to:") else {
            candidate.reason = "pause_not_yielded".into();
            candidates.push(candidate);
            continue;
        };
        candidate.stale_active_work_item_id = Some(stale_target.to_string());
        candidate.evidence.extend([
            format!("parent_generation:{generation}"),
            format!(
                "parent_source_revision:{}",
                parent_execution.source_revision
            ),
            format!("canonical_target:{stale_target}"),
            format!("frame_target:{}", frame.active_work_item_id),
        ]);
        if stale_target == frame.active_work_item_id {
            candidate.reason = "already_matched".into();
            candidates.push(candidate);
            continue;
        }
        let Some(parent) = runtime_db
            .work_items()
            .latest(&frame.suspended_work_item_id)?
        else {
            candidate.reason = "parent_missing".into();
            candidates.push(candidate);
            continue;
        };
        let Some(active) = runtime_db.work_items().latest(&frame.active_work_item_id)? else {
            candidate.reason = "active_target_missing".into();
            candidates.push(candidate);
            continue;
        };
        let Some(stale) = runtime_db.work_items().latest(stale_target)? else {
            candidate.reason = "stale_target_missing".into();
            candidates.push(candidate);
            continue;
        };
        if parent.agent_id != agent_id || active.agent_id != agent_id || stale.agent_id != agent_id
        {
            candidate.reason = "cross_agent_identity".into();
            candidates.push(candidate);
            continue;
        }
        if parent.state != WorkItemState::Open
            || parent.revision != parent_execution.source_revision
        {
            candidate.reason = "parent_source_fence_stale".into();
            candidates.push(candidate);
            continue;
        }
        if active.state != WorkItemState::Open {
            candidate.reason = "active_target_not_open".into();
            candidates.push(candidate);
            continue;
        }
        if stale.state != WorkItemState::Completed {
            candidate.reason = "stale_target_not_completed".into();
            candidates.push(candidate);
            continue;
        }
        candidate.eligible = true;
        candidate.reason = "stale_yield_target_reconcilable".into();
        candidate.proposed_command = Some(
            ExecutionProtocolCommand::ReconcileWorkItemContinuationYield(Box::new(
                ReconcileWorkItemContinuationYield {
                    command_id: format!("continuation:reconcile:{}", frame.id),
                    work_item_id: frame.suspended_work_item_id.clone(),
                    continuation_id: frame.id.clone(),
                    stale_active_work_item_id: stale.id.clone(),
                    active_work_item_id: active.id.clone(),
                    expected: parent_execution.clone(),
                    expected_frame_updated_at: frame.updated_at.to_rfc3339(),
                    active_work_item_source_revision: active.revision,
                    stale_active_work_item_source_revision: stale.revision,
                },
            )),
        );
        candidates.push(candidate);
    }
    candidates.sort_by(|left, right| left.continuation_id.cmp(&right.continuation_id));
    Ok(candidates)
}

pub fn scheduler_recovery_report(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    agent_id: &str,
) -> Result<SchedulerRecoveryReport> {
    let partition_initialized = runtime_db
        .transitions()
        .retired_scheduler_partition_exists(agent_id)?;
    let authority_inventory_records = runtime_db
        .transitions()
        .inspect_scheduler_authority_inventory()?;
    let authority_inventory = authority_inventory_records
        .into_iter()
        .map(|entry| SchedulerAuthorityInventoryEntry {
            storage: entry.storage.to_string(),
            role: entry.role.to_string(),
            canonical_reader: entry.canonical_reader,
            target: entry.target.to_string(),
            row_count: entry.row_count,
        })
        .collect();
    let retired = runtime_db
        .transitions()
        .inspect_retired_scheduler_rollout_metadata()?;
    let retired_rollout_metadata = SchedulerRetiredRolloutMetadata {
        retirement_marked: retired.retirement_marked,
        compatibility_data_present: retired.preflight_count > 0
            || retired.manifest_count > 0
            || retired.scenario_count > 0
            || retired.hard_blocker_count > 0
            || retired.command_result_count > 0,
        protocol_mode: retired.protocol_mode,
        config_revision: retired.config_revision,
        preflight_count: retired.preflight_count,
        manifest_count: retired.manifest_count,
        scenario_count: retired.scenario_count,
        authoritative_scenario_count: retired.authoritative_scenario_count,
        stale_authoritative_scenario_count: retired.stale_authoritative_scenario_count,
        hard_blocker_count: retired.hard_blocker_count,
        command_result_count: retired.command_result_count,
    };
    let execution_state = runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized(agent_id)?;
    let active_continuations = runtime_db
        .work_item_continuations()
        .active_for_agent(agent_id)?;
    let continuation_reconciliations = scheduler_continuation_reconciliation_candidates(
        runtime_db,
        agent_id,
        execution_state.as_ref(),
        &active_continuations,
    )?;
    let task_result_claim_recoveries = scheduler_task_result_claim_recovery_candidates(
        storage,
        runtime_db,
        agent_id,
        execution_state.as_ref(),
    )?;
    let all_queue_entries = runtime_db
        .queue_entries()
        .recent(Some(agent_id), usize::MAX)?;
    let mut candidates = Vec::new();

    if let Some(state) = execution_state.as_ref() {
        for attempt in state.attempts.values() {
            let Some(message_id) = attempt.source_message_id.as_deref() else {
                continue;
            };
            if task_result_claim_recoveries
                .iter()
                .any(|candidate| candidate.activation_id == attempt.attempt_id)
            {
                continue;
            }
            let Some(entry) = all_queue_entries
                .iter()
                .find(|entry| entry.message_id == message_id)
                .cloned()
            else {
                candidates.push(SchedulerRecoveryCandidate {
                    kind: SchedulerRecoveryCandidateKind::UnsettledExecution,
                    message_id: message_id.to_string(),
                    activation_id: attempt.attempt_id.clone(),
                    work_item_id: match &attempt.binding {
                        crate::domain::execution_protocol::ExecutionBinding::WorkItem {
                            work_item_id,
                        } => Some(work_item_id.clone()),
                        _ => None,
                    },
                    queue_status: QueueEntryStatus::Processed,
                    terminal_turn_id: None,
                    eligible: false,
                    reason: "queue_entry_missing".into(),
                    target_queue_status: None,
                    evidence: vec![format!("execution_attempt:{}", attempt.attempt_id)],
                    expected_queue_entry: None,
                    proposed_queue_entry: None,
                    proposed_commands: Vec::new(),
                });
                continue;
            };
            let work_item_id = match &attempt.binding {
                crate::domain::execution_protocol::ExecutionBinding::WorkItem { work_item_id } => {
                    Some(work_item_id.clone())
                }
                _ => None,
            };
            if attempt.state == crate::domain::execution_protocol::ExecutionAttemptState::Open {
                let mut proposed = entry.clone();
                proposed.status = QueueEntryStatus::Processed;
                proposed.updated_at = Utc::now();
                let (transition, reason, terminal_turn_id) =
                    match execution_protocol_settlement_transition_from_facts(
                        storage, runtime_db, &proposed, None,
                    ) {
                        Ok(transition) if !transition.commands.is_empty() => {
                            let terminal_turn_id = runtime_db
                                .turn_records()
                                .recent_for_agent(agent_id, usize::MAX)?
                                .into_iter()
                                .find(|turn| {
                                    turn.terminal.is_some()
                                        && turn
                                            .trigger
                                            .as_ref()
                                            .and_then(|trigger| trigger.message_id.as_deref())
                                            == Some(message_id)
                                })
                                .map(|turn| turn.turn_id);
                            (transition, "execution_settlement", terminal_turn_id)
                        }
                        Ok(_) => continue,
                        Err(error) if execution_settlement_conflict(&error) => {
                            proposed.status = QueueEntryStatus::Aborted;
                            let transition = execution_protocol_settlement_transition_from_facts(
                                storage, runtime_db, &proposed, None,
                            )?;
                            (transition, "execution_interruption", None)
                        }
                        Err(error) => return Err(error),
                    };
                candidates.push(SchedulerRecoveryCandidate {
                    kind: SchedulerRecoveryCandidateKind::UnsettledExecution,
                    message_id: message_id.to_string(),
                    activation_id: attempt.attempt_id.clone(),
                    work_item_id,
                    queue_status: entry.status.clone(),
                    terminal_turn_id,
                    eligible: true,
                    reason: reason.into(),
                    target_queue_status: Some(proposed.status.clone()),
                    evidence: vec![
                        format!("execution_attempt:{}", attempt.attempt_id),
                        format!("execution_state:{:?}", attempt.state),
                    ],
                    expected_queue_entry: Some(entry),
                    proposed_queue_entry: Some(proposed),
                    proposed_commands: transition.commands,
                });
            } else if entry.status == QueueEntryStatus::Dequeued {
                let mut proposed = entry.clone();
                proposed.status = if attempt.state
                    == crate::domain::execution_protocol::ExecutionAttemptState::Settled
                {
                    QueueEntryStatus::Processed
                } else {
                    QueueEntryStatus::Aborted
                };
                proposed.updated_at = Utc::now();
                candidates.push(SchedulerRecoveryCandidate {
                    kind: SchedulerRecoveryCandidateKind::DequeuedClaim,
                    message_id: message_id.to_string(),
                    activation_id: attempt.attempt_id.clone(),
                    work_item_id,
                    queue_status: entry.status.clone(),
                    terminal_turn_id: None,
                    eligible: true,
                    reason: "terminal_execution_queue_pending".into(),
                    target_queue_status: Some(proposed.status.clone()),
                    evidence: vec![format!("execution_state:{:?}", attempt.state)],
                    expected_queue_entry: Some(entry),
                    proposed_queue_entry: Some(proposed),
                    proposed_commands: Vec::new(),
                });
            }
        }
    }

    for entry in all_queue_entries
        .iter()
        .filter(|entry| entry.status == QueueEntryStatus::Dequeued)
    {
        if candidates
            .iter()
            .any(|candidate| candidate.message_id == entry.message_id)
            || task_result_claim_recoveries
                .iter()
                .any(|candidate| candidate.message_id == entry.message_id)
        {
            continue;
        }
        candidates.push(SchedulerRecoveryCandidate {
            kind: SchedulerRecoveryCandidateKind::DequeuedClaim,
            message_id: entry.message_id.clone(),
            activation_id: scheduler_executor::canonical_activation_id(&entry.message_id),
            work_item_id: None,
            queue_status: entry.status.clone(),
            terminal_turn_id: None,
            eligible: false,
            reason: "execution_attempt_missing".into(),
            target_queue_status: None,
            evidence: vec!["queue_status=dequeued".into()],
            expected_queue_entry: Some(entry.clone()),
            proposed_queue_entry: None,
            proposed_commands: Vec::new(),
        });
    }
    candidates.sort_by(|left, right| left.message_id.cmp(&right.message_id));

    Ok(SchedulerRecoveryReport {
        agent_id: agent_id.to_string(),
        partition_initialized,
        execution_partition_initialized: execution_state.is_some(),
        authority_inventory,
        retired_rollout_metadata,
        candidates,
        task_result_claim_recoveries,
        continuation_reconciliations,
    })
}

pub fn apply_scheduler_recovery_plan(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    agent_id: &str,
    report: &SchedulerRecoveryReport,
) -> Result<(usize, Option<std::path::PathBuf>)> {
    apply_scheduler_recovery_plan_with_options(
        storage,
        runtime_db,
        agent_id,
        report,
        SchedulerRecoveryBackupPolicy::Required,
        None,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerRecoveryBackupPolicy {
    Required,
    SkipApproved,
}

pub fn apply_scheduler_recovery_plan_with_backup_policy(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    agent_id: &str,
    report: &SchedulerRecoveryReport,
    backup_policy: SchedulerRecoveryBackupPolicy,
) -> Result<(usize, Option<std::path::PathBuf>)> {
    apply_scheduler_recovery_plan_with_options(
        storage,
        runtime_db,
        agent_id,
        report,
        backup_policy,
        None,
    )
}

#[cfg(test)]
fn apply_scheduler_recovery_plan_with_task_result_fault(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    agent_id: &str,
    report: &SchedulerRecoveryReport,
    fault: crate::runtime_db::transitions::TransitionFaultPoint,
) -> Result<(usize, Option<std::path::PathBuf>)> {
    apply_scheduler_recovery_plan_with_options(
        storage,
        runtime_db,
        agent_id,
        report,
        SchedulerRecoveryBackupPolicy::SkipApproved,
        Some(fault),
    )
}

fn apply_scheduler_recovery_plan_with_options(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    agent_id: &str,
    report: &SchedulerRecoveryReport,
    backup_policy: SchedulerRecoveryBackupPolicy,
    task_result_fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
) -> Result<(usize, Option<std::path::PathBuf>)> {
    let recoveries = report
        .candidates
        .iter()
        .filter(|candidate| candidate.eligible)
        .filter_map(|candidate| {
            Some((
                candidate,
                candidate.expected_queue_entry.as_ref()?,
                candidate.proposed_queue_entry.as_ref()?,
            ))
        })
        .collect::<Vec<_>>();
    let continuation_reconciliation = report
        .continuation_reconciliations
        .iter()
        .filter(|candidate| candidate.eligible)
        .filter_map(|candidate| candidate.proposed_command.clone())
        .collect::<Vec<_>>();
    let task_result_claim_recovery = report
        .task_result_claim_recoveries
        .iter()
        .filter(|candidate| candidate.eligible)
        .filter_map(|candidate| {
            Some((
                candidate,
                candidate.expected_queue_entry.as_ref()?,
                candidate.proposed_queue_entry.as_ref()?,
                candidate.proposed_command.as_ref()?,
            ))
        })
        .collect::<Vec<_>>();
    if recoveries.is_empty()
        && continuation_reconciliation.is_empty()
        && task_result_claim_recovery.is_empty()
    {
        return Ok((0, None));
    }
    let backup_path = match backup_policy {
        SchedulerRecoveryBackupPolicy::Required => {
            Some(runtime_db.create_verified_backup("scheduler-recovery")?)
        }
        SchedulerRecoveryBackupPolicy::SkipApproved => None,
    };
    let mut applied = false;
    for (candidate, expected_entry, proposed_entry) in recoveries {
        let Some(current_entry) = runtime_db
            .queue_entries()
            .recent(Some(agent_id), usize::MAX)?
            .into_iter()
            .find(|entry| entry.message_id == candidate.message_id)
        else {
            return Err(anyhow!(
                "scheduler recovery queue entry disappeared: {}",
                candidate.message_id
            ));
        };
        if &current_entry != expected_entry && &current_entry != proposed_entry {
            return Err(anyhow!(
                "scheduler recovery queue fence changed: {}",
                candidate.message_id
            ));
        }
        if &current_entry == proposed_entry && candidate.proposed_commands.is_empty() {
            continue;
        }
        let commit = runtime_db
            .transitions()
            .commit_queue_with_execution_protocol(
                &crate::runtime_db::transitions::QueueTransitionCommand {
                    agent_id: agent_id.to_string(),
                    operation: if proposed_entry.status == QueueEntryStatus::Queued {
                        crate::runtime_db::transitions::QueueOperation::Requeue
                    } else {
                        crate::runtime_db::transitions::QueueOperation::Settle
                    },
                    mutation: crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                        expected: current_entry,
                        record: proposed_entry.clone(),
                    },
                    scheduler_claim_work_item: None,
                    agent_state: None,
                    message_evidence: Vec::new(),
                    transcript_entries: Vec::new(),
                    turn_record: None,
                    audit_events: vec![AuditEvent::legacy(
                        "scheduler_execution_recovered",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "message_id": candidate.message_id,
                            "activation_id": candidate.activation_id,
                            "work_item_id": candidate.work_item_id,
                            "reason": candidate.reason,
                        }),
                    )],
                    notify_scheduler: true,
                    fault: None,
                    brief_evidence: Vec::new(),
                },
                &crate::runtime_db::transitions::ExecutionProtocolTransition {
                    bootstrap: None,
                    commands: candidate.proposed_commands.clone(),
                },
            )?;
        applied |= commit.applied;
    }
    for (candidate, expected_entry, proposed_entry, command) in task_result_claim_recovery {
        let Some(current_entry) = runtime_db
            .queue_entries()
            .recent(Some(agent_id), usize::MAX)?
            .into_iter()
            .find(|entry| entry.message_id == candidate.message_id)
        else {
            return Err(anyhow!(
                "TaskResult claim recovery queue entry disappeared: {}",
                candidate.message_id
            ));
        };
        if &current_entry == proposed_entry {
            continue;
        }
        if &current_entry != expected_entry {
            return Err(anyhow!(
                "TaskResult claim recovery queue fence changed: {}",
                candidate.message_id
            ));
        }
        let commit = runtime_db
            .transitions()
            .commit_queue_with_execution_protocol(
                &crate::runtime_db::transitions::QueueTransitionCommand {
                    agent_id: agent_id.to_string(),
                    operation: if proposed_entry.status == QueueEntryStatus::Queued {
                        crate::runtime_db::transitions::QueueOperation::Requeue
                    } else {
                        crate::runtime_db::transitions::QueueOperation::Settle
                    },
                    mutation: crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                        expected: expected_entry.clone(),
                        record: proposed_entry.clone(),
                    },
                    scheduler_claim_work_item: None,
                    agent_state: None,
                    message_evidence: Vec::new(),
                    transcript_entries: Vec::new(),
                    turn_record: None,
                    audit_events: vec![AuditEvent::legacy(
                        "scheduler_task_result_claim_recovered",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "message_id": candidate.message_id,
                            "activation_id": candidate.activation_id,
                            "work_item_id": candidate.work_item_id,
                            "reason": candidate.reason,
                        }),
                    )],
                    notify_scheduler: true,
                    fault: task_result_fault,
                    brief_evidence: Vec::new(),
                },
                &crate::runtime_db::transitions::ExecutionProtocolTransition {
                    bootstrap: None,
                    commands: vec![command.clone()],
                },
            )?;
        applied |= commit.applied;
        if commit.applied {
            crate::diagnostics::record_unsettled_claim_recovery();
            if proposed_entry.status == QueueEntryStatus::Quarantined {
                crate::diagnostics::record_poison_message_quarantined();
            }
        }
    }
    if !continuation_reconciliation.is_empty() {
        let audit_events = report
            .continuation_reconciliations
            .iter()
            .filter(|candidate| candidate.eligible && candidate.proposed_command.is_some())
            .map(|candidate| {
                AuditEvent::legacy(
                    "work_item_continuation_reconciled",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "continuation_id": candidate.continuation_id,
                        "work_item_id": candidate.work_item_id,
                        "stale_active_work_item_id": candidate.stale_active_work_item_id,
                        "active_work_item_id": candidate.active_work_item_id,
                        "reason": candidate.reason,
                    }),
                )
            })
            .collect::<Vec<_>>();
        match runtime_db
            .transitions()
            .commit_execution_protocol_recovery_plan(
                agent_id,
                &continuation_reconciliation,
                &audit_events,
            )?
        {
            crate::runtime_db::transitions::execution_protocol_repository::ExecutionProtocolRecoveryCommitOutcome::Applied(
                changed,
            ) => applied |= changed,
            crate::runtime_db::transitions::execution_protocol_repository::ExecutionProtocolRecoveryCommitOutcome::Rejected {
                reason,
            } => {
                return Err(anyhow!(
                    "continuation reconciliation unexpectedly rejected: {reason}"
                ));
            }
        }
    }
    storage.append_event(&AuditEvent::legacy(
        "scheduler_recovery_applied",
        serde_json::json!({
            "agent_id": agent_id,
            "changed": applied,
            "backup_policy": backup_policy,
            "backup_created": backup_path.is_some(),
            "backup_path": backup_path,
            "execution_recovery_count": report.candidates.iter().filter(|candidate| candidate.eligible).count(),
            "continuation_reconciliation_count": continuation_reconciliation.len(),
            "task_result_claim_recovery_count": report
                .task_result_claim_recoveries
                .iter()
                .filter(|candidate| candidate.eligible)
                .count(),
        }),
    ))?;
    Ok((usize::from(applied), backup_path))
}

fn runtime_error_queue_settlement(
    message_kind: &MessageKind,
    error: &anyhow::Error,
) -> (QueueEntryStatus, &'static str) {
    let retry_task_result = matches!(message_kind, MessageKind::TaskResult)
        && error.chain().any(|source| {
            source
                .downcast_ref::<task_state_reducer::TaskTransitionRetryExhausted>()
                .is_some()
        });
    if retry_task_result {
        (
            QueueEntryStatus::Interrupted,
            "task_transition_retry_exhausted",
        )
    } else {
        (QueueEntryStatus::Aborted, "runtime_error")
    }
}

#[derive(Debug, Clone)]
struct AgentRuntimeProjectionCache {
    agent_id: String,
    tasks: HashMap<String, TaskRecord>,
    work_items: HashMap<String, crate::types::WorkItemRecord>,
    timers: HashMap<String, TimerRecord>,
    external_triggers: HashMap<String, ExternalTriggerRecord>,
}

impl AgentRuntimeProjectionCache {
    fn rebuild(
        agent_id: String,
        tasks: Vec<TaskRecord>,
        work_items: Vec<crate::types::WorkItemRecord>,
        timers: Vec<TimerRecord>,
        external_triggers: Vec<ExternalTriggerRecord>,
    ) -> Self {
        crate::diagnostics::record_runtime_projection_cache_rebuild();
        let task_agent_id = agent_id.clone();
        let work_item_agent_id = agent_id.clone();
        let timer_agent_id = agent_id.clone();
        let external_trigger_agent_id = agent_id.clone();
        Self {
            agent_id,
            tasks: latest_by(
                tasks
                    .into_iter()
                    .filter(|record| record.agent_id == task_agent_id),
                |record| record.id.clone(),
            ),
            work_items: latest_by(
                work_items
                    .into_iter()
                    .filter(|record| record.agent_id == work_item_agent_id),
                |record| record.id.clone(),
            ),
            timers: latest_by(
                timers
                    .into_iter()
                    .filter(|record| record.agent_id == timer_agent_id),
                |record| record.id.clone(),
            ),
            external_triggers: latest_by(
                external_triggers
                    .into_iter()
                    .filter(|record| record.target_agent_id == external_trigger_agent_id),
                |record| record.external_trigger_id.clone(),
            ),
        }
    }

    fn upsert_task(&mut self, record: TaskRecord) {
        if record.agent_id == self.agent_id {
            self.tasks.insert(record.id.clone(), record);
        }
    }

    fn upsert_work_item(&mut self, record: crate::types::WorkItemRecord) {
        if record.agent_id == self.agent_id {
            self.work_items.insert(record.id.clone(), record);
        }
    }

    fn upsert_timer(&mut self, record: TimerRecord) {
        if record.agent_id == self.agent_id {
            self.timers.insert(record.id.clone(), record);
        }
    }

    fn upsert_external_trigger(&mut self, record: ExternalTriggerRecord) {
        if record.target_agent_id == self.agent_id {
            self.external_triggers
                .insert(record.external_trigger_id.clone(), record);
        }
    }

    fn active_tasks(&self, limit: usize) -> Vec<TaskRecord> {
        let mut records = self
            .tasks
            .values()
            .filter(|record| {
                matches!(
                    record.status,
                    TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.created_at.cmp(&left.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        take_limit(records, limit)
    }

    fn latest_tasks(&self, limit: usize) -> Vec<TaskRecord> {
        let mut records = self.tasks.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.created_at.cmp(&left.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        take_limit(records, limit)
    }

    fn latest_work_items(&self, limit: usize) -> Vec<crate::types::WorkItemRecord> {
        let mut records = self.work_items.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.created_at.cmp(&left.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        take_limit(records, limit)
    }

    fn recent_timers(&self, limit: usize) -> Vec<TimerRecord> {
        let mut records = self.timers.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        take_limit(records, limit)
    }

    fn latest_external_triggers(&self) -> Vec<ExternalTriggerRecord> {
        let mut records = self.external_triggers.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        records
    }
}

fn latest_by<T, F>(records: impl IntoIterator<Item = T>, key: F) -> HashMap<String, T>
where
    F: Fn(&T) -> String,
{
    let mut latest = HashMap::new();
    for record in records {
        latest.insert(key(&record), record);
    }
    latest
}

fn take_limit<T>(mut records: Vec<T>, limit: usize) -> Vec<T> {
    if records.len() > limit {
        records.truncate(limit);
    }
    records
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ViewImageObservationCacheKey {
    pub(crate) visual_reference_id: String,
    pub(crate) prompt: String,
    pub(crate) observation_schema: String,
    pub(crate) generation_policy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BuiltinWebSearchProbeKey {
    provider_id: String,
    provider_model_ref: String,
    provider_transport: String,
    provider_base_url: String,
    advertised_tool_type: String,
    backend_kind: String,
}

impl BuiltinWebSearchProbeKey {
    fn from_capability(capability: &ProviderBuiltinWebSearchCapability) -> Self {
        Self {
            provider_id: capability.provider_id.clone(),
            provider_model_ref: capability.provider_model_ref.clone(),
            provider_transport: capability.provider_transport.clone(),
            provider_base_url: capability.provider_base_url.clone(),
            advertised_tool_type: capability.advertised_tool_type.clone(),
            backend_kind: capability.backend_kind.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuiltinWebSearchProbeCacheEntry {
    status: BuiltinWebSearchProbeStatus,
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[allow(dead_code)]
#[serde(rename_all = "snake_case")]
enum BuiltinWebSearchProbeStatus {
    Supported,
    Unsupported,
    TransientFailure,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BuiltinWebSearchSelectionStatus {
    Selected,
    Disabled,
    Unsupported,
    NotDeclared,
    NotRequested,
    TransientProbeFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BuiltinWebSearchSelectionDiagnostics {
    status: BuiltinWebSearchSelectionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_model_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    advertised_tool_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    backend_kind: Option<String>,
    probe_status: BuiltinWebSearchProbeStatus,
    probe_cache_hit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuiltinWebSearchSelection {
    request: Option<ProviderNativeWebSearchRequest>,
    diagnostics: BuiltinWebSearchSelectionDiagnostics,
}

#[derive(Debug)]
struct RuntimeAgent {
    state: AgentState,
    last_persisted_state: AgentState,
    queue: RuntimeQueue,
    current_run_abort: Option<CurrentRunAbortHandle>,
}

impl RuntimeAgent {
    fn persist_state(&mut self, storage: &AppStorage) -> Result<()> {
        let started = std::time::Instant::now();
        // persist_state bypasses OCC (uses upsert, not expected-snapshot
        // validation). This is intentional for control-plane operations
        // (Start/Stop/shutdown) that need to force-write agent state.
        // After a successful write, last_persisted_state is set to
        // self.state so subsequent commit_queue OCC checks use the
        // correct baseline. On failure, self.state is reverted.
        if let Err(error) = storage.write_agent(&self.state) {
            self.state = self.last_persisted_state.clone();
            crate::diagnostics::record_storage_persist_state(started.elapsed());
            return Err(error);
        }
        self.last_persisted_state = self.state.clone();
        crate::diagnostics::record_storage_persist_state(started.elapsed());
        Ok(())
    }

    fn restore_bootstrap_replay_message(
        &mut self,
        storage: &AppStorage,
        message: &MessageEnvelope,
    ) -> Result<()> {
        if self
            .queue
            .peek_next_matching(|queued| queued.id == message.id)
            .is_none()
        {
            self.queue.push(message.clone());
        }
        let queued_messages = self.queue.len();
        self.state.pending = queued_messages;
        scheduler_executor::apply_bootstrap_recovered_projection(
            &mut self.state,
            scheduler_executor::BootstrapRecoveryFacts { queued_messages },
        );
        self.persist_state(storage)
    }
}

#[derive(Debug, Clone)]
struct CurrentRunAbortHandle {
    run_id: String,
    token: CancellationToken,
    reason: Arc<StdMutex<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrentRunAbortMode {
    StopAfterAbort,
}

impl CurrentRunAbortMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StopAfterAbort => "stop_after_abort",
        }
    }
}

impl Default for CurrentRunAbortMode {
    fn default() -> Self {
        Self::StopAfterAbort
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentRunAbortRequest {
    pub run_id: Option<String>,
    pub mode: CurrentRunAbortMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentRunAbortOutcome {
    pub agent_id: String,
    pub run_id: String,
    pub mode: CurrentRunAbortMode,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CurrentRunAbortError {
    #[error("agent {agent_id} has no current run to abort")]
    NoCurrentRun { agent_id: String },
    #[error("stale run_id {requested_run_id}; current run is {current_run_id}")]
    StaleRunId {
        requested_run_id: String,
        current_run_id: String,
    },
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("current run aborted: {reason}")]
pub struct CurrentRunAborted {
    pub run_id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CurrentRunAbortSnapshot {
    pub(crate) run_id: String,
    pub(crate) token: CancellationToken,
    pub(crate) reason: Arc<StdMutex<String>>,
}

impl CurrentRunAbortSnapshot {
    pub(crate) fn reason(&self) -> String {
        self.reason
            .lock()
            .map(|reason| reason.clone())
            .unwrap_or_else(|_| "operator_aborted".into())
    }
}

impl RuntimeHandle {
    pub(super) fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.inner.clock.now()
    }

    fn take_transition_fault(&self) -> Option<TransitionFaultPoint> {
        self.inner
            .transition_faults
            .lock()
            .expect("transition fault plan lock poisoned")
            .pop_front()
    }

    pub(super) fn commit_work_item_transition(
        &self,
        command: &crate::runtime_db::transitions::WorkItemTransitionCommand,
    ) -> Result<crate::runtime_db::transitions::TransitionCommit> {
        self.commit_work_item_transition_with_execution(
            command,
            &crate::runtime_db::transitions::ExecutionProtocolTransition::default(),
        )
    }

    pub(super) fn commit_work_item_transition_with_execution(
        &self,
        command: &crate::runtime_db::transitions::WorkItemTransitionCommand,
        execution_protocol: &crate::runtime_db::transitions::ExecutionProtocolTransition,
    ) -> Result<crate::runtime_db::transitions::TransitionCommit> {
        self.inner
            .runtime_db
            .transitions()
            .commit_work_item_with_execution_protocol(command, execution_protocol)
    }

    pub(super) fn commit_work_item_focus_transition(
        &self,
        command: &crate::runtime_db::transitions::WorkItemFocusTransitionCommand,
    ) -> Result<crate::runtime_db::transitions::TransitionCommit> {
        self.commit_work_item_focus_transition_with_execution(
            command,
            &crate::runtime_db::transitions::ExecutionProtocolTransition::default(),
        )
    }

    pub(super) fn commit_work_item_focus_transition_with_execution(
        &self,
        command: &crate::runtime_db::transitions::WorkItemFocusTransitionCommand,
        execution_protocol: &crate::runtime_db::transitions::ExecutionProtocolTransition,
    ) -> Result<crate::runtime_db::transitions::TransitionCommit> {
        self.inner
            .runtime_db
            .transitions()
            .commit_work_item_focus_with_execution_protocol(command, execution_protocol)
    }

    pub(super) fn commit_task_transition(
        &self,
        command: &crate::runtime_db::transitions::TaskTransitionCommand,
    ) -> Result<crate::runtime_db::transitions::TransitionCommit> {
        self.inner
            .runtime_db
            .transitions()
            .commit_task_with_execution_protocol(
                command,
                &crate::runtime_db::transitions::ExecutionProtocolTransition::default(),
            )
    }

    pub(super) fn inject_next_acceptance_transition_fault(
        &self,
        fault: TransitionFaultPoint,
    ) -> Result<()> {
        require_scheduler_acceptance_fixtures_enabled()?;
        self.inject_next_transition_fault_unchecked(fault)
    }

    #[cfg(test)]
    pub(crate) fn inject_next_transition_fault(&self, fault: TransitionFaultPoint) {
        self.inject_next_transition_fault_unchecked(fault)
            .expect("a transition fault is already armed for this runtime fixture");
    }

    #[cfg(test)]
    pub(crate) fn inject_completion_binding_replacement_before_commit(
        &self,
        binding: WorkItemExecutionBinding,
    ) {
        let mut replacement = self
            .inner
            .completion_binding_replacement
            .lock()
            .expect("completion binding replacement lock poisoned");
        assert!(
            replacement.replace(binding).is_none(),
            "a completion binding replacement is already armed"
        );
    }

    #[cfg(test)]
    async fn apply_completion_binding_replacement_before_commit(&self) -> Result<()> {
        let replacement = self
            .inner
            .completion_binding_replacement
            .lock()
            .expect("completion binding replacement lock poisoned")
            .take();
        let Some(binding) = replacement else {
            return Ok(());
        };
        let mut guard = self.inner.agent.lock().await;
        guard.state.current_execution_binding = Some(binding);
        guard.persist_state(&self.inner.storage)
    }

    fn inject_next_transition_fault_unchecked(&self, fault: TransitionFaultPoint) -> Result<()> {
        let mut faults = self
            .inner
            .transition_faults
            .lock()
            .expect("transition fault plan lock poisoned");
        if !faults.is_empty() {
            return Err(anyhow!(
                "a transition fault is already armed for this runtime fixture"
            ));
        }
        faults.push_back(fault);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn inject_runtime_loop_failure_after_next_claim(&self) {
        self.inner
            .fail_after_next_runtime_claim
            .store(true, Ordering::SeqCst);
        self.inner.notify.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn inject_non_retryable_runtime_loop_failure_after_next_claim(&self) {
        self.inner
            .fail_non_retryable_after_next_runtime_claim
            .store(true, Ordering::SeqCst);
        self.inner.notify.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn inject_claim_work_item_plan_status_before_commit(
        &self,
        work_item_id: String,
        plan_status: crate::types::WorkItemPlanStatus,
    ) {
        let mut injected = self
            .inner
            .claim_work_item_plan_status_before_commit
            .lock()
            .expect("claim WorkItem mutation lock poisoned");
        assert!(
            injected.replace((work_item_id, plan_status)).is_none(),
            "a claim WorkItem mutation is already armed"
        );
    }

    #[cfg(test)]
    async fn apply_claim_work_item_plan_status_before_commit(&self) -> Result<()> {
        let injected = self
            .inner
            .claim_work_item_plan_status_before_commit
            .lock()
            .expect("claim WorkItem mutation lock poisoned")
            .take();
        let Some((work_item_id, plan_status)) = injected else {
            return Ok(());
        };
        self.update_work_item_fields(work_item_id, None, Some(plan_status), None, None, None)
            .await?;
        Ok(())
    }

    pub(super) fn take_transition_warnings(&self) -> Vec<PostCommitWarning> {
        std::mem::take(
            &mut *self
                .inner
                .transition_warnings
                .lock()
                .expect("transition warning lock poisoned"),
        )
    }

    pub(crate) async fn apply_transition_commit(
        &self,
        commit: TransitionCommit,
    ) -> TransitionApplyResult {
        if !commit.applied {
            return TransitionApplyResult::default();
        }
        let effects = commit.effects;
        let mut warnings = Vec::new();
        if effects.fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
            warnings.push(PostCommitWarning {
                effect: "projection_cache_update",
                message: "injected runtime transition post-commit fault".into(),
            });
        } else {
            let mut cache = self.inner.projection_cache.lock().await;
            for record in &effects.work_items {
                cache.upsert_work_item(record.clone());
            }
            for record in &effects.tasks {
                cache.upsert_task(record.clone());
            }
        }
        if let Some(mutation) = effects.agent_state.as_ref() {
            let mut guard = self.inner.agent.lock().await;
            if mutation
                .expected
                .as_ref()
                .is_none_or(|expected| guard.state == **expected)
            {
                guard.state = mutation.record.as_ref().clone();
                guard.last_persisted_state = mutation.record.as_ref().clone();
            } else {
                warnings.push(PostCommitWarning {
                    effect: "agent_state_projection_update",
                    message: "agent state changed after transition commit; retained newer in-memory state"
                        .into(),
                });
            }
        }
        if effects.fault == Some(TransitionFaultPoint::BeforeEventPublication) {
            warnings.push(PostCommitWarning {
                effect: "event_publication",
                message: "injected runtime transition post-commit fault".into(),
            });
        } else {
            warnings.extend(self.inner.storage.publish_transition_events(&effects));
        }
        warnings.extend(self.inner.storage.notify_transition_memory_index(&effects));
        if effects.notify_scheduler {
            if effects.fault == Some(TransitionFaultPoint::BeforeSchedulerNotification) {
                warnings.push(PostCommitWarning {
                    effect: "scheduler_notification",
                    message: "injected runtime transition post-commit fault".into(),
                });
            } else {
                self.inner.notify.notify_one();
            }
        }
        let result = TransitionApplyResult {
            applied: true,
            warnings,
        };
        for warning in &result.warnings {
            tracing::warn!(
                effect = warning.effect,
                error = %warning.message,
                "runtime transition committed with post-commit warning"
            );
        }
        self.inner
            .transition_warnings
            .lock()
            .expect("transition warning lock poisoned")
            .extend(result.warnings.iter().cloned());
        result
    }

    pub(crate) async fn record_timer_projection(&self, record: &TimerRecord) -> Result<()> {
        self.inner.storage.append_timer(record)?;
        self.inner
            .projection_cache
            .lock()
            .await
            .upsert_timer(record.clone());
        Ok(())
    }

    pub(crate) async fn cache_external_trigger_projection(&self, record: &ExternalTriggerRecord) {
        self.inner
            .projection_cache
            .lock()
            .await
            .upsert_external_trigger(record.clone());
    }

    pub(crate) fn work_item_written_event(
        &self,
        action: &str,
        record: &crate::types::WorkItemRecord,
        extra: Value,
    ) -> AuditEvent {
        let payload = WorkItemLifecycleAuditEvent::from_work_item(action, record);
        let mut event = AuditEvent::typed(RuntimeEventKind::WorkItemWritten, &payload)
            .expect("work item lifecycle payload must serialize");
        if let (Some(payload), Some(extra)) = (event.data.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                payload.insert(key.clone(), value.clone());
            }
        }
        event
    }

    pub(crate) fn work_item_plan_artifact_refreshed_event(
        &self,
        record: &crate::types::WorkItemRecord,
    ) -> Option<AuditEvent> {
        let Some(artifact) = record.plan_artifact.as_ref() else {
            return None;
        };
        Some(AuditEvent::legacy(
            "work_item_plan_artifact_refreshed",
            serde_json::json!({
                "work_item_id": record.id,
                "revision": record.revision,
                "plan_artifact_path": artifact.path,
                "plan_artifact_hash": artifact.hash,
                "plan_artifact_bytes": artifact.bytes,
                "plan_artifact_updated_at": artifact.updated_at,
                "preview_complete": artifact.preview_complete,
            }),
        ))
    }
}

impl RuntimeHandle {
    pub(crate) async fn update_agent_state<F>(&self, mutate: F) -> Result<AgentState>
    where
        F: FnOnce(&mut AgentState) -> Result<()>,
    {
        let mut guard = self.inner.agent.lock().await;
        mutate(&mut guard.state)?;
        guard.persist_state(&self.inner.storage)?;
        Ok(guard.state.clone())
    }

    fn build_execution_root_id(
        workspace_id: &str,
        projection_kind: WorkspaceProjectionKind,
        execution_root: &Path,
    ) -> Result<String> {
        workspace::build_execution_root_id(workspace_id, projection_kind, execution_root)
    }

    fn agent_home_workspace_entry(data_dir: &Path, agent_id: &str) -> crate::types::WorkspaceEntry {
        workspace::agent_home_workspace_entry(data_dir, agent_id)
    }

    pub fn storage(&self) -> &AppStorage {
        &self.inner.storage
    }

    #[cfg(test)]
    pub(crate) fn runtime_db(&self) -> &crate::runtime_db::RuntimeDb {
        &self.inner.runtime_db
    }

    pub fn object_query_cache(&self) -> Arc<crate::object_query_cache::ObjectQueryCache> {
        self.inner.object_query_cache.clone()
    }

    pub fn poll_activity_marker(&self) -> Result<PollActivityMarker> {
        self.inner.storage.poll_activity_marker()
    }

    pub async fn abort_current_run(
        &self,
        request: CurrentRunAbortRequest,
    ) -> Result<CurrentRunAbortOutcome> {
        let mut guard = self.inner.agent.lock().await;
        let agent_id = guard.state.id.clone();
        let Some(handle) = guard.current_run_abort.as_ref().cloned() else {
            return Err(CurrentRunAbortError::NoCurrentRun { agent_id }.into());
        };
        if let Some(expected_run_id) = request.run_id.as_deref() {
            if expected_run_id != handle.run_id {
                return Err(CurrentRunAbortError::StaleRunId {
                    requested_run_id: expected_run_id.to_string(),
                    current_run_id: handle.run_id.clone(),
                }
                .into());
            }
        }

        if let Ok(mut reason) = handle.reason.lock() {
            *reason = "operator_aborted".into();
        }
        handle.token.cancel();
        scheduler::apply_stop_projection(&mut guard.state);
        guard.persist_state(&self.inner.storage)?;
        drop(guard);

        self.inner.storage.append_event(&AuditEvent::legacy(
            "current_run_aborted",
            serde_json::json!({
                "agent_id": agent_id,
                "run_id": handle.run_id,
                "mode": request.mode.as_str(),
                "reason": "operator_aborted",
            }),
        ))?;
        self.inner.notify.notify_waiters();
        Ok(CurrentRunAbortOutcome {
            agent_id,
            run_id: handle.run_id,
            mode: request.mode,
        })
    }

    pub(crate) async fn current_run_abort_token(&self) -> Option<CurrentRunAbortSnapshot> {
        let guard = self.inner.agent.lock().await;
        guard
            .current_run_abort
            .as_ref()
            .map(|handle| CurrentRunAbortSnapshot {
                run_id: handle.run_id.clone(),
                token: handle.token.clone(),
                reason: handle.reason.clone(),
            })
    }

    pub fn all_events(&self) -> Result<Vec<AuditEvent>> {
        self.inner.storage.read_recent_events(usize::MAX)
    }

    pub fn all_messages(&self) -> Result<Vec<MessageEnvelope>> {
        self.inner.storage.read_all_messages()
    }

    pub fn all_tool_executions(&self) -> Result<Vec<ToolExecutionRecord>> {
        self.inner.storage.read_recent_tool_executions(usize::MAX)
    }

    pub async fn latest_task_records_snapshot(&self) -> Result<Vec<TaskRecord>> {
        let mut tasks_by_id = self
            .inner
            .storage
            .latest_task_records()?
            .into_iter()
            .map(|task| (task.id.clone(), task))
            .collect::<HashMap<_, _>>();
        for task in self
            .inner
            .projection_cache
            .lock()
            .await
            .latest_tasks(usize::MAX)
        {
            match tasks_by_id.entry(task.id.clone()) {
                Entry::Occupied(mut entry) => {
                    if task_state_reducer::should_ignore_task_update(
                        Some(entry.get().clone()),
                        &task,
                    ) {
                        continue;
                    }
                    entry.insert(task);
                }
                Entry::Vacant(entry) => {
                    entry.insert(task);
                }
            }
        }
        let mut tasks = tasks_by_id.into_values().collect::<Vec<_>>();
        tasks.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(tasks)
    }

    pub(crate) fn agent_home(&self) -> PathBuf {
        self.inner.storage.data_dir().to_path_buf()
    }

    pub fn workspace_root(&self) -> PathBuf {
        self.execution_root_sync()
    }

    pub(crate) fn system(&self) -> Arc<LocalSystem> {
        self.inner.system.clone()
    }

    pub(crate) fn web_config(&self) -> WebConfig {
        self.inner.config_snapshot.load().web_config.clone()
    }

    pub(crate) fn x_search_config(&self) -> Option<crate::config::XSearchRuntimeConfig> {
        self.inner.config_snapshot.load().x_search_config.clone()
    }

    fn user_home(&self) -> Option<PathBuf> {
        if let Some(provider_reconfig) =
            self.inner.config_snapshot.load().provider_reconfig.as_ref()
        {
            return Some(provider_reconfig.config.home_dir.clone());
        }
        std::env::var_os("HOME").map(PathBuf::from)
    }

    fn fallback_identity_view(&self, agent_id: &str) -> AgentIdentityView {
        let kind = if agent_id == self.inner.default_agent_id {
            AgentKind::Default
        } else {
            AgentKind::Named
        };
        AgentIdentityView {
            agent_id: agent_id.to_string(),
            kind,
            visibility: crate::types::AgentVisibility::Public,
            ownership: crate::types::AgentOwnership::SelfOwned,
            profile_preset: crate::types::AgentProfilePreset::PublicNamed,
            status: crate::types::AgentRegistryStatus::Active,
            is_default_agent: agent_id == self.inner.default_agent_id,
            parent_agent_id: None,
            lineage_parent_agent_id: None,
            delegated_from_task_id: None,
        }
    }

    pub(crate) async fn agent_identity_view(&self) -> Result<AgentIdentityView> {
        let agent_id = self.agent_id().await?;
        if let Some(bridge) = self.inner.host_bridge.as_ref() {
            if let Some(identity) = bridge.identity_for_agent(&agent_id).await? {
                return Ok(AgentIdentityView::from_record(
                    &identity,
                    &self.inner.default_agent_id,
                ));
            }
        }
        Ok(self.fallback_identity_view(&agent_id))
    }

    fn skill_visibility(&self, identity: &AgentIdentityView) -> SkillVisibility {
        if identity.kind == AgentKind::Default {
            SkillVisibility::DefaultAgent
        } else {
            SkillVisibility::NonDefaultAgent
        }
    }

    pub(crate) async fn inherit_from_parent_state(&self, parent_state: &AgentState) -> Result<()> {
        let next_state = {
            let guard = self.inner.agent.lock().await;
            let mut next_state = guard.state.clone();
            next_state.attached_workspaces =
                workspace::inherited_attached_workspaces_for_agent(parent_state, &next_state.id);
            next_state.active_workspace_entry = parent_state.active_workspace_entry.clone();
            next_state.worktree_session = parent_state.worktree_session.clone();
            workspace::canonicalize_agent_home_bindings(
                &mut next_state,
                self.inner.storage.data_dir(),
                &guard.state.id,
            )?;
            if next_state
                .active_workspace_entry
                .as_ref()
                .is_some_and(|entry| {
                    entry.workspace_id == AGENT_HOME_WORKSPACE_ID
                        || entry.workspace_id.starts_with("agent_home:")
                })
            {
                let access_mode = next_state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.access_mode)
                    .unwrap_or(WorkspaceAccessMode::ExclusiveWrite);
                next_state.active_workspace_entry =
                    Some(workspace::canonical_agent_home_active_entry(
                        self.inner.storage.data_dir(),
                        &guard.state.id,
                        access_mode,
                    )?);
                next_state.worktree_session = None;
            }
            next_state.execution_profile = parent_state.execution_profile.clone();
            next_state.model_override = parent_state.model_override.clone();
            next_state
        };
        if self
            .inner
            .config_snapshot
            .load()
            .provider_reconfig
            .is_some()
        {
            self.reconfigure_provider_for_state(&next_state).await?;
        }
        self.update_agent_state(|state| {
            *state = next_state;
            Ok(())
        })
        .await?;
        Ok(())
    }

    pub(crate) async fn inherit_attached_workspaces_from_parent_state(
        &self,
        parent_state: &AgentState,
    ) -> Result<()> {
        let next_state = {
            let guard = self.inner.agent.lock().await;
            let mut next_state = guard.state.clone();
            next_state.attached_workspaces =
                workspace::inherited_attached_workspaces_for_agent(parent_state, &next_state.id);
            next_state.active_workspace_entry = None;
            next_state.worktree_session = None;
            workspace::canonicalize_agent_home_bindings(
                &mut next_state,
                self.inner.storage.data_dir(),
                &guard.state.id,
            )?;
            next_state.execution_profile = parent_state.execution_profile.clone();
            next_state.model_override = parent_state.model_override.clone();
            next_state
        };
        if self
            .inner
            .config_snapshot
            .load()
            .provider_reconfig
            .is_some()
        {
            self.reconfigure_provider_for_state(&next_state).await?;
        }
        self.update_agent_state(|state| {
            *state = next_state;
            Ok(())
        })
        .await?;
        Ok(())
    }

    pub(crate) async fn workspace_view(&self) -> Result<WorkspaceView> {
        let guard = self.inner.agent.lock().await;
        self.workspace_view_from_state(&guard.state)
    }

    pub(crate) fn workspace_view_for_root(
        &self,
        execution_root: PathBuf,
        cwd: PathBuf,
        worktree_root: Option<PathBuf>,
    ) -> Result<WorkspaceView> {
        workspace::workspace_view_for_root(&self.inner.storage, execution_root, cwd, worktree_root)
    }

    fn workspace_view_from_state(&self, state: &AgentState) -> Result<WorkspaceView> {
        workspace::workspace_view_from_state(state, self.inner.storage.data_dir().to_path_buf())
    }

    fn execution_snapshot_for_view(
        &self,
        profile: crate::system::ExecutionProfile,
        workspace: &WorkspaceView,
        attached_workspace_ids: &[String],
    ) -> ExecutionSnapshot {
        let mut snapshot = workspace::execution_snapshot_for_view(
            profile,
            workspace,
            attached_workspace_ids,
            &self.inner.storage,
        );
        // Populate execution_roots from the runtime DB registry for all
        // attached workspaces, so the provider turn resolver can resolve
        // `?root=` parameters in workspace:// URIs.
        let repo = self.inner.runtime_db.execution_root_entries();
        let mut roots = Vec::new();
        for ws_id in attached_workspace_ids {
            if let Ok(entries) = repo.active_for_workspace(ws_id) {
                for entry in entries {
                    roots.push(crate::system::ExecutionRootRef {
                        execution_root_id: entry.execution_root_id,
                        workspace_id: entry.workspace_id,
                        filesystem_path: entry.filesystem_path,
                    });
                }
            }
        }
        snapshot.execution_roots = roots;
        snapshot
    }

    fn workspace_anchor_for_state_ref<'a>(&self, state: &'a AgentState) -> Option<&'a Path> {
        workspace::workspace_anchor_for_state_ref(state)
    }

    fn execution_root_sync(&self) -> PathBuf {
        workspace::execution_root_sync(&self.inner.storage)
    }

    pub(crate) async fn effective_execution(
        &self,
        scope: ExecutionScopeKind,
    ) -> Result<EffectiveExecution> {
        let guard = self.inner.agent.lock().await;
        let profile = guard.state.execution_profile.clone();
        let attached_workspace_ids = guard.state.attached_workspaces.clone();
        drop(guard);
        let workspace = self.workspace_view().await?;
        Ok(workspace::build_effective_execution(
            &self.inner.storage,
            scope,
            profile,
            workspace,
            &attached_workspace_ids,
        ))
    }

    pub(crate) async fn effective_execution_for_workspace(
        &self,
        scope: ExecutionScopeKind,
        workspace: WorkspaceView,
    ) -> Result<EffectiveExecution> {
        let guard = self.inner.agent.lock().await;
        let profile = guard.state.execution_profile.clone();
        let attached_workspace_ids = guard.state.attached_workspaces.clone();
        drop(guard);
        Ok(workspace::build_effective_execution(
            &self.inner.storage,
            scope,
            profile,
            workspace,
            &attached_workspace_ids,
        ))
    }

    pub async fn execution_snapshot(&self) -> Result<ExecutionSnapshot> {
        Ok(self
            .effective_execution(ExecutionScopeKind::AgentTurn)
            .await?
            .snapshot())
    }

    pub(crate) async fn loaded_agents_md(&self) -> Result<LoadedAgentsMd> {
        let guard = self.inner.agent.lock().await;
        self.loaded_agents_md_for_state(&guard.state)
    }

    fn loaded_agents_md_for_state(&self, state: &AgentState) -> Result<LoadedAgentsMd> {
        load_agents_md(
            self.user_home().as_deref(),
            self.agent_home().as_path(),
            self.workspace_anchor_for_state_ref(state),
        )
    }

    fn loaded_agent_memory_for_state(&self) -> Result<LoadedAgentMemory> {
        load_agent_memory(self.agent_home().as_path())
    }

    pub(crate) async fn skills_runtime_view(
        &self,
        identity: &AgentIdentityView,
    ) -> Result<SkillsRuntimeView> {
        let guard = self.inner.agent.lock().await;
        self.skills_runtime_view_for_state(&guard.state, identity)
            .await
    }

    async fn skills_runtime_view_for_state(
        &self,
        state: &AgentState,
        identity: &AgentIdentityView,
    ) -> Result<SkillsRuntimeView> {
        let skill_roots = effective_skill_root_registrations(
            self.skill_visibility(identity),
            self.user_home().as_deref(),
            &state.id,
            self.agent_home().as_path(),
            state
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.execution_root.as_path()),
        );
        let mut view = if let Some(bridge) = self.inner.host_bridge.as_ref() {
            let registry = bridge.skills_registry()?;
            let mut registry = registry.write().await;
            registry.sync_effective_roots(skill_roots.clone())?;
            skills_runtime_view_from_catalog(
                registry.catalog_for_roots(&skill_roots, None),
                &skill_roots,
                &state.active_skills,
            )
        } else {
            let mut registry = crate::skills::SkillsRegistry::new();
            registry.replace_roots(skill_roots.clone())?;
            skills_runtime_view_from_catalog(registry.catalog(), &skill_roots, &state.active_skills)
        };
        view.agent_templates_catalog = discover_agent_templates_catalog(
            self.user_home().as_deref(),
            self.agent_home().as_path(),
        );
        Ok(view)
    }

    pub(crate) async fn sync_effective_skill_roots_for_state(
        &self,
        state: &AgentState,
    ) -> Result<()> {
        let Some(bridge) = self.inner.host_bridge.as_ref() else {
            return Ok(());
        };
        let identity = self.agent_identity_view().await?;
        let skill_roots = effective_skill_root_registrations(
            self.skill_visibility(&identity),
            self.user_home().as_deref(),
            &state.id,
            self.agent_home().as_path(),
            state
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.execution_root.as_path()),
        );
        let registry = bridge.skills_registry()?;
        registry.write().await.sync_effective_roots(skill_roots)?;
        Ok(())
    }

    async fn begin_interactive_turn_with_provenance(
        &self,
        message: Option<&MessageEnvelope>,
        operator_binding_id: Option<&str>,
        operator_reply_route_id: Option<&str>,
        execution_admission_provenance: ExecutionAdmissionProvenance,
    ) -> Result<()> {
        let replay_source = if let Some(message) = message {
            if let Some(source_turn_id) = message.source_refs.get("replay_source_turn_id") {
                Some((
                    source_turn_id.clone(),
                    self.inner
                        .runtime_db
                        .turn_records()
                        .by_id(Some(&message.agent_id), source_turn_id)?,
                ))
            } else {
                None
            }
        } else {
            None
        };
        let terminal_source_turn = if replay_source.is_none() {
            if let Some(message) = message {
                if let Some(source_turn_id) = normalized_turn_id(message.turn_id.as_deref()) {
                    self.inner
                        .runtime_db
                        .turn_records()
                        .by_id(Some(&message.agent_id), &source_turn_id)?
                        .filter(|turn| turn.terminal.is_some())
                        .map(|turn| (source_turn_id, turn))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        let canonical_execution_binding = match &execution_admission_provenance {
            ExecutionAdmissionProvenance::Canonical { activation_id, .. } => {
                let message = message.ok_or_else(|| {
                    anyhow!("canonical execution admission requires a source message")
                })?;
                let attempt = self
                    .inner
                    .runtime_db
                    .transitions()
                    .load_execution_protocol_state_if_initialized(&message.agent_id)?
                    .and_then(|state| state.attempts.get(activation_id).cloned())
                    .ok_or_else(|| {
                        anyhow!("canonical execution admission references an unknown attempt")
                    })?;
                let work_item_id = match attempt.binding {
                    crate::domain::execution_protocol::ExecutionBinding::WorkItem {
                        work_item_id,
                    } => Some(work_item_id),
                    crate::domain::execution_protocol::ExecutionBinding::Conversation {
                        ..
                    }
                    | crate::domain::execution_protocol::ExecutionBinding::AgentLifecycle {
                        ..
                    }
                    | crate::domain::execution_protocol::ExecutionBinding::Command => None,
                };
                Some((activation_id.clone(), work_item_id))
            }
            ExecutionAdmissionProvenance::LegacyCompat { .. } => None,
        };
        if let Some(message) = message {
            let work_item_id = message.work_item_id.clone().or_else(|| {
                self.inner.agent.try_lock().ok().and_then(|guard| {
                    guard
                        .state
                        .current_turn_work_item_id
                        .clone()
                        .or_else(|| guard.state.current_work_item_id.clone())
                })
            });
            let activation_id = scheduler_executor::canonical_activation_id(&message.id);
            let scenario = match message.delivery_surface {
                Some(MessageDeliverySurface::TaskRejoin) => {
                    Some(scheduler::EXACT_TASK_REJOIN_SCENARIO)
                }
                Some(MessageDeliverySurface::RuntimeSystem) => {
                    Some(scheduler::WORK_ITEM_AUTONOMOUS_CONTINUATION_SCENARIO)
                }
                _ => None,
            };
            let authoritative_without_activation = work_item_id.is_some()
                && message.authority_class == AuthorityClass::RuntimeInstruction
                && scenario.is_some()
                && self
                    .inner
                    .runtime_db
                    .transitions()
                    .load_execution_protocol_state_if_initialized(&message.agent_id)?
                    .is_none_or(|state| !state.attempts.contains_key(&activation_id));
            if authoritative_without_activation {
                self.inner.storage.append_event(&AuditEvent::legacy(
                    "authoritative_work_item_turn_without_activation",
                    serde_json::json!({
                        "agent_id": message.agent_id,
                        "message_id": message.id,
                        "work_item_id": work_item_id,
                        "scenario_class": scenario.map(|scenario| scenario.as_str()),
                        "delivery_surface": message.delivery_surface,
                    }),
                ))?;
            }
        }
        let state = {
            let mut guard = self.inner.agent.lock().await;
            guard.state.turn_index += 1;
            let turn_id = if replay_source.is_some() || terminal_source_turn.is_some() {
                crate::ids::turn_id()
            } else {
                message
                    .and_then(|message| normalized_turn_id(message.turn_id.as_deref()))
                    .unwrap_or_else(crate::ids::turn_id)
            };
            guard.state.current_turn_id = Some(turn_id.clone());
            guard.state.last_turn_terminal = None;
            if let Some((_, work_item_id)) = canonical_execution_binding.as_ref() {
                guard.state.current_turn_work_item_id = work_item_id.clone();
            } else if let Some((_, source_turn)) = replay_source.as_ref() {
                guard.state.current_turn_work_item_id = source_turn
                    .as_ref()
                    .and_then(|turn| turn.current_work_item_id.clone());
            } else if guard.state.current_turn_work_item_id.is_none() {
                guard.state.current_turn_work_item_id = guard.state.current_work_item_id.clone();
            }
            guard.state.current_execution_binding = message.map(|message| {
                let work_item_id = canonical_execution_binding
                    .as_ref()
                    .map(|(_, work_item_id)| work_item_id.clone())
                    .unwrap_or_else(|| {
                        message
                            .work_item_id
                            .clone()
                            .or_else(|| guard.state.current_turn_work_item_id.clone())
                    });
                let claimed_work_revision = work_item_id
                    .as_deref()
                    .and_then(|work_item_id| {
                        self.inner
                            .runtime_db
                            .work_items()
                            .latest(work_item_id)
                            .ok()
                            .flatten()
                    })
                    .map(|work_item| work_item.revision);
                let activation_id = canonical_execution_binding
                    .as_ref()
                    .map(|(activation_id, _)| activation_id.clone());
                WorkItemExecutionBinding {
                    activation_id,
                    admission_provenance: Some(execution_admission_provenance.clone()),
                    source_message_id: message.id.clone(),
                    turn_id,
                    work_item_id,
                    claimed_work_revision,
                }
            });
            guard.state.current_turn_operator_binding_id =
                operator_binding_id.and_then(|binding_id| {
                    let binding_id = binding_id.trim();
                    if binding_id.is_empty() {
                        None
                    } else {
                        Some(binding_id.to_string())
                    }
                });
            guard.state.current_turn_operator_reply_route_id =
                operator_reply_route_id.and_then(|route| {
                    let route = route.trim();
                    if route.is_empty() {
                        None
                    } else {
                        Some(route.to_string())
                    }
                });
            guard.state.active_skills.retain(|skill| {
                matches!(skill.activation_state, SkillActivationState::SessionActive)
            });
            guard.persist_state(&self.inner.storage)?;
            guard.state.clone()
        };
        self.append_state_changed_events(&state)?;
        if let Some(message) = message {
            self.inner.storage.append_event(&AuditEvent::legacy(
                "turn_started",
                serde_json::json!({
                    "agent_id": message.agent_id.clone(),
                    "message_id": message.id.clone(),
                    "turn_id": state.current_turn_id.clone(),
                    "message_kind": message.kind.clone(),
                    "run_id": state.current_run_id,
                    "turn_index": state.turn_index,
                }),
            ))?;
            if let Some((source_turn_id, source_turn)) = replay_source {
                self.inner.storage.append_event(&AuditEvent::legacy(
                    "turn_replay_started",
                    serde_json::json!({
                        "agent_id": message.agent_id,
                        "message_id": message.id,
                        "source_turn_id": source_turn_id,
                        "replay_turn_id": state.current_turn_id,
                        "reason": "interrupted_queue_claim_reentry",
                        "source_work_item_id": source_turn
                            .as_ref()
                            .and_then(|turn| turn.current_work_item_id.clone()),
                        "prior_terminal": source_turn.and_then(|turn| turn.terminal),
                    }),
                ))?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    async fn begin_interactive_turn(
        &self,
        message: Option<&MessageEnvelope>,
        operator_binding_id: Option<&str>,
        operator_reply_route_id: Option<&str>,
    ) -> Result<()> {
        let provenance = if let Some(message) = message {
            let activation_id = scheduler_executor::canonical_activation_id(&message.id);
            let execution = self
                .inner
                .runtime_db
                .transitions()
                .load_execution_protocol_state_if_initialized(&message.agent_id)?;
            if execution
                .as_ref()
                .is_some_and(|state| state.attempts.contains_key(&activation_id))
            {
                let scenario_class =
                    scheduler::canonical_activation_candidate(message, None, None)?
                        .map(|candidate| candidate.scenario_class())
                        .unwrap_or(scheduler::EXACT_WAIT_RESUME_SCENARIO);
                ExecutionAdmissionProvenance::Canonical {
                    scenario_class,
                    activation_id,
                }
            } else {
                self.execution_admission_provenance(message, None, None)?
            }
        } else {
            ExecutionAdmissionProvenance::LegacyCompat {
                scenario_class: None,
                effective_mode: crate::domain::scheduler::ScenarioMode::Off,
            }
        };
        self.begin_interactive_turn_with_provenance(
            message,
            operator_binding_id,
            operator_reply_route_id,
            provenance,
        )
        .await
    }

    #[cfg(test)]
    async fn begin_interactive_turn_for_test(
        &self,
        operator_binding_id: Option<&str>,
        operator_reply_route_id: Option<&str>,
    ) -> Result<()> {
        self.begin_interactive_turn_with_provenance(
            None,
            operator_binding_id,
            operator_reply_route_id,
            ExecutionAdmissionProvenance::LegacyCompat {
                scenario_class: None,
                effective_mode: crate::domain::scheduler::ScenarioMode::Off,
            },
        )
        .await
    }

    fn operator_transport_from_message(
        message: &MessageEnvelope,
    ) -> (Option<String>, Option<String>) {
        let transport = message
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("operator_transport"))
            .cloned();
        let binding_id = transport
            .as_ref()
            .and_then(|metadata| metadata.get("binding_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|binding_id| !binding_id.is_empty())
            .map(ToString::to_string);
        let reply_route_id = transport
            .as_ref()
            .and_then(|metadata| metadata.get("reply_route_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|route| !route.is_empty())
            .map(ToString::to_string);
        (binding_id, reply_route_id)
    }

    pub(crate) async fn promote_turn_active_skills(&self) -> Result<()> {
        let mut guard = self.inner.agent.lock().await;
        for skill in &mut guard.state.active_skills {
            if matches!(skill.activation_state, SkillActivationState::TurnActive) {
                skill.activation_state = SkillActivationState::SessionActive;
            }
        }
        guard.persist_state(&self.inner.storage)?;
        Ok(())
    }

    pub(crate) async fn record_skill_tool_activation(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
        result: &ToolResult,
    ) -> Result<()> {
        match tool_name {
            "Read" | "ReadFile" => {
                if let Some(file_path) = input.get("file_path").and_then(|value| value.as_str()) {
                    self.record_skill_read_activation(file_path, SkillLoadReason::ReadSkillMd)
                        .await?;
                }
            }
            crate::tool::names::EXEC_COMMAND => {
                if let Some(command) = input.get("cmd").and_then(|value| value.as_str()) {
                    self.record_skill_command_activation(command).await?;
                }
            }
            crate::tool::names::EXEC_COMMAND_BATCH => {
                if let Some(batch) = result
                    .envelope
                    .result
                    .as_ref()
                    .and_then(decode_exec_command_batch_result)
                {
                    for item in batch.items {
                        if matches!(item.status, ExecCommandBatchItemStatus::Completed) {
                            self.record_skill_command_activation(&item.cmd).await?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) async fn record_skill_read_activation(
        &self,
        file_path: &str,
        load_reason: SkillLoadReason,
    ) -> Result<()> {
        let execution = self
            .effective_execution(ExecutionScopeKind::AgentTurn)
            .await?;
        let resolved_path = execution.workspace.resolve_read_path(file_path)?;
        let state_snapshot = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        let identity = self.agent_identity_view().await?;
        let skills = self
            .skills_runtime_view_for_state(&state_snapshot, &identity)
            .await?;
        let Some(skill) = skill_for_activation_path(&skills.discoverable_skills, &resolved_path)
        else {
            return Ok(());
        };
        let mut guard = self.inner.agent.lock().await;
        let turn_index = guard.state.turn_index;
        let agent_id = guard.state.id.clone();
        let run_id = guard.state.current_run_id.clone();

        let repeated = if let Some(existing) = guard
            .state
            .active_skills
            .iter_mut()
            .find(|record| record.skill_id == skill.skill_id)
        {
            existing.activation_state = SkillActivationState::TurnActive;
            existing.activation_source = SkillActivationSource::ImplicitFromCatalog;
            existing.activated_at_turn = turn_index;
            true
        } else {
            guard
                .state
                .active_skills
                .push(crate::types::ActiveSkillRecord {
                    skill_id: skill.skill_id.clone(),
                    name: skill.name.clone(),
                    path: skill.path.clone(),
                    scope: skill.scope.clone(),
                    agent_id: agent_id.clone(),
                    activation_source: SkillActivationSource::ImplicitFromCatalog,
                    activation_state: SkillActivationState::TurnActive,
                    activated_at_turn: turn_index,
                });
            false
        };
        guard.persist_state(&self.inner.storage)?;
        self.inner.storage.append_event(&AuditEvent::legacy(
            "skill_activated",
            serde_json::json!({
                "agent_id": agent_id,
                "skill_id": skill.skill_id,
                "skill_name": skill.name,
                "path": resolved_path,
                "entrypoint_path": skill.path,
                "scope": skill.scope,
                "activation_source": SkillActivationSource::ImplicitFromCatalog,
                "activation_state": SkillActivationState::TurnActive,
                "load_reason": load_reason,
                "turn_index": turn_index,
                "run_id": run_id,
                "repeated": repeated,
            }),
        ))?;
        Ok(())
    }

    async fn record_skill_command_activation(&self, command: &str) -> Result<()> {
        let execution = self
            .effective_execution(ExecutionScopeKind::AgentTurn)
            .await?;
        let state_snapshot = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        let identity = self.agent_identity_view().await?;
        let skills = self
            .skills_runtime_view_for_state(&state_snapshot, &identity)
            .await?;

        for skill in skills.discoverable_skills {
            if let Some((activation_path, load_reason)) =
                command_skill_activation(command, &skill, execution.workspace.workspace_anchor())
            {
                let activation_path = activation_path.to_string_lossy().into_owned();
                self.record_skill_read_activation(&activation_path, load_reason)
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn enqueue(&self, mut message: MessageEnvelope) -> Result<MessageEnvelope> {
        message.normalize_admission_fields();
        message.turn_id = normalized_turn_id(message.turn_id.as_deref());
        if message.turn_id.is_none() {
            message.turn_id = Some(crate::ids::turn_id());
        }
        for attempt in 0..ENQUEUE_AGENT_STATE_MAX_ATTEMPTS {
            match self.enqueue_attempt(&message).await {
                Ok(mut commit) => {
                    commit.effects.notify_scheduler = true;
                    self.apply_transition_commit(commit).await;
                    return Ok(message);
                }
                Err(error) => {
                    let can_retry = attempt + 1 < ENQUEUE_AGENT_STATE_MAX_ATTEMPTS
                        && retryable_enqueue_conflict(&error, message.agent_id.as_str());
                    if !can_retry
                        || !self
                            .refresh_enqueue_agent_state_baseline(&message.agent_id)
                            .await?
                    {
                        return Err(error);
                    }
                }
            }
        }
        unreachable!("enqueue attempts always return or retry")
    }

    async fn enqueue_attempt(&self, message: &MessageEnvelope) -> Result<TransitionCommit> {
        let wait_trigger = self.wait_trigger_transition_for_message(message)?;
        let message_is_new = self
            .inner
            .storage
            .read_message_by_id(&message.id)?
            .is_none();
        let existing_queue_entry = self.inner.runtime_db.queue_entries().latest(&message.id)?;
        if existing_queue_entry.as_ref().is_some_and(|entry| {
            matches!(
                entry.status,
                QueueEntryStatus::Dequeued
                    | QueueEntryStatus::Interjected
                    | QueueEntryStatus::Processed
                    | QueueEntryStatus::Aborted
                    | QueueEntryStatus::Dropped
                    | QueueEntryStatus::Quarantined
            )
        }) {
            return Ok(TransitionCommit::default());
        }
        let mut audit_events = vec![
            AuditEvent::legacy(
                "message_admitted",
                serde_json::json!({
                    "message_id": message.id.clone(),
                    "agent_id": message.agent_id.clone(),
                    "kind": message.kind.clone(),
                    "origin": message.origin.clone(),
                    "authority_class": message.authority_class,
                    "delivery_surface": message.delivery_surface,
                    "admission_context": message.admission_context,
                    "trigger_kind": message.trigger_kind,
                    "work_item_id": message.work_item_id.clone(),
                    "task_id": message.task_id.clone(),
                    "source_refs": message.source_refs.clone(),
                    "correlation_id": message.correlation_id.clone(),
                    "causation_id": message.causation_id.clone(),
                }),
            ),
            AuditEvent::typed(
                RuntimeEventKind::MessageEnqueued,
                &MessageLifecycleAuditEvent::from_message(&message),
            )?,
        ];
        if let Some(wait_trigger) = wait_trigger.as_ref() {
            audit_events.push(AuditEvent::legacy(
                "wait_condition_triggered",
                serde_json::json!({
                    "agent_id": message.agent_id,
                    "wait_condition_id": wait_trigger.record.id,
                    "trigger_message_id": message.id,
                    "work_item_id": wait_trigger.record.work_item_id,
                }),
            ));
        }
        let commit = {
            let mut guard = self.inner.agent.lock().await;
            let queue_needs_push = guard
                .queue
                .peek_next_matching(|queued| queued.id == message.id)
                .is_none();
            let expected_persisted_state = guard.last_persisted_state.clone();
            let mut committed_state = guard.state.clone();
            let previous_status = committed_state.status.clone();
            let previous_sleeping_until = committed_state.sleeping_until;
            committed_state.pending = guard
                .queue
                .len()
                .saturating_add(usize::from(queue_needs_push));
            committed_state.last_wake_reason = Some(format!("{:?}", message.kind));
            committed_state.total_message_count = self
                .inner
                .storage
                .count_messages()?
                .saturating_add(usize::from(message_is_new));
            if scheduler::apply_message_wake_projection(&mut committed_state) {
                audit_events.push(AuditEvent::legacy(
                    "scheduler_posture_decision",
                    serde_json::json!({
                        "boundary": "message_admission",
                        "reason": "message_admission_wake",
                        "previous_status": previous_status,
                        "next_status": committed_state.status,
                        "evidence": [
                            format!("message_id={}", message.id),
                            format!("message_kind={:?}", message.kind),
                            format!("previous_sleeping_until={previous_sleeping_until:?}"),
                        ],
                    }),
                ));
            }
            let mut commit = self
                .inner
                .runtime_db
                .transitions()
                .commit_queue_with_wait_trigger(
                    &crate::runtime_db::transitions::QueueTransitionCommand {
                        agent_id: message.agent_id.clone(),
                        operation: crate::runtime_db::transitions::QueueOperation::Admit,
                        mutation: crate::runtime_db::transitions::QueueMutation::Upsert(
                            QueueEntryRecord {
                                message_id: message.id.clone(),
                                agent_id: message.agent_id.clone(),
                                priority: message.priority.clone(),
                                status: QueueEntryStatus::Queued,
                                created_at: existing_queue_entry
                                    .as_ref()
                                    .map_or(message.created_at, |entry| entry.created_at),
                                updated_at: Utc::now(),
                            },
                        ),
                        scheduler_claim_work_item: None,
                        agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                            expected: Some(Box::new(expected_persisted_state)),
                            record: Box::new(committed_state.clone()),
                        }),
                        message_evidence: vec![message.clone()],
                        transcript_entries: Vec::new(),
                        turn_record: None,
                        audit_events,
                        notify_scheduler: true,
                        fault: self.take_transition_fault(),
                        brief_evidence: Vec::new(),
                    },
                    wait_trigger.as_ref(),
                )?;
            if queue_needs_push {
                guard.queue.push(message.clone());
            }
            guard.state = committed_state.clone();
            guard.last_persisted_state = committed_state;
            commit.effects.agent_state = None;
            commit
        };
        Ok(commit)
    }

    pub(crate) async fn refresh_enqueue_agent_state_baseline(
        &self,
        agent_id: &str,
    ) -> Result<bool> {
        let mut guard = self.inner.agent.lock().await;
        let Some(latest_persisted_state) = self.inner.runtime_db.agent_states().latest(agent_id)?
        else {
            return Ok(false);
        };
        if latest_persisted_state.id != agent_id
            || guard.state != guard.last_persisted_state
            || latest_persisted_state.pending != guard.queue.len()
        {
            return Ok(false);
        }
        guard.state = latest_persisted_state.clone();
        guard.last_persisted_state = latest_persisted_state;
        Ok(true)
    }

    pub(crate) fn append_audit_event(&self, kind: &str, data: serde_json::Value) -> Result<()> {
        self.inner
            .storage
            .append_event(&AuditEvent::legacy(kind, data))
    }

    #[cfg(test)]
    pub(crate) async fn commit_queue_settlement(
        &self,
        record: QueueEntryRecord,
        audit_events: Vec<AuditEvent>,
        notify_scheduler: bool,
    ) -> Result<bool> {
        self.commit_queue_terminal_settlement(record, audit_events, notify_scheduler, None)
            .await
    }

    async fn commit_queue_terminal_settlement(
        &self,
        record: QueueEntryRecord,
        audit_events: Vec<AuditEvent>,
        notify_scheduler: bool,
        terminal_transition: Option<&turn::TurnTerminalTransition>,
    ) -> Result<bool> {
        self.commit_queue_terminal_settlement_with_evidence(
            record,
            audit_events,
            notify_scheduler,
            terminal_transition,
            None,
            Vec::new(),
            Vec::new(),
        )
        .await
    }

    async fn commit_terminal_transition(
        &self,
        terminal_transition: &turn::TurnTerminalTransition,
        audit_events: Vec<AuditEvent>,
    ) -> Result<bool> {
        for attempt in 0..ENQUEUE_AGENT_STATE_MAX_ATTEMPTS {
            let agent_state = {
                let guard = self.inner.agent.lock().await;
                let mut state = guard.state.clone();
                state.current_turn_id = Some(terminal_transition.terminal.turn_id.clone());
                state.last_turn_terminal = Some(terminal_transition.terminal.clone());
                crate::runtime_db::transitions::AgentStateMutation {
                    expected: Some(Box::new(guard.last_persisted_state.clone())),
                    record: Box::new(state),
                }
            };
            let mut transition_audit_events = audit_events.clone();
            transition_audit_events.push(AuditEvent::legacy(
                "turn_terminal",
                serde_json::to_value(&terminal_transition.terminal)?,
            ));
            transition_audit_events.push(Self::turn_record_audit_event(
                &terminal_transition.turn_record,
            ));
            let command = crate::runtime_db::transitions::TurnTerminalTransitionCommand {
                agent_id: terminal_transition.turn_record.agent_id.clone(),
                agent_state,
                turn_record: terminal_transition.turn_record.clone(),
                terminal_tool_executions: terminal_transition.terminal_tool_executions.clone(),
                audit_events: transition_audit_events,
                fault: self.take_transition_fault(),
            };
            match self
                .inner
                .runtime_db
                .transitions()
                .commit_turn_terminal(&command)
            {
                Ok(commit) => {
                    return Ok(self.apply_transition_commit(commit).await.applied);
                }
                Err(error) => {
                    let can_retry = attempt + 1 < ENQUEUE_AGENT_STATE_MAX_ATTEMPTS
                        && retryable_enqueue_conflict(
                            &error,
                            &terminal_transition.turn_record.agent_id,
                        );
                    if !can_retry
                        || !self
                            .refresh_enqueue_agent_state_baseline(
                                &terminal_transition.turn_record.agent_id,
                            )
                            .await?
                    {
                        return Err(error);
                    }
                }
            }
        }
        unreachable!("terminal transition OCC retry loop always returns or errors")
    }

    async fn commit_queue_terminal_settlement_with_evidence(
        &self,
        record: QueueEntryRecord,
        audit_events: Vec<AuditEvent>,
        notify_scheduler: bool,
        terminal_transition: Option<&turn::TurnTerminalTransition>,
        committed_agent_state: Option<AgentState>,
        transcript_entries: Vec<TranscriptEntry>,
        brief_evidence: Vec<BriefRecord>,
    ) -> Result<bool> {
        let prepared_completion = terminal_transition
            .and_then(|transition| transition.prepared_work_item_completion.as_ref());
        let mut execution_protocol = if let Some(prepared) = prepared_completion {
            execution_protocol_completion_transition_from_prepared(
                &record,
                terminal_transition
                    .map(|transition| &transition.turn_record)
                    .expect("prepared completion requires a terminal Turn"),
                prepared,
            )?
        } else {
            execution_protocol_settlement_transition_from_facts(
                &self.inner.storage,
                &self.inner.runtime_db,
                &record,
                terminal_transition.map(|transition| &transition.turn_record),
            )?
        };
        if let Some(prepared) = prepared_completion {
            let state = prepared
                .expected_execution_protocol_state
                .as_ref()
                .ok_or_else(|| anyhow!("completion commit requires canonical execution state"))?;
            for continuation in &prepared.continuations {
                if continuation.state != crate::types::WorkItemContinuationState::Resumed {
                    continue;
                }
                let parent = state
                    .work_items
                    .get(&continuation.suspended_work_item_id)
                    .ok_or_else(|| anyhow!("completion parent execution state is missing"))?;
                let (source, outcome) = work_item_continuation_resume_source(
                    &self.inner.storage,
                    &self.inner.runtime_db,
                    &record.agent_id,
                    &continuation.suspended_work_item_id,
                    None,
                    &prepared.wait_conditions,
                )?;
                execution_protocol.commands.push(
                        crate::domain::execution_protocol::ExecutionProtocolCommand::ResumeWorkItemContinuation(
                            Box::new(
                                crate::domain::execution_protocol::ResumeWorkItemContinuation {
                                    command_id: format!("completion:resume:{}", continuation.id),
                                    work_item_id: continuation.suspended_work_item_id.clone(),
                                    active_work_item_id: continuation.active_work_item_id.clone(),
                                    continuation_id: continuation.id.clone(),
                                    expected: parent.clone(),
                                    source,
                                    outcome,
                                },
                            ),
                        ),
                    );
            }
        }
        let original_audit_len = audit_events.len();
        let mut command = crate::runtime_db::transitions::QueueTransitionCommand {
            agent_id: record.agent_id.clone(),
            operation: crate::runtime_db::transitions::QueueOperation::Settle,
            mutation: crate::runtime_db::transitions::QueueMutation::Upsert(record.clone()),
            scheduler_claim_work_item: None,
            agent_state: None,
            message_evidence: Vec::new(),
            transcript_entries: transcript_entries
                .into_iter()
                .chain(
                    prepared_completion
                        .into_iter()
                        .flat_map(|prepared| prepared.transcript_entries.clone()),
                )
                .collect(),
            turn_record: terminal_transition.map(|transition| transition.turn_record.clone()),
            audit_events: audit_events.clone(),
            notify_scheduler,
            fault: None,
            brief_evidence: brief_evidence
                .into_iter()
                .chain(
                    prepared_completion
                        .into_iter()
                        .map(|prepared| prepared.brief.clone()),
                )
                .collect(),
        };
        for attempt in 0..ENQUEUE_AGENT_STATE_MAX_ATTEMPTS {
            // Rebuild guard-dependent fields from the current baseline.
            let agent_state = {
                let guard = self.inner.agent.lock().await;
                let mut state = if let Some(prepared) = prepared_completion {
                    rebase_prepared_completion_agent_state(prepared, &guard.last_persisted_state)?
                } else {
                    committed_agent_state
                        .clone()
                        .unwrap_or_else(|| guard.state.clone())
                };
                let agent_state = if let Some(transition) = terminal_transition {
                    state.current_turn_id = Some(transition.terminal.turn_id.clone());
                    state.last_turn_terminal = Some(transition.terminal.clone());
                    Some(crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(guard.last_persisted_state.clone())),
                        record: Box::new(state.clone()),
                    })
                } else {
                    None
                };
                agent_state
            };
            command.agent_state = agent_state;
            command.audit_events = audit_events.clone();
            command.audit_events.truncate(original_audit_len);
            if let Some(transition) = terminal_transition {
                command.audit_events.push(AuditEvent::legacy(
                    "turn_terminal",
                    serde_json::to_value(&transition.terminal)?,
                ));
                command
                    .audit_events
                    .push(Self::turn_record_audit_event(&transition.turn_record));
            }
            if let Some(prepared) = prepared_completion {
                command.audit_events.extend(prepared.audit_events.clone());
            }
            command.fault = self.take_transition_fault();
            let commit = if let Some(prepared) = prepared_completion {
                let tool_execution = prepared.tool_execution.clone().ok_or_else(|| {
                    anyhow!("completion commit is missing tool execution evidence")
                })?;
                self.inner
                    .runtime_db
                    .transitions()
                    .commit_queue_with_completion(
                        &command,
                        &execution_protocol,
                        &crate::runtime_db::transitions::CompletionTransition {
                            requires_execution_continuation: true,
                            work_items: vec![
                                crate::runtime_db::transitions::WorkItemMutation::Update {
                                    record: prepared.record.clone(),
                                    expected_revision: prepared.record.revision - 1,
                                },
                            ],
                            wait_conditions: prepared.wait_conditions.clone(),
                            continuations: prepared.continuations.clone(),
                            tool_execution,
                            index_changes: prepared.index_changes.clone(),
                        },
                    )
            } else if terminal_transition
                .is_some_and(|transition| !transition.terminal_tool_executions.is_empty())
            {
                self.inner
                    .runtime_db
                    .transitions()
                    .commit_queue_with_execution_protocol_and_terminal_tool_executions(
                        &command,
                        &execution_protocol,
                        &terminal_transition
                            .expect("terminal tool executions require a terminal transition")
                            .terminal_tool_executions,
                    )
            } else {
                self.inner
                    .runtime_db
                    .transitions()
                    .commit_queue_with_execution_protocol(&command, &execution_protocol)
            };
            match commit {
                Ok(commit) => {
                    return Ok(self.apply_transition_commit(commit).await.applied);
                }
                Err(error) => {
                    let can_retry = attempt + 1 < ENQUEUE_AGENT_STATE_MAX_ATTEMPTS
                        && retryable_enqueue_conflict(&error, &record.agent_id);
                    if !can_retry
                        || !self
                            .refresh_enqueue_agent_state_baseline(&record.agent_id)
                            .await?
                    {
                        return Err(error);
                    }
                }
            }
        }
        unreachable!("settlement OCC retry loop always returns or errors")
    }

    async fn maybe_supersede_queued_provider_recovery(
        &self,
        superseding_message: &MessageEnvelope,
        terminal_transition: Option<&turn::TurnTerminalTransition>,
    ) -> Result<usize> {
        let Some(turn_record) = terminal_transition.map(|transition| &transition.turn_record)
        else {
            return Ok(0);
        };
        if turn_record
            .terminal
            .as_ref()
            .is_none_or(|terminal| terminal.kind != TurnTerminalKind::Completed)
        {
            return Ok(0);
        }
        let made_progress = !turn_record.produced_brief_ids.is_empty()
            || !turn_record.tool_execution_ids.is_empty()
            || !turn_record.completed_work_item_ids.is_empty()
            || !turn_record.waiting_condition_ids.is_empty();
        if !made_progress {
            return Ok(0);
        }

        let turns = self
            .inner
            .runtime_db
            .turn_records()
            .recent_for_agent(&superseding_message.agent_id, usize::MAX)?;
        let queued = self
            .inner
            .runtime_db
            .queue_entries()
            .latest_all()?
            .into_iter()
            .filter(|entry| {
                entry.agent_id == superseding_message.agent_id
                    && entry.status == QueueEntryStatus::Queued
                    && entry.message_id != superseding_message.id
            })
            .collect::<Vec<_>>();
        let mut superseded = 0;

        for expected in queued {
            let Some(recovery_message) = self
                .inner
                .storage
                .read_message_by_id(&expected.message_id)?
            else {
                continue;
            };
            let Ok(selection) = turn::TurnModelSelection::from_message(&recovery_message) else {
                continue;
            };
            let Some(recovery) = selection.recovery else {
                continue;
            };
            if recovery_message.work_item_id != turn_record.current_work_item_id {
                continue;
            }
            let source_is_earlier = turns.iter().any(|turn| {
                turn.turn_id == recovery.source_turn_id && turn.turn_index < turn_record.turn_index
            });
            if !source_is_earlier {
                continue;
            }

            let mut dropped = expected.clone();
            dropped.status = QueueEntryStatus::Dropped;
            dropped.updated_at = self.now();
            let mut guard = self.inner.agent.lock().await;
            if guard
                .queue
                .peek_next_matching(|message| message.id == recovery_message.id)
                .is_none()
            {
                continue;
            }
            let mut next_state = guard.state.clone();
            next_state.pending = guard.queue.len().saturating_sub(1);
            let mut commit = self.inner.runtime_db.transitions().commit_queue(
                &crate::runtime_db::transitions::QueueTransitionCommand {
                    agent_id: recovery_message.agent_id.clone(),
                    operation: crate::runtime_db::transitions::QueueOperation::Settle,
                    mutation: crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                        expected,
                        record: dropped,
                    },
                    scheduler_claim_work_item: None,
                    agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(guard.last_persisted_state.clone())),
                        record: Box::new(next_state.clone()),
                    }),
                    message_evidence: Vec::new(),
                    transcript_entries: Vec::new(),
                    turn_record: None,
                    audit_events: vec![
                        AuditEvent::legacy(
                            "recovery_superseded",
                            serde_json::json!({
                                "agent_id": recovery_message.agent_id,
                                "recovery_message_id": recovery_message.id,
                                "fallback_model_ref": recovery.fallback_model_ref,
                                "source_turn_id": recovery.source_turn_id,
                                "source_message_id": recovery.source_message_id,
                                "superseding_message_id": superseding_message.id,
                                "superseding_turn_id": turn_record.turn_id,
                                "work_item_id": turn_record.current_work_item_id,
                            }),
                        ),
                        AuditEvent::legacy(
                            "queue_entry_settled",
                            serde_json::json!({
                                "message_id": recovery_message.id,
                                "message_kind": recovery_message.kind,
                                "status": QueueEntryStatus::Dropped,
                                "reason": "provider_recovery_superseded",
                            }),
                        ),
                    ],
                    notify_scheduler: true,
                    fault: self.take_transition_fault(),
                    brief_evidence: Vec::new(),
                },
            )?;
            if !commit.applied {
                continue;
            }
            let _ = guard
                .queue
                .pop_next_matching(|message| message.id == recovery_message.id);
            guard.state = next_state.clone();
            guard.last_persisted_state = next_state;
            commit.effects.agent_state = None;
            drop(guard);
            self.apply_transition_commit(commit).await;
            superseded += 1;
        }

        Ok(superseded)
    }

    pub(crate) fn persist_transcript_evidence(&self, entry: &TranscriptEntry) -> Result<()> {
        self.inner.storage.append_transcript_entry(entry)?;
        self.inner.notify.notify_one();
        Ok(())
    }

    pub(crate) fn persist_tool_execution_evidence(
        &self,
        record: &ToolExecutionRecord,
    ) -> Result<()> {
        self.inner.storage.append_tool_execution(record)?;
        self.inner.notify.notify_one();
        Ok(())
    }

    pub(crate) fn persist_brief_evidence(&self, brief: &BriefRecord) -> Result<()> {
        self.inner.storage.append_brief(brief)?;
        self.inner.notify.notify_one();
        Ok(())
    }

    pub async fn run(self) -> Result<()> {
        let bootstrap = async {
            self.bootstrap_recovery().await?;
            scheduler_executor::SchedulerDecisionExecutor::new(&self)
                .bootstrap_recovered()
                .await?;
            self.recover_scheduler_bootstrap_claims().await?;
            Ok(())
        }
        .await;
        self.complete_bootstrap(&bootstrap);
        bootstrap?;

        loop {
            let poll = scheduler_executor::SchedulerDecisionExecutor::new(&self)
                .poll()
                .await?;

            let scheduled = match poll {
                scheduler_executor::RunLoopPoll::Shutdown => return Ok(()),
                scheduler_executor::RunLoopPoll::Stopped(state, queue_len) => {
                    let projection = scheduler::SchedulerProjection::from_state_with_queue_len_at(
                        &self.inner.storage,
                        &state,
                        queue_len,
                        self.now(),
                    )?;
                    let decision = scheduler::decide_next_action(
                        &projection,
                        scheduler::SchedulerBoundary::RunLoop,
                        scheduler::SchedulerInput::Idle,
                    );
                    scheduler::append_scheduler_decision(
                        &self.inner.storage,
                        &self.inner.default_agent_id,
                        &decision,
                    )?;
                    return Ok(());
                }
                scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
                scheduler_executor::RunLoopPoll::AuthorityBlocked => {
                    let retry_at =
                        self.now() + chrono::Duration::seconds(AUTHORITY_BLOCKED_RETRY_SECONDS);
                    tokio::select! {
                        _ = self.inner.notify.notified() => {}
                        _ = self.inner.clock.sleep_until(retry_at) => {}
                    }
                    continue;
                }
                scheduler_executor::RunLoopPoll::Idle => {
                    let notified = self.inner.notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if self.maybe_emit_pending_system_tick(None).await? {
                        continue;
                    }
                    let idle_snapshot = {
                        let guard = self.inner.agent.lock().await;
                        (guard.state.clone(), guard.queue.len())
                    };
                    let projection = scheduler::SchedulerProjection::from_state_with_queue_len_at(
                        &self.inner.storage,
                        &idle_snapshot.0,
                        idle_snapshot.1,
                        self.now(),
                    )?;
                    let decision = scheduler::decide_next_action(
                        &projection,
                        scheduler::SchedulerBoundary::RunLoopIdle,
                        scheduler::SchedulerInput::Idle,
                    );
                    if !matches!(
                        decision.kind,
                        scheduler::SchedulerDecisionKind::Sleep
                            | scheduler::SchedulerDecisionKind::StayIdle
                    ) {
                        scheduler::append_scheduler_decision(
                            &self.inner.storage,
                            &self.inner.default_agent_id,
                            &decision,
                        )?;
                    }
                    let next_recheck_at = self.next_blocked_work_item_recheck_at().await?;
                    let idle_state = scheduler_executor::SchedulerDecisionExecutor::new(&self)
                        .transition_run_loop_idle_to_sleep(next_recheck_at)
                        .await?;
                    let Some(idle_state) = idle_state else {
                        continue;
                    };
                    self.append_state_changed_events(&idle_state)?;
                    if let Some(next_recheck_at) = next_recheck_at {
                        if next_recheck_at > self.now() {
                            tokio::select! {
                                _ = &mut notified => {}
                                _ = self.inner.clock.sleep_until(next_recheck_at) => {}
                            }
                        }
                    } else {
                        notified.await;
                    }
                    continue;
                }
            };

            let message = scheduled.message.clone();
            #[cfg(test)]
            if self
                .inner
                .fail_after_next_runtime_claim
                .swap(false, Ordering::SeqCst)
            {
                return Err(anyhow!(RuntimeError::new(
                    RuntimeErrorDomain::Storage,
                    "injected_runtime_storage_failure",
                    "injected agent runtime loop failure after queue claim",
                )
                .with_retryable(true)));
            }
            #[cfg(test)]
            if self
                .inner
                .fail_non_retryable_after_next_runtime_claim
                .swap(false, Ordering::SeqCst)
            {
                return Err(anyhow!(RuntimeError::new(
                    RuntimeErrorDomain::Runtime,
                    "injected_non_retryable_runtime_failure",
                    "injected non-retryable agent runtime loop failure after queue claim",
                )));
            }
            self.append_state_changed_events(&scheduled.running_state)?;

            let terminal_transition = match self
                .process_message_with_plan_deferred(
                    scheduled.message,
                    scheduled.dispatch_plan,
                    &scheduled.scheduler_decision,
                )
                .await
            {
                Ok(transition) => transition,
                Err(err) => {
                    let aborted = err.downcast_ref::<CurrentRunAborted>().cloned();
                    let (terminal, queue_status, mut audit_events, failure_artifacts) =
                        if let Some(aborted) = aborted.as_ref() {
                            (
                                self.build_turn_aborted_record(&aborted.reason, None, 0)
                                    .await,
                                QueueEntryStatus::Interrupted,
                                vec![AuditEvent::legacy(
                                    "message_processing_aborted",
                                    serde_json::json!({
                                        "message_id": message.id.clone(),
                                        "message_kind": message.kind.clone(),
                                        "run_id": aborted.run_id.clone(),
                                        "reason": aborted.reason.clone(),
                                    }),
                                )],
                                None,
                            )
                        } else {
                            let descriptor = describe_runtime_error(&err);
                            let terminal = self
                                .build_turn_aborted_record("runtime_error", None, 0)
                                .await;
                            error!(
                                message_id = %message.id,
                                turn_id = %terminal.turn_id,
                                domain = ?descriptor.domain,
                                code = %descriptor.code,
                                retryable = descriptor.retryable,
                                error = %descriptor.operator_message,
                                "failed to process message"
                            );
                            let (queue_status, settlement_reason) =
                                runtime_error_queue_settlement(&message.kind, &err);
                            let artifacts = self
                                .build_runtime_failure_artifacts(&message, &err, &terminal)
                                .await?;
                            let terminal_turn_id = terminal.turn_id.clone();
                            (
                                terminal,
                                queue_status.clone(),
                                vec![
                                    AuditEvent::legacy(
                                        "runtime_error",
                                        serde_json::json!({
                                            "message_id": message.id.clone(),
                                            "turn_id": terminal_turn_id,
                                            "message_kind": message.kind.clone(),
                                            "domain": descriptor.domain,
                                            "code": descriptor.code,
                                            "retryable": descriptor.retryable,
                                            "error": descriptor.operator_message,
                                            "recovery_hint": descriptor.recovery_hint,
                                            "safe_context": descriptor.safe_context,
                                            "source_chain": descriptor.source_chain,
                                            "token_usage": provider_attempt_timeline(&err)
                                                .and_then(|timeline| timeline.aggregated_token_usage.clone()),
                                            "provider_attempt_timeline": provider_attempt_timeline(&err),
                                        }),
                                    ),
                                    AuditEvent::legacy(
                                        "queue_entry_settled",
                                        serde_json::json!({
                                            "message_id": message.id.clone(),
                                            "message_kind": message.kind.clone(),
                                            "status": queue_status,
                                            "reason": settlement_reason,
                                        }),
                                    ),
                                ],
                                Some(artifacts),
                            )
                        };
                    if let Some(aborted) = aborted.as_ref() {
                        audit_events.push(AuditEvent::legacy(
                            "turn_terminal_aborted",
                            serde_json::json!({
                                "run_id": aborted.run_id,
                                "reason": aborted.reason,
                                "turn_id": terminal.turn_id,
                                "turn_index": terminal.turn_index,
                                "kind": terminal.kind,
                                "completed_at": terminal.completed_at,
                                "duration_ms": terminal.duration_ms,
                            }),
                        ));
                    }
                    let mut turn_record = self.build_turn_record(&terminal).await?;
                    if let Some(artifacts) = failure_artifacts.as_ref() {
                        if !turn_record.produced_brief_ids.contains(&artifacts.brief.id) {
                            turn_record
                                .produced_brief_ids
                                .push(artifacts.brief.id.clone());
                        }
                    }
                    let terminal_transition = turn::TurnTerminalTransition {
                        terminal,
                        turn_record,
                        prepared_work_item_completion: None,
                        terminal_tool_executions: Vec::new(),
                    };
                    let committed_state = {
                        let guard = self.inner.agent.lock().await;
                        let mut state = guard.state.clone();
                        if !matches!(state.status, AgentStatus::Stopped) {
                            scheduler::apply_idle_projection(&mut state, &self.inner.storage)?;
                        }
                        if let Some(artifacts) = failure_artifacts.as_ref() {
                            state.last_runtime_failure = Some(artifacts.failure_summary.clone());
                        }
                        state
                    };
                    let settlement = self
                        .commit_queue_terminal_settlement_with_evidence(
                            QueueEntryRecord {
                                message_id: message.id.clone(),
                                agent_id: message.agent_id.clone(),
                                priority: message.priority.clone(),
                                status: queue_status,
                                created_at: message.created_at,
                                updated_at: Utc::now(),
                            },
                            audit_events,
                            true,
                            Some(&terminal_transition),
                            Some(committed_state),
                            failure_artifacts
                                .as_ref()
                                .map(|artifacts| vec![artifacts.transcript.clone()])
                                .unwrap_or_default(),
                            failure_artifacts
                                .as_ref()
                                .map(|artifacts| vec![artifacts.brief.clone()])
                                .unwrap_or_default(),
                        )
                        .await;
                    if let Err(error) = settlement {
                        if execution_settlement_conflict(&error) {
                            crate::diagnostics::record_missing_terminal_turn_detected();
                            if self.recover_scheduler_bootstrap_claims().await? > 0 {
                                continue;
                            }
                        }
                        return Err(error);
                    }
                    let failed_state = {
                        let mut guard = self.inner.agent.lock().await;
                        guard.current_run_abort = None;
                        guard.state.clone()
                    };
                    self.append_state_changed_events(&failed_state)?;
                    self.maybe_commit_turn_end_work_item_transition().await?;
                    self.record_closure_decision_event(Some(true)).await?;
                    self.maybe_emit_pending_system_tick(None).await?;
                    continue;
                }
            };
            {
                let processed_state = {
                    let mut guard = self.inner.agent.lock().await;
                    guard.current_run_abort = None;
                    guard.state.clone()
                };
                self.append_state_changed_events(&processed_state)?;
                let settlement = self
                    .commit_queue_terminal_settlement(
                        QueueEntryRecord {
                            message_id: message.id.clone(),
                            agent_id: message.agent_id.clone(),
                            priority: message.priority.clone(),
                            status: QueueEntryStatus::Processed,
                            created_at: message.created_at,
                            updated_at: Utc::now(),
                        },
                        vec![AuditEvent::legacy(
                            "queue_entry_settled",
                            serde_json::json!({
                                "message_id": message.id,
                                "message_kind": message.kind,
                                "status": QueueEntryStatus::Processed,
                            }),
                        )],
                        true,
                        Some(&terminal_transition),
                    )
                    .await;
                if let Err(error) = settlement {
                    if execution_settlement_conflict(&error) {
                        crate::diagnostics::record_missing_terminal_turn_detected();
                        if self.recover_scheduler_bootstrap_claims().await? > 0 {
                            continue;
                        }
                    }
                    return Err(error);
                }
                self.maybe_supersede_queued_provider_recovery(&message, Some(&terminal_transition))
                    .await?;
            }
        }
    }

    pub(crate) async fn record_runtime_loop_failure(&self, error: &anyhow::Error) {
        let descriptor = describe_runtime_error(error);
        let summary =
            Self::summarize_runtime_failure_error(&anyhow!(descriptor.operator_message.clone()));
        let occurred_at = Utc::now();
        let agent_id = self.inner.agent.lock().await.state.id.clone();
        let released_claims = 0;
        tracing::error!(
            agent_id = %agent_id,
            domain = ?descriptor.domain,
            code = %descriptor.code,
            retryable = descriptor.retryable,
            error = %error,
            "agent runtime loop failed"
        );
        if let Err(persist_error) = self.inner.storage.append_event(&AuditEvent::legacy(
            "agent_runtime_loop_failed",
            serde_json::json!({
                "agent_id": agent_id,
                "error": summary,
                "domain": descriptor.domain,
                "code": descriptor.code,
                "retryable": descriptor.retryable,
                "recovery_hint": descriptor.recovery_hint,
                "safe_context": descriptor.safe_context,
                "source_chain": descriptor.source_chain,
                "recovery": "bounded_restart",
                "released_claims": released_claims,
            }),
        )) {
            tracing::error!(
                agent_id = %agent_id,
                error = %persist_error,
                "failed to persist agent runtime loop failure audit event"
            );
        }
        let mut guard = self.inner.agent.lock().await;
        guard.state.current_run_id = None;
        guard.current_run_abort = None;
        guard.state.last_runtime_failure = Some(RuntimeFailureSummary {
            occurred_at,
            summary,
            phase: RuntimeFailurePhase::RuntimeTurn,
            detail_hint: Some("the next host access will rebuild the runtime loop".into()),
            failure_artifact: None,
        });
        if let Err(persist_error) = guard.persist_state(&self.inner.storage) {
            tracing::error!(
                agent_id = %agent_id,
                error = %persist_error,
                "failed to persist agent runtime loop failure state"
            );
        }
    }

    async fn recover_scheduler_bootstrap_claims(&self) -> Result<usize> {
        let agent_id = self.inner.agent.lock().await.state.id.clone();
        let execution_state = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&agent_id)?;
        let claimed = self
            .inner
            .runtime_db
            .queue_entries()
            .recent(Some(&agent_id), usize::MAX)?
            .into_iter()
            .filter(|entry| entry.status == QueueEntryStatus::Dequeued)
            .collect::<Vec<_>>();
        let turns = self
            .inner
            .runtime_db
            .turn_records()
            .recent_for_agent(&agent_id, usize::MAX)?;
        let mut recovered = 0;

        for mut entry in claimed {
            let mut guard = self.inner.agent.lock().await;
            let expected_entry = entry.clone();
            let Some(message) = self.inner.storage.read_message_by_id(&entry.message_id)? else {
                continue;
            };
            let Some(attempt) = execution_state
                .as_ref()
                .and_then(|state| execution_attempt_for_message(state, &entry.message_id))
            else {
                continue;
            };
            let attempt = attempt.clone();
            let activation_id = attempt.attempt_id.clone();
            let work_item_id = match &attempt.binding {
                crate::domain::execution_protocol::ExecutionBinding::WorkItem { work_item_id } => {
                    Some(work_item_id.clone())
                }
                crate::domain::execution_protocol::ExecutionBinding::AgentLifecycle {
                    agent_id: owner_agent_id,
                } if owner_agent_id == &agent_id => None,
                crate::domain::execution_protocol::ExecutionBinding::Conversation { .. }
                | crate::domain::execution_protocol::ExecutionBinding::Command => None,
                crate::domain::execution_protocol::ExecutionBinding::AgentLifecycle { .. } => {
                    continue;
                }
            };
            let work_queue_claim = work_item_id.as_deref().is_some_and(|work_item_id| {
                matches!(
                    (&message.kind, &message.origin),
                    (MessageKind::SystemTick, MessageOrigin::System { subsystem })
                        if subsystem == "work_queue"
                ) && message.work_item_id.as_deref() == Some(work_item_id)
            });
            let (task_result_recovery, task_result_replay_fence) = if let Some(work_item_id) =
                work_item_id.as_deref()
            {
                let task_result_recovery = exact_task_result_claim_recovery(
                    &self.inner.storage,
                    &self.inner.runtime_db,
                    &message,
                    &attempt,
                    work_item_id,
                    self.now(),
                    TaskResultClaimRecoveryAuthority::RuntimeTerminatedBootstrap,
                )?;
                match task_result_recovery {
                    TaskResultClaimRecovery::Replayable { transition, .. } => (
                        Some(transition),
                        unsettled_claim::ReplayFence::ExactReplayable,
                    ),
                    TaskResultClaimRecovery::Revoked { .. } => {
                        (None, unsettled_claim::ReplayFence::Revoked)
                    }
                    TaskResultClaimRecovery::RequiresInactiveRuntime => {
                        tracing::error!(
                            agent_id = %agent_id,
                            message_id = %entry.message_id,
                            activation_id = %activation_id,
                            "bootstrap task-result recovery unexpectedly required an inactive runtime"
                        );
                        continue;
                    }
                    TaskResultClaimRecovery::Ineligible { .. } if !work_queue_claim => continue,
                    TaskResultClaimRecovery::Ineligible { .. } => {
                        (None, unsettled_claim::ReplayFence::Ambiguous)
                    }
                }
            } else {
                (None, unsettled_claim::ReplayFence::Ambiguous)
            };

            let terminal_turn = turns.iter().find(|turn| {
                turn.terminal.is_some()
                    && turn
                        .trigger
                        .as_ref()
                        .and_then(|trigger| trigger.message_id.as_deref())
                        == Some(entry.message_id.as_str())
                    && message.turn_id.as_deref() == Some(turn.turn_id.as_str())
                    && turn.current_work_item_id.as_deref() == work_item_id.as_deref()
            });
            let terminal_is_completed = terminal_turn.is_some_and(|turn| {
                turn.terminal.as_ref().is_some_and(|terminal| {
                    terminal.kind == crate::types::TurnTerminalKind::Completed
                })
            });
            let canonical_first_attempt = attempt.attempt_id
                == scheduler_executor::canonical_activation_id(&entry.message_id);
            let decision =
                unsettled_claim::plan_unsettled_claim(&unsettled_claim::UnsettledClaimFacts {
                    queue_status: entry.status.clone(),
                    attempt_state: attempt.state,
                    terminal_turn_completed: terminal_turn.map(|_| terminal_is_completed),
                    replay_fence: if task_result_replay_fence
                        == unsettled_claim::ReplayFence::Revoked
                    {
                        unsettled_claim::ReplayFence::Revoked
                    } else if work_queue_claim || canonical_first_attempt {
                        unsettled_claim::ReplayFence::ExactReplayable
                    } else {
                        task_result_replay_fence
                    },
                    recovery_of_attempt_id: attempt.recovery_of_attempt_id.clone(),
                });
            if let unsettled_claim::UnsettledClaimDecision::QuarantineSettled { reason } = decision
            {
                entry.status = QueueEntryStatus::Quarantined;
                entry.updated_at = self.now();
                let message_id = entry.message_id.clone();
                let execution_protocol =
                    crate::runtime_db::transitions::ExecutionProtocolTransition::default();
                let commit = self
                    .inner
                    .runtime_db
                    .transitions()
                    .commit_queue_with_execution_protocol(
                        &crate::runtime_db::transitions::QueueTransitionCommand {
                            agent_id: agent_id.clone(),
                            operation: crate::runtime_db::transitions::QueueOperation::Settle,
                            mutation:
                                crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                                    expected: expected_entry,
                                    record: entry,
                                },
                            scheduler_claim_work_item: None,
                            agent_state: None,
                            message_evidence: Vec::new(),
                            transcript_entries: Vec::new(),
                            turn_record: terminal_turn.cloned(),
                            audit_events: vec![AuditEvent::legacy(
                                "unsettled_claim_reconciled",
                                serde_json::json!({
                                    "agent_id": agent_id,
                                    "message_id": message_id,
                                    "activation_id": activation_id,
                                    "work_item_id": work_item_id,
                                    "decision": "quarantine_settled",
                                    "reason": reason,
                                    "recovery_generation": u32::from(
                                        attempt.recovery_of_attempt_id.is_some()
                                    ),
                                    "provenance": "bootstrap_reconciliation",
                                }),
                            )],
                            notify_scheduler: true,
                            fault: self.take_transition_fault(),
                            brief_evidence: Vec::new(),
                        },
                        &execution_protocol,
                    )?;
                if commit.applied {
                    recovered += 1;
                    crate::diagnostics::record_unsettled_claim_recovery();
                    crate::diagnostics::record_poison_message_quarantined();
                }
                drop(guard);
                self.apply_transition_commit(commit).await;
                continue;
            }
            if let unsettled_claim::UnsettledClaimDecision::InterruptAndQuarantine { reason } =
                decision
            {
                entry.status = QueueEntryStatus::Quarantined;
                entry.updated_at = self.now();
                let message_id = entry.message_id.clone();
                let execution_protocol =
                    crate::runtime_db::transitions::ExecutionProtocolTransition {
                        bootstrap: None,
                        commands: vec![
                            crate::domain::execution_protocol::ExecutionProtocolCommand::Interrupt(
                                crate::domain::execution_protocol::InterruptExecution {
                                    attempt_id: attempt.attempt_id.clone(),
                                    outcome_id: format!(
                                        "outcome:interrupted:{}",
                                        attempt.attempt_id
                                    ),
                                    reason: reason.into(),
                                    interrupted_at: entry.updated_at.to_rfc3339(),
                                },
                            ),
                        ],
                    };
                let commit = self
                    .inner
                    .runtime_db
                    .transitions()
                    .commit_queue_with_execution_protocol(
                        &crate::runtime_db::transitions::QueueTransitionCommand {
                            agent_id: agent_id.clone(),
                            operation: crate::runtime_db::transitions::QueueOperation::Settle,
                            mutation:
                                crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                                    expected: expected_entry,
                                    record: entry,
                                },
                            scheduler_claim_work_item: None,
                            agent_state: None,
                            message_evidence: Vec::new(),
                            transcript_entries: Vec::new(),
                            turn_record: terminal_turn.cloned(),
                            audit_events: vec![AuditEvent::legacy(
                                "unsettled_claim_reconciled",
                                serde_json::json!({
                                    "agent_id": agent_id,
                                    "message_id": message_id,
                                    "activation_id": activation_id,
                                    "work_item_id": work_item_id,
                                    "decision": "interrupt_and_quarantine",
                                    "reason": reason,
                                    "recovery_generation": u32::from(
                                        attempt.recovery_of_attempt_id.is_some()
                                    ),
                                    "provenance": "bootstrap_reconciliation",
                                }),
                            )],
                            notify_scheduler: true,
                            fault: self.take_transition_fault(),
                            brief_evidence: Vec::new(),
                        },
                        &execution_protocol,
                    )?;
                if commit.applied {
                    recovered += 1;
                    crate::diagnostics::record_unsettled_claim_recovery();
                    crate::diagnostics::record_poison_message_quarantined();
                }
                drop(guard);
                self.apply_transition_commit(commit).await;
                continue;
            }
            if attempt.state == crate::domain::execution_protocol::ExecutionAttemptState::Open
                && terminal_turn.is_none()
                && task_result_recovery.is_some()
            {
                entry.status = QueueEntryStatus::Queued;
                entry.updated_at = self.now();
                let message_id = entry.message_id.clone();
                let execution_protocol =
                    task_result_recovery.expect("TaskResult recovery was checked");
                let commit = self
                    .inner
                    .runtime_db
                    .transitions()
                    .commit_queue_with_execution_protocol(
                        &crate::runtime_db::transitions::QueueTransitionCommand {
                            agent_id: agent_id.clone(),
                            operation: crate::runtime_db::transitions::QueueOperation::Requeue,
                            mutation:
                                crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                                    expected: expected_entry,
                                    record: entry,
                                },
                            scheduler_claim_work_item: None,
                            agent_state: None,
                            message_evidence: Vec::new(),
                            transcript_entries: Vec::new(),
                            turn_record: None,
                            audit_events: vec![AuditEvent::legacy(
                                "scheduler_bootstrap_claim_recovered",
                                serde_json::json!({
                                    "agent_id": agent_id,
                                    "message_id": message_id,
                                    "activation_id": activation_id,
                                    "work_item_id": work_item_id,
                                    "queue_status": QueueEntryStatus::Queued,
                                    "recovery_outcome": "task_result_attempt_requeued_for_reentry",
                                    "provenance": "bootstrap_reconciliation",
                                }),
                            )],
                            notify_scheduler: true,
                            fault: self.take_transition_fault(),
                            brief_evidence: Vec::new(),
                        },
                        &execution_protocol,
                    )?;
                if commit.applied {
                    recovered += 1;
                    crate::diagnostics::record_unsettled_claim_recovery();
                    guard.restore_bootstrap_replay_message(&self.inner.storage, &message)?;
                }
                drop(guard);
                self.apply_transition_commit(commit).await;
                continue;
            }
            // Execution attempt state is authoritative. Terminal attempts only
            // reconcile the retained legacy queue claim.
            if attempt.state == crate::domain::execution_protocol::ExecutionAttemptState::Settled {
                entry.status = if terminal_is_completed {
                    QueueEntryStatus::Processed
                } else {
                    QueueEntryStatus::Aborted
                };
                entry.updated_at = self.now();
                let message_id = entry.message_id.clone();
                let queue_status = entry.status.clone();
                let commit = self.inner.runtime_db.transitions().commit_queue(
                    &crate::runtime_db::transitions::QueueTransitionCommand {
                        agent_id: agent_id.clone(),
                        operation: crate::runtime_db::transitions::QueueOperation::Settle,
                        mutation:
                            crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                                expected: expected_entry,
                                record: entry,
                            },
                        scheduler_claim_work_item: None,
                        agent_state: None,
                        message_evidence: Vec::new(),
                        transcript_entries: Vec::new(),
                        turn_record: terminal_turn.cloned(),
                        audit_events: vec![AuditEvent::legacy(
                            "scheduler_bootstrap_claim_recovered",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "message_id": message_id,
                                "activation_id": activation_id,
                                "work_item_id": work_item_id,
                                "queue_status": queue_status,
                                "recovery_outcome": "legacy_queue_reconciled_from_execution_settlement",
                                "terminal_turn_id": terminal_turn.map(|turn| turn.turn_id.clone()),
                                "provenance": "bootstrap_reconciliation",
                            }),
                        )],
                        notify_scheduler: true,
                        fault: self.take_transition_fault(),
                        brief_evidence: Vec::new(),
                    },
                )?;
                if commit.applied {
                    recovered += 1;
                }
                drop(guard);
                self.apply_transition_commit(commit).await;
                continue;
            }
            if matches!(
                attempt.state,
                crate::domain::execution_protocol::ExecutionAttemptState::Interrupted
                    | crate::domain::execution_protocol::ExecutionAttemptState::ProtocolViolation
            ) {
                entry.status = QueueEntryStatus::Interrupted;
                entry.updated_at = self.now();
                let message_id = entry.message_id.clone();
                let commit = self.inner.runtime_db.transitions().commit_queue(
                    &crate::runtime_db::transitions::QueueTransitionCommand {
                        agent_id: agent_id.clone(),
                        operation: crate::runtime_db::transitions::QueueOperation::Settle,
                        mutation:
                            crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                                expected: expected_entry,
                                record: entry,
                            },
                        scheduler_claim_work_item: None,
                        agent_state: None,
                        message_evidence: Vec::new(),
                        transcript_entries: Vec::new(),
                        turn_record: terminal_turn.cloned(),
                        audit_events: vec![AuditEvent::legacy(
                            "scheduler_bootstrap_claim_recovered",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "message_id": message_id,
                                "activation_id": activation_id,
                                "work_item_id": work_item_id,
                                "queue_status": QueueEntryStatus::Interrupted,
                                "recovery_outcome": "legacy_queue_reconciled_from_execution_interruption",
                                "terminal_turn_id": terminal_turn.map(|turn| turn.turn_id.clone()),
                                "provenance": "bootstrap_reconciliation",
                            }),
                        )],
                        notify_scheduler: true,
                        fault: self.take_transition_fault(),
                        brief_evidence: Vec::new(),
                    },
                )?;
                if commit.applied {
                    recovered += 1;
                    guard.restore_bootstrap_replay_message(&self.inner.storage, &message)?;
                }
                drop(guard);
                self.apply_transition_commit(commit).await;
                continue;
            }
            entry.status = if terminal_is_completed {
                QueueEntryStatus::Processed
            } else {
                QueueEntryStatus::Interrupted
            };
            entry.updated_at = self.now();
            let message_id = entry.message_id.clone();
            let queue_status = entry.status.clone();
            let terminal_turn_id = terminal_turn.map(|turn| turn.turn_id.clone());
            let recovery_outcome = if terminal_is_completed {
                "settled_from_terminal_turn"
            } else {
                "attempt_interrupted_for_reentry"
            };
            let execution_protocol = execution_protocol_settlement_transition_from_facts(
                &self.inner.storage,
                &self.inner.runtime_db,
                &entry,
                terminal_turn,
            )?;
            let commit = self
                .inner
                .runtime_db
                .transitions()
                .commit_queue_with_execution_protocol(
                    &crate::runtime_db::transitions::QueueTransitionCommand {
                        agent_id: agent_id.clone(),
                        operation: crate::runtime_db::transitions::QueueOperation::Settle,
                        mutation: crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                            expected: expected_entry,
                            record: entry,
                        },
                        scheduler_claim_work_item: None,
                        agent_state: None,
                        message_evidence: Vec::new(),
                        transcript_entries: Vec::new(),
                        turn_record: terminal_turn.cloned(),
                        audit_events: vec![AuditEvent::legacy(
                            "scheduler_bootstrap_claim_recovered",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "message_id": message_id,
                                "activation_id": activation_id,
                                "work_item_id": work_item_id,
                                "queue_status": queue_status,
                                "recovery_outcome": recovery_outcome,
                                "terminal_turn_id": terminal_turn_id,
                                "provenance": "bootstrap_reconciliation",
                            }),
                        )],
                        notify_scheduler: true,
                        fault: self.take_transition_fault(),
                        brief_evidence: Vec::new(),
                    },
                    &execution_protocol,
                )?;
            if commit.applied {
                recovered += 1;
                if !terminal_is_completed {
                    guard.restore_bootstrap_replay_message(&self.inner.storage, &message)?;
                }
            }
            drop(guard);
            self.apply_transition_commit(commit).await;
        }
        Ok(recovered)
    }

    async fn bootstrap_recovery(&self) -> Result<()> {
        if let Some(tasks) = self.inner.recovered_tasks.lock().await.take() {
            if self.agent_state().await?.status == AgentStatus::Stopped {
                self.interrupt_active_tasks_for_lifecycle_stop(tasks)
                    .await?;
            } else {
                let (reattached, interrupted_tasks) =
                    self.recover_supervised_child_tasks(tasks).await?;
                let interrupted = self.interrupt_active_tasks(interrupted_tasks).await?;
                if !reattached.is_empty() {
                    self.inner.storage.append_event(&AuditEvent::legacy(
                        "supervised_child_task_monitor_reattached",
                        serde_json::json!({
                            "agent_id": self.agent_id().await?,
                            "task_ids": reattached.iter().map(|task| task.id.clone()).collect::<Vec<_>>(),
                        }),
                    ))?;
                }
                drop(interrupted);
            }
        }
        if let Some(timers) = self.inner.recovered_timers.lock().await.take() {
            self.recover_active_timers(timers).await?;
        }
        self.emit_recovered_pending_wake_hint().await?;
        Ok(())
    }

    fn complete_bootstrap(&self, result: &Result<()>) {
        *self.inner.bootstrap_result.lock().unwrap() =
            Some(result.as_ref().map(|_| ()).map_err(ToString::to_string));
        self.inner.bootstrap_notify.notify_waiters();
    }

    pub(crate) async fn wait_for_bootstrap(&self) -> Result<()> {
        loop {
            let notified = self.inner.bootstrap_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self.inner.bootstrap_result.lock().unwrap().clone() {
                return result.map_err(|error| anyhow!("runtime bootstrap failed: {error}"));
            }
            notified.await;
        }
    }
}

fn decode_exec_command_batch_result(value: &serde_json::Value) -> Option<ExecCommandBatchResult> {
    let mut value = value.clone();
    if let serde_json::Value::Object(map) = &mut value {
        map.entry("summary_text").or_insert(serde_json::Value::Null);
        if let Some(serde_json::Value::Array(items)) = map.get_mut("items") {
            for item in items {
                if let serde_json::Value::Object(item) = item {
                    if let Some(serde_json::Value::Object(result)) = item.get_mut("result") {
                        result
                            .entry("summary_text")
                            .or_insert(serde_json::Value::Null);
                    }
                }
            }
        }
    }
    serde_json::from_value(value).ok()
}

fn command_mentions_path(command: &str, path: &Path) -> bool {
    let display = path.to_string_lossy();
    command.contains(display.as_ref())
}

fn command_skill_activation(
    command: &str,
    skill: &SkillCatalogEntry,
    workspace_anchor: &Path,
) -> Option<(PathBuf, SkillLoadReason)> {
    if command_mentions_path(command, &skill.path)
        || skill
            .path
            .strip_prefix(workspace_anchor)
            .map(|relative| command_mentions_path(command, relative))
            .unwrap_or(false)
    {
        return Some((skill.path.clone(), SkillLoadReason::ReadSkillMd));
    }

    let skill_root = skill.path.parent()?;
    let scripts_root = skill_root.join("scripts");
    for script_path in script_paths_under(&scripts_root) {
        if command_mentions_path(command, &script_path)
            || script_path
                .strip_prefix(workspace_anchor)
                .map(|relative| command_mentions_path(command, relative))
                .unwrap_or(false)
        {
            return Some((script_path, SkillLoadReason::RunSkillScript));
        }
    }

    if command_mentions_path(command, &scripts_root)
        || scripts_root
            .strip_prefix(workspace_anchor)
            .map(|relative| command_mentions_path(command, relative))
            .unwrap_or(false)
    {
        return Some((scripts_root, SkillLoadReason::RunSkillScript));
    }

    None
}

fn script_paths_under(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_script_paths(root, &mut paths);
    paths
}

fn collect_script_paths(path: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.is_file() {
        paths.push(path.to_path_buf());
        return;
    }
    if !metadata.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        collect_script_paths(&entry.path(), paths);
    }
}

fn skill_for_activation_path<'a>(
    skills: &'a [SkillCatalogEntry],
    path: &Path,
) -> Option<&'a SkillCatalogEntry> {
    find_skill_by_entrypoint(skills, path).or_else(|| find_skill_by_script_path(skills, path))
}

#[cfg(test)]
fn current_input_summary(effective_prompt: &EffectivePrompt) -> String {
    let current_input = effective_prompt
        .context_sections
        .iter()
        .find(|section| section.name == "current_input")
        .map(|section| section.content.as_str())
        .unwrap_or_default();

    current_input
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .trim_start_matches("- ")
        .rsplit_once("] ")
        .map(|(_, body)| body.to_string())
        .unwrap_or_else(|| current_input.to_string())
}

fn combine_text_history(history: &[String], text_blocks: &[String]) -> Vec<String> {
    history
        .iter()
        .cloned()
        .chain(text_blocks.iter().cloned())
        .collect()
}

fn is_max_output_stop_reason(stop_reason: Option<&str>) -> bool {
    matches!(
        stop_reason,
        Some("max_tokens") | Some("max_output_tokens") | Some("model_context_window_exceeded")
    )
}

fn normalized_turn_id(turn_id: Option<&str>) -> Option<String> {
    turn_id
        .map(str::trim)
        .filter(|turn_id| !turn_id.is_empty())
        .map(ToString::to_string)
}

pub(crate) fn retryable_enqueue_conflict(error: &anyhow::Error, agent_id: &str) -> bool {
    error.chain().any(|source| {
        source
            .downcast_ref::<RuntimeStateTransitionConflict>()
            .is_some_and(|conflict| {
                conflict.retryable()
                    && ((conflict.domain() == "agent_state" && conflict.record_id() == agent_id)
                        || conflict.domain() == "wait_condition_trigger")
            })
    })
}

#[cfg(test)]
mod tests;
