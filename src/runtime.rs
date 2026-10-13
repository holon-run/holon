mod agent_message_delivery;
mod agent_services;
mod bootstrap;
mod callback;
mod clock;
mod closure;
mod command_task;
pub(crate) mod completion_sources;
mod context_capture;
mod continuation;
mod control;
mod control_cancellation;
mod control_entry;
pub(crate) mod control_reconcile;
pub(crate) mod control_recovery;
pub(crate) mod decision_models;
pub(crate) mod decision_openai;
mod delivery;
pub(crate) mod ended_dispatch;
pub(crate) mod episode_recovery;
pub(crate) mod execution_admission;
mod failure;
mod first_run_intro;
pub(crate) mod input_evidence_budget;
mod input_preparation;
pub(crate) mod input_recovery;
mod lifecycle;
mod message_dispatch;
pub(crate) mod mutation_owner;
pub(crate) mod mutation_witness;
mod operator;
mod operator_dispatch;
pub(crate) mod provider_recovery;
mod provider_turn;
mod repair;
pub(crate) mod resident_input_evidence;
pub(crate) mod resident_state;
pub(crate) mod resident_work;
mod scheduler;
mod scheduler_acceptance;
mod scheduler_continuation;
pub(crate) mod scheduler_executor;
mod state_mutation;
pub(crate) use state_mutation::RuntimeMutationFacts;
mod subagent;
mod task_result_recovery;
mod task_state_reducer;
mod task_supervisor;
mod tasks;
#[cfg(test)]
mod test_util;
mod turn;
mod unsettled_claim;
mod waiting;
mod wake_matching;
mod work_reads;
pub(crate) mod workspace;
pub(crate) mod workspace_control;
mod worktree;

#[cfg(test)]
pub(crate) use agent_message_delivery::{
    enable_delivery_checkpoint, release_delivery_checkpoint, wait_for_delivery_checkpoint,
};
pub(crate) use agent_message_delivery::{
    legacy_cross_agent_caller_bindings_match, AgentMessageDeliveryService,
    PreparedAgentMessageDelivery,
};
pub(crate) use closure::runtime_error_active;
pub use first_run_intro::maybe_enqueue_first_run_intro;
pub(crate) use lifecycle::LightweightAgentStateProjection;
pub(crate) use repair::is_wake_only_message;
pub use repair::{
    SchedulerRepairInspection, SchedulerRepairOperation, SchedulerRepairRequest,
    SchedulerRepairResult,
};
pub(crate) use scheduler::{SchedulerAgentSnapshot, SchedulerObservations, SchedulerProjection};
pub use scheduler_acceptance::{
    seed_scheduler_restart_fixture, seed_scheduler_terminal_recovery_fixture,
    SchedulerIngressAdmissionRestartFixture, SchedulerTerminalRecoveryFixture,
};
pub use tasks::{
    PickedWorkItem, WorkItemContinuationSummary, WorkItemFocusTransition,
    WorkItemFocusTransitionWarning,
};
pub(crate) use waiting::{
    PrepareWaitForOutcome, PreparedWaitForSettlement, WaitForBriefPublicationScope,
    WaitForContinuation, WaitForRegistrationOutcome, WaitForScope, WaitForWakeKind,
};
pub(crate) use worktree::format_worktree_task_summary;

