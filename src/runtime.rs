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
mod scheduler;
mod scheduler_executor;
mod subagent;
mod task_state_reducer;
mod task_supervisor;
mod tasks;
#[cfg(test)]
mod test_util;
mod turn;
mod waiting;
pub(crate) mod workspace;
pub(crate) mod workspace_control;
mod worktree;

pub use first_run_intro::maybe_enqueue_first_run_intro;
pub(crate) use lifecycle::LightweightAgentStateProjection;
pub use tasks::{
    PickedWorkItem, WorkItemContinuationSummary, WorkItemFocusTransition,
    WorkItemFocusTransitionWarning,
};
pub(crate) use waiting::{WaitForScope, WaitForWakeKind};

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

use anyhow::{anyhow, Context, Result};
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
use crate::{
    agent_memory::load_agent_memory,
    agent_template::discover_agent_templates_catalog,
    agents_md::load_agents_md,
    brief,
    config::RuntimeModelCatalog,
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
        ExternalTriggerCapability, ExternalTriggerRecord, ExternalTriggerScope,
        ExternalTriggerStatus, ExternalTriggerSummary, LoadedAgentsMd, MessageBody,
        MessageDeliverySurface, MessageEnvelope, MessageKind, MessageLifecycleAuditEvent,
        MessageOrigin, PendingWakeHint, Priority, QueueEntryRecord, QueueEntryStatus,
        RuntimeFailurePhase, RuntimeFailureSummary, RuntimePosture, SkillActivationSource,
        SkillActivationState, SkillCatalogEntry, SkillLoadReason, SkillsRuntimeView, TaskKind,
        TaskLifecycleAuditEvent, TaskRecord, TaskRecoverySpec, TaskStatus, TimerRecord,
        TimerStatus, ToolExecutionRecord, TranscriptEntry, TranscriptEntryKind,
        ViewImageObservation, WaitingReason, WorkItemLifecycleAuditEvent, WorkspaceEntry,
        AGENT_HOME_WORKSPACE_ID,
    },
    web::{WebConfig, WebProviderKind},
};
use command_task::ManagedTaskHandle;
use continuation::{resolve_continuation, ContinuationTrigger};
#[cfg(test)]
use subagent::sanitize_subagent_result;
use turn::LoopControlOptions;