#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::{
    collections::{hash_map::Entry, BTreeSet, HashMap, HashSet},
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
pub(crate) use bootstrap::{
    ViewImageCandidateFailureStatus, VIEW_IMAGE_PROTOCOL_FAILURE_THRESHOLD,
};
use chrono::{DateTime, Utc};
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
    context::ContextConfig,
    host::RuntimeHostBridge,
    ingress::WakeDisposition,
    memory::refresh_working_memory,
    prompt::{build_effective_prompt_with_apply_patch_surface, EffectivePrompt},
    provider::{
        provider_attempt_timeline, AgentProvider, ModelBlock, ProviderNativeWebSearchKind,
        ProviderNativeWebSearchRequest,
    },
    queue::RuntimeQueue,
    runtime_db::{
        transitions::{
            PostCommitWarning, TransitionApplyResult, TransitionCommit, TransitionFaultPoint,
        },
        RuntimeDb,
    },
    runtime_error::describe_runtime_error,
    runtime_event::RuntimeEventKind,
    skills::{
        effective_skill_root_registrations, find_skill_by_entrypoint, find_skill_by_script_path,
        skills_runtime_view_from_catalog, SkillVisibility,
    },
    storage::{to_json_value, AppStorage, PollActivityMarker},
    system::{
        EffectiveExecution, ExecutionProfile, ExecutionScopeKind, ExecutionSnapshot, LocalSystem,
        WorkspaceAccessMode, WorkspaceProjectionKind, WorkspaceView,
    },
    tool::{ToolRegistry, ToolResult},
    types::LoadedAgentMemory,
    types::{
        brief_created_event_for, ActiveWorkspaceEntry, AdmissionContext, AgentIdentityView,
        AgentKind, AgentMessageDeliveryOutcome, AgentMessageDeliveryReceipt,
        AgentMessageDeliveryRecord, AgentModelOverrideAuditEvent, AgentModelSource,
        AgentModelState, AgentState, AgentStateChangedEvent, AgentStatus, AgentSummary, AuditEvent,
        AuthorityClass, BriefCreatedAuditEvent, BriefRecord, CallbackDeliveryMode,
        CallbackDeliveryPayload, CallbackDeliveryResult, CallbackIngressDisposition,
        ClosureDecision, ContinuationResolution, ControlAction, ExecCommandBatchItemStatus,
        ExecCommandBatchResult, ExecutionAdmissionProvenance, ExternalTriggerCapability,
        ExternalTriggerRecord, ExternalTriggerScope, ExternalTriggerStatus, ExternalTriggerSummary,
        LoadedAgentsMd, MessageBody, MessageDeliverySurface, MessageEnvelope, MessageKind,
        MessageLifecycleAuditEvent, MessageOrigin, PendingWakeHint, Priority, QueueEntryRecord,
        QueueEntryStatus, RuntimeFailurePhase, RuntimeFailureSummary, RuntimePosture,
        SkillActivationSource, SkillActivationState, SkillCatalogEntry, SkillLoadReason,
        SkillsRuntimeView, TaskKind, TaskLifecycleAuditEvent, TaskRecord, TaskRecoverySpec,
        TaskStatus, TimerRecord, TimerStatus, ToolExecutionRecord, ToolExecutionStatus,
        TranscriptEntry, TranscriptEntryKind, TurnNoBriefReason, TurnRecord, TurnTerminalKind,
        ViewImageObservation, WaitConditionRecord, WaitConditionStatus, WaitingReason,
        WorkItemExecutionBinding, WorkItemLifecycleAuditEvent, WorkItemRecord, WorkItemState,
        WorkspaceEntry, AGENT_HOME_WORKSPACE_ID,
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
    pub(crate) settlement: WorkItemCompletionSettlement,
    pub(crate) record: crate::types::WorkItemRecord,
    pub(crate) brief: crate::types::BriefRecord,
    pub(crate) expected_execution_protocol_state:
        Option<crate::domain::execution_protocol::ExecutionProtocolState>,
    pub(crate) legacy_work_item_execution:
        Option<crate::domain::execution_protocol::WorkItemExecutionRecord>,
    pub(crate) expected_agent_state: crate::types::AgentState,
    pub(crate) committed_agent_state: crate::types::AgentState,
    pub(crate) wait_conditions: Vec<crate::types::WaitConditionRecord>,
    pub(crate) continuation_expectations: Vec<crate::types::WorkItemContinuationFrame>,
    pub(crate) continuations: Vec<crate::types::WorkItemContinuationFrame>,
    pub(crate) continuation_resumed: Option<WorkItemContinuationSummary>,
    continuation_resumes: std::collections::BTreeMap<
        String,
        (
            crate::domain::execution_protocol::WorkItemContinuationResumeSource,
            crate::domain::execution_protocol::WorkItemOutcome,
        ),
    >,
    pub(crate) audit_events: Vec<crate::types::AuditEvent>,
    pub(crate) index_changes: Vec<crate::runtime_db::RuntimeIndexChange>,
    pub(crate) tool_execution: Option<crate::types::ToolExecutionRecord>,
    pub(crate) transcript_entries: Vec<crate::types::TranscriptEntry>,
    _work_read_budget: Option<Arc<input_evidence_budget::BudgetViewGuard>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkItemCompletionSettlement {
    BoundExecution,
    Detached,
}

#[derive(Debug, Clone)]
pub(crate) enum WorkItemCompletionDispatch {
    Unchanged(crate::types::WorkItemRecord),
    Prepared(PreparedWorkItemCompletion),
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

fn rebase_committed_agent_state(
    expected: &AgentState,
    committed: &AgentState,
    current: &AgentState,
) -> Result<(AgentState, Vec<String>)> {
    let expected = serde_json::to_value(expected)?;
    let committed = serde_json::to_value(committed)?;
    let mut current = serde_json::to_value(current)?;
    let expected = expected
        .as_object()
        .context("expected agent state did not serialize as an object")?;
    let committed = committed
        .as_object()
        .context("committed agent state did not serialize as an object")?;
    let current_fields = current
        .as_object_mut()
        .context("current agent state did not serialize as an object")?;
    let fields = expected
        .keys()
        .chain(committed.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut conflicts = Vec::new();

    for field in fields {
        let expected_value = expected.get(&field);
        let committed_value = committed.get(&field);
        if committed_value == expected_value {
            continue;
        }
        let current_value = current_fields.get(&field);
        if current_value == expected_value {
            match committed_value {
                Some(value) => {
                    current_fields.insert(field, value.clone());
                }
                None => {
                    current_fields.remove(&field);
                }
            }
        } else if current_value != committed_value {
            conflicts.push(field);
        }
    }

    Ok((serde_json::from_value(current)?, conflicts))
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
    AgentExecution {
        binding: WorkItemExecutionBinding,
        effective_work_item_id: Option<String>,
    },
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
        requested_model: Some(effective_model.clone()),
        active_model: Some(active_model.clone()),
        fallback_active,
        effective_fallback_models: effective_chain.into_iter().skip(1).collect(),
        override_model: state.model_override.clone(),
        override_reasoning_effort: state.model_override_reasoning_effort.clone(),
        override_service_tier: state.model_override_service_tier,
        effective_service_tier: if fallback_active {
            model_catalog.route_service_tier(&active_model)
        } else {
            state
                .model_override_service_tier
                .or_else(|| model_catalog.route_service_tier(&effective_model))
        },
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
    identity_incarnation: u64,
    mutation_owner: mutation_owner::MutationOwner,
    input_evidence_budget: input_evidence_budget::InputEvidenceBudgetLedger,
    standalone_related_mutation_owners: mutation_owner::MutationOwnerRegistry,
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
    context_resource_observation: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    context_capture_checkpoint: context_capture::ContextCaptureCheckpoint,
    config_snapshot: ArcSwap<ConfigSnapshot>,
    view_image_observation_cache:
        Mutex<HashMap<ViewImageObservationCacheKey, ViewImageObservation>>,
    view_image_candidate_health: Mutex<bootstrap::ViewImageCandidateHealth>,
    model_discovery_refreshes: Mutex<HashSet<crate::config::ProviderId>>,
    model_discovery_refresh_notify: Notify,
    callback_base_url: String,
    tools: ToolRegistry,
    system: Arc<LocalSystem>,
    default_agent_id: String,
    host_bridge: Option<RuntimeHostBridge>,
    task_handles: Mutex<HashMap<String, ManagedTaskHandle>>,
    owned_task_handles: StdMutex<Vec<tokio::task::JoinHandle<()>>>,
    owned_task_drain: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    control_cancellations: control_cancellation::ControlCancellationRegistry,
    mutation_tasks: StdMutex<state_mutation::OwnedMutationTasks>,
    recovered_tasks: Mutex<Option<Vec<TaskRecord>>>,
    recovered_timers: Mutex<Option<Vec<TimerRecord>>>,
    bootstrap_result: StdMutex<Option<std::result::Result<(), String>>>,
    bootstrap_notify: Notify,
    autonomous_continuation_hook: Arc<dyn scheduler::SemanticCandidateSelectionHook>,
    autonomous_continuation_decision_hook:
        RwLock<Option<Arc<dyn scheduler::AsyncSemanticCandidateSelectionHook>>>,
    suppress_next_continue_active_tick: Mutex<bool>,
    shutdown_requested: AtomicBool,
    execution_admission: execution_admission::ExecutionAdmission,
    transition_faults: StdMutex<std::collections::VecDeque<TransitionFaultPoint>>,
    #[cfg(test)]
    terminal_transition_faults: StdMutex<std::collections::VecDeque<TransitionFaultPoint>>,
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
    #[cfg(test)]
    terminal_tool_interjection_checkpoint: TerminalToolInterjectionCheckpoint,
    transition_warnings: StdMutex<Vec<PostCommitWarning>>,
}

#[cfg(test)]
#[derive(Default)]
struct TerminalToolInterjectionCheckpoint {
    armed: AtomicBool,
    reached: Notify,
    release: Notify,
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

fn execution_protocol_completion_transition_from_prepared(
    record: &QueueEntryRecord,
    terminal_turn: &TurnRecord,
    prepared: &PreparedWorkItemCompletion,
) -> Result<crate::runtime_db::transitions::ExecutionProtocolTransition> {
    use crate::domain::execution_protocol::{
        CompleteWorkItemExecution, ConversationOutcome, ExecutionAttemptState, ExecutionBinding,
        ExecutionOutcome, ExecutionOutcomeRecord, ExecutionProtocolCommand,
        RegisterWorkItemExecution, SettleExecution, WorkItemExecutionState, WorkItemOutcome,
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
        .or(prepared.legacy_work_item_execution.as_ref())
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
            let mut commands = Vec::with_capacity(3);
            if !state.work_items.contains_key(&prepared.record.id) {
                commands.push(ExecutionProtocolCommand::RegisterWorkItem(Box::new(
                    RegisterWorkItemExecution {
                        work_item_id: prepared.record.id.clone(),
                        record: authoritative.clone(),
                    },
                )));
            }
            commands.extend([
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
            ]);
            commands
        }
        ExecutionBinding::Conversation { .. } => {
            anyhow::ensure!(
                attempt.agent_id == record.agent_id
                    && authoritative.source_revision == intent.expected_work_revision
                    && !matches!(
                        authoritative.state,
                        WorkItemExecutionState::InFlight { .. }
                            | WorkItemExecutionState::Terminal { .. }
                    ),
                "completion commit conversation WorkItem fence is stale"
            );
            let mut commands = Vec::with_capacity(3);
            if !state.work_items.contains_key(&prepared.record.id) {
                commands.push(ExecutionProtocolCommand::RegisterWorkItem(Box::new(
                    RegisterWorkItemExecution {
                        work_item_id: prepared.record.id.clone(),
                        record: authoritative.clone(),
                    },
                )));
            }
            commands.extend([
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
            ]);
            commands
        }
        ExecutionBinding::Command => {
            bail!(
                "completion commit requires a WorkItem, conversation, or agent-lifecycle execution"
            )
        }
    };

    Ok(
        crate::runtime_db::transitions::ExecutionProtocolTransition {
            bootstrap: None,
            commands,
        },
    )
}

fn execution_protocol_detached_completion_transition_from_prepared(
    prepared: &PreparedWorkItemCompletion,
) -> Result<crate::runtime_db::transitions::ExecutionProtocolTransition> {
    use crate::domain::execution_protocol::{
        CompleteWorkItemExecution, ExecutionProtocolCommand, RegisterWorkItemExecution,
    };

    let Some(state) = prepared.expected_execution_protocol_state.as_ref() else {
        return Ok(crate::runtime_db::transitions::ExecutionProtocolTransition::default());
    };
    let authoritative = state
        .work_items
        .get(&prepared.record.id)
        .or(prepared.legacy_work_item_execution.as_ref())
        .ok_or_else(|| anyhow!("detached completion WorkItem execution state is missing"))?;
    let mut commands = Vec::new();
    if !state.work_items.contains_key(&prepared.record.id) {
        commands.push(ExecutionProtocolCommand::RegisterWorkItem(Box::new(
            RegisterWorkItemExecution {
                work_item_id: prepared.record.id.clone(),
                record: authoritative.clone(),
            },
        )));
    }
    commands.push(ExecutionProtocolCommand::CompleteWorkItem(Box::new(
        CompleteWorkItemExecution {
            command_id: format!("completion:detached:{}", prepared.record.id),
            work_item_id: prepared.record.id.clone(),
            expected: authoritative.clone(),
            completion: prepared.brief.id.clone(),
        },
    )));
    for continuation in &prepared.continuations {
        if continuation.state != crate::types::WorkItemContinuationState::Resumed {
            continue;
        }
        let parent = state
            .work_items
            .get(&continuation.suspended_work_item_id)
            .ok_or_else(|| anyhow!("completion parent execution state is missing"))?;
        let (source, outcome) = prepared
            .continuation_resumes
            .get(&continuation.id)
            .context("prepared completion continuation source missing")?
            .clone();
        commands.push(ExecutionProtocolCommand::ResumeWorkItemContinuation(
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
        ));
    }
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
    owner: &crate::types::TurnOwner,
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
            && turn.effective_owner() == *owner
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
    let owner = match &attempt.binding {
        ExecutionBinding::WorkItem { work_item_id } => crate::types::TurnOwner::WorkItem {
            work_item_id: work_item_id.clone(),
        },
        ExecutionBinding::Conversation { interaction_id } => {
            crate::types::TurnOwner::Conversation {
                interaction_id: interaction_id.clone(),
            }
        }
        ExecutionBinding::AgentLifecycle { agent_id } => crate::types::TurnOwner::AgentLifecycle {
            agent_id: agent_id.clone(),
        },
        ExecutionBinding::Command => crate::types::TurnOwner::Command,
    };
    let matching_terminal_turn =
        canonical_matching_terminal_turn(runtime_db, record, &message, &owner, terminal_turn)?;
    let Some(terminal_turn) = matching_terminal_turn else {
        return Err(
            crate::domain::execution_protocol::ExecutionSettlementConflict::MissingTerminalTurn {
                attempt_id: attempt.attempt_id.clone(),
                message_id: record.message_id.clone(),
                owner_work_item_id: owner.work_item_id().map(ToString::to_string),
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
                                    ExecutionOutcome::WorkItem(
                                        match work_item.blocked_by.as_ref() {
                                            Some(reason) => WorkItemOutcome::Pause {
                                                reason: reason.clone(),
                                            },
                                            None => WorkItemOutcome::Continue,
                                        },
                                    )
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
                .latest_wait_conditions_for_agent(&record.agent_id)?
                .into_iter()
                .filter(|wait| {
                    wait.work_item_id.is_none()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalSettlementReadback {
    Committed,
    NotCommitted,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalSettlementDisposition {
    Intended,
    AbortedFallback,
}

#[derive(Debug, Clone)]
struct TerminalAgentStateUpdate {
    last_runtime_failure: Option<RuntimeFailureSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalExecutionSettlementWitness {
    attempt_id: String,
    expected_outcome: Option<crate::domain::execution_protocol::ExecutionOutcomeRecord>,
    expected_attempt_state: Option<crate::domain::execution_protocol::ExecutionAttemptState>,
}

impl TerminalExecutionSettlementWitness {
    fn unmaterialized(attempt_id: String) -> Self {
        Self {
            attempt_id,
            expected_outcome: None,
            expected_attempt_state: None,
        }
    }
}

fn terminal_execution_open_attempt_id(
    runtime_db: &RuntimeDb,
    record: &QueueEntryRecord,
) -> Result<Option<String>> {
    Ok(runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized(&record.agent_id)?
        .as_ref()
        .and_then(|state| execution_attempt_for_message(state, &record.message_id))
        .filter(|attempt| {
            attempt.state == crate::domain::execution_protocol::ExecutionAttemptState::Open
        })
        .map(|attempt| attempt.attempt_id.clone()))
}

fn terminal_execution_settlement_witness(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    record: &QueueEntryRecord,
    transition: &turn::TurnTerminalTransition,
) -> Result<Option<TerminalExecutionSettlementWitness>> {
    let execution_protocol =
        if let Some(prepared) = transition.prepared_work_item_completion.as_ref() {
            execution_protocol_completion_transition_from_prepared(
                record,
                &transition.turn_record,
                prepared,
            )?
        } else if let Some(prepared) = transition.prepared_wait_for.as_ref() {
            prepared.execution_protocol.clone()
        } else {
            execution_protocol_settlement_transition_from_facts(
                storage,
                runtime_db,
                record,
                Some(&transition.turn_record),
            )?
        };
    let settlements = execution_protocol
        .commands
        .iter()
        .filter_map(|command| match command {
            crate::domain::execution_protocol::ExecutionProtocolCommand::Settle(command) => {
                Some(Ok(TerminalExecutionSettlementWitness {
                    attempt_id: command.outcome.attempt_id.clone(),
                    expected_outcome: Some(command.outcome.clone()),
                    expected_attempt_state: Some(
                        crate::domain::execution_protocol::ExecutionAttemptState::Settled,
                    ),
                }))
            }
            crate::domain::execution_protocol::ExecutionProtocolCommand::Interrupt(command) => {
                Some((|| {
                    let state = runtime_db
                        .transitions()
                        .load_execution_protocol_state_if_initialized(&record.agent_id)?
                        .ok_or_else(|| {
                            anyhow!("terminal interruption requires canonical execution state")
                        })?;
                    let interrupted =
                        crate::domain::execution_protocol::interrupt_execution(&state, command)
                            .map_err(|error| anyhow!(error))?;
                    let outcome = interrupted
                        .state
                        .outcomes
                        .get(&command.outcome_id)
                        .cloned()
                        .ok_or_else(|| {
                            anyhow!(
                                "terminal interruption did not materialize its execution outcome"
                            )
                        })?;
                    Ok(TerminalExecutionSettlementWitness {
                        attempt_id: command.attempt_id.clone(),
                        expected_outcome: Some(outcome),
                        expected_attempt_state: Some(
                            crate::domain::execution_protocol::ExecutionAttemptState::Interrupted,
                        ),
                    })
                })())
            }
            _ => None,
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(
        settlements.len() <= 1,
        "terminal settlement prepared multiple execution outcomes"
    );
    Ok(settlements.into_iter().next())
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

fn execution_attempt_has_task_result_source(
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
) -> bool {
    matches!(
        &attempt.source.identity,
        crate::domain::execution_protocol::ExecutionSourceIdentity::TaskResult { .. }
    )
}

pub(crate) enum TaskResultClaimRecovery {
    Replayable {
        transition: crate::runtime_db::transitions::ExecutionProtocolTransition,
        reason: &'static str,
    },
    Revoked {
        wait: WaitConditionRecord,
        reason: &'static str,
    },
    MissingWait,
    RequiresInactiveRuntime,
    Ineligible {
        reason: &'static str,
    },
}

#[derive(Clone, Copy)]
pub(crate) enum TaskResultClaimRecoveryAuthority {
    Diagnostic,
    RuntimeTerminatedBootstrap,
}

pub(crate) const TASK_RESULT_WAIT_BLOCKER: &str = "Waiting on a task result.";

pub(crate) fn task_result_reentry_is_allowed(
    work_item: &WorkItemRecord,
    work_item_id: &str,
    wait: &WaitConditionRecord,
) -> bool {
    work_item.state == WorkItemState::Open
        && work_item.id == work_item_id
        && wait.work_item_id.as_deref() == Some(work_item_id)
        && wait.kind == crate::types::WaitConditionKind::Task
        && wait.status == WaitConditionStatus::Resolved
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
    let task = message
        .task_id
        .as_deref()
        .map(|id| storage.latest_task_record(id))
        .transpose()?
        .flatten();
    let waits = storage.latest_wait_conditions_for_agent(&message.agent_id)?;
    let work_item = runtime_db.work_items().latest(work_item_id)?;
    exact_task_result_claim_recovery_from_facts(
        message,
        attempt,
        work_item_id,
        interrupted_at,
        authority,
        task.as_ref(),
        &waits,
        work_item.as_ref(),
    )
}

pub(crate) fn exact_task_result_claim_recovery_from_facts(
    message: &MessageEnvelope,
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
    work_item_id: &str,
    interrupted_at: chrono::DateTime<Utc>,
    authority: TaskResultClaimRecoveryAuthority,
    task: Option<&TaskRecord>,
    waits: &[WaitConditionRecord],
    work_item: Option<&WorkItemRecord>,
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
    let Some(task) = task else {
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
        || !matches!(
            task.status,
            crate::types::TaskStatus::Completed
                | crate::types::TaskStatus::Failed
                | crate::types::TaskStatus::Cancelled
                | crate::types::TaskStatus::Interrupted
        )
        || attempt.admitted_fences.rejoin.as_ref() != Some(&rejoin)
    {
        return Ok(TaskResultClaimRecovery::Ineligible {
            reason: "task_result_rejoin_identity_mismatch",
        });
    }
    let matching_waits = waits
        .into_iter()
        .filter(|wait| {
            wait.work_item_id.as_deref() == Some(work_item_id)
                && matches!(
                    wait.status,
                    WaitConditionStatus::Resolved | WaitConditionStatus::Cancelled
                )
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
    let wait = match matching_waits.as_slice() {
        [] => None,
        [wait] => Some((*wait).clone()),
        _ => {
            return Ok(TaskResultClaimRecovery::Ineligible {
                reason: "task_result_wait_ambiguous",
            });
        }
    };
    if let Some(wait) = wait.as_ref() {
        if wait.status == WaitConditionStatus::Cancelled {
            return Ok(TaskResultClaimRecovery::Revoked {
                wait: wait.clone(),
                reason: "task_result_wait_cancelled",
            });
        }
    }
    let Some(work_item) = work_item else {
        return Ok(TaskResultClaimRecovery::Ineligible {
            reason: "task_result_work_item_missing",
        });
    };
    let Some(expected_source_revision) = attempt.admitted_fences.work_item_source_revision else {
        return Ok(TaskResultClaimRecovery::Ineligible {
            reason: "task_result_work_item_revision_fence_missing",
        });
    };
    let Some(wait) = wait else {
        return Ok(TaskResultClaimRecovery::MissingWait);
    };
    if !task_result_reentry_is_allowed(work_item, work_item_id, &wait) {
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
        .filter(|attempt| execution_attempt_has_task_result_source(attempt))
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
                TaskResultClaimRecovery::MissingWait => {
                    candidate.reason = "task_result_wait_missing".into();
                    candidate
                        .evidence
                        .push("task_result_wait:missing".to_string());
                    unsettled_claim::ReplayFence::Missing
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
                | TaskResultClaimRecovery::MissingWait
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
    pub execution_partition_initialized: bool,
    pub candidates: Vec<SchedulerRecoveryCandidate>,
    pub task_result_claim_recoveries: Vec<SchedulerTaskResultClaimRecoveryCandidate>,
    pub continuation_reconciliations: Vec<SchedulerContinuationReconciliationCandidate>,
    pub pending_task_results: Vec<SchedulerPendingTaskResult>,
    pub pending_task_results_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerPendingTaskResult {
    pub task_id: String,
    pub message_id: String,
    pub owner: crate::types::TurnOwner,
    pub state: String,
    pub deferred_reason: Option<String>,
    pub next_recheck_at: Option<DateTime<Utc>>,
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

    let mut pending_task_results = runtime_db
        .task_result_settlements()
        .unsettled_for_agent(agent_id, 129)?;
    let pending_task_results_truncated = pending_task_results.len() > 128;
    pending_task_results.truncate(128);
    let pending_task_results = pending_task_results
        .into_iter()
        .map(|record| SchedulerPendingTaskResult {
            task_id: record.task_id,
            message_id: record.message_id,
            owner: record.work_item_id.map_or_else(
                || crate::types::TurnOwner::AgentLifecycle {
                    agent_id: record.agent_id,
                },
                |work_item_id| crate::types::TurnOwner::WorkItem { work_item_id },
            ),
            state: match record.state {
                crate::runtime_db::task_result_settlement::TaskResultSettlementState::PersistedPending => "persisted_pending",
                crate::runtime_db::task_result_settlement::TaskResultSettlementState::CallerAdmitted => "caller_admitted",
                crate::runtime_db::task_result_settlement::TaskResultSettlementState::Settled => "settled",
            }
            .into(),
            deferred_reason: record.deferred_reason,
            next_recheck_at: record.next_recheck_at,
        })
        .collect();
    Ok(SchedulerRecoveryReport {
        agent_id: agent_id.to_string(),
        execution_partition_initialized: execution_state.is_some(),
        candidates,
        task_result_claim_recoveries,
        continuation_reconciliations,
        pending_task_results,
        pending_task_results_truncated,
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
        None,
    )
}

pub fn apply_scheduler_recovery_plan_with_maintenance(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    agent_id: &str,
    report: &SchedulerRecoveryReport,
    backup_policy: SchedulerRecoveryBackupPolicy,
    maintenance: &crate::runtime_db::OfflineInputMaintenance<'_>,
) -> Result<(usize, Option<std::path::PathBuf>)> {
    apply_scheduler_recovery_plan_with_options(
        storage,
        runtime_db,
        agent_id,
        report,
        backup_policy,
        None,
        Some(maintenance),
    )
}

fn apply_scheduler_recovery_plan_with_options(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    agent_id: &str,
    report: &SchedulerRecoveryReport,
    backup_policy: SchedulerRecoveryBackupPolicy,
    task_result_fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
    maintenance: Option<&crate::runtime_db::OfflineInputMaintenance<'_>>,
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
            .commit_scheduler_recovery_with_maintenance(
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
                maintenance,
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
            .commit_scheduler_recovery_with_maintenance(
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
                maintenance,
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
            .commit_execution_protocol_recovery_plan_with_maintenance(
                agent_id,
                &continuation_reconciliation,
                &audit_events,
                maintenance,
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
    if error.chain().any(|source| {
        source
            .downcast_ref::<context_capture::ContextPreparationRetryExhausted>()
            .is_some()
    }) {
        (
            QueueEntryStatus::Interrupted,
            "context_preparation_retry_exhausted",
        )
    } else if retry_task_result {
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
    timers: HashMap<String, TimerRecord>,
    external_triggers: HashMap<String, ExternalTriggerRecord>,
}

impl AgentRuntimeProjectionCache {
    fn rebuild(
        agent_id: String,
        tasks: Vec<TaskRecord>,
        timers: Vec<TimerRecord>,
        external_triggers: Vec<ExternalTriggerRecord>,
    ) -> Self {
        crate::diagnostics::record_runtime_projection_cache_rebuild();
        let task_agent_id = agent_id.clone();
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BuiltinWebSearchSelectionStatus {
    Selected,
    Disabled,
    Unsupported,
    NotDeclared,
    NotRequested,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuiltinWebSearchSelection {
    request: Option<ProviderNativeWebSearchRequest>,
    diagnostics: BuiltinWebSearchSelectionDiagnostics,
}

#[derive(Debug, Clone)]
struct RuntimeAgent {
    #[cfg(test)]
    mutation_owner: mutation_owner::MutationOwner,
    state: AgentState,
    last_persisted_state: AgentState,
    queue: RuntimeQueue,
    _queue_budget_view: Option<Arc<input_evidence_budget::BudgetViewGuard>>,
    current_run_abort: Option<CurrentRunAbortHandle>,
}

impl RuntimeAgent {
    #[cfg(test)]
    fn persist_state(&mut self, storage: &AppStorage) -> Result<()> {
        let started = std::time::Instant::now();
        // Fixture setup bypasses OCC but must not erase pending recovery.
        if let Err(error) = self.mutation_owner.capture() {
            self.state = self.last_persisted_state.clone();
            crate::diagnostics::record_storage_persist_state(started.elapsed());
            return Err(error.into());
        }
        // Keep the fixture's durable baseline aligned with its resident state.
        if let Err(error) = storage.write_agent_for_fixture(&self.state) {
            self.state = self.last_persisted_state.clone();
            crate::diagnostics::record_storage_persist_state(started.elapsed());
            return Err(error);
        }
        self.last_persisted_state = self.state.clone();
        crate::diagnostics::record_storage_persist_state(started.elapsed());
        Ok(())
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
    IdleAfterAbort,
}

impl CurrentRunAbortMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StopAfterAbort => "stop_after_abort",
            Self::IdleAfterAbort => "idle_after_abort",
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
    async fn read_committed_agent<T>(
        &self,
        read: impl FnOnce(&RuntimeAgent) -> T + Send,
    ) -> Result<T> {
        loop {
            let lock_wait = crate::diagnostics::attribution::AGENT_LOCK.start();
            let guard = self.inner.agent.lock().await;
            drop(lock_wait);
            match self.inner.mutation_owner.capture() {
                Ok(_) => return Ok(read(&guard)),
                Err(mutation_owner::MutationOwnerError::NeedsReconcile(_)) => {}
                Err(error) => return Err(error.into()),
            }
            drop(guard);
            // Wait for normal publication without holding the state lock.
            // Unknown outcomes remain fenced rather than borrowing a DB row.
            self.reconcile_posture_observation().await?;
        }
    }

    pub(super) fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.inner.clock.now()
    }

    pub(crate) async fn mark_current_turn_external_side_effect_possible(
        &self,
        tool_name: &str,
    ) -> Result<()> {
        let (agent_id, turn_id) = {
            let guard = self.inner.agent.lock().await;
            (guard.state.id.clone(), guard.state.current_turn_id.clone())
        };
        let Some(turn_id) = turn_id else {
            return Ok(());
        };
        let Some(mut turn) = self
            .inner
            .runtime_db
            .turn_records()
            .by_id(Some(&agent_id), &turn_id)?
        else {
            return Ok(());
        };
        if turn.terminal.is_some() || turn.external_side_effect_possible {
            return Ok(());
        }
        turn.external_side_effect_possible = true;
        self.inner.runtime_db.turn_records().upsert(&turn)?;
        self.inner.storage.append_event(&AuditEvent::legacy(
            "external_command_no_replay_barrier",
            serde_json::json!({
                "agent_id": agent_id,
                "turn_id": turn_id,
                "tool_name": tool_name,
                "recovery_policy": "never_replay_unknown_external_side_effect",
            }),
        ))?;
        Ok(())
    }

    pub(crate) async fn clear_current_turn_external_side_effect_possible(
        &self,
        tool_name: &str,
        status: &ToolExecutionStatus,
    ) -> Result<()> {
        if !matches!(
            status,
            ToolExecutionStatus::Success | ToolExecutionStatus::Error
        ) || !matches!(
            tool_name,
            crate::tool::names::EXEC_COMMAND
                | crate::tool::names::EXEC_COMMAND_BATCH
                | crate::tool::names::SCHEDULE_DESTRUCTIVE_OPERATION
        ) {
            return Ok(());
        }
        let (agent_id, turn_id) = {
            let guard = self.inner.agent.lock().await;
            (guard.state.id.clone(), guard.state.current_turn_id.clone())
        };
        let Some(turn_id) = turn_id else {
            return Ok(());
        };
        let Some(mut turn) = self
            .inner
            .runtime_db
            .turn_records()
            .by_id(Some(&agent_id), &turn_id)?
        else {
            return Ok(());
        };
        if turn.terminal.is_some() || !turn.external_side_effect_possible {
            return Ok(());
        }
        turn.external_side_effect_possible = false;
        self.inner.runtime_db.turn_records().upsert(&turn)?;
        self.inner.storage.append_event(&AuditEvent::legacy(
            "external_command_checkpoint_durable",
            serde_json::json!({
                "agent_id": agent_id,
                "turn_id": turn_id,
                "tool_name": tool_name,
                "recovery_policy": "resume_from_durable_tool_result",
            }),
        ))?;
        Ok(())
    }

    fn take_transition_fault(&self) -> Option<TransitionFaultPoint> {
        self.inner
            .transition_faults
            .lock()
            .expect("transition fault plan lock poisoned")
            .pop_front()
    }

    fn take_terminal_transition_fault(&self) -> Option<TransitionFaultPoint> {
        #[cfg(test)]
        if let Some(fault) = self
            .inner
            .terminal_transition_faults
            .lock()
            .expect("terminal transition fault plan lock poisoned")
            .pop_front()
        {
            return Some(fault);
        }
        self.take_transition_fault()
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
    pub(crate) fn inject_next_terminal_transition_fault(&self, fault: TransitionFaultPoint) {
        let mut faults = self
            .inner
            .terminal_transition_faults
            .lock()
            .expect("terminal transition fault plan lock poisoned");
        assert!(
            faults.is_empty(),
            "a terminal transition fault is already armed"
        );
        faults.push_back(fault);
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
        let mut guard = self.begin_mutation().await?;
        guard.state.current_execution_binding = Some(binding);
        guard.persist_state(&self.inner.storage).await
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
    pub(crate) fn arm_terminal_tool_interjection_checkpoint(&self) {
        assert!(
            !self
                .inner
                .terminal_tool_interjection_checkpoint
                .armed
                .swap(true, Ordering::SeqCst),
            "terminal tool interjection checkpoint is already armed"
        );
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_terminal_tool_interjection_checkpoint(&self) {
        self.inner
            .terminal_tool_interjection_checkpoint
            .reached
            .notified()
            .await;
    }

    #[cfg(test)]
    pub(crate) fn release_terminal_tool_interjection_checkpoint(&self) {
        self.inner
            .terminal_tool_interjection_checkpoint
            .release
            .notify_one();
    }

    #[cfg(test)]
    async fn pause_at_terminal_tool_interjection_checkpoint(&self) {
        let checkpoint = &self.inner.terminal_tool_interjection_checkpoint;
        if checkpoint.armed.swap(false, Ordering::SeqCst) {
            checkpoint.reached.notify_one();
            checkpoint.release.notified().await;
        }
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
    fn take_claim_work_item_plan_status_before_commit(
        &self,
    ) -> Option<(String, crate::types::WorkItemPlanStatus)> {
        self.inner
            .claim_work_item_plan_status_before_commit
            .lock()
            .expect("claim WorkItem mutation lock poisoned")
            .take()
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

    #[cfg(test)]
    pub(crate) async fn record_timer_projection(&self, record: &TimerRecord) -> Result<()> {
        let mut mutation = self.begin_mutation().await?;
        let commit = mutation
            .commit_transition(crate::ids::audit_event_id(), |_scope| {
                self.inner.storage.append_timer(record)?;
                Ok(TransitionCommit {
                    applied: true,
                    effects: crate::runtime_db::transitions::PostCommitEffects {
                        timers: vec![record.clone()],
                        ..Default::default()
                    },
                    delivery_receipt: None,
                })
            })
            .await?;
        drop(mutation);
        self.apply_transition_commit(commit).await;
        Ok(())
    }

    pub(crate) fn work_item_written_event(
        &self,
        action: &str,
        record: &crate::types::WorkItemRecord,
        extra: Value,
    ) -> AuditEvent {
        self.work_item_written_event_with_readiness(action, record, record.readiness(), extra)
    }

    pub(crate) fn work_item_written_event_with_readiness(
        &self,
        action: &str,
        record: &crate::types::WorkItemRecord,
        readiness: crate::types::WorkItemReadiness,
        extra: Value,
    ) -> AuditEvent {
        let payload =
            WorkItemLifecycleAuditEvent::from_work_item_with_readiness(action, record, readiness);
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
        let mut guard = self.begin_mutation().await?;
        mutate(&mut guard.state)?;
        guard.persist_state(&self.inner.storage).await?;
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
    pub(crate) fn set_autonomous_continuation_hook_for_test(
        &mut self,
        hook: Arc<dyn scheduler::SemanticCandidateSelectionHook>,
    ) {
        Arc::get_mut(&mut self.inner)
            .expect("test hook must be installed before cloning the runtime")
            .autonomous_continuation_hook = hook;
    }

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
        let mut guard = self.begin_mutation().await?;
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

        match request.mode {
            CurrentRunAbortMode::StopAfterAbort => {
                scheduler::apply_stop_projection(&mut guard.state);
            }
            // Turn-scoped interrupt: the agent stays awake so the operator can
            // immediately submit the next prompt without a lifecycle start.
            CurrentRunAbortMode::IdleAfterAbort => {
                scheduler::apply_idle_projection(&mut guard.state, &self.inner.storage)?;
            }
        }
        guard.defer_abort_current_run("operator_aborted");
        guard.persist_state(&self.inner.storage).await?;
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

    #[cfg(test)]
    pub(crate) fn has_provider_reconfig(&self) -> bool {
        self.inner
            .config_snapshot
            .load()
            .provider_reconfig
            .is_some()
    }

    pub(crate) fn advisory_decision_tool_config(&self) -> (bool, Option<usize>, u64, f32) {
        let snap = self.inner.config_snapshot.load();
        (
            snap.decision_tool_enabled,
            snap.decision_tool_max_calls_per_turn,
            snap.decision_tool_timeout_ms,
            snap.decision_tool_min_confidence,
        )
    }

    pub(crate) async fn execute_advisory_decision(
        &self,
        request: decision_core::DecisionRequest<Value, Value>,
    ) -> Result<(decision_core::DecisionResponse<Value>, String, String)> {
        let snap = self.inner.config_snapshot.load();
        let reconfig = snap
            .provider_reconfig
            .as_ref()
            .ok_or_else(|| anyhow!("Decision provider configuration is unavailable"))?;
        let executor =
            decision_openai::AdvisoryDecisionExecutor::from_app_config(&reconfig.config)?
                .ok_or_else(|| anyhow!("Decision provider is disabled"))?;
        let provider = executor.provider().to_string();
        let model = executor.model().to_string();
        let response = executor
            .decide(request)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        Ok((response, provider, model))
    }

    pub(crate) fn x_search_config(&self) -> Option<crate::config::XSearchRuntimeConfig> {
        self.inner.config_snapshot.load().x_search_config.clone()
    }

    fn user_home(&self) -> Option<PathBuf> {
        self.inner.config_snapshot.load().user_home_dir.clone()
    }

    fn fallback_identity_view(&self, agent_id: &str) -> AgentIdentityView {
        let kind = if agent_id == self.inner.default_agent_id {
            AgentKind::Default
        } else {
            AgentKind::Named
        };
        AgentIdentityView {
            agent_id: agent_id.to_string(),
            name: None,
            kind,
            visibility: crate::types::AgentVisibility::Public,
            ownership: crate::types::AgentOwnership::SelfOwned,
            profile_preset: crate::types::AgentProfilePreset::PublicNamed,
            can_rename: true,
            status: crate::types::AgentRegistryStatus::Active,
            is_default_agent: agent_id == self.inner.default_agent_id,
            incarnation: 1,
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

    pub(crate) async fn agent_capability_policy(
        &self,
    ) -> Result<Option<crate::types::AgentCapabilityPolicyRecord>> {
        let agent_id = self.agent_id().await?;
        let Some(bridge) = self.inner.host_bridge.as_ref() else {
            return Ok(None);
        };
        Ok(bridge
            .canonical_relations_for_agent(&agent_id)
            .await?
            .and_then(|relations| relations.capability_policy))
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
            next_state.model_override_reasoning_effort =
                parent_state.model_override_reasoning_effort.clone();
            next_state.model_override_service_tier = parent_state.model_override_service_tier;
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
            state.attached_workspaces = next_state.attached_workspaces;
            state.active_workspace_entry = next_state.active_workspace_entry;
            state.worktree_session = next_state.worktree_session;
            state.execution_profile = next_state.execution_profile;
            state.model_override = next_state.model_override;
            state.model_override_reasoning_effort = next_state.model_override_reasoning_effort;
            state.model_override_service_tier = next_state.model_override_service_tier;
            Ok(())
        })
        .await?;
        Ok(())
    }

    #[cfg(test)]
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
            next_state.model_override_reasoning_effort =
                parent_state.model_override_reasoning_effort.clone();
            next_state.model_override_service_tier = parent_state.model_override_service_tier;
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
            state.attached_workspaces = next_state.attached_workspaces;
            state.active_workspace_entry = next_state.active_workspace_entry;
            state.worktree_session = next_state.worktree_session;
            state.execution_profile = next_state.execution_profile;
            state.model_override = next_state.model_override;
            state.model_override_reasoning_effort = next_state.model_override_reasoning_effort;
            state.model_override_service_tier = next_state.model_override_service_tier;
            Ok(())
        })
        .await?;
        Ok(())
    }

    pub(crate) async fn apply_bootstrap_workspace_state(
        &self,
        attached_workspaces: Vec<String>,
        execution_profile: ExecutionProfile,
        model_override: Option<crate::config::ModelRouteRef>,
        model_override_reasoning_effort: Option<String>,
        model_override_service_tier: Option<crate::config::ServiceTier>,
    ) -> Result<()> {
        let next_state = {
            let guard = self.inner.agent.lock().await;
            let mut next_state = guard.state.clone();
            next_state.attached_workspaces = attached_workspaces;
            next_state.active_workspace_entry = None;
            next_state.worktree_session = None;
            workspace::canonicalize_agent_home_bindings(
                &mut next_state,
                self.inner.storage.data_dir(),
                &guard.state.id,
            )?;
            next_state.execution_profile = execution_profile;
            next_state.model_override = model_override;
            next_state.model_override_reasoning_effort = model_override_reasoning_effort;
            next_state.model_override_service_tier = model_override_service_tier;
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
            state.attached_workspaces = next_state.attached_workspaces;
            state.active_workspace_entry = next_state.active_workspace_entry;
            state.worktree_session = next_state.worktree_session;
            state.execution_profile = next_state.execution_profile;
            state.model_override = next_state.model_override;
            state.model_override_reasoning_effort = next_state.model_override_reasoning_effort;
            state.model_override_service_tier = next_state.model_override_service_tier;
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

    /// Registered execution roots for the given workspaces. Used to populate
    /// execution snapshots so historical `workspace://` references with
    /// `?root=` parameters can still be resolved.
    #[cfg(test)]
    pub(crate) fn execution_root_refs_for_workspaces(
        &self,
        workspace_ids: &[String],
    ) -> Vec<crate::system::ExecutionRootRef> {
        let repo = self.inner.runtime_db.execution_root_entries();
        let mut roots = Vec::new();
        for ws_id in workspace_ids {
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
        roots
    }

    fn workspace_view_from_state(&self, state: &AgentState) -> Result<WorkspaceView> {
        workspace::workspace_view_from_state(state, self.inner.storage.data_dir().to_path_buf())
    }

    #[cfg(test)]
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
        // historical `?root=` parameters in workspace:// URIs.
        snapshot.execution_roots = self.execution_root_refs_for_workspaces(attached_workspace_ids);
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

    #[inline(never)]
    fn begin_interactive_turn_with_provenance<'a>(
        &'a self,
        message: Option<&'a MessageEnvelope>,
        operator_binding_id: Option<&'a str>,
        operator_reply_route_id: Option<&'a str>,
        execution_admission_provenance: Option<ExecutionAdmissionProvenance>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<(String, u64)>> + Send + 'a>>
    {
        // The prepared input and private candidate stay on this heap frame,
        // rather than enlarging every dispatch/recovery caller's stack frame.
        Box::pin(self.begin_interactive_turn_with_provenance_inner(
            message,
            operator_binding_id,
            operator_reply_route_id,
            execution_admission_provenance,
            None,
        ))
    }

    #[inline(never)]
    fn begin_dispatch_turn<'a>(
        &'a self,
        message: &'a MessageEnvelope,
        operator_binding_id: Option<&'a str>,
        operator_reply_route_id: Option<&'a str>,
        execution_admission_provenance: Option<ExecutionAdmissionProvenance>,
        prior_closure: &'a ClosureDecision,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<(String, u64)>> + Send + 'a>>
    {
        Box::pin(self.begin_interactive_turn_with_provenance_inner(
            Some(message),
            operator_binding_id,
            operator_reply_route_id,
            execution_admission_provenance,
            Some(prior_closure),
        ))
    }

    async fn begin_interactive_turn_with_provenance_inner(
        &self,
        message: Option<&MessageEnvelope>,
        operator_binding_id: Option<&str>,
        operator_reply_route_id: Option<&str>,
        execution_admission_provenance: Option<ExecutionAdmissionProvenance>,
        dispatch_prior_closure: Option<&ClosureDecision>,
    ) -> Result<(String, u64)> {
        use self::resident_input_evidence::{
            InputEvidenceFamily as Family, InputEvidenceKey as Key,
        };
        let mut guard = self.begin_mutation().await?;
        let mut roots = std::collections::BTreeSet::new();
        if let Some(message) = message {
            anyhow::ensure!(
                message.agent_id == guard.state.id,
                "turn input agent mismatch"
            );
            roots.insert(Key::new(Family::Message, &message.id));
        }
        if let Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) =
            execution_admission_provenance.as_ref()
        {
            roots.insert(Key::new(Family::Attempt, activation_id));
        }
        guard
            .prepare_owner_inputs(roots, Default::default())
            .await?;
        let inputs = guard.capture_input_evidence()?;
        let execution = inputs.execution_state_if_initialized()?;
        // A payload Turn ID is a preallocation/replay hint. Only this captured
        // canonical assignment grants the previous Turn's body and terminal.
        let assigned_source_turn = if let Some(message) = message {
            let assignment = inputs.message_turn_keys.get(&message.id).ok_or_else(|| {
                self::resident_input_evidence::InputEvidenceReadError::NeedsPreparation(Key::new(
                    Family::Message,
                    &message.id,
                ))
            })?;
            assignment
                .as_deref()
                .map(|turn_id| {
                    provider_recovery::captured_turn(&inputs, turn_id)?
                        .map(|turn| (turn_id.to_owned(), turn))
                        .ok_or_else(|| anyhow!("assigned turn input source is unavailable"))
                })
                .transpose()?
        } else {
            None
        };
        let replay_source = message
            .and_then(|message| message.source_refs.get("replay_source_turn_id"))
            .map(|source_turn_id| {
                if let Some((assigned_id, source)) = &assigned_source_turn {
                    (assigned_id.clone(), Some(source.clone()))
                } else {
                    // Interrupted claim-only reentry may have no started Turn.
                    // Keep its hint without reading or granting an old body.
                    (source_turn_id.clone(), None)
                }
            });
        let terminal_source_turn = replay_source
            .is_none()
            .then_some(assigned_source_turn.as_ref())
            .flatten()
            .filter(|(_, turn)| turn.terminal.is_some())
            .cloned();
        #[allow(deprecated)]
        let canonical_execution_binding = match execution_admission_provenance.as_ref() {
            Some(ExecutionAdmissionProvenance::Canonical { activation_id, .. }) => {
                let message = message.ok_or_else(|| {
                    anyhow!("canonical execution admission requires a source message")
                })?;
                let attempt = execution
                    .as_ref()
                    .and_then(|state| state.attempts.get(activation_id).cloned())
                    .ok_or_else(|| {
                        anyhow!("canonical execution admission references an unknown attempt")
                    })?;
                anyhow::ensure!(
                    attempt.agent_id == message.agent_id
                        && attempt.source_message_id.as_deref() == Some(message.id.as_str()),
                    "canonical turn admission source differs from the original claim"
                );
                let claimed_work_revision = attempt.admitted_fences.work_item_source_revision;
                let owner = match attempt.binding {
                    crate::domain::execution_protocol::ExecutionBinding::WorkItem {
                        work_item_id,
                    } => crate::types::TurnOwner::WorkItem { work_item_id },
                    crate::domain::execution_protocol::ExecutionBinding::Conversation {
                        interaction_id,
                    } => crate::types::TurnOwner::Conversation { interaction_id },
                    crate::domain::execution_protocol::ExecutionBinding::AgentLifecycle {
                        agent_id,
                    } => crate::types::TurnOwner::AgentLifecycle { agent_id },
                    crate::domain::execution_protocol::ExecutionBinding::Command => {
                        crate::types::TurnOwner::Command
                    }
                };
                Some((activation_id.clone(), owner, claimed_work_revision))
            }
            Some(ExecutionAdmissionProvenance::LegacyCompat { .. }) => {
                return Err(anyhow!(
                    "legacy-compatible execution admission is historical read-only state"
                ));
            }
            None => None,
        };
        if let Some(message) = message {
            let work_item_id = message.work_item_id.clone().or_else(|| {
                guard
                    .state
                    .current_turn_work_item_id
                    .clone()
                    .or_else(|| guard.state.current_work_item_id.clone())
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
                && execution
                    .as_ref()
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
        let projected_turn_work_item_id = if let Some((_, owner, _)) = &canonical_execution_binding
        {
            owner.work_item_id().map(ToString::to_string)
        } else if let Some((_, source_turn)) = &replay_source {
            source_turn
                .as_ref()
                .and_then(|turn| turn.current_work_item_id.clone())
        } else {
            guard
                .state
                .current_turn_work_item_id
                .clone()
                .or_else(|| guard.state.current_work_item_id.clone())
        };
        let binding_work_item_id = message.and_then(|message| {
            if canonical_execution_binding.is_some() {
                projected_turn_work_item_id.clone()
            } else {
                message
                    .work_item_id
                    .clone()
                    .or_else(|| projected_turn_work_item_id.clone())
            }
        });
        let fallback_work_revision = if canonical_execution_binding
            .as_ref()
            .and_then(|(_, _, revision)| *revision)
            .is_none()
        {
            if let Some(work_item_id) = &binding_work_item_id {
                let key =
                    resident_work::WorkKey::new(resident_work::WorkFamily::Work, work_item_id);
                let mut request = resident_work::WorkCaptureRequest::current();
                request.include_live = false;
                request.nonopen_limit = 0;
                request.mandatory_roots.insert(key.clone());
                guard.prepare_owner_work_request(&request).await?;
                let work = guard.capture_work(&request)?;
                match work.lookup(&key)? {
                    Some(record) => match &record.record {
                        resident_work::CanonicalWorkRecord::Work(item) => Some(item.revision),
                        _ => anyhow::bail!("StartedTurn Work revision has a different family"),
                    },
                    None => None,
                }
            } else {
                None
            }
        } else {
            None
        };
        let state = {
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
            if let Some((_, owner, _)) = canonical_execution_binding.as_ref() {
                guard.state.current_turn_work_item_id =
                    owner.work_item_id().map(ToString::to_string);
            } else if let Some((_, source_turn)) = replay_source.as_ref() {
                guard.state.current_turn_work_item_id = source_turn
                    .as_ref()
                    .and_then(|turn| turn.current_work_item_id.clone());
            } else if guard.state.current_turn_work_item_id.is_none() {
                guard.state.current_turn_work_item_id = guard.state.current_work_item_id.clone();
            }
            guard.state.current_execution_binding = message.map(|message| {
                let work_item_id = if canonical_execution_binding.is_some() {
                    canonical_execution_binding
                        .as_ref()
                        .and_then(|(_, owner, _)| owner.work_item_id().map(ToString::to_string))
                } else {
                    message
                        .work_item_id
                        .clone()
                        .or_else(|| guard.state.current_turn_work_item_id.clone())
                };
                let claimed_work_revision = canonical_execution_binding
                    .as_ref()
                    .and_then(|(_, _, claimed_work_revision)| *claimed_work_revision)
                    .or(fallback_work_revision);
                let activation_id = canonical_execution_binding
                    .as_ref()
                    .map(|(activation_id, _, _)| activation_id.clone());
                WorkItemExecutionBinding {
                    activation_id,
                    admission_provenance: execution_admission_provenance.clone(),
                    source_message_id: message.id.clone(),
                    turn_id,
                    owner: canonical_execution_binding
                        .as_ref()
                        .map(|(_, owner, _)| owner.clone()),
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
            let prior_terminal = terminal_source_turn
                .as_ref()
                .map(|(_, turn)| turn)
                .or_else(|| replay_source.as_ref().and_then(|(_, turn)| turn.as_ref()))
                .and_then(|turn| turn.terminal.clone());
            let mut started_turn =
                Self::build_started_turn_record(&guard.state, message, prior_terminal.clone())?;
            if let (Some(message), Some((source_id, _))) = (
                message,
                replay_source
                    .as_ref()
                    .map(|(id, turn)| (id, turn.as_ref()))
                    .or_else(|| {
                        terminal_source_turn
                            .as_ref()
                            .map(|(id, turn)| (id, Some(turn)))
                    }),
            ) {
                started_turn.replay = Some(crate::types::TurnReplayProvenance {
                    source_message_id: message.id.clone(),
                    source_turn_id: source_id.clone(),
                    reason: "interrupted_queue_claim_reentry".into(),
                    prior_terminal,
                });
            }
            // The posture, actual input assignment and started Turn publish as
            // one acknowledged candidate, so no reader sees a dangling binding.
            if let Some(prior_closure) = dispatch_prior_closure {
                use crate::runtime_db::{
                    dispatch_memory::DispatchMemoryStart,
                    episode_memory::{EpisodeMemoryInvocation, EpisodeMemoryKey},
                };
                let source = message.context("dispatch memory requires its actual source")?;
                let start = DispatchMemoryStart::new(
                    EpisodeMemoryKey {
                        agent_id: guard.state.id.clone(),
                        identity_incarnation: self.inner.identity_incarnation,
                        actual_dispatch_turn_id: started_turn.turn_id.clone(),
                    },
                    EpisodeMemoryInvocation {
                        message_id: source.id.clone(),
                        turn_index: started_turn.turn_index,
                    },
                    source,
                    prior_closure.clone(),
                    guard.last_persisted_state.working_memory.clone(),
                )?;
                guard.persist_started_dispatch(started_turn, start).await?;
            } else {
                guard.persist_state_with_started_turn(started_turn).await?;
            }
            guard.state.clone()
        };
        {
            // StartedTurn initializes its complete Turn/trigger Wait relations,
            // including standalone execution and reducer-only dispatches.
            let mut request = resident_work::WorkCaptureRequest::current();
            request.required_turn_ids.insert(
                state
                    .current_turn_id
                    .clone()
                    .context("started dispatch Turn missing")?,
            );
            if let Some(message) = message {
                request
                    .required_trigger_message_ids
                    .insert(message.id.clone());
            }
            guard.prepare_owner_work_request(&request).await?;
        }
        drop(guard);
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
        Ok((
            state.current_turn_id.expect("started turn identity"),
            state.turn_index,
        ))
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
                Some(ExecutionAdmissionProvenance::Canonical {
                    scenario_class,
                    activation_id,
                })
            } else {
                None
            }
        } else {
            None
        };
        self.begin_interactive_turn_with_provenance(
            message,
            operator_binding_id,
            operator_reply_route_id,
            provenance,
        )
        .await
        .map(|_| ())
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
        let mut guard = self.begin_mutation().await?;
        for skill in &mut guard.state.active_skills {
            if matches!(skill.activation_state, SkillActivationState::TurnActive) {
                skill.activation_state = SkillActivationState::SessionActive;
            }
        }
        guard.persist_state(&self.inner.storage).await?;
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
        let mut guard = self.begin_mutation().await?;
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
        guard.persist_state(&self.inner.storage).await?;
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

    pub async fn enqueue(&self, message: MessageEnvelope) -> Result<MessageEnvelope> {
        let (message, _) = self.enqueue_internal(message, None).await?;
        Ok(message)
    }

    pub(crate) async fn enqueue_delivery(
        &self,
        message: MessageEnvelope,
        delivery: &AgentMessageDeliveryRecord,
    ) -> Result<AgentMessageDeliveryReceipt> {
        let (_, receipt) = self.enqueue_internal(message, Some(delivery)).await?;
        receipt.context("delivery admission completed without a receipt")
    }

    async fn enqueue_internal(
        &self,
        mut message: MessageEnvelope,
        delivery: Option<&AgentMessageDeliveryRecord>,
    ) -> Result<(MessageEnvelope, Option<AgentMessageDeliveryReceipt>)> {
        let _admission = self.execution_admission_lease()?;
        message.normalize_admission_fields();
        message.turn_id = normalized_turn_id(message.turn_id.as_deref());
        if message.turn_id.is_none() {
            message.turn_id = Some(crate::ids::turn_id());
        }
        if message.trace_context.is_none() {
            message.trace_context = Some(crate::observability::TraceContext::new_root(true));
        }
        let enqueue_started_at = chrono::Utc::now();
        for attempt in 0..ENQUEUE_AGENT_STATE_MAX_ATTEMPTS {
            match self.enqueue_attempt(&message, delivery).await {
                Ok(mut commit) => {
                    commit.effects.notify_scheduler =
                        commit.delivery_receipt.as_ref().is_none_or(|receipt| {
                            receipt.outcome == AgentMessageDeliveryOutcome::Accepted
                                && !receipt.idempotent_replay
                        });
                    let receipt = commit.delivery_receipt.clone();
                    self.apply_transition_commit(commit).await;
                    if let Some(parent) = message.trace_context.as_ref() {
                        let span_context = parent.child();
                        crate::observability::record_span(
                            &span_context,
                            crate::observability::completed_span(
                                "holon.message.enqueue",
                                &span_context,
                                Some(parent.span_id.clone()),
                                enqueue_started_at,
                                crate::observability::TraceSpanStatus::Ok,
                                crate::observability::TraceAttributes {
                                    agent_id: Some(message.agent_id.clone()),
                                    message_id: Some(message.id.clone()),
                                    turn_id: message.turn_id.clone(),
                                    work_item_id: message.work_item_id.clone(),
                                    task_id: message.task_id.clone(),
                                    ..Default::default()
                                },
                            ),
                        );
                    }
                    return Ok((message, receipt));
                }
                Err(error) => {
                    let can_retry = attempt + 1 < ENQUEUE_AGENT_STATE_MAX_ATTEMPTS
                        && retryable_enqueue_conflict(&error, message.agent_id.as_str());
                    if !can_retry {
                        if retryable_enqueue_conflict(&error, message.agent_id.as_str()) {
                            tracing::warn!(
                                error = %error,
                                retryable = true,
                                agent_id = %message.agent_id,
                                attempt = attempt + 1,
                                max_attempts = ENQUEUE_AGENT_STATE_MAX_ATTEMPTS,
                                "runtime message enqueue OCC retries exhausted"
                            );
                        }
                        return Err(error);
                    }
                    if !self
                        .refresh_enqueue_agent_state_baseline(&message.agent_id)
                        .await?
                    {
                        tracing::warn!(
                            error = %error,
                            retryable = true,
                            agent_id = %message.agent_id,
                            attempt = attempt + 1,
                            max_attempts = ENQUEUE_AGENT_STATE_MAX_ATTEMPTS,
                            "runtime message enqueue OCC baseline refresh failed"
                        );
                        return Err(error);
                    }
                }
            }
        }
        unreachable!("enqueue attempts always return or retry")
    }

    async fn enqueue_attempt(
        &self,
        message: &MessageEnvelope,
        delivery: Option<&AgentMessageDeliveryRecord>,
    ) -> Result<TransitionCommit> {
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
        let mut audit_events = vec![AuditEvent::legacy(
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
                "routing_context": message.routing_context.clone(),
            }),
        )];
        let commit = {
            let mut guard = self.begin_mutation().await?;
            anyhow::ensure!(
                !self.inner.shutdown_requested.load(Ordering::SeqCst),
                "runtime_execution_admission_closed"
            );
            if matches!(
                &message.origin,
                MessageOrigin::System { subsystem } if subsystem == "task_result_recovery"
            ) && message.authority_class == AuthorityClass::RuntimeInstruction
            {
                if let Some(owner_key) = message.source_refs.get("task_result_owner_key") {
                    if let Some(existing_id) =
                        task_result_recovery::outstanding_recovery_message_id(
                            self,
                            &message.agent_id,
                            message.work_item_id.as_deref(),
                            owner_key,
                        )?
                    {
                        if existing_id != message.id {
                            return Ok(TransitionCommit::default());
                        }
                    }
                }
            }
            let queue_needs_push = guard
                .queue
                .peek_next_matching(|queued| queued.id == message.id)
                .is_none();
            let expected_persisted_state = guard.last_persisted_state.clone();
            let mut committed_state = guard.state.clone();
            let discard_due_to_stop = committed_state.status == AgentStatus::Stopped;
            if matches!(&message.origin, MessageOrigin::System { subsystem } if subsystem == "subagent_reclamation")
            {
                anyhow::ensure!(
                    !discard_due_to_stop,
                    "subagent_reminder_deferred: parent_stopped"
                );
                anyhow::ensure!(
                    committed_state
                        .turn_budget
                        .as_ref()
                        .is_none_or(|budget| committed_state
                            .turn_index
                            .saturating_sub(budget.run_start_turn_index)
                            < budget.max_turns),
                    "subagent_reminder_deferred: parent_budget_exhausted"
                );
            }
            let previous_status = committed_state.status.clone();
            let previous_sleeping_until = committed_state.sleeping_until;
            committed_state.pending = guard
                .queue
                .len()
                .saturating_add(usize::from(queue_needs_push && !discard_due_to_stop));
            committed_state.last_wake_reason = Some(format!("{:?}", message.kind));
            // The original transaction returns its actual canonical count.
            committed_state.total_message_count = guard.state.total_message_count;
            if !discard_due_to_stop
                && scheduler::apply_message_wake_projection(&mut committed_state)
            {
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
            let queue_status = if discard_due_to_stop {
                audit_events.push(AuditEvent::legacy(
                    "message_discarded_agent_stopped",
                    serde_json::json!({
                        "message_id": message.id,
                        "agent_id": message.agent_id,
                        "message_kind": message.kind,
                        "origin": message.origin,
                    }),
                ));
                QueueEntryStatus::Aborted
            } else {
                audit_events.push(AuditEvent::typed(
                    RuntimeEventKind::MessageEnqueued,
                    &MessageLifecycleAuditEvent::from_message(&message),
                )?);
                QueueEntryStatus::Queued
            };
            let command = crate::runtime_db::transitions::QueueTransitionCommand {
                agent_id: message.agent_id.clone(),
                operation: if discard_due_to_stop {
                    crate::runtime_db::transitions::QueueOperation::Discard
                } else {
                    crate::runtime_db::transitions::QueueOperation::Admit
                },
                mutation: crate::runtime_db::transitions::QueueMutation::Upsert(QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: message.agent_id.clone(),
                    priority: message.priority.clone(),
                    status: queue_status.clone(),
                    created_at: existing_queue_entry
                        .as_ref()
                        .map_or(message.created_at, |entry| entry.created_at),
                    updated_at: self.now(),
                }),
                scheduler_claim_work_item: None,
                agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                    expected: Some(Box::new(expected_persisted_state)),
                    record: Box::new(committed_state.clone()),
                }),
                message_evidence: vec![message.clone()],
                transcript_entries: Vec::new(),
                turn_record: None,
                audit_events,
                notify_scheduler: queue_status == QueueEntryStatus::Queued,
                fault: self.take_transition_fault(),
                brief_evidence: Vec::new(),
            };
            let mut prepared =
                crate::runtime_db::transitions::PreparedQueueTransition::new(command);
            prepared.delivery = delivery.cloned();
            guard
                .commit_prepared_queue(crate::ids::audit_event_id(), prepared)
                .await?
        };
        Ok(commit)
    }

    pub(crate) async fn refresh_enqueue_agent_state_baseline(
        &self,
        agent_id: &str,
    ) -> Result<bool> {
        let mut guard = self.begin_mutation().await?;
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
        guard.install_durable_readback().await?;
        Ok(true)
    }

    pub(crate) fn append_audit_event(&self, kind: &str, data: serde_json::Value) -> Result<()> {
        self.inner
            .storage
            .append_event(&AuditEvent::legacy(kind, data))
    }

    pub(crate) fn append_decision_advisory_event(
        &self,
        event: &crate::decision_telemetry::DecisionAdvisoryCompletedEvent,
    ) -> Result<()> {
        self.inner.storage.append_event(&AuditEvent::typed(
            crate::runtime_event::RuntimeEventKind::DecisionAdvisoryCompleted,
            event,
        )?)
    }

    pub(crate) fn append_decision_outcome_event(
        &self,
        event: &crate::decision_telemetry::DecisionOutcomeRecordedEvent,
    ) -> Result<()> {
        self.inner.storage.append_event(&AuditEvent::typed(
            crate::runtime_event::RuntimeEventKind::DecisionOutcomeRecorded,
            event,
        )?)
    }

    pub(crate) fn record_decision_outcome_for_task(
        &self,
        task: &crate::types::TaskRecord,
    ) -> Result<()> {
        let Some(parent_message_id) = task.parent_message_id.as_deref() else {
            return Ok(());
        };
        let agent_id = self.inner.storage.current_agent_id()?;
        let events = self
            .inner
            .runtime_db
            .audit_events()
            // This bounded lookup intentionally correlates only recent
            // advisory/task events; older telemetry remains valid but may
            // lack an outcome association until an indexed lookup exists.
            .recent(agent_id.as_deref(), 512)?;
        let Some(advisory) = events.iter().rev().find(|event| {
            event.kind == "decision_advisory_completed"
                && event.data["message_id"].as_str() == Some(parent_message_id)
                && task.work_item_id.as_deref().is_none_or(|work_item_id| {
                    event.data["work_item_id"].as_str() == Some(work_item_id)
                })
        }) else {
            return Ok(());
        };
        let Some(decision_id) = advisory.data["decision_id"].as_str() else {
            return Ok(());
        };
        if events.iter().any(|event| {
            event.kind == "decision_outcome_recorded"
                && event.data["decision_id"].as_str() == Some(decision_id)
                && event.data["task_id"].as_str() == Some(task.id.as_str())
        }) {
            return Ok(());
        }
        let task_status = match task.status {
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Interrupted => "interrupted",
            TaskStatus::Cancelling => "cancelling",
            TaskStatus::Running => "running",
            TaskStatus::Queued => "queued",
        };
        let detail = task.detail.as_ref();
        let actual_choice = detail
            .and_then(|value| value.get("actual_choice").or_else(|| value.get("choice")))
            .and_then(|value| crate::decision_telemetry::safe_value(Some(value), 256));
        let operator_feedback = detail
            .and_then(|value| value.get("operator_feedback"))
            .and_then(|value| crate::decision_telemetry::safe_value(Some(value), 512));
        let baseline_choice = detail
            .and_then(|value| value.get("baseline_choice"))
            .and_then(|value| crate::decision_telemetry::safe_value(Some(value), 256));
        let result_label = detail
            .and_then(|value| value.get("result_label"))
            .and_then(|value| crate::decision_telemetry::safe_value(Some(value), 240))
            .or_else(|| {
                task.summary
                    .as_deref()
                    .map(|value| value.chars().take(240).collect::<String>())
            });
        let event = crate::decision_telemetry::DecisionOutcomeRecordedEvent {
            decision_id: decision_id.to_string(),
            agent_id: task.agent_id.clone(),
            turn_id: advisory.data["turn_id"].as_str().map(ToString::to_string),
            message_id: Some(parent_message_id.to_string()),
            work_item_id: task.work_item_id.clone(),
            task_id: task.id.clone(),
            task_status: task_status.to_string(),
            actual_choice,
            operator_feedback,
            result_label,
            error_class: matches!(task.status, TaskStatus::Failed)
                .then_some("task_failed".to_string()),
            baseline_choice,
            recorded_at: Utc::now(),
        };
        self.append_decision_outcome_event(&event)
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

    fn commit_queue_terminal_settlement<'a>(
        &'a self,
        record: QueueEntryRecord,
        audit_events: Vec<AuditEvent>,
        notify_scheduler: bool,
        terminal_transition: Option<&'a turn::TurnTerminalTransition>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<bool>> + Send + 'a>> {
        self.commit_queue_terminal_settlement_with_evidence(
            record,
            audit_events,
            notify_scheduler,
            terminal_transition,
            None,
            Vec::new(),
            Vec::new(),
        )
    }

    fn validate_terminal_brief_settlement(
        &self,
        transition: &turn::TurnTerminalTransition,
    ) -> Result<()> {
        let has_brief = !transition.turn_record.produced_brief_ids.is_empty();
        let no_brief_reason = transition.terminal.no_brief_reason.as_ref();
        let valid_no_brief = match (transition.terminal.kind, no_brief_reason) {
            (TurnTerminalKind::Completed, Some(TurnNoBriefReason::ReducerOnly { reason })) => {
                !reason.trim().is_empty()
            }
            (TurnTerminalKind::Completed, Some(TurnNoBriefReason::ToolOnlyWait)) => true,
            (TurnTerminalKind::Aborted, Some(TurnNoBriefReason::Aborted)) => true,
            (TurnTerminalKind::Interrupted, Some(TurnNoBriefReason::Interrupted)) => true,
            _ => false,
        };
        if (has_brief && no_brief_reason.is_none()) || (!has_brief && valid_no_brief) {
            return Ok(());
        }

        let diagnostic = serde_json::json!({
            "agent_id": transition.turn_record.agent_id,
            "turn_id": transition.terminal.turn_id,
            "turn_index": transition.terminal.turn_index,
            "terminal_kind": transition.terminal.kind,
            "produced_brief_ids": transition.turn_record.produced_brief_ids,
            "no_brief_reason": transition.terminal.no_brief_reason,
            "failure": if has_brief {
                "brief_and_no_brief_reason_are_mutually_exclusive"
            } else {
                "terminal_missing_brief_settlement"
            },
        });
        self.inner.storage.append_event(&AuditEvent::legacy(
            "turn_terminal_brief_settlement_failed",
            diagnostic,
        ))?;
        Err(anyhow!(
            "turn {} cannot settle: expected exactly one of canonical Brief or valid typed NoBrief reason",
            transition.terminal.turn_id
        ))
    }

    fn validate_wait_for_terminal_publication(
        &self,
        transition: &turn::TurnTerminalTransition,
        prepared: &PreparedWaitForSettlement,
        audit_events: &[AuditEvent],
    ) -> Result<()> {
        use crate::tool::tools::wait_for::WaitForDeliveryArg;

        match prepared.delivery {
            WaitForDeliveryArg::Silent => {
                let scope = prepared.brief_publication_scope.as_ref().ok_or_else(|| {
                    anyhow!("silent WaitFor settlement is missing its Brief publication scope")
                })?;
                anyhow::ensure!(
                    prepared.brief.is_none()
                        && transition.turn_record.produced_brief_ids == scope.existing_brief_ids
                        && transition.terminal.no_brief_reason
                            == if scope.existing_brief_ids.is_empty() {
                                Some(TurnNoBriefReason::ToolOnlyWait)
                            } else {
                                None
                            }
                        && audit_events
                            .iter()
                            .all(|event| event.kind != "brief_created"),
                    "silent WaitFor terminal settlement cannot publish a Brief"
                );
            }
            WaitForDeliveryArg::Final => {
                let brief = prepared.brief.as_ref().ok_or_else(|| {
                    anyhow!("final WaitFor settlement is missing its result Brief")
                })?;
                let scope = prepared.brief_publication_scope.as_ref().ok_or_else(|| {
                    anyhow!("final WaitFor settlement is missing its Brief publication scope")
                })?;
                let mut expected_brief_ids = scope.existing_brief_ids.clone();
                expected_brief_ids.push(brief.id.clone());
                anyhow::ensure!(
                    transition.turn_record.produced_brief_ids == expected_brief_ids,
                    "final WaitFor settlement must publish exactly its prepared Brief"
                );
                anyhow::ensure!(
                    brief.turn_id.as_deref() == Some(transition.terminal.turn_id.as_str())
                        && brief.turn_id.as_deref()
                            == Some(transition.turn_record.turn_id.as_str()),
                    "final WaitFor Brief must belong to the terminal Turn"
                );
                anyhow::ensure!(
                    brief.finalizes_assistant_round_id.is_some(),
                    "final WaitFor Brief must finalize its source assistant round"
                );
                let matching_created_events = audit_events
                    .iter()
                    .filter(|event| {
                        event.kind == "brief_created"
                            && event
                                .data
                                .get("brief_id")
                                .and_then(serde_json::Value::as_str)
                                == Some(brief.id.as_str())
                    })
                    .count();
                anyhow::ensure!(
                    matching_created_events == 1
                        && audit_events
                            .iter()
                            .filter(|event| event.kind == "brief_created")
                            .count()
                            == 1,
                    "final WaitFor settlement must contain exactly one matching brief_created event"
                );
            }
        }
        Ok(())
    }

    #[inline(never)]
    fn commit_terminal_transition<'a>(
        &'a self,
        terminal_transition: &'a turn::TurnTerminalTransition,
        audit_events: Vec<AuditEvent>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(self.commit_terminal_transition_inner(terminal_transition, audit_events))
    }

    async fn commit_terminal_transition_inner(
        &self,
        terminal_transition: &turn::TurnTerminalTransition,
        audit_events: Vec<AuditEvent>,
    ) -> Result<bool> {
        let mut normalized_transition = terminal_transition.clone();
        normalized_transition.normalize_brief_settlement();
        let terminal_transition = &normalized_transition;
        self.validate_terminal_brief_settlement(terminal_transition)?;
        for attempt in 0..ENQUEUE_AGENT_STATE_MAX_ATTEMPTS {
            let mut mutation = self.begin_mutation().await?;
            let agent_state = {
                let guard = &mutation;
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
                fault: self.take_terminal_transition_fault(),
            };
            let result = mutation
                .commit_prepared_terminal(crate::ids::audit_event_id(), command)
                .await;
            drop(mutation);
            match result {
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

    #[inline(never)]
    fn commit_queue_terminal_settlement_with_evidence<'a>(
        &'a self,
        record: QueueEntryRecord,
        audit_events: Vec<AuditEvent>,
        notify_scheduler: bool,
        terminal_transition: Option<&'a turn::TurnTerminalTransition>,
        agent_update: Option<TerminalAgentStateUpdate>,
        transcript_entries: Vec<TranscriptEntry>,
        brief_evidence: Vec<BriefRecord>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(self.commit_queue_terminal_settlement_with_evidence_inner(
            record,
            audit_events,
            notify_scheduler,
            terminal_transition,
            agent_update,
            transcript_entries,
            brief_evidence,
        ))
    }

    async fn commit_queue_terminal_settlement_with_evidence_inner(
        &self,
        record: QueueEntryRecord,
        audit_events: Vec<AuditEvent>,
        notify_scheduler: bool,
        terminal_transition: Option<&turn::TurnTerminalTransition>,
        agent_update: Option<TerminalAgentStateUpdate>,
        transcript_entries: Vec<TranscriptEntry>,
        brief_evidence: Vec<BriefRecord>,
    ) -> Result<bool> {
        let normalized_terminal_transition = terminal_transition.cloned().map(|mut transition| {
            transition.normalize_brief_settlement();
            transition
        });
        let terminal_transition = normalized_terminal_transition.as_ref();
        if let Some(transition) = terminal_transition {
            self.validate_terminal_brief_settlement(transition)?;
        }
        let prepared_completion = terminal_transition
            .and_then(|transition| transition.prepared_work_item_completion.as_ref());
        let mut prepared_wait =
            terminal_transition.and_then(|transition| transition.prepared_wait_for.clone());
        anyhow::ensure!(
            prepared_completion.is_none() || prepared_wait.is_none(),
            "terminal settlement cannot complete a WorkItem and register WaitFor together"
        );
        let task_result_settlement = if terminal_transition
            .is_some_and(|transition| !transition.terminal.kind.is_failure())
        {
            let guard = self.inner.agent.lock().await;
            guard
                .state
                .current_execution_binding
                .as_ref()
                .and_then(|binding| {
                    let activation_id = binding.activation_id.as_ref()?;
                    match binding.admission_provenance.as_ref()? {
                        crate::types::ExecutionAdmissionProvenance::Canonical {
                            activation_id: admitted_activation_id,
                            ..
                        } if admitted_activation_id == activation_id => {
                            Some(crate::runtime_db::task_result_settlement::TaskResultActivationSettlement {
                                activation_id: activation_id.clone(),
                                disposition: crate::runtime_db::TaskResultSettlementDisposition::ModelDelivered,
                                settled_at: self.now(),
                            })
                        }
                        _ => None,
                    }
                })
        } else {
            None
        };
        let mut execution_protocol = if let Some(prepared) = prepared_completion {
            execution_protocol_completion_transition_from_prepared(
                &record,
                terminal_transition
                    .map(|transition| &transition.turn_record)
                    .expect("prepared completion requires a terminal Turn"),
                prepared,
            )?
        } else if let Some(prepared) = prepared_wait.as_deref() {
            prepared.execution_protocol.clone()
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
                let (source, outcome) = prepared
                    .continuation_resumes
                    .get(&continuation.id)
                    .context("prepared completion continuation source missing")?
                    .clone();
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
                .chain(
                    prepared_wait
                        .as_deref()
                        .into_iter()
                        .filter_map(|prepared| prepared.brief.clone()),
                )
                .collect(),
        };
        for attempt in 0..ENQUEUE_AGENT_STATE_MAX_ATTEMPTS {
            // Rebuild guard-dependent fields from the current baseline.
            let mut mutation = self.begin_mutation().await?;
            let agent_state = {
                let guard = &mutation;
                let mut state = if let Some(prepared) = prepared_completion {
                    rebase_prepared_completion_agent_state(prepared, &guard.last_persisted_state)?
                } else if let Some(prepared) = prepared_wait.as_deref() {
                    if let Some(mutation) = prepared.command.agent_state.as_ref() {
                        rebase_committed_agent_state(
                            mutation.expected.as_deref().ok_or_else(|| {
                                anyhow!("prepared wait is missing its agent state baseline")
                            })?,
                            &mutation.record,
                            &guard.state,
                        )?
                        .0
                    } else {
                        guard.state.clone()
                    }
                } else {
                    let mut state = guard.state.clone();
                    if let Some(update) = agent_update.as_ref() {
                        if state.status != AgentStatus::Stopped {
                            scheduler::apply_idle_projection(&mut state, &self.inner.storage)?;
                        }
                        if let Some(failure) = update.last_runtime_failure.as_ref() {
                            state.last_runtime_failure = Some(failure.clone());
                        }
                    }
                    state
                };
                if let Some(admission) = prepared_wait
                    .as_ref()
                    .and_then(|wait| wait.command.task_result_admission.as_ref())
                {
                    state.pending = guard.queue.len()
                        + usize::from(
                            guard
                                .queue
                                .peek_next_matching(|message| message.id == admission.message.id)
                                .is_none(),
                        );
                    state.last_wake_reason = Some("TaskResult".into());
                    scheduler::apply_message_wake_projection(&mut state);
                }
                let agent_state = if let Some(transition) = terminal_transition {
                    if state.current_run_id.is_some()
                        && state.current_run_id == transition.turn_record.run_id
                        && state.current_turn_id.as_deref()
                            == Some(transition.terminal.turn_id.as_str())
                        && state.turn_index == transition.terminal.turn_index
                        && state
                            .current_execution_binding
                            .as_ref()
                            .is_some_and(|binding| {
                                binding.turn_id == transition.terminal.turn_id
                                    && binding.source_message_id == record.message_id
                            })
                    {
                        state.current_run_id = None;
                    }
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
            if let Some(prepared) = prepared_wait.as_deref() {
                command
                    .audit_events
                    .extend(prepared.command.audit_events.clone());
                if let Some(brief) = prepared.brief.as_ref() {
                    command.audit_events.push(brief_created_event_for(brief)?);
                }
                if let Some(transition) = terminal_transition {
                    self.validate_wait_for_terminal_publication(
                        transition,
                        prepared,
                        &command.audit_events,
                    )?;
                }
            }
            command.fault = self.take_terminal_transition_fault();
            let completion = prepared_completion
                .map(|prepared| {
                    let tool_execution = prepared.tool_execution.clone().ok_or_else(|| {
                        anyhow!("completion commit is missing tool execution evidence")
                    })?;
                    Ok::<_, anyhow::Error>(crate::runtime_db::transitions::CompletionTransition {
                        requires_execution_continuation: true,
                        work_items: vec![
                            crate::runtime_db::transitions::WorkItemMutation::Update {
                                record: prepared.record.clone(),
                                expected_revision: prepared.record.revision - 1,
                            },
                        ],
                        wait_conditions: prepared.wait_conditions.clone(),
                        continuation_expectations: prepared.continuation_expectations.clone(),
                        continuations: prepared.continuations.clone(),
                        tool_execution,
                        index_changes: prepared.index_changes.clone(),
                    })
                })
                .transpose()?;
            let mut prepared =
                crate::runtime_db::transitions::PreparedQueueTransition::new(command.clone());
            prepared.execution_protocol = execution_protocol.clone();
            prepared.task_result_settlement = task_result_settlement.clone();
            if let Some(completion) = completion {
                prepared.completion = Some(completion);
            } else if let Some(wait) = prepared_wait.as_deref() {
                prepared.wait_registration = Some(wait.command.clone());
                prepared.task_expectation = wait.expected_task.clone();
                prepared.timer_wake_claim = wait.command.timer_wake.clone();
                prepared.terminal_tool_executions = wait
                    .tool_execution
                    .as_ref()
                    .into_iter()
                    .chain(
                        terminal_transition
                            .into_iter()
                            .flat_map(|transition| &transition.terminal_tool_executions),
                    )
                    .cloned()
                    .collect();
            } else if let Some(terminal) = terminal_transition {
                prepared.terminal_tool_executions = terminal.terminal_tool_executions.clone();
            }
            let commit = mutation
                .commit_prepared_queue(crate::ids::audit_event_id(), prepared)
                .await;
            drop(mutation);
            match commit {
                Ok(commit) => {
                    return Ok(self.apply_transition_commit(commit).await.applied);
                }
                Err(error) => {
                    let wait_conflict = prepared_wait.is_some()
                        && crate::runtime_db::retryable_state_transition_conflict(&error)
                            .is_some_and(|conflict| {
                                matches!(conflict.domain(), "task_wait" | "task_result_admission")
                            });
                    if wait_conflict && attempt + 1 < ENQUEUE_AGENT_STATE_MAX_ATTEMPTS {
                        let previous = prepared_wait.as_deref().expect("prepared wait conflict");
                        let condition = &previous.registration.condition;
                        let PrepareWaitForOutcome::Prepared(mut refreshed) = self
                            .prepare_wait_for_outcome_with_id(
                                &record.agent_id,
                                condition.work_item_id.clone(),
                                previous.wake,
                                condition.subject_ref.clone(),
                                condition.waiting_for.clone(),
                                previous.registration.recheck_after_ms,
                                Some(condition.id.clone()),
                            )
                            .await?
                        else {
                            return Err(error);
                        };
                        refreshed.delivery = previous.delivery;
                        refreshed.tool_execution = previous.tool_execution.clone();
                        refreshed.brief = previous.brief.clone();
                        refreshed.brief_publication_scope =
                            previous.brief_publication_scope.clone();
                        // Preparation owns wait audit events; retain only the tool/report evidence.
                        refreshed.command.audit_events.extend(
                            previous
                                .command
                                .audit_events
                                .iter()
                                .filter(|event| {
                                    matches!(
                                        event.kind.as_str(),
                                        "tool_executed" | "wait_report_request_completed"
                                    )
                                })
                                .cloned(),
                        );
                        execution_protocol = refreshed.execution_protocol.clone();
                        prepared_wait = Some(refreshed);
                        continue;
                    }
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

    fn terminal_settlement_readback(
        &self,
        record: &QueueEntryRecord,
        transition: &turn::TurnTerminalTransition,
        execution_witness: Option<&TerminalExecutionSettlementWitness>,
    ) -> Result<TerminalSettlementReadback> {
        let durable_queue = self
            .inner
            .runtime_db
            .queue_entries()
            .latest(&record.message_id)?;
        let durable_turn = self
            .inner
            .storage
            .read_turn_by_id(&transition.terminal.turn_id)?;
        let durable_execution = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&record.agent_id)?;
        let execution_committed = execution_witness.is_none_or(|witness| {
            let (Some(expected_outcome), Some(expected_attempt_state)) = (
                witness.expected_outcome.as_ref(),
                witness.expected_attempt_state,
            ) else {
                return false;
            };
            durable_execution.as_ref().is_some_and(|state| {
                state
                    .attempts
                    .get(&witness.attempt_id)
                    .is_some_and(|attempt| {
                        attempt.state == expected_attempt_state
                            && attempt.terminal_outcome_id.as_deref()
                                == Some(expected_outcome.outcome_id.as_str())
                    })
                    && state.outcomes.get(&expected_outcome.outcome_id) == Some(expected_outcome)
            })
        });
        if durable_queue
            .as_ref()
            .is_some_and(|queue| queue.status == record.status)
            && durable_turn
                .as_ref()
                .is_some_and(|turn| turn.terminal == transition.turn_record.terminal)
            && execution_committed
        {
            return Ok(TerminalSettlementReadback::Committed);
        }
        let open_attempt_matches = execution_witness.is_some_and(|witness| {
            durable_execution.as_ref().is_some_and(|state| {
                state
                    .attempts
                    .get(&witness.attempt_id)
                    .is_some_and(|attempt| {
                        attempt.state
                            == crate::domain::execution_protocol::ExecutionAttemptState::Open
                            && attempt.turn_id.as_deref()
                                == Some(transition.terminal.turn_id.as_str())
                            && attempt.terminal_outcome_id.is_none()
                            && witness.expected_outcome.as_ref().is_none_or(|expected| {
                                !state.outcomes.contains_key(&expected.outcome_id)
                            })
                    })
            })
        });
        if durable_queue
            .as_ref()
            .is_some_and(|queue| queue.status == QueueEntryStatus::Dequeued)
            && durable_turn
                .as_ref()
                .is_some_and(|turn| turn.terminal.is_none())
            && open_attempt_matches
        {
            return Ok(TerminalSettlementReadback::NotCommitted);
        }
        Ok(TerminalSettlementReadback::Ambiguous)
    }

    #[inline(never)]
    fn commit_queue_terminal_settlement_resilient<'a>(
        &'a self,
        record: QueueEntryRecord,
        audit_events: Vec<AuditEvent>,
        terminal_transition: &'a turn::TurnTerminalTransition,
        agent_update: Option<TerminalAgentStateUpdate>,
        transcript_entries: Vec<TranscriptEntry>,
        brief_evidence: Vec<BriefRecord>,
        duration_ms: u64,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<TerminalSettlementDisposition>> + Send + 'a>,
    > {
        Box::pin(self.commit_queue_terminal_settlement_resilient_inner(
            record,
            audit_events,
            terminal_transition,
            agent_update,
            transcript_entries,
            brief_evidence,
            duration_ms,
        ))
    }

    async fn commit_queue_terminal_settlement_resilient_inner(
        &self,
        record: QueueEntryRecord,
        audit_events: Vec<AuditEvent>,
        terminal_transition: &turn::TurnTerminalTransition,
        agent_update: Option<TerminalAgentStateUpdate>,
        transcript_entries: Vec<TranscriptEntry>,
        brief_evidence: Vec<BriefRecord>,
        duration_ms: u64,
    ) -> Result<TerminalSettlementDisposition> {
        let open_attempt_id = terminal_execution_open_attempt_id(&self.inner.runtime_db, &record)?;
        let materialized_witness = terminal_execution_settlement_witness(
            &self.inner.storage,
            &self.inner.runtime_db,
            &record,
            terminal_transition,
        );
        let (error, execution_witness, materialization_failed) = match materialized_witness {
            Ok(materialized_witness) => {
                let execution_witness = materialized_witness.or_else(|| {
                    open_attempt_id
                        .clone()
                        .map(TerminalExecutionSettlementWitness::unmaterialized)
                });
                let first_commit = self
                    .commit_queue_terminal_settlement_with_evidence(
                        record.clone(),
                        audit_events.clone(),
                        true,
                        Some(terminal_transition),
                        agent_update.clone(),
                        transcript_entries.clone(),
                        brief_evidence.clone(),
                    )
                    .await;
                let Err(error) = first_commit else {
                    return Ok(TerminalSettlementDisposition::Intended);
                };
                (error, execution_witness, false)
            }
            Err(error) => (
                error,
                open_attempt_id.map(TerminalExecutionSettlementWitness::unmaterialized),
                true,
            ),
        };
        if !materialization_failed && execution_settlement_conflict(&error) {
            return Err(error);
        }
        match self.terminal_settlement_readback(
            &record,
            terminal_transition,
            execution_witness.as_ref(),
        )? {
            TerminalSettlementReadback::Committed => {
                tracing::warn!(
                    message_id = %record.message_id,
                    turn_id = %terminal_transition.terminal.turn_id,
                    error = %error,
                    "terminal settlement reported an error after durable commit"
                );
                return Ok(TerminalSettlementDisposition::Intended);
            }
            TerminalSettlementReadback::Ambiguous => {
                return Err(error.context(
                    "terminal settlement outcome is ambiguous; fenced abort was not attempted",
                ));
            }
            TerminalSettlementReadback::NotCommitted => {}
        }

        if !materialization_failed
            && terminal_transition.terminal.kind != TurnTerminalKind::Completed
        {
            self.commit_queue_terminal_settlement_with_evidence(
                record,
                audit_events,
                true,
                Some(terminal_transition),
                agent_update,
                transcript_entries,
                brief_evidence,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to retry confirmed-uncommitted {:?} terminal settlement after: {error}",
                    terminal_transition.terminal.kind
                )
            })?;
            return Ok(TerminalSettlementDisposition::Intended);
        }

        let message_id = record.message_id.clone();
        let failure_summary = Self::summarize_runtime_failure_error(&error);
        let mut fallback_terminal = crate::types::TurnTerminalRecord {
            turn_id: terminal_transition.terminal.turn_id.clone(),
            turn_index: terminal_transition.terminal.turn_index,
            kind: TurnTerminalKind::Aborted,
            reason: Some("terminal_settlement_failed".into()),
            last_assistant_message: terminal_transition.terminal.last_assistant_message.clone(),
            no_brief_reason: Some(TurnNoBriefReason::Aborted),
            checkpoint: terminal_transition.terminal.checkpoint.clone(),
            completed_at: Utc::now(),
            duration_ms,
        };
        let mut fallback_turn = self.build_turn_record(&fallback_terminal).await?;
        if !fallback_turn.produced_brief_ids.is_empty() {
            fallback_terminal.no_brief_reason = None;
        }
        fallback_turn.terminal = Some(crate::types::TurnTerminalSummary::from_terminal(
            &fallback_terminal,
        ));
        let fallback_transition = turn::TurnTerminalTransition {
            terminal: fallback_terminal,
            turn_record: fallback_turn,
            prepared_work_item_completion: None,
            prepared_wait_for: None,
            terminal_tool_executions: Vec::new(),
        };
        let fallback_state = TerminalAgentStateUpdate {
            last_runtime_failure: Some(RuntimeFailureSummary {
                occurred_at: Utc::now(),
                summary: failure_summary.clone(),
                phase: RuntimeFailurePhase::RuntimeTurn,
                detail_hint: Some(
                    "the intended terminal settlement was replaced by a fenced abort".into(),
                ),
                failure_artifact: None,
            }),
        };
        self.commit_queue_terminal_settlement_with_evidence(
            QueueEntryRecord {
                status: QueueEntryStatus::Aborted,
                updated_at: Utc::now(),
                ..record
            },
            vec![
                AuditEvent::legacy(
                    "turn_terminal_settlement_failed",
                    serde_json::json!({
                        "message_id": message_id,
                        "turn_id": fallback_transition.terminal.turn_id,
                        "failed_terminal_kind": terminal_transition.terminal.kind,
                        "error": failure_summary,
                        "recovery": "aborted_confirmed_uncommitted_settlement",
                    }),
                ),
                AuditEvent::legacy(
                    "queue_entry_settled",
                    serde_json::json!({
                        "message_id": message_id,
                        "status": QueueEntryStatus::Aborted,
                        "reason": "terminal_settlement_failed",
                    }),
                ),
            ],
            true,
            Some(&fallback_transition),
            Some(fallback_state),
            Vec::new(),
            Vec::new(),
        )
        .await
        .with_context(|| {
            format!("failed to abort confirmed-uncommitted terminal settlement after: {error}")
        })?;
        Ok(TerminalSettlementDisposition::AbortedFallback)
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
            let mut guard = self.begin_mutation().await?;
            if guard
                .queue
                .peek_next_matching(|message| message.id == recovery_message.id)
                .is_none()
            {
                continue;
            }
            let mut next_state = guard.state.clone();
            next_state.pending = guard.queue.len().saturating_sub(1);
            let command = &crate::runtime_db::transitions::QueueTransitionCommand {
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
            };
            let commit = guard
                .commit_prepared_queue(
                    crate::ids::audit_event_id(),
                    crate::runtime_db::transitions::PreparedQueueTransition::new(Clone::clone(
                        command,
                    )),
                )
                .await?;
            if !commit.applied {
                continue;
            }
            drop(guard);
            self.apply_transition_commit(commit).await;
            superseded += 1;
        }

        Ok(superseded)
    }

    pub(crate) async fn persist_transcript_evidence(&self, entry: &TranscriptEntry) -> Result<()> {
        self.persist_prepared_evidence(
            crate::runtime_db::prepared_input_evidence::InputEvidenceWrite::Transcript(
                entry.clone(),
            ),
        )
        .await
    }

    pub(crate) async fn persist_tool_execution_evidence(
        &self,
        record: &ToolExecutionRecord,
    ) -> Result<()> {
        self.persist_prepared_evidence(
            crate::runtime_db::prepared_input_evidence::InputEvidenceWrite::Tool(record.clone()),
        )
        .await
    }

    #[inline(never)]
    pub fn run(self) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send>> {
        // Keep construction separate from polling: callers and Tokio tasks
        // retain a pointer rather than the full runtime-loop state machine.
        Box::pin(self.run_inner())
    }

    async fn run_inner(self) -> Result<()> {
        if let Some(bridge) = self.inner.host_bridge.as_ref() {
            bridge.wait_for_bootstrap_ready().await?;
        }
        let bootstrap = async {
            // Restore known pilot commits before legacy bootstrap force writes.
            self.reconcile_posture_observation().await?;
            self.reconcile_loaded_control_obligations().await?;
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
            // Original processing and terminal ACK finish before this boundary.
            // A retired instance cannot plan another activation or idle wake.
            if !self.accepts_execution_admission() {
                return Ok(());
            }
            if self.emit_due_task_result_recovery().await? {
                continue;
            }
            if self.emit_due_agent_wait_recheck().await? {
                continue;
            }
            let poll = scheduler_executor::SchedulerDecisionExecutor::new(&self)
                .poll()
                .await?;

            let scheduled = match poll {
                scheduler_executor::RunLoopPoll::Shutdown => return Ok(()),
                scheduler_executor::RunLoopPoll::Stopped(state, _queue_len) => {
                    // Stop depends only on the observed lifecycle State.
                    let decision = scheduler::SchedulerDecision::new(
                        scheduler::SchedulerDecisionKind::Stop,
                        "stopped",
                    )
                    .boundary(scheduler::SchedulerBoundary::RunLoop.as_str())
                    .liveness_only(true)
                    .evidence(format!("status={:?}", state.status));
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
                    let idle_read = self.capture_scheduler_projection().await?;
                    let decision = scheduler::decide_next_action(
                        &idle_read.projection,
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
                    let next_recheck_at = [
                        idle_read.work.next_work_recheck_at()?,
                        idle_read.work.next_agent_wait_recheck_at()?,
                        self.next_queued_available_retry_at(
                            &idle_read.projection.queued_runnable_work_items,
                        )?,
                    ]
                    .into_iter()
                    .flatten()
                    .min();
                    let result_recheck_at = self
                        .inner
                        .runtime_db
                        .task_result_settlements()
                        .next_recheck_at(&idle_read.state.id)?;
                    let next_recheck_at = match (next_recheck_at, result_recheck_at) {
                        (Some(left), Some(right)) => Some(left.min(right)),
                        (left, right) => left.or(right),
                    };
                    // A sleeping loop retains deadlines, not owned Work bodies or Pins.
                    drop(idle_read);
                    let idle_transition = scheduler_executor::SchedulerDecisionExecutor::new(&self)
                        .transition_run_loop_idle_to_sleep(next_recheck_at)
                        .await?;
                    let Some(idle_transition) = idle_transition else {
                        continue;
                    };
                    if idle_transition.posture_changed {
                        self.append_state_changed_events(&idle_transition.state)?;
                    }
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
            let original_run_id = scheduled
                .running_state
                .current_run_id
                .clone()
                .context("scheduled message has no original run identity")?;
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

            let queue_wait = self
                .now()
                .signed_duration_since(scheduled.message.created_at)
                .to_std()
                .unwrap_or_default();
            let trigger_kind = scheduled
                .message
                .trigger_kind
                .map(|kind| match kind {
                    crate::types::ContinuationTriggerKind::OperatorInput => "operator_input",
                    crate::types::ContinuationTriggerKind::TaskResult => "task_result",
                    crate::types::ContinuationTriggerKind::ExternalEvent => "external_event",
                    crate::types::ContinuationTriggerKind::TimerFire => "timer_fire",
                    crate::types::ContinuationTriggerKind::InternalFollowup => "internal_followup",
                    crate::types::ContinuationTriggerKind::SystemTick => "system_tick",
                })
                .unwrap_or("unknown");
            crate::diagnostics::record_scheduler_queue_wait(trigger_kind, queue_wait);

            // Cancellation and setup failures can bypass the provider's turn timer.
            let processing_started = std::time::Instant::now();
            let original_completion = scheduled.run_completion.lease();
            let processing_result = self
                .process_message_with_plan_deferred(
                    scheduled.message,
                    scheduled.dispatch_plan,
                    &scheduled.scheduler_decision,
                )
                .await;
            let settlement = self
                .finish_scheduled_message_processing(
                    &message,
                    &original_run_id,
                    processing_started,
                    processing_result,
                )
                .await;
            drop(scheduled.run_completion);
            self.finish_original_stopped_dispatch(original_completion)
                .await?;
            settlement?;
        }
    }

    #[inline(never)]
    fn finish_scheduled_message_processing<'a>(
        &'a self,
        message: &'a MessageEnvelope,
        original_run_id: &'a str,
        processing_started: std::time::Instant,
        processing_result: Result<turn::TurnTerminalTransition>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.finish_scheduled_message_processing_inner(
            message,
            original_run_id,
            processing_started,
            processing_result,
        ))
    }

    async fn finish_scheduled_message_processing_inner(
        &self,
        message: &MessageEnvelope,
        original_run_id: &str,
        processing_started: std::time::Instant,
        processing_result: Result<turn::TurnTerminalTransition>,
    ) -> Result<()> {
        if self.original_run_was_settled_by_control_stop(message, original_run_id)? {
            // The caller retains the original completion guard across this
            // helper, then drops it before attempting ended memory recovery.
            return Ok(());
        }
        let terminal_transition = match processing_result {
            Ok(transition) => transition,
            Err(err) => {
                let aborted = err.downcast_ref::<CurrentRunAborted>().cloned();
                let (terminal, queue_status, mut audit_events, failure_artifacts) = if let Some(
                    aborted,
                ) =
                    aborted.as_ref()
                {
                    (
                        self.build_turn_aborted_record(
                            &aborted.reason,
                            None,
                            u64::try_from(processing_started.elapsed().as_millis())
                                .unwrap_or(u64::MAX),
                        )
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
                        .build_turn_aborted_record(
                            "runtime_error",
                            None,
                            u64::try_from(processing_started.elapsed().as_millis())
                                .unwrap_or(u64::MAX),
                        )
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
                // Setup can fail after Claim and before StartedTurn. This
                // admitted settlement owns preparation for its fallback Turn.
                {
                    let mut request = resident_work::WorkCaptureRequest::current();
                    request.include_live = false;
                    request.nonopen_limit = 0;
                    request.required_turn_ids.insert(terminal.turn_id.clone());
                    let mut mutation = self.begin_mutation().await?;
                    mutation.prepare_owner_work_request(&request).await?;
                }
                let mut turn_record = self.build_turn_record(&terminal).await?;
                if let Some(artifacts) = failure_artifacts.as_ref() {
                    if !turn_record.produced_brief_ids.contains(&artifacts.brief.id) {
                        turn_record
                            .produced_brief_ids
                            .push(artifacts.brief.id.clone());
                    }
                }
                let mut terminal = terminal;
                if failure_artifacts.is_some() {
                    terminal.no_brief_reason = None;
                    turn_record.terminal =
                        Some(crate::types::TurnTerminalSummary::from_terminal(&terminal));
                }
                let terminal_transition = turn::TurnTerminalTransition {
                    terminal,
                    turn_record,
                    prepared_work_item_completion: None,
                    prepared_wait_for: None,
                    terminal_tool_executions: Vec::new(),
                };
                let committed_state = TerminalAgentStateUpdate {
                    last_runtime_failure: failure_artifacts
                        .as_ref()
                        .map(|artifacts| artifacts.failure_summary.clone()),
                };
                let settlement = self
                    .commit_queue_terminal_settlement_resilient(
                        QueueEntryRecord {
                            message_id: message.id.clone(),
                            agent_id: message.agent_id.clone(),
                            priority: message.priority.clone(),
                            status: queue_status,
                            created_at: message.created_at,
                            updated_at: Utc::now(),
                        },
                        audit_events,
                        &terminal_transition,
                        Some(committed_state),
                        failure_artifacts
                            .as_ref()
                            .map(|artifacts| vec![artifacts.transcript.clone()])
                            .unwrap_or_default(),
                        failure_artifacts
                            .as_ref()
                            .map(|artifacts| vec![artifacts.brief.clone()])
                            .unwrap_or_default(),
                        u64::try_from(processing_started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    )
                    .await;
                if let Err(error) = settlement {
                    if self.original_run_was_settled_by_control_stop(message, original_run_id)? {
                        return Ok(());
                    }
                    if execution_settlement_conflict(&error) {
                        crate::diagnostics::record_missing_terminal_turn_detected();
                        if self.recover_scheduler_bootstrap_claims().await? > 0 {
                            return Ok(());
                        }
                    }
                    return Err(error);
                }
                let failed_state = {
                    let mut guard = self.begin_mutation().await?;
                    if guard
                        .current_run_abort
                        .as_ref()
                        .is_some_and(|handle| handle.run_id == original_run_id)
                    {
                        guard.current_run_abort = None;
                    }
                    guard.install_transient().await?;
                    guard.state.clone()
                };
                self.append_state_changed_events(&failed_state)?;
                if !self.accepts_execution_admission() {
                    return Ok(());
                }
                self.maybe_commit_turn_end_work_item_transition().await?;
                self.record_closure_decision_event(Some(true)).await?;
                self.maybe_emit_pending_system_tick(None).await?;
                return Ok(());
            }
        };
        {
            let settlement = self
                .commit_queue_terminal_settlement_resilient(
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
                    &terminal_transition,
                    None,
                    Vec::new(),
                    Vec::new(),
                    u64::try_from(processing_started.elapsed().as_millis()).unwrap_or(u64::MAX),
                )
                .await;
            match settlement {
                Ok(TerminalSettlementDisposition::Intended) => {}
                Ok(TerminalSettlementDisposition::AbortedFallback) => {
                    let failed_state = {
                        let mut guard = self.begin_mutation().await?;
                        if guard
                            .current_run_abort
                            .as_ref()
                            .is_some_and(|handle| handle.run_id == original_run_id)
                        {
                            guard.current_run_abort = None;
                        }
                        guard.install_transient().await?;
                        guard.state.clone()
                    };
                    self.append_state_changed_events(&failed_state)?;
                    if !self.accepts_execution_admission() {
                        return Ok(());
                    }
                    self.maybe_commit_turn_end_work_item_transition().await?;
                    self.record_closure_decision_event(Some(true)).await?;
                    self.maybe_emit_pending_system_tick(None).await?;
                    return Ok(());
                }
                Err(error) => {
                    if self.original_run_was_settled_by_control_stop(message, original_run_id)? {
                        return Ok(());
                    }
                    if execution_settlement_conflict(&error) {
                        crate::diagnostics::record_missing_terminal_turn_detected();
                        if self.recover_scheduler_bootstrap_claims().await? > 0 {
                            return Ok(());
                        }
                    }
                    return Err(error);
                }
            }
            let processed_state = {
                let mut guard = self.begin_mutation().await?;
                if guard
                    .current_run_abort
                    .as_ref()
                    .is_some_and(|handle| handle.run_id == original_run_id)
                {
                    guard.current_run_abort = None;
                }
                guard.install_transient().await?;
                guard.state.clone()
            };
            self.append_state_changed_events(&processed_state)?;
            if !self.accepts_execution_admission() {
                return Ok(());
            }
            self.maybe_supersede_queued_provider_recovery(message, Some(&terminal_transition))
                .await?;
        }
        Ok(())
    }

    fn original_run_was_settled_by_control_stop(
        &self,
        source: &MessageEnvelope,
        run_id: &str,
    ) -> Result<bool> {
        // The normal running path needs no extra SQLite read. An unresolved
        // gate may still have an immutable Stop receipt; reading it is not ACK
        // recovery and must not reopen that gate or install historical State.
        if self
            .inner
            .mutation_owner
            .capture()
            .ok()
            .flatten()
            .is_some_and(|root| root.state.current_run_id.as_deref() == Some(run_id))
        {
            return Ok(false);
        }
        let Some(lease) = self.inner.control_cancellations.run_for(run_id) else {
            return Ok(false);
        };
        anyhow::ensure!(
            lease.source_message_id() == source.id,
            "original run source lease changed"
        );
        self.inner.runtime_db.committed_control_stop_for_run(
            &crate::runtime_db::control_transition::ControlStoppedRun {
                agent_id: &source.agent_id,
                identity_incarnation: self.inner.identity_incarnation,
                owner_epoch: self.inner.mutation_owner.owner_epoch(),
                run_id,
                runtime_lease_id: lease.lease_id(),
                activation_id: lease.activation_id(),
                source,
            },
        )
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
        // Failure cleanup is still a legacy whole-row writer. It must never
        // overwrite a committed posture with the pre-publication memory copy.
        let mut guard = match self.begin_mutation().await {
            Ok(guard) => guard,
            Err(owner_error) => {
                tracing::warn!(agent_id = %agent_id, error = %owner_error,
                    "skipped runtime failure state write while mutation owner is unavailable");
                return;
            }
        };
        guard.state.current_run_id = None;
        guard.current_run_abort = None;
        guard.state.last_runtime_failure = Some(RuntimeFailureSummary {
            occurred_at,
            summary,
            phase: RuntimeFailurePhase::RuntimeTurn,
            detail_hint: Some("the next host access will rebuild the runtime loop".into()),
            failure_artifact: None,
        });
        if let Err(persist_error) = guard.persist_state(&self.inner.storage).await {
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
            let mut guard = self.begin_mutation().await?;
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
            let owner = match &attempt.binding {
                crate::domain::execution_protocol::ExecutionBinding::WorkItem { work_item_id } => {
                    crate::types::TurnOwner::WorkItem {
                        work_item_id: work_item_id.clone(),
                    }
                }
                crate::domain::execution_protocol::ExecutionBinding::Conversation {
                    interaction_id,
                } => crate::types::TurnOwner::Conversation {
                    interaction_id: interaction_id.clone(),
                },
                crate::domain::execution_protocol::ExecutionBinding::AgentLifecycle {
                    agent_id: owner_agent_id,
                } => crate::types::TurnOwner::AgentLifecycle {
                    agent_id: owner_agent_id.clone(),
                },
                crate::domain::execution_protocol::ExecutionBinding::Command => {
                    crate::types::TurnOwner::Command
                }
            };
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
                work_item_id
                    .as_deref()
                    .filter(|_| execution_attempt_has_task_result_source(&attempt))
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
                    TaskResultClaimRecovery::MissingWait => {
                        (None, unsettled_claim::ReplayFence::Missing)
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
                    TaskResultClaimRecovery::Ineligible { .. } => continue,
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
                    && turn.effective_owner() == owner
            });
            let terminal_is_completed = terminal_turn.is_some_and(|turn| {
                turn.terminal.as_ref().is_some_and(|terminal| {
                    terminal.kind == crate::types::TurnTerminalKind::Completed
                })
            });
            let canonical_first_attempt = attempt.attempt_id
                == scheduler_executor::canonical_activation_id(&entry.message_id);
            let current_canonical_lifecycle = if matches!(
                &attempt.binding,
                crate::domain::execution_protocol::ExecutionBinding::AgentLifecycle { .. }
            ) {
                let delivery = self
                    .inner
                    .runtime_db
                    .agent_message_deliveries()
                    .latest_for_message(&message.id)?;
                let scenario = scheduler_executor::canonical_lifecycle_scenario_for_attempt(
                    &message, &attempt,
                );
                delivery.is_some_and(|delivery| {
                    delivery.target_agent_id == agent_id
                        && delivery.message_id.as_deref() == Some(message.id.as_str())
                        && delivery.activation_id.as_deref() == Some(attempt.attempt_id.as_str())
                        && scenario.as_ref().is_some_and(|scenario| {
                            execution_state.as_ref().is_some_and(|state| {
                                scheduler_executor::canonical_lifecycle_attempt_lineage_is_valid(
                                    state,
                                    &message,
                                    scenario,
                                    &attempt.attempt_id,
                                )
                            })
                        })
                })
            } else {
                false
            };
            let decision =
                unsettled_claim::plan_unsettled_claim(&unsettled_claim::UnsettledClaimFacts {
                    queue_status: entry.status.clone(),
                    attempt_state: attempt.state,
                    terminal_turn_completed: terminal_turn.map(|_| terminal_is_completed),
                    replay_fence: if task_result_replay_fence
                        == unsettled_claim::ReplayFence::Revoked
                    {
                        unsettled_claim::ReplayFence::Revoked
                    } else if current_canonical_lifecycle {
                        unsettled_claim::ReplayFence::CurrentCanonicalLifecycle
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
                let commit = guard
                    .commit_prepared_queue(crate::ids::audit_event_id(), { let mut prepared = crate::runtime_db::transitions::PreparedQueueTransition::new(Clone::clone(&crate::runtime_db::transitions::QueueTransitionCommand {
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
                            })); prepared.execution_protocol = Clone::clone(&execution_protocol); prepared })
                    .await?;
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
                let commit = guard
                    .commit_prepared_queue(crate::ids::audit_event_id(), { let mut prepared = crate::runtime_db::transitions::PreparedQueueTransition::new(Clone::clone(&crate::runtime_db::transitions::QueueTransitionCommand {
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
                            })); prepared.execution_protocol = Clone::clone(&execution_protocol); prepared })
                    .await?;
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
                let commit = guard
                    .commit_prepared_queue(crate::ids::audit_event_id(), { let mut prepared = crate::runtime_db::transitions::PreparedQueueTransition::new(Clone::clone(&crate::runtime_db::transitions::QueueTransitionCommand {
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
                        })); prepared.execution_protocol = Clone::clone(&execution_protocol); prepared })
                    .await?;
                if commit.applied {
                    recovered += 1;
                    crate::diagnostics::record_unsettled_claim_recovery();
                    guard
                        .restore_bootstrap_replay_message(&self.inner.storage, &message)
                        .await?;
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
                let commit = guard.commit_prepared_queue(crate::ids::audit_event_id(), crate::runtime_db::transitions::PreparedQueueTransition::new(Clone::clone(&crate::runtime_db::transitions::QueueTransitionCommand {
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
                    }))).await?;
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
                let commit = guard.commit_prepared_queue(crate::ids::audit_event_id(), crate::runtime_db::transitions::PreparedQueueTransition::new(Clone::clone(&crate::runtime_db::transitions::QueueTransitionCommand {
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
                    }))).await?;
                if commit.applied {
                    recovered += 1;
                    guard
                        .restore_bootstrap_replay_message(&self.inner.storage, &message)
                        .await?;
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
            let commit = guard
                .commit_prepared_queue(crate::ids::audit_event_id(), {
                    let mut prepared = crate::runtime_db::transitions::PreparedQueueTransition::new(
                        Clone::clone(&crate::runtime_db::transitions::QueueTransitionCommand {
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
                                    "recovery_outcome": recovery_outcome,
                                    "terminal_turn_id": terminal_turn_id,
                                    "provenance": "bootstrap_reconciliation",
                                }),
                            )],
                            notify_scheduler: true,
                            fault: self.take_transition_fault(),
                            brief_evidence: Vec::new(),
                        }),
                    );
                    prepared.execution_protocol = Clone::clone(&execution_protocol);
                    prepared
                })
                .await?;
            if commit.applied {
                recovered += 1;
                if !terminal_is_completed {
                    guard
                        .restore_bootstrap_replay_message(&self.inner.storage, &message)
                        .await?;
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
    crate::runtime_db::retryable_state_transition_conflict(error).is_some_and(|conflict| {
        (conflict.domain() == "agent_state" && conflict.record_id() == agent_id)
            || conflict.domain() == "wait_condition_trigger"
    })
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
#[path = "runtime/state_mutation_2b_tests.rs"]
mod state_mutation_2b_tests;

#[cfg(test)]
#[path = "runtime/tests/control_stop_settlement.rs"]
mod control_stop_settlement_tests;

#[cfg(test)]
#[path = "runtime/begin_turn_input_tests.rs"]
mod begin_turn_input_tests;

#[cfg(test)]
#[path = "runtime/dispatch_run_projection_tests.rs"]
mod dispatch_run_projection_tests;