#[derive(Debug, Clone)]
pub(super) struct WorkItemCompletionReportPromotion {
    pub(super) record: crate::types::WorkItemRecord,
    pub(super) brief_id: String,
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
    let effective_chain = model_catalog.provider_chain_for_turn(
        state.model_override.as_ref(),
        state.pending_fallback_model.as_ref(),
    );
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
    provider: RwLock<Arc<dyn AgentProvider>>,
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
    suppress_next_continue_active_tick: Mutex<bool>,
    shutdown_requested: AtomicBool,
    #[cfg(test)]
    transition_faults: StdMutex<std::collections::VecDeque<TransitionFaultPoint>>,
    #[cfg(test)]
    transition_warnings: StdMutex<Vec<PostCommitWarning>>,
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
        if let Err(error) = storage.write_agent(&self.state) {
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
        #[cfg(test)]
        {
            return self
                .inner
                .transition_faults
                .lock()
                .expect("transition fault plan lock poisoned")
                .pop_front();
        }
        #[cfg(not(test))]
        {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn inject_next_transition_fault(&self, fault: TransitionFaultPoint) {
        let mut faults = self
            .inner
            .transition_faults
            .lock()
            .expect("transition fault plan lock poisoned");
        assert!(
            faults.is_empty(),
            "a transition fault is already armed for this runtime fixture"
        );
        faults.push_back(fault);
    }

    #[cfg(test)]
    pub(crate) fn take_transition_warnings(&self) -> Vec<PostCommitWarning> {
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
        #[cfg(test)]
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
                .map(|entry| entry.workspace_anchor.as_path()),
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
                .map(|entry| entry.workspace_anchor.as_path()),
        );
        let registry = bridge.skills_registry()?;
        registry.write().await.sync_effective_roots(skill_roots)?;
        Ok(())
    }

    async fn begin_interactive_turn(
        &self,
        message: Option<&MessageEnvelope>,
        operator_binding_id: Option<&str>,
        operator_reply_route_id: Option<&str>,
    ) -> Result<()> {
        let state = {
            let mut guard = self.inner.agent.lock().await;
            guard.state.turn_index += 1;
            guard.state.current_turn_id = message
                .and_then(|message| normalized_turn_id(message.turn_id.as_deref()))
                .or_else(|| Some(crate::ids::turn_id()));
            guard.state.last_turn_terminal = None;
            if guard.state.current_turn_work_item_id.is_none() {
                guard.state.current_turn_work_item_id = guard.state.current_work_item_id.clone();
            }
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
        }
        Ok(())
    }

    #[cfg(test)]
    async fn begin_interactive_turn_for_test(
        &self,
        operator_binding_id: Option<&str>,
        operator_reply_route_id: Option<&str>,
    ) -> Result<()> {
        self.begin_interactive_turn(None, operator_binding_id, operator_reply_route_id)
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
        let message_is_new = self
            .inner
            .storage
            .read_message_by_id(&message.id)?
            .is_none();
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
        let mut commit = {
            let mut guard = self.inner.agent.lock().await;
            let expected_persisted_state = guard.last_persisted_state.clone();
            let mut committed_state = guard.state.clone();
            let previous_status = committed_state.status.clone();
            let previous_sleeping_until = committed_state.sleeping_until;
            committed_state.pending = guard.queue.len().saturating_add(1);
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
            let commit = self.inner.runtime_db.transitions().commit_queue(
                &crate::runtime_db::transitions::QueueTransitionCommand {
                    agent_id: message.agent_id.clone(),
                    operation: crate::runtime_db::transitions::QueueOperation::Admit,
                    mutation: crate::runtime_db::transitions::QueueMutation::Upsert(
                        QueueEntryRecord {
                            message_id: message.id.clone(),
                            agent_id: message.agent_id.clone(),
                            priority: message.priority.clone(),
                            status: QueueEntryStatus::Queued,
                            created_at: message.created_at,
                            updated_at: Utc::now(),
                        },
                    ),
                    agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(expected_persisted_state)),
                        record: Box::new(committed_state.clone()),
                    }),
                    message_evidence: vec![message.clone()],
                    transcript_entries: Vec::new(),
                    audit_events,
                    scheduler_shadow_comparison: None,
                    scheduler_semantic_shadow: None,
                    notify_scheduler: true,
                    fault: self.take_transition_fault(),
                },
            )?;
            guard.queue.push(message.clone());
            guard.state = committed_state.clone();
            guard.last_persisted_state = committed_state;
            let mut commit = commit;
            commit.effects.agent_state = None;
            commit
        };
        commit.effects.notify_scheduler = true;
        self.apply_transition_commit(commit).await;
        Ok(message)
    }

    pub(crate) fn append_audit_event(&self, kind: &str, data: serde_json::Value) -> Result<()> {
        self.inner
            .storage
            .append_event(&AuditEvent::legacy(kind, data))
    }

    pub(crate) async fn commit_queue_settlement(
        &self,
        record: QueueEntryRecord,
        audit_events: Vec<AuditEvent>,
        notify_scheduler: bool,
    ) -> Result<bool> {
        let commit = self.inner.runtime_db.transitions().commit_queue(
            &crate::runtime_db::transitions::QueueTransitionCommand {
                agent_id: record.agent_id.clone(),
                operation: crate::runtime_db::transitions::QueueOperation::Settle,
                mutation: crate::runtime_db::transitions::QueueMutation::Upsert(record),
                agent_state: None,
                message_evidence: Vec::new(),
                transcript_entries: Vec::new(),
                audit_events,
                scheduler_shadow_comparison: None,
                scheduler_semantic_shadow: None,
                notify_scheduler,
                fault: self.take_transition_fault(),
            },
        )?;
        Ok(self.apply_transition_commit(commit).await.applied)
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
        self.bootstrap_recovery().await?;
        scheduler_executor::SchedulerDecisionExecutor::new(&self)
            .bootstrap_recovered()
            .await?;

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
                    scheduler::append_scheduler_decision(&self.inner.storage, &decision)?;
                    return Ok(());
                }
                scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
                scheduler_executor::RunLoopPoll::Idle => {
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
                        scheduler::append_scheduler_decision(&self.inner.storage, &decision)?;
                    }
                    let next_recheck_at = self.next_blocked_work_item_recheck_at().await?;
                    let idle_state = scheduler_executor::SchedulerDecisionExecutor::new(&self)
                        .transition_run_loop_idle_to_sleep(next_recheck_at)
                        .await?;
                    if let Some(idle_state) = idle_state {
                        self.append_state_changed_events(&idle_state)?;
                    }
                    if let Some(next_recheck_at) = next_recheck_at {
                        if next_recheck_at > self.now() {
                            tokio::select! {
                                _ = self.inner.notify.notified() => {}
                                _ = self.inner.clock.sleep_until(next_recheck_at) => {}
                            }
                        }
                    } else {
                        self.inner.notify.notified().await;
                    }
                    continue;
                }
            };

            let message = scheduled.message.clone();
            self.append_state_changed_events(&scheduled.running_state)?;

            if let Err(err) = self
                .process_message_with_plan(
                    scheduled.message,
                    scheduled.dispatch_plan,
                    &scheduled.scheduler_decision,
                )
                .await
            {
                let aborted = err.downcast_ref::<CurrentRunAborted>().cloned();
                if let Some(aborted) = aborted.as_ref() {
                    self.commit_queue_settlement(
                        QueueEntryRecord {
                            message_id: message.id.clone(),
                            agent_id: message.agent_id.clone(),
                            priority: message.priority.clone(),
                            status: QueueEntryStatus::Interrupted,
                            created_at: message.created_at,
                            updated_at: Utc::now(),
                        },
                        vec![AuditEvent::legacy(
                            "message_processing_aborted",
                            serde_json::json!({
                                "message_id": message.id,
                                "message_kind": message.kind,
                                "run_id": aborted.run_id,
                                "reason": aborted.reason,
                            }),
                        )],
                        true,
                    )
                    .await?;
                } else {
                    let descriptor = describe_runtime_error(&err);
                    let terminal = self.ensure_runtime_failure_terminal(None, 0).await?;
                    error!(
                        message_id = %message.id,
                        turn_id = %terminal.turn_id,
                        domain = ?descriptor.domain,
                        code = %descriptor.code,
                        retryable = descriptor.retryable,
                        error = %descriptor.operator_message,
                        "failed to process message"
                    );
                    self.inner.storage.append_event(&AuditEvent::legacy(
                        "runtime_error",
                        serde_json::json!({
                            "message_id": message.id,
                            "turn_id": terminal.turn_id,
                            "message_kind": message.kind,
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
                    ))?;
                    self.persist_runtime_failure_artifacts(&message, &err)
                        .await?;
                    self.commit_queue_settlement(
                        QueueEntryRecord {
                            message_id: message.id.clone(),
                            agent_id: message.agent_id.clone(),
                            priority: message.priority.clone(),
                            status: QueueEntryStatus::Aborted,
                            created_at: message.created_at,
                            updated_at: Utc::now(),
                        },
                        vec![AuditEvent::legacy(
                            "queue_entry_settled",
                            serde_json::json!({
                                "message_id": message.id,
                                "message_kind": message.kind,
                                "status": QueueEntryStatus::Aborted,
                                "reason": "runtime_error",
                            }),
                        )],
                        true,
                    )
                    .await?;
                }
                let failed_state = {
                    let mut guard = self.inner.agent.lock().await;
                    if !matches!(guard.state.status, AgentStatus::Stopped) {
                        // Defense-in-depth: clear a stale pending_fallback_model when
                        // the current error has no further fallback to delegate to.
                        // This prevents the agent from becoming permanently stuck on
                        // a fallback model that is unsupported or unavailable.
                        if guard.state.pending_fallback_model.is_some() {
                            let has_fallback = provider_attempt_timeline(&err)
                                .and_then(|t| t.pending_fallback_model_ref.as_deref())
                                .is_some();
                            if !has_fallback {
                                guard.state.pending_fallback_model = None;
                            }
                        }
                        scheduler::apply_idle_projection(&mut guard.state, &self.inner.storage)?;
                    }
                    guard.current_run_abort = None;
                    guard.persist_state(&self.inner.storage)?;
                    guard.state.clone()
                };
                self.append_state_changed_events(&failed_state)?;
                self.maybe_commit_turn_end_work_item_transition().await?;
                self.record_closure_decision_event(Some(true)).await?;
                self.maybe_emit_pending_system_tick(None).await?;
            } else {
                let processed_state = {
                    let mut guard = self.inner.agent.lock().await;
                    guard.current_run_abort = None;
                    guard.state.clone()
                };
                self.append_state_changed_events(&processed_state)?;
                self.commit_queue_settlement(
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
                )
                .await?;
            }
        }
    }

    async fn bootstrap_recovery(&self) -> Result<()> {
        if let Some(tasks) = self.inner.recovered_tasks.lock().await.take() {
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
            if !interrupted.is_empty() {
                self.emit_system_tick_from_interrupted_tasks(&interrupted)
                    .await?;
            }
        }
        if let Some(timers) = self.inner.recovered_timers.lock().await.take() {
            self.recover_active_timers(timers).await?;
        }
        self.emit_recovered_pending_wake_hint().await?;
        Ok(())
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

#[cfg(test)]
mod tests;
