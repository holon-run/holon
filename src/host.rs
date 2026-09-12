use std::{
    collections::{HashMap, HashSet},
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use serde_json::{json, Value};
use tokio::{
    sync::{mpsc, watch, Mutex as AsyncMutex, Notify, RwLock},
    task::{spawn_blocking, JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::{
    agent_memory::load_agent_memory,
    agent_template::{
        discover_agent_templates_catalog, ensure_agent_home_agents_md_from_template_with_catalog,
        ensure_agent_home_agents_md_from_template_with_home,
        ensure_agent_home_agents_md_without_template_with_home, ensure_agent_home_layout,
        initialize_agent_home_from_template_with_catalog,
        initialize_agent_home_without_template_with_home, template_provenance_path,
        TemplateProvenanceRecord, DEFAULT_AGENT_TEMPLATE_ID,
    },
    agents_md::load_agents_md,
    callbacks::hash_callback_token,
    config::{AppConfig, RuntimeModelCatalog},
    context::ContextConfig,
    host_registry::{RuntimeRegistry, WorkspaceCleanupLeaseGuard},
    ids,
    prompt::{build_effective_prompt_with_apply_patch_surface, EffectivePrompt},
    provider::{build_provider_from_config, AgentProvider},
    runtime::{
        InitialWorkspaceBinding, LightweightAgentStateProjection, RuntimeHandle,
        SchedulerRepairInspection,
    },
    runtime_db::{
        agent_relations::{independent_creation_records, supervised_creation_records},
        RuntimeDb,
    },
    runtime_error::{describe_runtime_error, RuntimeError},
    skills::{
        effective_skill_root_registrations, skills_runtime_view_from_catalog, SkillVisibility,
        SkillsRegistry,
    },
    storage::{AppStorage, EventBus, PublishedAuditEvent},
    system::{
        ExecutionProfile, ExecutionScopeKind, ExecutionSnapshot, HostLocalBoundary,
        WorkspaceAccessMode,
    },
    tool::{apply_patch::ApplyPatchSurface, ToolError, ToolRegistry},
    types::{
        normalize_agent_name, AdmissionContext, AgentBootstrapDesiredState,
        AgentBootstrapInitialMessage, AgentBootstrapRecord, AgentBootstrapStatus,
        AgentBootstrapStep, AgentBootstrapStepStatus, AgentBootstrapWorkspaceState,
        AgentCanonicalDurability, AgentCreateReceipt, AgentCreateResult, AgentCreateStage,
        AgentDeletionJob, AgentDeletionStatus, AgentDetail, AgentDurability, AgentIdentityRecord,
        AgentIdentityView, AgentKind, AgentLifecycleHint, AgentListEntry,
        AgentMessageCallerContext, AgentMessageDeliveryOutcome, AgentMessageDeliveryRejectionCode,
        AgentMessageDeliveryState, AgentMessagePrincipalKind, AgentMessageSendRequest,
        AgentModelResolution, AgentModelResolutionStatus, AgentOwnership, AgentProfilePreset,
        AgentRegistryStatus, AgentState, AgentStatus, AgentSummary, AgentSupervisionState,
        AgentTokenUsageSummary, AgentTreeNode, AgentTreeProjection, AgentVisibility,
        AuthorityClass, ChildAgentSummary, ClosureOutcome, CreateAgentRequest,
        ExternalTriggerRecord, ExternalTriggerStatus, ExternalTriggerSummary, LoadedAgentsMdView,
        MessageBody, MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin,
        OperatorNotificationRecord, Priority, QueueEntryStatus, RuntimeFailureSummary, TaskKind,
        TaskRecord, TaskStatus, TimerRecord, TokenUsage, TranscriptEntry, TranscriptEntryKind,
        WaitConditionSummary, WorkspaceEntry, WorkspaceOccupancyRecord,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicAgentActivitySnapshot {
    pub agent_id: String,
    pub status: AgentStatus,
    pub active_task_count: usize,
    pub last_runtime_failure: Option<RuntimeFailureSummary>,
}

/// Host-level roster snapshot data assembled from one committed read view.
/// The HTTP layer maps this onto the wire `AgentRosterSnapshot` contract.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentRosterSnapshotData {
    pub runtime_id: String,
    pub event_log_epoch: String,
    pub visibility_policy_generation: u64,
    pub agents: Vec<AgentRosterEntryData>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentRosterEntryData {
    pub agent: AgentListEntry,
    pub event_head_seq: u64,
    pub oldest_retained_seq: u64,
    pub latest_brief: Option<AgentRosterLatestBriefData>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentRosterLatestBriefData {
    pub brief_id: String,
    pub created_event_seq: Option<u64>,
    pub created_at: chrono::DateTime<Utc>,
    pub preview: String,
}

/// Host-level per-Agent projection snapshot data assembled from one
/// committed read view. The HTTP layer maps this onto the wire
/// `AgentProjectionSnapshot` contract.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentProjectionSnapshotData {
    pub runtime_id: String,
    pub event_log_epoch: String,
    pub visibility_policy_generation: u64,
    pub agent_id: String,
    /// Consistency boundary: equals the per-Agent committed event head of
    /// the same read view, because every display-affecting event family
    /// commits its canonical record no later than its event.
    pub snapshot_through_seq: u64,
    pub event_head_seq: u64,
    pub oldest_retained_seq: u64,
    pub agent: AgentListEntry,
    pub canonical_relations: crate::types::AgentCanonicalRelationsProjection,
    pub current_work_item: Option<AgentWorkItemAnchorData>,
    pub conversation: ConversationRevisionAnchorsData,
    pub latest_brief: Option<AgentRosterLatestBriefData>,
    /// Records referenced by the projection and resolvable through the
    /// per-family batch record APIs. Tombstones stay empty in v1: no
    /// durable per-record deletion ledger exists for these families yet,
    /// and absence is represented by the null anchors above.
    pub hydration_references: Vec<AgentHydrationReferenceData>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentWorkItemAnchorData {
    pub work_item_id: String,
    pub state: crate::types::WorkItemState,
    pub plan_status: crate::types::WorkItemPlanStatus,
    pub revision: u64,
    pub updated_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConversationRevisionAnchorsData {
    pub latest_message_id: Option<String>,
    pub latest_transcript_entry_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentHydrationReferenceData {
    pub record_kind: ObserverSyncRecordKindData,
    pub record_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObserverSyncRecordKindData {
    Message,
    Brief,
    TranscriptEntry,
}

/// Bounds a stored Brief preview to the roster contract's UTF-8 byte limit,
/// cutting on a char boundary so the value stays valid UTF-8.
fn brief_preview(preview: &Option<String>) -> String {
    const MAX_UTF8_BYTES: usize = crate::http::observer_sync::LATEST_BRIEF_PREVIEW_MAX_UTF8_BYTES;
    let text = preview.as_deref().unwrap_or("");
    if text.len() <= MAX_UTF8_BYTES {
        return text.to_string();
    }
    let mut end = MAX_UTF8_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentStateProjectionSource {
    Loaded,
    Storage,
}

impl AgentStateProjectionSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Loaded => "loaded",
            Self::Storage => "storage",
        }
    }
}

pub(crate) struct AgentStateReadProjection {
    pub(crate) source: AgentStateProjectionSource,
    pub(crate) agent: LightweightAgentStateProjection,
    pub(crate) tasks: Vec<TaskRecord>,
    pub(crate) timers: Vec<TimerRecord>,
    pub(crate) external_triggers: Vec<ExternalTriggerRecord>,
}

#[derive(Clone)]
pub struct RuntimeHost {
    pub(crate) inner: Arc<HostInner>,
}

pub(crate) const TEMP_AGENT_PREFIX: &str = "tmp_";
const TEMP_RUN_AGENT_PREFIX: &str = "tmp_run_";
const TEMP_CHILD_AGENT_PREFIX: &str = "tmp_child_";
// Give runtime loops a short cleanup window while keeping daemon stop bounded.
#[cfg(not(test))]
const HOST_SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
#[cfg(test)]
const HOST_SHUTDOWN_GRACE: Duration = Duration::from_millis(50);
#[cfg(not(test))]
const RUNTIME_RECOVERY_BACKOFF: &[Duration] = &[
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
];
#[cfg(test)]
const RUNTIME_RECOVERY_BACKOFF: &[Duration] = &[
    Duration::from_millis(10),
    Duration::from_millis(20),
    Duration::from_millis(50),
];

#[derive(Debug)]
pub enum PublicAgentError {
    NotFound { agent_id: String },
    Deleting { agent_id: String },
    Deleted { agent_id: String },
    DeleteForbidden { agent_id: String, reason: String },
    RenameForbidden { agent_id: String, reason: String },
    InvalidName { agent_id: String, reason: String },
    NameConflict { agent_id: String, name: String },
    Private { agent_id: String },
    Stopped { agent_id: String },
    ShuttingDown,
    Runtime(anyhow::Error),
}

impl std::fmt::Display for PublicAgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { agent_id } => write!(
                f,
                "agent {agent_id} not found; create it first with 'holon agent create {agent_id}'"
            ),
            Self::Deleting { agent_id } => write!(f, "agent {} is being deleted", agent_id),
            Self::Deleted { agent_id } => write!(f, "agent {} was deleted", agent_id),
            Self::DeleteForbidden { agent_id, reason } => {
                write!(f, "agent {} cannot be deleted: {}", agent_id, reason)
            }
            Self::RenameForbidden { agent_id, reason } => {
                write!(f, "agent {} cannot be renamed: {}", agent_id, reason)
            }
            Self::InvalidName { agent_id, reason } => {
                write!(f, "agent {} has an invalid name: {}", agent_id, reason)
            }
            Self::NameConflict { agent_id, name } => {
                write!(
                    f,
                    "agent {} cannot use name {:?}: name is already in use",
                    agent_id, name
                )
            }
            Self::Private { agent_id } => write!(f, "agent {} is private", agent_id),
            Self::Stopped { agent_id } => {
                write!(f, "agent {} is stopped; start first", agent_id)
            }
            Self::ShuttingDown => write!(f, "runtime is shutting down"),
            Self::Runtime(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PublicAgentError {}

pub(crate) struct HostInner {
    registry: RuntimeRegistry,
    runtime_db: RuntimeDb,
    event_bus: EventBus,
    memory_index_notify: Arc<Notify>,
    daemon_indexer_token: CancellationToken,
    daemon_indexer_handle: Mutex<Option<JoinHandle<()>>>,
    daemon_retention_token: CancellationToken,
    daemon_retention_handle: Mutex<Option<JoinHandle<()>>>,
    pub(crate) daemon_deletion_token: CancellationToken,
    runtime_db_maintenance_lock: Mutex<Option<crate::runtime_db::RuntimeDbLock>>,
    skills_registry: Arc<RwLock<SkillsRegistry>>,
    static_provider: Option<Arc<dyn AgentProvider>>,
    runtimes: RwLock<HostRuntimeRegistry>,
    runtime_recovery_tx: mpsc::UnboundedSender<RuntimeRecoveryNotice>,
    runtime_recovery_rx: Mutex<Option<mpsc::UnboundedReceiver<RuntimeRecoveryNotice>>>,
    runtime_recovery_token: CancellationToken,
    runtime_recovery_handle: Mutex<Option<JoinHandle<()>>>,
    bootstrap_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

struct AgentEntry {
    runtime: RuntimeHandle,
    task: JoinHandle<()>,
    phase: watch::Receiver<AgentRuntimePhase>,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentRuntimePhase {
    Bootstrapping,
    Running,
    FailedCleaning,
    Terminated,
}

impl AgentEntry {
    fn accepts_host_access(&self) -> bool {
        !self.task.is_finished()
            && matches!(
                *self.phase.borrow(),
                AgentRuntimePhase::Bootstrapping | AgentRuntimePhase::Running
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostRuntimePhase {
    Open,
    Closing,
    Closed,
}

struct HostRuntimeRegistry {
    phase: HostRuntimePhase,
    agents: HashMap<String, AgentEntry>,
    next_generation: u64,
    recovering: HashMap<String, RuntimeRecoveryClaim>,
}

#[derive(Debug, Clone)]
struct RuntimeRecoveryClaim {
    generation: u64,
    retryable: bool,
    notify: Arc<Notify>,
}

#[derive(Debug)]
struct RuntimeRecoveryNotice {
    agent_id: String,
    generation: u64,
    retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum RuntimeActivationReason {
    SchedulerDispatch,
    OperatorControl,
    ExternalIngress,
    Wake,
    StartupRecovery,
    AgentLifecycle,
    ChildSupervision,
}

impl RuntimeActivationReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::SchedulerDispatch => "scheduler_dispatch",
            Self::OperatorControl => "operator_control",
            Self::ExternalIngress => "external_ingress",
            Self::Wake => "wake",
            Self::StartupRecovery => "startup_recovery",
            Self::AgentLifecycle => "agent_lifecycle",
            Self::ChildSupervision => "child_supervision",
        }
    }
}

#[derive(Debug)]
struct RuntimeAdmissionClosed;

impl std::fmt::Display for RuntimeAdmissionClosed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("runtime is shutting down")
    }
}

impl std::error::Error for RuntimeAdmissionClosed {}

fn stopped_unloaded_agent(agent_id: &str) -> AgentState {
    let mut agent = AgentState::new(agent_id.to_string());
    agent.status = AgentStatus::Stopped;
    agent
}

fn skill_visibility(identity: &AgentIdentityView) -> SkillVisibility {
    if identity.kind == AgentKind::Default {
        SkillVisibility::DefaultAgent
    } else {
        SkillVisibility::NonDefaultAgent
    }
}

#[derive(Clone)]
pub(crate) struct RuntimeHostBridge {
    inner: Weak<HostInner>,
}

#[derive(Debug, Clone)]
pub(crate) struct ChildTaskSpawn {
    pub child_agent_id: String,
    pub child_turn_baseline: u64,
    pub delivery_id: Option<String>,
    pub task_detail: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedAgentExistingBehavior {
    Reuse,
    Reject,
}

#[derive(Debug, Clone, Copy)]
enum AgentBootstrapStepKind {
    Template,
    Runtime,
    Workspace,
    Model,
    InitialMessage,
}

fn agent_bootstrap_step_mut(
    bootstrap: &mut AgentBootstrapRecord,
    kind: AgentBootstrapStepKind,
) -> &mut AgentBootstrapStep {
    match kind {
        AgentBootstrapStepKind::Template => &mut bootstrap.template,
        AgentBootstrapStepKind::Runtime => &mut bootstrap.runtime,
        AgentBootstrapStepKind::Workspace => &mut bootstrap.workspace,
        AgentBootstrapStepKind::Model => &mut bootstrap.model,
        AgentBootstrapStepKind::InitialMessage => &mut bootstrap.initial_message,
    }
}

fn bounded_bootstrap_error(error: &anyhow::Error) -> String {
    const MAX_CHARS: usize = 512;
    let summary = error.to_string();
    if summary.chars().count() <= MAX_CHARS {
        return summary;
    }
    summary.chars().take(MAX_CHARS).collect()
}

fn named_agent_already_exists_error(agent_id: &str) -> anyhow::Error {
    anyhow::Error::from(
        ToolError::new(
            "already_exists",
            format!("public named agent {agent_id} already exists"),
        )
        .with_domain(crate::runtime_error::RuntimeErrorDomain::Conflict)
        .with_details(json!({
            "agent_id": agent_id,
            "preset": AgentProfilePreset::PublicNamed,
        }))
        .with_recovery_hint(
            "use an explicit agent invocation or enqueue operation to deliver work to an existing agent",
        ),
    )
}

fn named_agent_name_already_exists_error(agent_id: &str, name: &str) -> anyhow::Error {
    anyhow::Error::from(
        ToolError::new(
            "already_exists",
            format!("public named agent name {name:?} is already in use"),
        )
        .with_domain(crate::runtime_error::RuntimeErrorDomain::Conflict)
        .with_details(json!({
            "agent_id": agent_id,
            "name": name,
            "preset": AgentProfilePreset::PublicNamed,
        }))
        .with_recovery_hint("choose a different name for the new public named agent"),
    )
}

fn named_agent_deletion_incomplete_error(
    agent_id: &str,
    job: Option<&AgentDeletionJob>,
) -> anyhow::Error {
    let mut details = json!({
        "agent_id": agent_id,
        "preset": AgentProfilePreset::PublicNamed,
    });
    let message = match job {
        Some(job) => {
            details["deletion_id"] = json!(job.deletion_id);
            details["deletion_status"] =
                serde_json::to_value(job.status).unwrap_or(serde_json::Value::Null);
            details["deletion_phase"] =
                serde_json::to_value(job.phase).unwrap_or(serde_json::Value::Null);
            if let Some(last_error) = &job.last_error {
                details["last_error"] = json!(last_error);
            }
            format!(
                "agent {agent_id} has an incomplete deletion (status {:?}); the id cannot be reused until the deletion completes",
                job.status
            )
        }
        None => format!(
            "agent {agent_id} was deleted without a completed deletion job; the id cannot be reused"
        ),
    };
    anyhow::Error::from(
        ToolError::new("deletion_incomplete", message)
            .with_domain(crate::runtime_error::RuntimeErrorDomain::Conflict)
            .with_details(details)
            .with_recovery_hint(
                "inspect the agent delete-status, let the deletion complete or resolve its failure, then retry create",
            ),
    )
}

fn named_agent_invalid_name_error(agent_id: &str, error: anyhow::Error) -> anyhow::Error {
    anyhow::Error::from(
        ToolError::new("agent_name_invalid", error.to_string())
            .with_domain(crate::runtime_error::RuntimeErrorDomain::Validation)
            .with_details(json!({
                "agent_id": agent_id,
                "preset": AgentProfilePreset::PublicNamed,
            }))
            .with_recovery_hint("choose a valid name for the new public named agent"),
    )
}

fn named_agent_create_failed_error(
    agent_id: &str,
    stage: AgentCreateStage,
    source: anyhow::Error,
) -> anyhow::Error {
    source.context(
        ToolError::new(
            "agent_create_failed",
            format!("failed to create public named agent {agent_id}"),
        )
        .with_details(json!({
            "agent_id": agent_id,
            "preset": AgentProfilePreset::PublicNamed,
            "stage": stage,
        }))
        .with_recovery_hint(
            "inspect the creation stage and retry after correcting the reported failure",
        ),
    )
}

#[derive(Debug, Clone)]
pub(crate) struct ChildTaskTerminalResult {
    pub status: TaskStatus,
    pub text: String,
    pub task_detail: Option<Value>,
}

#[derive(Debug, Clone)]
struct InvocationTerminalEvidence {
    status: TaskStatus,
    text: String,
    activation_id: Option<String>,
    turn_id: Option<String>,
    completion_ref: Option<String>,
}

#[derive(Debug, Clone)]
enum InvocationSettlementCursor {
    Attempt(String),
    WorkItem(String),
}

async fn apply_spawn_model_resolution(
    runtime: &RuntimeHandle,
    resolution: &AgentModelResolution,
) -> Result<()> {
    if resolution.resolution_status == AgentModelResolutionStatus::Inherited {
        return Ok(());
    }
    let provider = crate::config::ProviderId::parse(&resolution.resolved_provider)?;
    let model_ref = crate::config::ModelRouteRef::from_legacy_model_ref(
        &crate::config::ModelRef::new(provider, resolution.resolved_model.clone()),
    );
    let reasoning_effort = resolution
        .resolved_parameters
        .as_ref()
        .and_then(|parameters| parameters.get("reasoning_effort"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    runtime
        .set_model_override(model_ref, reasoning_effort)
        .await?;
    Ok(())
}

impl RuntimeHost {
    pub fn prepare_runtime_storage(config: &AppConfig) -> Result<()> {
        let runtime_db =
            RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())?;
        RuntimeHandle::prepare_runtime_storage(
            config.default_agent_id.clone(),
            config.agent_root_dir().join(&config.default_agent_id),
            InitialWorkspaceBinding::Detached,
            runtime_db,
        )
    }

    pub fn new(config: AppConfig) -> Result<Self> {
        let _ = build_provider_from_config(&config)?;
        Self::new_inner(config, None)
    }

    pub fn new_with_provider(config: AppConfig, provider: Arc<dyn AgentProvider>) -> Result<Self> {
        Self::new_inner(config, Some(provider))
    }

    #[doc(hidden)]
    pub fn new_with_provider_and_event_bus_capacity_for_test(
        config: AppConfig,
        provider: Arc<dyn AgentProvider>,
        event_bus_capacity: usize,
    ) -> Result<Self> {
        Self::new_inner_with_event_bus_capacity(config, Some(provider), event_bus_capacity)
    }

    fn new_inner(
        config: AppConfig,
        static_provider: Option<Arc<dyn AgentProvider>>,
    ) -> Result<Self> {
        Self::new_inner_with_event_bus_capacity(config, static_provider, 1024)
    }

    fn new_inner_with_event_bus_capacity(
        config: AppConfig,
        static_provider: Option<Arc<dyn AgentProvider>>,
        event_bus_capacity: usize,
    ) -> Result<Self> {
        let runtime_db =
            RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())?;
        let registry = RuntimeRegistry::new(config, runtime_db.clone())?;
        let (runtime_recovery_tx, runtime_recovery_rx) = mpsc::unbounded_channel();
        let host = Self {
            inner: Arc::new(HostInner {
                registry,
                runtime_db,
                event_bus: EventBus::new(event_bus_capacity),
                memory_index_notify: Arc::new(Notify::new()),
                daemon_indexer_token: CancellationToken::new(),
                daemon_indexer_handle: Mutex::new(None),
                daemon_retention_token: CancellationToken::new(),
                daemon_retention_handle: Mutex::new(None),
                daemon_deletion_token: CancellationToken::new(),
                runtime_db_maintenance_lock: Mutex::new(None),
                skills_registry: Arc::new(RwLock::new(SkillsRegistry::new())),
                static_provider,
                runtimes: RwLock::new(HostRuntimeRegistry {
                    phase: HostRuntimePhase::Open,
                    agents: HashMap::new(),
                    next_generation: 1,
                    recovering: HashMap::new(),
                }),
                runtime_recovery_tx,
                runtime_recovery_rx: Mutex::new(Some(runtime_recovery_rx)),
                runtime_recovery_token: CancellationToken::new(),
                runtime_recovery_handle: Mutex::new(None),
                bootstrap_locks: Mutex::new(HashMap::new()),
            }),
        };
        host.ensure_default_agent_identity()?;
        host.ensure_legacy_public_agent_bootstraps()?;
        host.converge_private_child_identities()?;
        host.import_legacy_external_triggers()?;
        Ok(host)
    }

    pub fn config(&self) -> Arc<AppConfig> {
        self.inner.registry.config()
    }

    /// Hot-reload config for all currently loaded agents.
    ///
    /// Re-reads the full config from disk (config file + credentials),
    /// rebuilds each agent's provider/catalog/model-availability, and atomically swaps
    /// the config snapshot. In-progress turns are unaffected; the next
    /// turn picks up the new config.
    pub async fn reload_all_agents_config(&self) -> Result<()> {
        let new_config = self
            .config()
            .reload_runtime_config()
            .map_err(|e| anyhow!("failed to reload config: {}", e))?;
        self.inner.registry.replace_config(new_config.clone());
        let agent_handles: Vec<RuntimeHandle> = {
            let registry = self.inner.runtimes.read().await;
            registry
                .agents
                .values()
                .map(|entry| entry.runtime.clone())
                .collect()
        };
        for runtime in &agent_handles {
            if let Err(e) = runtime.reload_config(&new_config).await {
                tracing::warn!(error = %e, "failed to reload config for agent");
            }
        }
        Ok(())
    }

    pub fn runtime_db(&self) -> &RuntimeDb {
        &self.inner.runtime_db
    }

    pub(crate) fn skills_registry(&self) -> Arc<RwLock<SkillsRegistry>> {
        self.inner.skills_registry.clone()
    }

    pub fn agent_storage(&self, agent_id: &str) -> Result<AppStorage> {
        let storage = AppStorage::new_for_agent(
            self.agent_data_dir(agent_id),
            agent_id.to_string(),
            self.runtime_db().clone(),
        )?;
        storage.enable_event_bus(self.inner.event_bus.clone())?;
        storage.enable_memory_index_notify(self.inner.memory_index_notify.clone())?;
        Ok(storage)
    }

    /// Spawn a single daemon-level memory indexer that covers all agents.
    ///
    /// Replaces the previous per-`RuntimeHandle` indexer model where every
    /// agent spawned its own indexer polling the shared runtime DB.  The
    /// daemon indexer discovers work precisely via
    /// `agent_ids_with_pending()`, processes each agent's outbox, and waits
    /// on a shared `Notify` driven by evidence writes.
    pub fn spawn_daemon_memory_indexer(&self) {
        if tokio::runtime::Handle::try_current().is_err() {
            tracing::debug!("daemon memory indexer not spawned: no Tokio runtime");
            return;
        }
        let host = self.clone();
        let handle = tokio::spawn(async move {
            host.run_daemon_memory_indexer().await;
        });
        *self.inner.daemon_indexer_handle.lock().unwrap() = Some(handle);
    }

    /// Signal the daemon memory indexer to stop and await its exit.
    ///
    /// Called during graceful shutdown so the indexer does not outlive the
    /// process servers.
    pub async fn shutdown_daemon_memory_indexer(&self) {
        self.inner.daemon_indexer_token.cancel();
        let handle = self.inner.daemon_indexer_handle.lock().unwrap().take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }

    pub fn spawn_daemon_runtime_db_retention(&self) {
        if tokio::runtime::Handle::try_current().is_err() {
            tracing::debug!("daemon runtime db retention not spawned: no Tokio runtime");
            return;
        }
        if self.inner.daemon_retention_handle.lock().unwrap().is_some() {
            return;
        }
        match crate::runtime_db::RuntimeDbLock::try_lock(
            self.config().runtime_db_maintenance_lock_path(),
        ) {
            Ok(lock) => {
                *self.inner.runtime_db_maintenance_lock.lock().unwrap() = Some(lock);
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "daemon runtime db retention disabled: maintenance lock unavailable"
                );
                return;
            }
        }
        let host = self.clone();
        let handle = tokio::spawn(async move {
            host.run_daemon_runtime_db_retention().await;
        });
        *self.inner.daemon_retention_handle.lock().unwrap() = Some(handle);
    }

    pub async fn shutdown_daemon_runtime_db_retention(&self) {
        self.inner.daemon_retention_token.cancel();
        let handle = self.inner.daemon_retention_handle.lock().unwrap().take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
        self.inner
            .runtime_db_maintenance_lock
            .lock()
            .unwrap()
            .take();
    }

    fn ensure_runtime_recovery_coordinator(&self) {
        if self.inner.runtime_recovery_handle.lock().unwrap().is_some() {
            return;
        }
        let Some(receiver) = self.inner.runtime_recovery_rx.lock().unwrap().take() else {
            return;
        };
        let host = self.clone();
        let handle = tokio::spawn(async move {
            host.run_runtime_recovery_coordinator(receiver).await;
        });
        *self.inner.runtime_recovery_handle.lock().unwrap() = Some(handle);
    }

    async fn shutdown_runtime_recovery_coordinator(&self) {
        self.inner.runtime_recovery_token.cancel();
        let handle = self.inner.runtime_recovery_handle.lock().unwrap().take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }

    async fn run_runtime_recovery_coordinator(
        self,
        mut receiver: mpsc::UnboundedReceiver<RuntimeRecoveryNotice>,
    ) {
        let mut recoveries = JoinSet::new();
        loop {
            tokio::select! {
                _ = self.inner.runtime_recovery_token.cancelled() => break,
                Some(notice) = receiver.recv() => {
                    if self.claim_runtime_recovery(&notice).await {
                        let host = self.clone();
                        recoveries.spawn(async move {
                            host.recover_runtime_after_failure(notice).await;
                        });
                    }
                }
                Some(result) = recoveries.join_next(), if !recoveries.is_empty() => {
                    if let Err(error) = result {
                        tracing::warn!(error = %error, "runtime recovery task failed");
                    }
                }
            }
        }
        recoveries.abort_all();
        while recoveries.join_next().await.is_some() {}
    }

    async fn claim_runtime_recovery(&self, notice: &RuntimeRecoveryNotice) -> bool {
        let mut registry = self.inner.runtimes.write().await;
        if registry.phase != HostRuntimePhase::Open
            || !registry.agents.get(&notice.agent_id).is_some_and(|entry| {
                entry.generation == notice.generation
                    && *entry.phase.borrow() == AgentRuntimePhase::Terminated
            })
        {
            return false;
        }
        if let Some(claim) = registry.recovering.get_mut(&notice.agent_id) {
            if notice.generation >= claim.generation {
                claim.generation = notice.generation;
                claim.retryable = notice.retryable;
                if !notice.retryable {
                    claim.notify.notify_one();
                }
            }
            return false;
        }
        if !notice.retryable {
            tracing::warn!(
                agent_id = notice.agent_id,
                generation = notice.generation,
                "runtime failure is not retryable; automatic recovery disabled"
            );
            return false;
        }
        registry.recovering.insert(
            notice.agent_id.clone(),
            RuntimeRecoveryClaim {
                generation: notice.generation,
                retryable: true,
                notify: Arc::new(Notify::new()),
            },
        );
        true
    }

    async fn clear_runtime_recovery(&self, agent_id: &str, generation: u64) {
        let mut registry = self.inner.runtimes.write().await;
        if registry
            .recovering
            .get(agent_id)
            .is_some_and(|claim| claim.generation == generation)
        {
            registry.recovering.remove(agent_id);
        }
    }

    async fn notify_runtime_recovery(&self, agent_id: &str) {
        let notify = self
            .inner
            .runtimes
            .read()
            .await
            .recovering
            .get(agent_id)
            .map(|claim| claim.notify.clone());
        if let Some(notify) = notify {
            notify.notify_one();
        }
    }

    async fn recover_runtime_after_failure(&self, notice: RuntimeRecoveryNotice) {
        let mut generation: u64;
        let mut attempt = 0usize;
        loop {
            let claim = {
                self.inner
                    .runtimes
                    .read()
                    .await
                    .recovering
                    .get(&notice.agent_id)
                    .cloned()
            };
            let Some(claim) = claim else {
                return;
            };
            generation = claim.generation;
            if !claim.retryable {
                tracing::warn!(
                    agent_id = notice.agent_id,
                    generation,
                    "runtime failure is not retryable; automatic recovery disabled"
                );
                self.clear_runtime_recovery(&notice.agent_id, generation)
                    .await;
                return;
            }
            let delay = RUNTIME_RECOVERY_BACKOFF
                [attempt.min(RUNTIME_RECOVERY_BACKOFF.len().saturating_sub(1))];
            tokio::select! {
                _ = self.inner.runtime_recovery_token.cancelled() => {
                    self.clear_runtime_recovery(&notice.agent_id, generation).await;
                    return;
                }
                _ = tokio::time::sleep(delay) => {}
                _ = claim.notify.notified() => {}
            }
            attempt = attempt.saturating_add(1);

            match self.active_agent_identity(&notice.agent_id) {
                Ok(_) => {}
                Err(PublicAgentError::Runtime(error)) => {
                    tracing::warn!(
                        agent_id = notice.agent_id,
                        error = %error,
                        "runtime recovery identity check failed"
                    );
                    continue;
                }
                Err(_) => {
                    self.clear_runtime_recovery(&notice.agent_id, generation)
                        .await;
                    return;
                }
            }
            let state = match self
                .agent_storage_read_only(&notice.agent_id)
                .and_then(|storage| storage.read_agent())
            {
                Ok(state) => state.unwrap_or_else(|| AgentState::new(&notice.agent_id)),
                Err(error) => {
                    tracing::warn!(
                        agent_id = notice.agent_id,
                        error = %error,
                        "runtime recovery state check failed"
                    );
                    continue;
                }
            };
            if state.status == AgentStatus::Stopped {
                self.clear_runtime_recovery(&notice.agent_id, generation)
                    .await;
                return;
            }

            let existing_runtime = {
                let mut registry = self.inner.runtimes.write().await;
                if registry.phase != HostRuntimePhase::Open {
                    registry.recovering.remove(&notice.agent_id);
                    return;
                }
                let Some(claim) = registry.recovering.get(&notice.agent_id) else {
                    return;
                };
                if !claim.retryable {
                    continue;
                }
                generation = claim.generation;
                let Some(entry) = registry.agents.get(&notice.agent_id) else {
                    registry.recovering.remove(&notice.agent_id);
                    return;
                };
                let entry_generation = entry.generation;
                let entry_terminated = entry.task.is_finished()
                    || *entry.phase.borrow() == AgentRuntimePhase::Terminated;
                let accessible_runtime = entry.accepts_host_access().then(|| entry.runtime.clone());
                if entry_generation != generation {
                    if accessible_runtime.is_some() {
                        registry.recovering.remove(&notice.agent_id);
                        return;
                    }
                    continue;
                } else if entry_terminated {
                    registry.agents.remove(&notice.agent_id);
                    None
                } else {
                    accessible_runtime
                }
            };

            let runtime = match existing_runtime {
                Some(runtime) => runtime,
                None => match self
                    .activate_agent(&notice.agent_id, RuntimeActivationReason::StartupRecovery)
                    .await
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        tracing::warn!(
                            agent_id = notice.agent_id,
                            attempt,
                            error = %error,
                            "runtime automatic recovery activation failed"
                        );
                        continue;
                    }
                },
            };
            {
                let mut registry = self.inner.runtimes.write().await;
                if let Some(entry) = registry.agents.get(&notice.agent_id) {
                    let previous_generation = generation;
                    generation = entry.generation;
                    if registry
                        .recovering
                        .get(&notice.agent_id)
                        .is_some_and(|claim| claim.generation == previous_generation)
                    {
                        if let Some(claim) = registry.recovering.get_mut(&notice.agent_id) {
                            claim.generation = generation;
                        }
                    }
                }
            }

            match runtime.wait_for_bootstrap().await {
                Ok(()) => {
                    let recovered = {
                        let mut registry = self.inner.runtimes.write().await;
                        let claim_matches = registry
                            .recovering
                            .get(&notice.agent_id)
                            .is_some_and(|claim| claim.generation == generation && claim.retryable);
                        let runtime_is_healthy =
                            registry.agents.get(&notice.agent_id).is_some_and(|entry| {
                                entry.generation == generation && entry.accepts_host_access()
                            });
                        if claim_matches && runtime_is_healthy {
                            registry.recovering.remove(&notice.agent_id);
                            true
                        } else {
                            false
                        }
                    };
                    if !recovered {
                        continue;
                    }
                    let _ = runtime
                        .storage()
                        .append_event(&crate::types::AuditEvent::legacy(
                            "runtime_loop_recovered",
                            json!({
                                "agent_id": notice.agent_id,
                                "failed_generation": notice.generation,
                                "recovered_generation": generation,
                                "attempt": attempt,
                            }),
                        ));
                    tracing::info!(
                        agent_id = notice.agent_id,
                        attempt,
                        "agent runtime loop recovered automatically"
                    );
                    return;
                }
                Err(error) => {
                    let descriptor = describe_runtime_error(&error);
                    tracing::warn!(
                        agent_id = notice.agent_id,
                        attempt,
                        retryable = descriptor.retryable,
                        error = %error,
                        "runtime automatic recovery bootstrap failed"
                    );
                    if !descriptor.retryable {
                        self.clear_runtime_recovery(&notice.agent_id, generation)
                            .await;
                        return;
                    }
                }
            }
        }
    }

    /// Signal the deletion coordinator to stop.
    pub async fn shutdown_daemon_deletion_coordinator(&self) {
        self.inner.daemon_deletion_token.cancel();
    }

    async fn run_daemon_runtime_db_retention(self) {
        loop {
            if self.inner.daemon_retention_token.is_cancelled() {
                break;
            }
            let policy = match self.config().runtime_db_retention_policy() {
                Ok(policy) => policy,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "daemon runtime db retention: invalid reloaded policy"
                    );
                    if self
                        .wait_daemon_retention_round(Duration::from_secs(60))
                        .await
                    {
                        break;
                    }
                    continue;
                }
            };
            if policy.enabled {
                let db = self.inner.runtime_db.clone();
                let result = tokio::task::spawn_blocking(move || {
                    db.run_retention_pass(policy, chrono::Utc::now())
                })
                .await;
                match result {
                    Ok(Ok(report)) => {
                        tracing::info!(
                            audit_deleted = report.audit_events.deleted_rows,
                            transcript_deleted = report.transcript_entries.deleted_rows,
                            tool_deleted = report.tool_executions.deleted_rows,
                            elapsed_ms = report.elapsed_ms,
                            freelist_pages = report.freelist_pages_after,
                            "daemon runtime db retention pass completed"
                        );
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(
                            error = %error,
                            "daemon runtime db retention pass failed"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "daemon runtime db retention task failed"
                        );
                    }
                }
            }
            let interval_hours = self
                .config()
                .runtime_db_retention_policy()
                .map(|policy| policy.interval_hours)
                .unwrap_or(1);
            if self
                .wait_daemon_retention_round(Duration::from_secs(
                    interval_hours.saturating_mul(60 * 60),
                ))
                .await
            {
                break;
            }
        }
    }

    async fn wait_daemon_retention_round(&self, duration: Duration) -> bool {
        tokio::select! {
            _ = self.inner.daemon_retention_token.cancelled() => true,
            _ = tokio::time::sleep(duration) => false,
        }
    }

    const DAEMON_INDEXER_BATCH: usize = 500;
    const DAEMON_INDEXER_FALLBACK_POLL: Duration = Duration::from_secs(60);
    /// First retry delay for a failing memory-index agent before exponential
    /// growth.
    const MEMORY_INDEXER_RETRY_BASE: Duration = Duration::from_millis(500);
    /// Upper bound for a failing agent's retry delay.
    const MEMORY_INDEXER_RETRY_MAX: Duration = Duration::from_secs(30);

    async fn run_daemon_memory_indexer(self) {
        use crate::memory::{memory_index_agent_ids_with_pending, refresh_memory_index_bounded};
        // Process-local per-agent retry backoff. A persistently failing agent
        // (for example a locked index) must not be retried on every global
        // notify driven by other agents' writes.
        let mut agent_retry_not_before: HashMap<String, (tokio::time::Instant, u32)> =
            HashMap::new();
        loop {
            if self.inner.daemon_indexer_token.is_cancelled() {
                break;
            }
            let runtime_agent_ids = match self
                .inner
                .runtime_db
                .runtime_index_outbox()
                .agent_ids_with_pending()
            {
                Ok(ids) => ids,
                Err(error) => {
                    tracing::warn!(error = %error, "daemon memory indexer: failed to query pending agents");
                    self.wait_daemon_indexer_round(None).await;
                    continue;
                }
            };
            let default_storage = match self.agent_storage(&self.config().default_agent_id) {
                Ok(storage) => storage,
                Err(error) => {
                    tracing::warn!(error = %error, "daemon memory indexer: failed to open shared index");
                    self.wait_daemon_indexer_round(None).await;
                    continue;
                }
            };
            let pending_source_agent_ids = match memory_index_agent_ids_with_pending(
                &default_storage,
            ) {
                Ok(ids) => ids.into_iter().collect::<std::collections::BTreeSet<_>>(),
                Err(error) => {
                    tracing::warn!(error = %error, "daemon memory indexer: failed to query pending source agents");
                    self.wait_daemon_indexer_round(None).await;
                    continue;
                }
            };
            let agent_ids = runtime_agent_ids
                .into_iter()
                .chain(pending_source_agent_ids.iter().cloned())
                .collect::<std::collections::BTreeSet<_>>();

            let mut did_work = false;
            for agent_id in &agent_ids {
                if let Some((not_before, _)) = agent_retry_not_before.get(agent_id) {
                    if tokio::time::Instant::now() < *not_before {
                        continue;
                    }
                }
                let storage = match self.agent_storage(agent_id) {
                    Ok(storage) => storage,
                    Err(error) => {
                        tracing::warn!(
                            agent_id = %agent_id,
                            error = %error,
                            "daemon memory indexer: failed to open storage"
                        );
                        continue;
                    }
                };
                let result = tokio::task::spawn_blocking(move || {
                    refresh_memory_index_bounded(&storage, None, Self::DAEMON_INDEXER_BATCH)
                })
                .await;
                match result {
                    Ok(Ok(status)) => {
                        agent_retry_not_before.remove(agent_id);
                        did_work |= status.lag > 0
                            || status.consumption_was_limited
                            // A successful rebuild consumes every pending source
                            // for the agent, so another immediate round is useful.
                            || (pending_source_agent_ids.contains(agent_id)
                                && status.skipped_error_count == 0);
                        tracing::debug!(
                            agent_id = %agent_id,
                            freshness = %status.freshness,
                            lag = status.lag,
                            "daemon memory indexer: processed agent"
                        );
                    }
                    Ok(Err(error)) => {
                        let delay = Self::memory_indexer_retry_delay(
                            agent_retry_not_before
                                .get(agent_id)
                                .map(|(_, attempts)| *attempts)
                                .unwrap_or(0),
                        );
                        tracing::warn!(
                            agent_id = %agent_id,
                            retry_in_ms = delay.as_millis() as u64,
                            error = %error,
                            "daemon memory indexer: refresh failed; backing off agent"
                        );
                        Self::back_off_memory_indexer_agent(&mut agent_retry_not_before, agent_id);
                    }
                    Err(error) => {
                        let delay = Self::memory_indexer_retry_delay(
                            agent_retry_not_before
                                .get(agent_id)
                                .map(|(_, attempts)| *attempts)
                                .unwrap_or(0),
                        );
                        tracing::warn!(
                            agent_id = %agent_id,
                            retry_in_ms = delay.as_millis() as u64,
                            error = %error,
                            "daemon memory indexer: task failed; backing off agent"
                        );
                        Self::back_off_memory_indexer_agent(&mut agent_retry_not_before, agent_id);
                    }
                }
            }

            if did_work {
                tokio::task::yield_now().await;
            } else {
                let next_retry_at = agent_retry_not_before
                    .values()
                    .map(|(not_before, _)| *not_before)
                    .min();
                self.wait_daemon_indexer_round(next_retry_at).await;
            }
        }
    }

    /// Exponential retry delay with a cap and ±25% jitter so concurrent
    /// failing agents do not retry in lockstep.
    fn memory_indexer_retry_delay(attempts: u32) -> Duration {
        let shift = attempts.min(8);
        let exponential = Self::MEMORY_INDEXER_RETRY_BASE
            .saturating_mul(1u32.checked_shl(shift).unwrap_or(u32::MAX));
        let capped = exponential.min(Self::MEMORY_INDEXER_RETRY_MAX);
        let jitter_unit = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos() % 1000)
            .unwrap_or(0) as f64;
        let factor = 0.75 + jitter_unit / 2000.0;
        Duration::from_secs_f64(capped.as_secs_f64() * factor)
    }

    fn back_off_memory_indexer_agent(
        agent_retry_not_before: &mut HashMap<String, (tokio::time::Instant, u32)>,
        agent_id: &str,
    ) {
        let attempts = agent_retry_not_before
            .get(agent_id)
            .map(|(_, attempts)| *attempts)
            .unwrap_or(0);
        let delay = Self::memory_indexer_retry_delay(attempts);
        agent_retry_not_before.insert(
            agent_id.to_string(),
            (tokio::time::Instant::now() + delay, attempts + 1),
        );
    }

    async fn wait_daemon_indexer_round(&self, next_retry_at: Option<tokio::time::Instant>) {
        let retry_wait = async {
            match next_retry_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = self.inner.daemon_indexer_token.cancelled() => {}
            _ = self.inner.memory_index_notify.notified() => {}
            _ = tokio::time::sleep(Self::DAEMON_INDEXER_FALLBACK_POLL) => {}
            _ = retry_wait => {}
        }
    }

    pub(crate) fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<PublishedAuditEvent> {
        self.inner.event_bus.subscribe()
    }

    fn agent_storage_read_only(&self, agent_id: &str) -> Result<AppStorage> {
        let storage = AppStorage::open_read_only_for_agent(
            self.agent_data_dir(agent_id),
            agent_id.to_string(),
            self.runtime_db().clone(),
        )?;
        Ok(storage)
    }

    pub(crate) fn bridge(&self) -> RuntimeHostBridge {
        RuntimeHostBridge {
            inner: Arc::downgrade(&self.inner),
        }
    }

    pub async fn shutdown(&self) -> Result<()> {
        let entries = {
            let mut registry = self.inner.runtimes.write().await;
            match registry.phase {
                HostRuntimePhase::Open => {
                    registry.phase = HostRuntimePhase::Closing;
                    registry
                        .agents
                        .drain()
                        .map(|(_, entry)| entry)
                        .collect::<Vec<_>>()
                }
                HostRuntimePhase::Closing | HostRuntimePhase::Closed => return Ok(()),
            }
        };
        self.shutdown_runtime_recovery_coordinator().await;
        self.shutdown_daemon_runtime_db_retention().await;
        self.shutdown_daemon_memory_indexer().await;
        self.shutdown_daemon_deletion_coordinator().await;
        let mut tasks = Vec::with_capacity(entries.len());
        for entry in entries {
            let _ = entry.runtime.request_service_shutdown().await;
            tasks.push(entry.task);
        }
        if tokio::time::timeout(HOST_SHUTDOWN_GRACE, async {
            for task in &mut tasks {
                let _ = task.await;
            }
        })
        .await
        .is_err()
        {
            for task in &tasks {
                task.abort();
            }
            for task in tasks {
                let _ = task.await;
            }
        }
        self.inner.runtimes.write().await.phase = HostRuntimePhase::Closed;
        Ok(())
    }

    pub(crate) async fn unload_runtime(&self, agent_id: &str) {
        let entry = self.inner.runtimes.write().await.agents.remove(agent_id);
        self.notify_runtime_recovery(agent_id).await;
        if let Some(entry) = entry {
            entry.task.abort();
            let _ = entry.task.await;
        }
    }

    pub async fn default_runtime(&self) -> Result<RuntimeHandle> {
        self.activate_agent(
            &self.config().default_agent_id,
            RuntimeActivationReason::AgentLifecycle,
        )
        .await
    }

    pub async fn recover_orphaned_queue_claims_at_startup(&self) -> Result<Vec<String>> {
        let recovered_queue_agent_ids = self
            .runtime_db()
            .recover_orphaned_dequeued_claims_at_startup()?;
        let queue_recovery_candidate_ids = self
            .runtime_db()
            .queue_entries()
            .recovery_candidate_agent_ids()?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let active_task_owner_ids = self
            .runtime_db()
            .tasks()
            .active_owner_agent_ids()?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let active_timer_owner_ids = self
            .runtime_db()
            .timers()
            .active_owner_agent_ids()?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let mut recovery_agent_ids = recovered_queue_agent_ids
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        recovery_agent_ids.extend(queue_recovery_candidate_ids.iter().cloned());
        recovery_agent_ids.extend(active_task_owner_ids.iter().cloned());
        recovery_agent_ids.extend(active_timer_owner_ids.iter().cloned());
        for agent_id in recovery_agent_ids {
            let state = self
                .agent_storage_read_only(&agent_id)?
                .read_agent()?
                .unwrap_or_else(|| AgentState::new(&agent_id));
            if state.status == AgentStatus::Stopped
                && !active_task_owner_ids.contains(&agent_id)
                && !queue_recovery_candidate_ids.contains(&agent_id)
            {
                continue;
            }
            let runtime = self
                .activate_agent(&agent_id, RuntimeActivationReason::StartupRecovery)
                .await?;
            runtime.wait_for_bootstrap().await?;
        }
        Ok(recovered_queue_agent_ids)
    }

    fn active_agent_identity(
        &self,
        agent_id: &str,
    ) -> std::result::Result<AgentIdentityRecord, PublicAgentError> {
        let identity = self
            .agent_identity_record(agent_id)
            .map_err(PublicAgentError::Runtime)?
            .ok_or_else(|| PublicAgentError::NotFound {
                agent_id: agent_id.to_string(),
            })?;
        match identity.status {
            AgentRegistryStatus::Active => Ok(identity),
            AgentRegistryStatus::Deleting => Err(PublicAgentError::Deleting {
                agent_id: agent_id.to_string(),
            }),
            AgentRegistryStatus::Deleted => Err(PublicAgentError::Deleted {
                agent_id: agent_id.to_string(),
            }),
        }
    }

    fn public_agent_identity(
        &self,
        agent_id: &str,
    ) -> std::result::Result<AgentIdentityRecord, PublicAgentError> {
        let identity = self.active_agent_identity(agent_id)?;
        if identity.visibility != AgentVisibility::Public {
            return Err(PublicAgentError::Private {
                agent_id: agent_id.to_string(),
            });
        }
        Ok(identity)
    }

    fn public_activation_error(error: anyhow::Error) -> PublicAgentError {
        if error.downcast_ref::<RuntimeAdmissionClosed>().is_some() {
            PublicAgentError::ShuttingDown
        } else {
            PublicAgentError::Runtime(error)
        }
    }

    pub(crate) fn operator_agent_read_storage(
        &self,
        agent_id: &str,
    ) -> std::result::Result<AppStorage, PublicAgentError> {
        self.active_agent_identity(agent_id)?;
        self.agent_storage_read_only(agent_id)
            .map_err(PublicAgentError::Runtime)
    }

    pub(crate) async fn try_get_loaded_runtime(&self, agent_id: &str) -> Option<RuntimeHandle> {
        let registry = self.inner.runtimes.read().await;
        registry
            .agents
            .get(agent_id)
            .filter(|entry| entry.accepts_host_access())
            .map(|entry| entry.runtime.clone())
    }

    pub(crate) async fn try_get_operator_loaded_runtime(
        &self,
        agent_id: &str,
    ) -> std::result::Result<Option<RuntimeHandle>, PublicAgentError> {
        self.active_agent_identity(agent_id)?;
        Ok(self.try_get_loaded_runtime(agent_id).await)
    }

    pub(crate) async fn operator_agent_skills_view(
        &self,
        agent_id: &str,
    ) -> std::result::Result<crate::types::SkillsRuntimeView, PublicAgentError> {
        let identity = self.active_agent_identity(agent_id)?;
        let identity_view =
            AgentIdentityView::from_record(&identity, &self.config().default_agent_id);
        let storage = self
            .agent_storage_read_only(agent_id)
            .map_err(PublicAgentError::Runtime)?;
        let state = storage
            .read_agent()
            .map_err(PublicAgentError::Runtime)?
            .unwrap_or_else(|| AgentState::new(agent_id));
        let agent_home = self.agent_data_dir(agent_id);
        let workspace_skill_root = state
            .active_workspace_entry
            .as_ref()
            .map(|entry| entry.execution_root.as_path());
        let config = self.config();
        let skill_roots = effective_skill_root_registrations(
            skill_visibility(&identity_view),
            config.user_home_dir.as_deref(),
            agent_id,
            &agent_home,
            workspace_skill_root,
        );
        let mut registry = self.inner.skills_registry.write().await;
        registry
            .sync_effective_roots(skill_roots.clone())
            .map_err(PublicAgentError::Runtime)?;
        let mut skills = skills_runtime_view_from_catalog(
            registry.catalog_for_roots(&skill_roots, None),
            &skill_roots,
            &state.active_skills,
        );
        skills.agent_templates_catalog =
            discover_agent_templates_catalog(config.user_home_dir.as_deref(), &agent_home);
        Ok(skills)
    }

    pub(crate) async fn search_memory_read_only(
        &self,
        query: &str,
        limit: usize,
        include_all_workspaces: bool,
        agent_ids: &[String],
        source_kinds: &[String],
    ) -> Result<crate::memory::MemorySearchQueryResult> {
        let default_agent_id = self.config().default_agent_id.clone();
        let storage = self.agent_storage_read_only(&default_agent_id)?;
        let active_workspace_id = storage
            .read_agent()?
            .and_then(|agent| agent.active_workspace_entry)
            .map(|entry| entry.workspace_id);
        let mut agent_storages = Vec::new();
        for agent_id in agent_ids {
            self.public_agent_identity(agent_id)
                .map_err(anyhow::Error::new)?;
            agent_storages.push(self.agent_storage_read_only(agent_id)?);
        }
        let query = query.to_string();
        let agent_ids = agent_ids.to_vec();
        let source_kinds = source_kinds.to_vec();
        tokio::task::spawn_blocking(move || {
            let _ = crate::memory::ensure_memory_indexes_fresh(
                &storage,
                active_workspace_id.as_deref(),
                &agent_storages,
            );
            crate::memory::search_memory_query_for_agent_storages(
                &storage,
                &query,
                limit,
                active_workspace_id.as_deref(),
                include_all_workspaces,
                &agent_ids,
                &source_kinds,
                &agent_storages,
            )
        })
        .await?
    }

    pub(crate) async fn get_memory_read_only(
        &self,
        source_ref: &str,
        max_chars: Option<usize>,
    ) -> Result<Option<crate::memory::MemoryGetResult>> {
        let storage = self.agent_storage_read_only(&self.config().default_agent_id)?;
        let active_workspace_id = storage
            .read_agent()?
            .and_then(|agent| agent.active_workspace_entry)
            .map(|entry| entry.workspace_id);
        let source_ref = source_ref.to_string();
        tokio::task::spawn_blocking(move || {
            crate::memory::get_memory(
                &storage,
                &source_ref,
                max_chars,
                active_workspace_id.as_deref(),
            )
        })
        .await?
    }

    pub(crate) fn public_agent_scheduler_repair_inspection(
        &self,
        agent_id: &str,
    ) -> std::result::Result<SchedulerRepairInspection, PublicAgentError> {
        self.public_agent_identity(agent_id)?;
        let storage = self
            .agent_storage_read_only(agent_id)
            .map_err(PublicAgentError::Runtime)?;
        let active_waits = storage
            .raw_active_wait_conditions_for_agent(agent_id)
            .map_err(PublicAgentError::Runtime)?;
        let mut wake_only_queue_entries = Vec::new();
        for entry in storage
            .latest_queue_entries()
            .map_err(PublicAgentError::Runtime)?
        {
            if entry.agent_id != agent_id
                || !matches!(
                    entry.status,
                    QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
                )
            {
                continue;
            }
            if storage
                .read_message_by_id(&entry.message_id)
                .map_err(PublicAgentError::Runtime)?
                .as_ref()
                .is_some_and(crate::runtime::is_wake_only_message)
            {
                wake_only_queue_entries.push(entry);
            }
        }
        Ok(SchedulerRepairInspection {
            agent_id: agent_id.to_string(),
            active_waits,
            wake_only_queue_entries,
        })
    }

    pub(crate) fn public_agent_worktree_summary(
        &self,
        agent_id: &str,
    ) -> std::result::Result<String, PublicAgentError> {
        self.public_agent_identity(agent_id)?;
        let storage = self
            .agent_storage_read_only(agent_id)
            .map_err(PublicAgentError::Runtime)?;
        let tasks = storage
            .latest_all_task_records(usize::MAX)
            .map_err(PublicAgentError::Runtime)?;
        let messages = storage
            .read_recent_messages(200)
            .map_err(PublicAgentError::Runtime)?;
        Ok(crate::runtime::format_worktree_task_summary(
            &tasks, &messages,
        ))
    }

    pub(crate) async fn local_agent_summary(
        &self,
        agent_id: &str,
    ) -> std::result::Result<AgentSummary, PublicAgentError> {
        self.active_agent_identity(agent_id)?;
        if let Some(runtime) = self.try_get_loaded_runtime(agent_id).await {
            return runtime
                .agent_summary()
                .await
                .map_err(PublicAgentError::Runtime);
        }
        // Storage-backed fallback: build AgentSummary without activating runtime.
        let identity = self.active_agent_identity(agent_id)?;
        let storage = self
            .agent_storage_read_only(agent_id)
            .map_err(PublicAgentError::Runtime)?;
        let agent_state = storage
            .read_agent()
            .map_err(PublicAgentError::Runtime)?
            .unwrap_or_else(|| stopped_unloaded_agent(agent_id));
        let work_queue = storage
            .work_queue_read_model()
            .map_err(PublicAgentError::Runtime)?;
        let scheduling_posture = storage
            .agent_posture_projection_with_work_queue(&agent_state, &work_queue)
            .map_err(PublicAgentError::Runtime)?;
        let closure = RuntimeHandle::closure_decision_from_storage(&storage, &agent_state)
            .map_err(PublicAgentError::Runtime)?;
        let active_children = self
            .child_agent_summaries(agent_id)
            .await
            .map_err(PublicAgentError::Runtime)?;
        let identity_view =
            AgentIdentityView::from_record(&identity, &self.config().default_agent_id);
        let model = crate::runtime::agent_model_state_for_catalog(
            &RuntimeModelCatalog::from_config(&self.config()),
            &self.runtime_context_config(),
            &agent_state,
        );
        let token_usage = AgentTokenUsageSummary {
            total: TokenUsage::new(
                agent_state.total_input_tokens,
                agent_state.total_output_tokens,
            ),
            total_model_rounds: agent_state.total_model_rounds,
            last_turn: agent_state.last_turn_token_usage.clone(),
        };
        let execution = if let Some(entry) = agent_state.active_workspace_entry.as_ref() {
            ExecutionSnapshot {
                profile: ExecutionProfile::default(),
                policy: ExecutionProfile::default().policy_snapshot(),
                attached_workspaces: Vec::new(),
                workspace_id: Some(entry.workspace_id.clone()),
                workspace_anchor: entry.workspace_anchor.clone(),
                execution_root: entry.execution_root.clone(),
                cwd: entry.cwd.clone(),
                execution_root_id: Some(entry.execution_root_id.clone()),
                projection_kind: Some(entry.projection_kind),
                access_mode: Some(entry.access_mode),
                worktree_root: None,
                execution_roots: Vec::new(),
            }
        } else {
            ExecutionSnapshot {
                profile: ExecutionProfile::default(),
                policy: ExecutionProfile::default().policy_snapshot(),
                attached_workspaces: Vec::new(),
                workspace_id: None,
                workspace_anchor: PathBuf::new(),
                execution_root: PathBuf::new(),
                cwd: PathBuf::new(),
                execution_root_id: None,
                projection_kind: None,
                access_mode: None,
                worktree_root: None,
                execution_roots: Vec::new(),
            }
        };
        let active_wait_conditions = storage
            .active_wait_conditions_for_agent(agent_id)
            .map_err(PublicAgentError::Runtime)?
            .into_iter()
            .map(WaitConditionSummary::from)
            .collect();
        let active_external_triggers = storage
            .latest_external_triggers()
            .map_err(PublicAgentError::Runtime)?
            .into_iter()
            .filter(|record| record.status == ExternalTriggerStatus::Active)
            .map(|record| ExternalTriggerSummary {
                external_trigger_id: record.external_trigger_id,
                target_agent_id: record.target_agent_id,
                scope: record.scope,
                delivery_mode: record.delivery_mode,
                status: record.status,
                delivery_count: record.delivery_count,
                created_at: record.created_at,
                revoked_at: record.revoked_at,
                last_delivered_at: record.last_delivered_at,
            })
            .collect();
        let skills = self.operator_agent_skills_view(agent_id).await?;
        let loaded_agents_md = LoadedAgentsMdView::default();
        let summary = AgentSummary {
            identity: identity_view,
            lifecycle: AgentLifecycleHint::from_status(agent_id, agent_state.status.clone()),
            agent: agent_state,
            scheduling_posture,
            active_task_count: storage
                .active_task_count_for_agent(agent_id)
                .map_err(PublicAgentError::Runtime)?,
            model,
            token_usage,
            closure,
            execution,
            active_workspace_occupancy: None,
            loaded_agents_md,
            skills,
            active_children,
            active_wait_conditions,
            active_external_triggers,
            recent_operator_notifications: storage
                .read_recent_operator_notifications(10)
                .map_err(PublicAgentError::Runtime)?,
            recent_brief_count: storage
                .read_recent_briefs(50)
                .map_err(PublicAgentError::Runtime)?
                .len(),
            recent_event_count: storage
                .read_recent_events(100)
                .map_err(PublicAgentError::Runtime)?
                .len(),
        };
        Ok(summary)
    }

    pub async fn get_public_agent(
        &self,
        agent_id: &str,
    ) -> std::result::Result<RuntimeHandle, PublicAgentError> {
        self.public_agent_identity(agent_id)?;
        let runtime = self
            .activate_agent(agent_id, RuntimeActivationReason::OperatorControl)
            .await
            .map_err(Self::public_activation_error)?;
        runtime
            .wait_for_bootstrap()
            .await
            .map_err(PublicAgentError::Runtime)?;
        Ok(runtime)
    }

    pub async fn get_operator_agent(
        &self,
        agent_id: &str,
    ) -> std::result::Result<RuntimeHandle, PublicAgentError> {
        self.active_agent_identity(agent_id)?;
        let runtime = self
            .activate_agent(agent_id, RuntimeActivationReason::OperatorControl)
            .await
            .map_err(Self::public_activation_error)?;
        runtime
            .wait_for_bootstrap()
            .await
            .map_err(PublicAgentError::Runtime)?;
        Ok(runtime)
    }

    pub async fn begin_public_agent_deletion(
        &self,
        agent_id: &str,
        cascade_private_children: bool,
        requested_by: &str,
    ) -> std::result::Result<(AgentIdentityRecord, AgentDeletionJob, bool), PublicAgentError> {
        self.validate_agent_id(agent_id)
            .map_err(PublicAgentError::Runtime)?;
        let bootstrap_lock = self.agent_bootstrap_lock(agent_id);
        let _bootstrap_guard = bootstrap_lock.lock().await;
        let identity = self
            .agent_identity_record(agent_id)
            .map_err(PublicAgentError::Runtime)?
            .ok_or_else(|| PublicAgentError::NotFound {
                agent_id: agent_id.to_string(),
            })?;
        if agent_id == self.config().default_agent_id {
            return Err(PublicAgentError::DeleteForbidden {
                agent_id: agent_id.to_string(),
                reason: "the configured default agent cannot be deleted".into(),
            });
        }
        if identity.visibility != AgentVisibility::Public
            || identity.ownership() != AgentOwnership::SelfOwned
        {
            return Err(PublicAgentError::DeleteForbidden {
                agent_id: agent_id.to_string(),
                reason: "only public self-owned agents have an operator delete surface".into(),
            });
        }
        let (updated_identity, job, created) = self
            .runtime_db()
            .agent_deletions()
            .begin(
                agent_id,
                identity.revision,
                requested_by,
                cascade_private_children,
            )
            .map_err(PublicAgentError::Runtime)?;
        self.inner
            .registry
            .cache_agent_identity(&updated_identity)
            .map_err(PublicAgentError::Runtime)?;
        self.unload_runtime(agent_id).await;
        // Trigger the deletion coordinator inline for immediate progress.
        if created {
            let host = self.clone();
            tokio::spawn(async move {
                let _ = host.execute_pending_deletions().await;
            });
        }
        Ok((updated_identity, job, created))
    }

    pub fn public_agent_deletion_status(
        &self,
        agent_id: &str,
    ) -> std::result::Result<(AgentIdentityRecord, Option<AgentDeletionJob>), PublicAgentError>
    {
        self.validate_agent_id(agent_id)
            .map_err(PublicAgentError::Runtime)?;
        let identity = self
            .agent_identity_record(agent_id)
            .map_err(PublicAgentError::Runtime)?
            .ok_or_else(|| PublicAgentError::NotFound {
                agent_id: agent_id.to_string(),
            })?;
        if identity.visibility != AgentVisibility::Public
            || identity.ownership() != AgentOwnership::SelfOwned
        {
            return Err(PublicAgentError::Private {
                agent_id: agent_id.to_string(),
            });
        }
        let job = self
            .runtime_db()
            .agent_deletions()
            .latest_for_agent(agent_id)
            .map_err(PublicAgentError::Runtime)?;
        Ok((identity, job))
    }

    pub fn public_agent_detail(
        &self,
        agent_id: &str,
    ) -> std::result::Result<AgentDetail, PublicAgentError> {
        self.validate_agent_id(agent_id)
            .map_err(PublicAgentError::Runtime)?;
        let identity = self
            .agent_identity_record(agent_id)
            .map_err(PublicAgentError::Runtime)?
            .ok_or_else(|| PublicAgentError::NotFound {
                agent_id: agent_id.to_string(),
            })?;
        if identity.visibility != AgentVisibility::Public
            || identity.ownership() != AgentOwnership::SelfOwned
        {
            return Err(PublicAgentError::Private {
                agent_id: agent_id.to_string(),
            });
        }
        self.agent_detail_from_identity(identity)
            .map_err(PublicAgentError::Runtime)
    }

    pub fn operator_agent_detail(
        &self,
        agent_id: &str,
    ) -> std::result::Result<AgentDetail, PublicAgentError> {
        self.validate_agent_id(agent_id)
            .map_err(PublicAgentError::Runtime)?;
        let identity = self
            .agent_identity_record(agent_id)
            .map_err(PublicAgentError::Runtime)?
            .ok_or_else(|| PublicAgentError::NotFound {
                agent_id: agent_id.to_string(),
            })?;
        self.agent_detail_from_identity(identity)
            .map_err(PublicAgentError::Runtime)
    }

    fn agent_detail_from_identity(&self, identity: AgentIdentityRecord) -> Result<AgentDetail> {
        let deletion = self
            .runtime_db()
            .agent_deletions()
            .latest_for_agent(&identity.agent_id)?;
        let bootstrap = self
            .runtime_db()
            .agent_bootstraps()
            .latest(&identity.agent_id)?
            .map(|record| record.summary());
        let canonical_relations = self
            .runtime_db()
            .agent_canonical_relations()
            .latest(&identity.agent_id)?
            .ok_or_else(|| anyhow!("missing canonical relations for {}", identity.agent_id))?;
        let lineage_children = self
            .runtime_db()
            .agent_canonical_relations()
            .lineage_children(&identity.agent_id)?;
        Ok(AgentDetail {
            display_name: identity.display_name(),
            name: identity.name.clone(),
            created_at: identity.created_at,
            updated_at: identity.updated_at,
            identity: AgentIdentityView::from_record(&identity, &self.config().default_agent_id),
            canonical_relations,
            lineage_children,
            bootstrap,
            deletion,
        })
    }

    pub fn rename_public_agent(
        &self,
        agent_id: &str,
        requested_name: &str,
        actor: &str,
    ) -> std::result::Result<AgentDetail, PublicAgentError> {
        let existing_identity = self
            .agent_identity_record(agent_id)
            .map_err(PublicAgentError::Runtime)?
            .ok_or_else(|| PublicAgentError::NotFound {
                agent_id: agent_id.to_string(),
            })?;
        if existing_identity.visibility != AgentVisibility::Public
            || existing_identity.ownership() != AgentOwnership::SelfOwned
        {
            return Err(PublicAgentError::Private {
                agent_id: agent_id.to_string(),
            });
        }
        if agent_id == self.config().default_agent_id {
            return Err(PublicAgentError::RenameForbidden {
                agent_id: agent_id.to_string(),
                reason: "the configured default agent cannot be renamed".into(),
            });
        }
        normalize_agent_name(requested_name).map_err(|error| PublicAgentError::InvalidName {
            agent_id: agent_id.to_string(),
            reason: error.to_string(),
        })?;
        let identity = self
            .runtime_db()
            .agent_identities()
            .rename(agent_id, requested_name, actor)
            .map_err(|error| {
                if error.to_string().contains("agent_name_conflict")
                    || error.to_string().contains("UNIQUE constraint failed")
                {
                    PublicAgentError::NameConflict {
                        agent_id: agent_id.to_string(),
                        name: requested_name.trim().to_string(),
                    }
                } else if error.to_string().contains("cannot be renamed while it is") {
                    match existing_identity.status {
                        AgentRegistryStatus::Deleting => PublicAgentError::Deleting {
                            agent_id: agent_id.to_string(),
                        },
                        AgentRegistryStatus::Deleted => PublicAgentError::Deleted {
                            agent_id: agent_id.to_string(),
                        },
                        AgentRegistryStatus::Active => PublicAgentError::Runtime(error),
                    }
                } else {
                    PublicAgentError::Runtime(error)
                }
            })?;
        self.inner
            .registry
            .cache_agent_identity(&identity)
            .map_err(PublicAgentError::Runtime)?;
        self.agent_detail_from_identity(identity)
            .map_err(PublicAgentError::Runtime)
    }

    pub(crate) async fn operator_agent_state_projection(
        &self,
        agent_id: &str,
        task_limit: usize,
        timer_limit: usize,
    ) -> std::result::Result<AgentStateReadProjection, PublicAgentError> {
        let identity = self.active_agent_identity(agent_id)?;
        let runtime = {
            let registry = self.inner.runtimes.read().await;
            registry
                .agents
                .get(agent_id)
                .filter(|entry| entry.accepts_host_access())
                .map(|entry| entry.runtime.clone())
        };

        if let Some(runtime) = runtime {
            crate::diagnostics::record_projection_state_source_loaded();
            tracing::debug!(
                agent_id,
                state_projection_source = AgentStateProjectionSource::Loaded.as_str(),
                "building public agent state projection"
            );
            let agent_started = std::time::Instant::now();
            let agent = runtime
                .lightweight_agent_state_projection()
                .await
                .map_err(PublicAgentError::Runtime)?;
            crate::diagnostics::record_projection_state_agent(agent_started.elapsed());

            let tasks_started = std::time::Instant::now();
            let tasks = runtime
                .active_tasks(task_limit)
                .await
                .map_err(PublicAgentError::Runtime)?;
            crate::diagnostics::record_projection_state_tasks(tasks_started.elapsed());

            let timers_started = std::time::Instant::now();
            let timers = runtime
                .recent_timers(timer_limit)
                .await
                .map_err(PublicAgentError::Runtime)?;
            crate::diagnostics::record_projection_state_timers(timers_started.elapsed());

            let triggers_started = std::time::Instant::now();
            let external_triggers = runtime
                .latest_external_triggers()
                .await
                .map_err(PublicAgentError::Runtime)?;
            crate::diagnostics::record_projection_state_external_triggers(
                triggers_started.elapsed(),
            );

            return Ok(AgentStateReadProjection {
                source: AgentStateProjectionSource::Loaded,
                agent,
                tasks,
                timers,
                external_triggers,
            });
        }

        crate::diagnostics::record_projection_state_source_storage();
        crate::diagnostics::record_projection_state_runtime_spawn_avoided();
        tracing::debug!(
            agent_id,
            state_projection_source = AgentStateProjectionSource::Storage.as_str(),
            "building public agent state projection"
        );
        let storage = self
            .agent_storage_read_only(agent_id)
            .map_err(PublicAgentError::Runtime)?;

        let agent_started = std::time::Instant::now();
        let agent_state = storage
            .read_agent()
            .map_err(PublicAgentError::Runtime)?
            .unwrap_or_else(|| stopped_unloaded_agent(agent_id));
        let work_queue = storage
            .work_queue_read_model()
            .map_err(PublicAgentError::Runtime)?;
        let scheduling_posture = storage
            .agent_posture_projection_with_work_queue(&agent_state, &work_queue)
            .map_err(PublicAgentError::Runtime)?;
        let closure = RuntimeHandle::closure_decision_from_storage(&storage, &agent_state)
            .map_err(PublicAgentError::Runtime)?;
        let active_children = self
            .child_agent_summaries(agent_id)
            .await
            .map_err(PublicAgentError::Runtime)?;
        let agent = LightweightAgentStateProjection {
            identity: AgentIdentityView::from_record(&identity, &self.config().default_agent_id),
            active_task_count: storage
                .active_task_count_for_agent(agent_id)
                .map_err(PublicAgentError::Runtime)?,
            lifecycle: AgentLifecycleHint::from_status(agent_id, agent_state.status.clone()),
            model: crate::runtime::agent_model_state_for_catalog(
                &RuntimeModelCatalog::from_config(&self.config()),
                &self.runtime_context_config(),
                &agent_state,
            ),
            agent: agent_state,
            scheduling_posture,
            closure,
            active_children,
            work_queue,
        };
        crate::diagnostics::record_projection_state_agent(agent_started.elapsed());

        let tasks_started = std::time::Instant::now();
        let tasks = storage
            .latest_active_task_records(task_limit)
            .map_err(PublicAgentError::Runtime)?;
        crate::diagnostics::record_projection_state_tasks(tasks_started.elapsed());

        let timers_started = std::time::Instant::now();
        let timers = storage
            .read_recent_timers(timer_limit)
            .map_err(PublicAgentError::Runtime)?;
        crate::diagnostics::record_projection_state_timers(timers_started.elapsed());

        let triggers_started = std::time::Instant::now();
        let external_triggers = storage
            .latest_external_triggers()
            .map_err(PublicAgentError::Runtime)?;
        crate::diagnostics::record_projection_state_external_triggers(triggers_started.elapsed());

        Ok(AgentStateReadProjection {
            source: AgentStateProjectionSource::Storage,
            agent,
            tasks,
            timers,
            external_triggers,
        })
    }

    /// Get an agent for the local control/status API, allowing private child
    /// agents in addition to public ones. This relies on the current local
    /// trusted control API boundary rather than hiding `agent_id`.
    pub async fn get_agent_for_local_status(
        &self,
        agent_id: &str,
    ) -> std::result::Result<RuntimeHandle, PublicAgentError> {
        self.active_agent_identity(agent_id)?;
        // Allow both Public and Private agents through the local status API.
        self.get_or_create_agent(agent_id)
            .await
            .map_err(PublicAgentError::Runtime)
    }

    pub async fn get_public_agent_for_external_ingress(
        &self,
        agent_id: &str,
    ) -> std::result::Result<RuntimeHandle, PublicAgentError> {
        self.public_agent_identity(agent_id)?;
        let state = self
            .agent_storage(agent_id)
            .map_err(PublicAgentError::Runtime)?
            .read_agent()
            .map_err(PublicAgentError::Runtime)?
            .unwrap_or_else(|| AgentState::new(agent_id.to_string()));
        if state.status == AgentStatus::Stopped {
            return Err(PublicAgentError::Stopped {
                agent_id: agent_id.to_string(),
            });
        }
        let runtime = self
            .activate_agent(agent_id, RuntimeActivationReason::ExternalIngress)
            .await
            .map_err(Self::public_activation_error)?;
        runtime
            .wait_for_bootstrap()
            .await
            .map_err(PublicAgentError::Runtime)?;
        Ok(runtime)
    }

    pub async fn control_public_agent(
        &self,
        agent_id: &str,
        action: crate::types::ControlAction,
    ) -> std::result::Result<RuntimeHandle, PublicAgentError> {
        let runtime = self.get_public_agent(agent_id).await?;
        let was_stopped = matches!(
            runtime
                .agent_state()
                .await
                .map_err(PublicAgentError::Runtime)?
                .status,
            AgentStatus::Stopped
        );
        if action.is_start() && was_stopped {
            self.unload_runtime(agent_id).await;
        }
        runtime
            .control(action.clone())
            .await
            .map_err(PublicAgentError::Runtime)?;
        self.notify_runtime_recovery(agent_id).await;
        if action.is_start() && was_stopped {
            return self.get_public_agent(agent_id).await;
        }
        Ok(runtime)
    }

    pub async fn abort_public_agent_current_run(
        &self,
        agent_id: &str,
        request: crate::runtime::CurrentRunAbortRequest,
    ) -> std::result::Result<crate::runtime::CurrentRunAbortOutcome, PublicAgentError> {
        let runtime = self.get_public_agent(agent_id).await?;
        runtime
            .abort_current_run(request)
            .await
            .map_err(PublicAgentError::Runtime)
    }

    pub async fn enqueue_public_work_item(
        &self,
        agent_id: &str,
        objective: String,
    ) -> std::result::Result<(RuntimeHandle, crate::types::WorkItemRecord), PublicAgentError> {
        let runtime = self.get_public_agent(agent_id).await?;
        let record = runtime
            .create_work_item(objective, None, None, Vec::new())
            .await
            .map_err(PublicAgentError::Runtime)?;
        Ok((runtime, record))
    }

    pub async fn create_named_agent(
        &self,
        agent_id: &str,
        template: Option<&str>,
    ) -> Result<AgentIdentityRecord> {
        let desired = AgentBootstrapDesiredState {
            template: template.map(ToString::to_string),
            catalog_agent_home: None,
            workspace: None,
            model_resolution: None,
            initial_message: None,
        };
        let (identity, created) = self
            .ensure_named_agent(
                agent_id,
                None,
                NamedAgentExistingBehavior::Reuse,
                None,
                desired,
            )
            .await?;
        if created {
            self.reconcile_agent_bootstrap(agent_id).await?;
        }
        Ok(identity)
    }

    pub async fn create_public_named_agent(
        &self,
        agent_id: &str,
        template: Option<&str>,
        lineage_parent_agent_id: Option<&str>,
        catalog_agent_home: Option<&Path>,
    ) -> Result<AgentCreateResult> {
        self.create_public_named_agent_with_name(
            agent_id,
            template,
            lineage_parent_agent_id,
            catalog_agent_home,
            None,
        )
        .await
    }

    pub async fn create_public_named_agent_with_name(
        &self,
        agent_id: &str,
        template: Option<&str>,
        lineage_parent_agent_id: Option<&str>,
        catalog_agent_home: Option<&Path>,
        requested_name: Option<&str>,
    ) -> Result<AgentCreateResult> {
        let desired = AgentBootstrapDesiredState {
            template: template.map(ToString::to_string),
            catalog_agent_home: catalog_agent_home.map(Path::to_path_buf),
            workspace: None,
            model_resolution: None,
            initial_message: None,
        };
        self.create_public_named_agent_with_bootstrap(
            agent_id,
            lineage_parent_agent_id,
            requested_name,
            desired,
        )
        .await
    }

    async fn create_public_named_agent_with_bootstrap(
        &self,
        agent_id: &str,
        lineage_parent_agent_id: Option<&str>,
        requested_name: Option<&str>,
        desired: AgentBootstrapDesiredState,
    ) -> Result<AgentCreateResult> {
        let (identity, created) = self
            .ensure_named_agent(
                agent_id,
                lineage_parent_agent_id,
                NamedAgentExistingBehavior::Reject,
                requested_name,
                desired,
            )
            .await?;
        let bootstrap = self.reconcile_agent_bootstrap(agent_id).await?;
        let bootstrap_summary = bootstrap.summary();
        Ok(AgentCreateResult {
            identity: AgentIdentityView::from_record(&identity, &self.config().default_agent_id),
            receipt: AgentCreateReceipt {
                receipt_id: ids::runtime_id("agent_create"),
                agent_id: identity.agent_id.clone(),
                name: identity.name.clone(),
                display_name: identity.display_name(),
                stage: if bootstrap_summary.status == AgentBootstrapStatus::Ready {
                    AgentCreateStage::Bootstrapped
                } else {
                    AgentCreateStage::Degraded
                },
                lifecycle: identity.status,
                created,
                bootstrap: bootstrap_summary,
            },
        })
    }

    async fn create_agent(
        &self,
        parent_runtime: RuntimeHandle,
        request: CreateAgentRequest,
    ) -> Result<AgentCreateResult> {
        if let Some(identity) = self.agent_identity_record(&request.agent_id)? {
            if identity.status == AgentRegistryStatus::Deleting {
                let job = self
                    .runtime_db()
                    .agent_deletions()
                    .latest_for_agent(&request.agent_id)?;
                return Err(named_agent_deletion_incomplete_error(
                    &request.agent_id,
                    job.as_ref(),
                ));
            }
            if identity.status != AgentRegistryStatus::Deleted {
                anyhow::ensure!(
                    identity.kind == AgentKind::Named
                        && identity.visibility == AgentVisibility::Public
                        && identity.ownership() == AgentOwnership::SelfOwned,
                    "agent {} already exists with an incompatible identity or lifecycle",
                    request.agent_id
                );
            } else {
                // A fully deleted id is creatable again. Fall through to
                // the creation path: it routes through the reincarnation
                // transaction when the deletion job completed and returns
                // a typed `deletion_incomplete` error otherwise.
                let job = self
                    .runtime_db()
                    .agent_deletions()
                    .latest_for_agent(&request.agent_id)?;
                if !matches!(job.as_ref(), Some(job) if job.status == AgentDeletionStatus::Completed)
                {
                    return Err(named_agent_deletion_incomplete_error(
                        &request.agent_id,
                        job.as_ref(),
                    ));
                }
            }
            if identity.status == AgentRegistryStatus::Active {
                let bootstrap = self.reconcile_agent_bootstrap(&request.agent_id).await?;
                let bootstrap_summary = bootstrap.summary();
                return Ok(AgentCreateResult {
                    identity: AgentIdentityView::from_record(
                        &identity,
                        &self.config().default_agent_id,
                    ),
                    receipt: AgentCreateReceipt {
                        receipt_id: ids::runtime_id("agent_create"),
                        agent_id: identity.agent_id.clone(),
                        name: identity.name.clone(),
                        display_name: identity.display_name(),
                        stage: if bootstrap_summary.status == AgentBootstrapStatus::Ready {
                            AgentCreateStage::Bootstrapped
                        } else {
                            AgentCreateStage::Degraded
                        },
                        lifecycle: identity.status,
                        created: false,
                        bootstrap: bootstrap_summary,
                    },
                });
            }
        }

        let parent_state = parent_runtime.agent_state().await?;
        let catalog_agent_home = request
            .inherit_parent_runtime
            .then(|| self.agent_data_dir(&parent_state.id));
        let workspace = request
            .inherit_parent_runtime
            .then(|| AgentBootstrapWorkspaceState {
                attached_workspaces:
                    crate::runtime::workspace::inherited_attached_workspaces_for_agent(
                        &parent_state,
                        &request.agent_id,
                    ),
                execution_profile: parent_state.execution_profile.clone(),
                inherited_model_override: parent_state.model_override.clone(),
                inherited_model_override_reasoning_effort: parent_state
                    .model_override_reasoning_effort
                    .clone(),
            });
        let initial_message = request
            .initial_message
            .map(|text| AgentBootstrapInitialMessage {
                message_id: format!("agent_bootstrap_message:{}", request.agent_id),
                text,
                authority_class: request.authority_class,
                creator_agent_id: parent_state.id,
            });
        let desired = AgentBootstrapDesiredState {
            template: request.template,
            catalog_agent_home,
            workspace,
            model_resolution: request.model_resolution,
            initial_message,
        };
        self.create_public_named_agent_with_bootstrap(
            &request.agent_id,
            request.lineage_parent_agent_id.as_deref(),
            request.name.as_deref(),
            desired,
        )
        .await
    }

    async fn ensure_named_agent(
        &self,
        agent_id: &str,
        lineage_parent_agent_id: Option<&str>,
        existing_behavior: NamedAgentExistingBehavior,
        requested_name: Option<&str>,
        desired: AgentBootstrapDesiredState,
    ) -> Result<(AgentIdentityRecord, bool)> {
        self.validate_agent_id(agent_id)?;
        if agent_id == self.config().default_agent_id {
            if existing_behavior == NamedAgentExistingBehavior::Reject {
                return Err(named_agent_already_exists_error(agent_id));
            }
            if desired.template.is_some() {
                return Err(anyhow!(
                    "default agent does not support template initialization through create_named_agent"
                ));
            }
            return self
                .ensure_default_agent_identity()
                .map(|record| (record, false));
        }
        if Self::is_temporary_agent_id(agent_id) {
            return Err(anyhow!(
                "agent id {} uses reserved temporary prefix {}",
                agent_id,
                TEMP_AGENT_PREFIX
            ));
        }
        let existing = self.agent_identity_record(agent_id)?;
        if let Some(existing) = existing {
            if existing_behavior == NamedAgentExistingBehavior::Reject {
                if existing.status == AgentRegistryStatus::Deleted {
                    // A fully deleted id is creatable again: route through
                    // the reincarnation transaction when the deletion job
                    // completed, or fail closed with a typed error.
                    return self
                        .reincarnate_deleted_agent(
                            existing,
                            lineage_parent_agent_id,
                            requested_name,
                            desired,
                        )
                        .await;
                }
                if existing.status == AgentRegistryStatus::Deleting {
                    let job = self
                        .runtime_db()
                        .agent_deletions()
                        .latest_for_agent(agent_id)?;
                    return Err(named_agent_deletion_incomplete_error(
                        agent_id,
                        job.as_ref(),
                    ));
                }
                return Err(named_agent_already_exists_error(agent_id));
            }
            if existing.status != AgentRegistryStatus::Active {
                return Err(anyhow!(self
                    .active_agent_identity(agent_id)
                    .expect_err("non-active identity must be fenced")));
            }
            if existing.kind != AgentKind::Named
                || existing.visibility != AgentVisibility::Public
                || existing.ownership() != AgentOwnership::SelfOwned
                || existing.profile_preset() != AgentProfilePreset::PublicNamed
            {
                return Err(anyhow!(
                    "agent {} already exists with a different identity type; expected a public self-owned named agent",
                    agent_id
                ));
            }
            if desired.template.is_some() {
                return Err(anyhow!(
                    "agent {} already exists; template initialization only applies when creating a new agent",
                    agent_id
                ));
            }
            return Ok((existing, false));
        }
        let normalized_name = requested_name
            .map(|name| {
                normalize_agent_name(name)
                    .map_err(|error| named_agent_invalid_name_error(agent_id, error))
            })
            .transpose()?;
        ensure_agent_home_layout(&self.agent_data_dir(agent_id)).map_err(|error| {
            named_agent_create_failed_error(agent_id, AgentCreateStage::Profiled, error)
        })?;
        let mut record = AgentIdentityRecord::new(
            agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        )
        .with_lineage_parent_agent_id(lineage_parent_agent_id.map(ToString::to_string));
        record.name = normalized_name;
        let bootstrap = AgentBootstrapRecord::new(agent_id, desired);
        let relations = independent_creation_records(
            &record,
            lineage_parent_agent_id,
            AgentCanonicalDurability::Persistent,
        );
        self.runtime_db()
            .agent_identities()
            .create_with_bootstrap_and_relations(&record, &bootstrap, &relations)
            .map_err(|error| {
                if error.to_string().contains("agent_identity_conflict") {
                    return named_agent_already_exists_error(agent_id);
                }
                if let Some(name) = record.name.as_deref() {
                    if error
                        .to_string()
                        .contains("UNIQUE constraint failed: agent_identities.name_key")
                    {
                        return named_agent_name_already_exists_error(agent_id, name);
                    }
                }
                named_agent_create_failed_error(agent_id, AgentCreateStage::Reserved, error)
            })?;
        self.inner.registry.cache_agent_identity(&record)?;
        Ok((record, true))
    }

    /// Creates a new incarnation of a fully deleted agent id. The latest
    /// deletion job must be `Completed`; anything else (in-flight, failed,
    /// or missing) fails closed with a typed `deletion_incomplete` error.
    /// The new incarnation is a fresh agent: new AgentHome bootstrap, no
    /// inherited WorkItems, tasks, waits, timers, triggers, or lineage
    /// beyond the explicitly requested parent.
    async fn reincarnate_deleted_agent(
        &self,
        tombstone: AgentIdentityRecord,
        lineage_parent_agent_id: Option<&str>,
        requested_name: Option<&str>,
        desired: AgentBootstrapDesiredState,
    ) -> Result<(AgentIdentityRecord, bool)> {
        let agent_id = tombstone.agent_id.clone();
        let job = self
            .runtime_db()
            .agent_deletions()
            .latest_for_agent(&agent_id)?;
        let completed = matches!(
            job.as_ref(),
            Some(job) if job.status == AgentDeletionStatus::Completed
        );
        if !completed {
            return Err(named_agent_deletion_incomplete_error(
                &agent_id,
                job.as_ref(),
            ));
        }
        let normalized_name = requested_name
            .map(|name| {
                normalize_agent_name(name)
                    .map_err(|error| named_agent_invalid_name_error(&agent_id, error))
            })
            .transpose()?;
        ensure_agent_home_layout(&self.agent_data_dir(&agent_id)).map_err(|error| {
            named_agent_create_failed_error(&agent_id, AgentCreateStage::Profiled, error)
        })?;
        let identity = self
            .runtime_db()
            .agent_identities()
            .reincarnate_with_bootstrap_and_relations(&agent_id, "agent_create", |tombstone| {
                let mut record = AgentIdentityRecord::new(
                    &agent_id,
                    AgentKind::Named,
                    AgentVisibility::Public,
                    AgentOwnership::SelfOwned,
                    AgentProfilePreset::PublicNamed,
                    None,
                    None,
                )
                .with_lineage_parent_agent_id(lineage_parent_agent_id.map(ToString::to_string));
                let now = std::cmp::max(
                    chrono::Utc::now(),
                    tombstone.updated_at + chrono::Duration::nanoseconds(1),
                );
                record.name = normalized_name.clone();
                record.incarnation = tombstone.incarnation.saturating_add(1);
                record.revision = tombstone.revision.saturating_add(1);
                record.created_at = now;
                record.updated_at = now;
                let bootstrap = AgentBootstrapRecord::new(&agent_id, desired.clone());
                let relations = independent_creation_records(
                    &record,
                    lineage_parent_agent_id,
                    AgentCanonicalDurability::Persistent,
                );
                (record, bootstrap, relations)
            })
            .map_err(|error| {
                let message = error.to_string();
                if message.contains("agent_reincarnation_rejected") {
                    // The tombstone or deletion job changed concurrently;
                    // re-read the job so the typed error carries fresh state.
                    let job = self
                        .runtime_db()
                        .agent_deletions()
                        .latest_for_agent(&agent_id)
                        .ok()
                        .flatten();
                    return named_agent_deletion_incomplete_error(&agent_id, job.as_ref());
                }
                if let Some(name) = normalized_name.as_deref() {
                    if message.contains("UNIQUE constraint failed: agent_identities.name_key") {
                        return named_agent_name_already_exists_error(&agent_id, name);
                    }
                }
                named_agent_create_failed_error(&agent_id, AgentCreateStage::Reserved, error)
            })?;
        self.inner.registry.cache_agent_identity(&identity)?;
        Ok((identity, true))
    }

    fn agent_bootstrap_lock(&self, agent_id: &str) -> Arc<AsyncMutex<()>> {
        self.inner
            .bootstrap_locks
            .lock()
            .expect("agent bootstrap locks poisoned")
            .entry(agent_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    async fn reconcile_agent_bootstrap(&self, agent_id: &str) -> Result<AgentBootstrapRecord> {
        let lock = self.agent_bootstrap_lock(agent_id);
        let _guard = lock.lock().await;
        let identity = self
            .agent_identity_record(agent_id)?
            .ok_or_else(|| anyhow!("agent {agent_id} not found"))?;
        anyhow::ensure!(
            identity.status == AgentRegistryStatus::Active,
            "agent {agent_id} cannot be repaired while it is {:?}",
            identity.status
        );
        let mut bootstrap = self
            .runtime_db()
            .agent_bootstraps()
            .latest(agent_id)?
            .ok_or_else(|| anyhow!("agent {agent_id} has no bootstrap state to repair"))?;

        if bootstrap.template.status != AgentBootstrapStepStatus::Succeeded {
            let result = self.apply_agent_template_bootstrap(&bootstrap).await;
            self.record_agent_bootstrap_result(
                &mut bootstrap,
                AgentBootstrapStepKind::Template,
                result,
            )?;
        }
        if bootstrap.template.status != AgentBootstrapStepStatus::Succeeded {
            return Ok(bootstrap);
        }
        if bootstrap.runtime.status != AgentBootstrapStepStatus::Succeeded {
            let result = async {
                let runtime = self
                    .activate_agent(agent_id, RuntimeActivationReason::AgentLifecycle)
                    .await?;
                runtime.wait_for_bootstrap().await
            }
            .await;
            self.record_agent_bootstrap_result(
                &mut bootstrap,
                AgentBootstrapStepKind::Runtime,
                result,
            )?;
        }
        if bootstrap.runtime.status != AgentBootstrapStepStatus::Succeeded {
            return Ok(bootstrap);
        }
        if bootstrap.workspace.status != AgentBootstrapStepStatus::Succeeded {
            let result = self.apply_agent_workspace_bootstrap(&bootstrap).await;
            self.record_agent_bootstrap_result(
                &mut bootstrap,
                AgentBootstrapStepKind::Workspace,
                result,
            )?;
        }
        if bootstrap.workspace.status != AgentBootstrapStepStatus::Succeeded {
            return Ok(bootstrap);
        }
        if bootstrap.model.status != AgentBootstrapStepStatus::Succeeded {
            let result = self.apply_agent_model_bootstrap(&bootstrap).await;
            self.record_agent_bootstrap_result(
                &mut bootstrap,
                AgentBootstrapStepKind::Model,
                result,
            )?;
        }
        if bootstrap.model.status != AgentBootstrapStepStatus::Succeeded {
            return Ok(bootstrap);
        }
        if bootstrap.initial_message.status != AgentBootstrapStepStatus::Succeeded {
            let result = self.apply_agent_initial_message_bootstrap(&bootstrap).await;
            self.record_agent_bootstrap_result(
                &mut bootstrap,
                AgentBootstrapStepKind::InitialMessage,
                result,
            )?;
        }
        Ok(bootstrap)
    }

    fn record_agent_bootstrap_result(
        &self,
        bootstrap: &mut AgentBootstrapRecord,
        kind: AgentBootstrapStepKind,
        result: Result<()>,
    ) -> Result<()> {
        let now = Utc::now();
        let succeeded = result.is_ok();
        {
            let step = agent_bootstrap_step_mut(bootstrap, kind);
            step.attempts = step.attempts.saturating_add(1);
            step.updated_at = now;
            match result {
                Ok(()) => {
                    step.status = AgentBootstrapStepStatus::Succeeded;
                    step.last_error = None;
                }
                Err(error) => {
                    step.status = AgentBootstrapStepStatus::Failed;
                    step.last_error = Some(bounded_bootstrap_error(&error));
                }
            }
        }
        if succeeded && matches!(kind, AgentBootstrapStepKind::InitialMessage) {
            bootstrap.desired.initial_message = None;
        }
        bootstrap.revision = bootstrap.revision.saturating_add(1);
        bootstrap.updated_at = now;
        self.runtime_db().agent_bootstraps().upsert(bootstrap)
    }

    async fn apply_agent_template_bootstrap(&self, bootstrap: &AgentBootstrapRecord) -> Result<()> {
        let agent_home = self.agent_data_dir(&bootstrap.agent_id);
        let agents_md = agent_home.join("AGENTS.md");
        let expected_selector = bootstrap
            .desired
            .template
            .as_deref()
            .unwrap_or(DEFAULT_AGENT_TEMPLATE_ID);
        let provenance_path = template_provenance_path(&agent_home);
        if provenance_path.is_file() {
            let content = fs::read_to_string(&provenance_path)?;
            let provenance: TemplateProvenanceRecord = serde_json::from_str(&content)?;
            anyhow::ensure!(
                provenance.selector == expected_selector,
                "agent {} template conflict: expected {:?}, found {:?}",
                bootstrap.agent_id,
                expected_selector,
                provenance.selector
            );
            anyhow::ensure!(
                agents_md.is_file(),
                "agent {} template marker exists but AGENTS.md is missing",
                bootstrap.agent_id
            );
            return Ok(());
        }
        anyhow::ensure!(
            !agents_md.exists(),
            "agent {} AGENTS.md already exists without a runtime template marker; repair refuses to overwrite user content",
            bootstrap.agent_id
        );
        let config = self.config();
        let template_home = config
            .user_home_dir
            .as_deref()
            .unwrap_or(config.home_dir.as_path());
        if let Some(template) = bootstrap.desired.template.as_deref() {
            if let Some(catalog_agent_home) = bootstrap.desired.catalog_agent_home.as_deref() {
                ensure_agent_home_agents_md_from_template_with_catalog(
                    &agent_home,
                    template_home,
                    catalog_agent_home,
                    template,
                )
                .await?;
            } else {
                ensure_agent_home_agents_md_from_template_with_home(
                    &agent_home,
                    template_home,
                    template,
                )
                .await?;
            }
        } else {
            ensure_agent_home_agents_md_without_template_with_home(&agent_home, template_home)
                .await?;
        }
        Ok(())
    }

    async fn apply_agent_workspace_bootstrap(
        &self,
        bootstrap: &AgentBootstrapRecord,
    ) -> Result<()> {
        let Some(desired) = bootstrap.desired.workspace.as_ref() else {
            return Ok(());
        };
        let runtime = self.get_or_create_agent(&bootstrap.agent_id).await?;
        let current = runtime.agent_state().await?;
        if current.attached_workspaces == desired.attached_workspaces
            && current.execution_profile == desired.execution_profile
            && current.model_override == desired.inherited_model_override
            && current.model_override_reasoning_effort
                == desired.inherited_model_override_reasoning_effort
        {
            return Ok(());
        }
        let default_state = AgentState::new(bootstrap.agent_id.clone());
        let initial_workspaces = vec![crate::types::agent_home_workspace_id(&bootstrap.agent_id)];
        anyhow::ensure!(
            (current.attached_workspaces == default_state.attached_workspaces
                || current.attached_workspaces == initial_workspaces)
                && current.execution_profile == default_state.execution_profile
                && current.model_override == default_state.model_override
                && current.model_override_reasoning_effort
                    == default_state.model_override_reasoning_effort,
            "agent {} workspace/profile changed after creation; repair refuses to overwrite user state",
            bootstrap.agent_id
        );
        runtime
            .apply_bootstrap_workspace_state(
                desired.attached_workspaces.clone(),
                desired.execution_profile.clone(),
                desired.inherited_model_override.clone(),
                desired.inherited_model_override_reasoning_effort.clone(),
            )
            .await
    }

    async fn apply_agent_model_bootstrap(&self, bootstrap: &AgentBootstrapRecord) -> Result<()> {
        let Some(resolution) = bootstrap.desired.model_resolution.as_ref() else {
            return Ok(());
        };
        let runtime = self.get_or_create_agent(&bootstrap.agent_id).await?;
        let current = runtime.agent_state().await?;
        if resolution.resolution_status == AgentModelResolutionStatus::Inherited {
            return Ok(());
        }
        let provider = crate::config::ProviderId::parse(&resolution.resolved_provider)?;
        let expected = crate::config::ModelRouteRef::from_legacy_model_ref(
            &crate::config::ModelRef::new(provider, resolution.resolved_model.clone()),
        );
        let expected_reasoning_effort = resolution
            .resolved_parameters
            .as_ref()
            .and_then(|parameters| parameters.get("reasoning_effort"))
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if current.model_override.as_ref() == Some(&expected)
            && current.model_override_reasoning_effort == expected_reasoning_effort
        {
            return Ok(());
        }
        let inherited_model = bootstrap
            .desired
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.inherited_model_override.as_ref());
        let inherited_effort = bootstrap
            .desired
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.inherited_model_override_reasoning_effort.as_ref());
        anyhow::ensure!(
            current.model_override.as_ref() == inherited_model
                && current.model_override_reasoning_effort.as_ref() == inherited_effort,
            "agent {} model changed after creation; repair refuses to overwrite user state",
            bootstrap.agent_id
        );
        apply_spawn_model_resolution(&runtime, resolution).await
    }

    async fn apply_agent_initial_message_bootstrap(
        &self,
        bootstrap: &AgentBootstrapRecord,
    ) -> Result<()> {
        let Some(initial) = bootstrap.desired.initial_message.as_ref() else {
            return Ok(());
        };
        let runtime = self.get_or_create_agent(&bootstrap.agent_id).await?;
        let mut message = MessageEnvelope::new(
            bootstrap.agent_id.clone(),
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "spawn_agent".into(),
            },
            initial.authority_class,
            Priority::Normal,
            MessageBody::Text {
                text: initial.text.clone(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        message.id = initial.message_id.clone();
        message.metadata = Some(json!({
            "spawn_preset": AgentProfilePreset::PublicNamed,
            "creator_agent_id": initial.creator_agent_id,
            "spawned_agent_id": bootstrap.agent_id,
            "bootstrap": true,
        }));
        runtime.enqueue(message).await?;
        Ok(())
    }

    pub async fn repair_public_agent(
        &self,
        agent_id: &str,
    ) -> std::result::Result<AgentDetail, PublicAgentError> {
        let identity = self.public_agent_identity(agent_id)?;
        match identity.status {
            AgentRegistryStatus::Active => {}
            AgentRegistryStatus::Deleting => {
                return Err(PublicAgentError::Deleting {
                    agent_id: agent_id.to_string(),
                });
            }
            AgentRegistryStatus::Deleted => {
                return Err(PublicAgentError::Deleted {
                    agent_id: agent_id.to_string(),
                });
            }
        }
        self.reconcile_agent_bootstrap(agent_id)
            .await
            .map_err(PublicAgentError::Runtime)?;
        self.public_agent_detail(agent_id)
    }

    pub fn get_or_create_agent<'a>(
        &'a self,
        agent_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<RuntimeHandle>> + Send + 'a>> {
        self.activate_agent(agent_id, RuntimeActivationReason::AgentLifecycle)
    }

    pub(crate) fn activate_agent<'a>(
        &'a self,
        agent_id: &'a str,
        reason: RuntimeActivationReason,
    ) -> Pin<Box<dyn Future<Output = Result<RuntimeHandle>> + Send + 'a>> {
        Box::pin(async move {
            self.ensure_runtime_recovery_coordinator();
            self.validate_agent_id(agent_id)?;
            if agent_id == self.config().default_agent_id {
                self.ensure_default_agent_identity()?;
            }
            self.active_agent_identity(agent_id)
                .map_err(anyhow::Error::new)?;
            if agent_id == self.config().default_agent_id {
                self.ensure_default_agent_home_initialized().await?;
            }
            loop {
                let mut stale_entry = None;
                let mut failed_runtime_phase = None;
                let mut registry = self.inner.runtimes.write().await;
                if registry.phase != HostRuntimePhase::Open {
                    return Err(anyhow::Error::new(RuntimeAdmissionClosed));
                }
                if let Err(error) = self.active_agent_identity(agent_id) {
                    return Err(anyhow::Error::new(error));
                }
                if let Some(entry) = registry.agents.get(agent_id) {
                    if entry.accepts_host_access() {
                        return Ok(entry.runtime.clone());
                    }
                    if !entry.task.is_finished()
                        && *entry.phase.borrow() == AgentRuntimePhase::FailedCleaning
                    {
                        failed_runtime_phase = Some(entry.phase.clone());
                    } else {
                        stale_entry = registry.agents.remove(agent_id);
                    }
                }
                if let Some(mut phase) = failed_runtime_phase {
                    drop(registry);
                    while *phase.borrow_and_update() != AgentRuntimePhase::Terminated {
                        if phase.changed().await.is_err() {
                            break;
                        }
                    }
                    continue;
                }
                let generation = registry.next_generation;
                registry.next_generation = registry.next_generation.saturating_add(1);
                let (runtime, runtime_task, phase) =
                    self.spawn_runtime(agent_id, Some(generation))?;
                tracing::debug!(
                    agent_id,
                    activation_reason = reason.as_str(),
                    "activating agent runtime"
                );
                registry.agents.insert(
                    agent_id.to_string(),
                    AgentEntry {
                        runtime: runtime.clone(),
                        task: runtime_task,
                        phase,
                        generation,
                    },
                );
                drop(registry);
                if let Some(entry) = stale_entry {
                    let _ = entry.task.await;
                }
                return Ok(runtime);
            }
        })
    }

    pub(crate) fn spawn_temporary_runtime(
        &self,
        category: &str,
    ) -> Result<(String, RuntimeHandle, JoinHandle<()>)> {
        let agent_id = match category {
            "run" => ids::runtime_id(TEMP_RUN_AGENT_PREFIX.trim_end_matches('_')),
            other => ids::runtime_id(&format!("{TEMP_AGENT_PREFIX}{other}")),
        };
        self.validate_agent_id(&agent_id)?;
        let mut identity = AgentIdentityRecord::new(
            agent_id.clone(),
            AgentKind::Named,
            AgentVisibility::Private,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        identity.durability = Some(AgentDurability::Ephemeral);
        let relations =
            independent_creation_records(&identity, None, AgentCanonicalDurability::Ephemeral);
        self.runtime_db()
            .agent_identities()
            .create_with_relations(&identity, &relations)?;
        self.cache_agent_identity(&identity)?;
        let (runtime, runtime_task, _phase) = match self.spawn_runtime(&agent_id, None) {
            Ok(spawned) => spawned,
            Err(error) => {
                let _ = self.archive_temporary_runtime_identity(&agent_id);
                return Err(error);
            }
        };
        Ok((agent_id, runtime, runtime_task))
    }

    pub(crate) fn archive_temporary_runtime_identity(&self, agent_id: &str) -> Result<()> {
        if !Self::is_temporary_agent_id(agent_id) {
            bail!("agent {agent_id} is not a temporary runtime");
        }
        let Some(identity) = self.agent_identity_record(agent_id)? else {
            bail!("temporary runtime {agent_id} is missing its host identity");
        };
        if identity.status != AgentRegistryStatus::Deleted {
            let identity = self
                .runtime_db()
                .agent_identities()
                .tombstone_with_closed_supervision(agent_id)?;
            self.cache_agent_identity(&identity)?;
        }
        Ok(())
    }

    pub fn workspace_entries(&self) -> Result<Vec<WorkspaceEntry>> {
        self.inner.registry.workspace_entries()
    }

    pub fn workspace_occupancies(&self) -> Result<Vec<WorkspaceOccupancyRecord>> {
        self.inner.registry.workspace_occupancies()
    }

    pub fn agent_identity_record(&self, agent_id: &str) -> Result<Option<AgentIdentityRecord>> {
        self.inner.registry.agent_identity_record(agent_id)
    }

    pub(crate) fn agent_identity_records(&self) -> Result<Vec<AgentIdentityRecord>> {
        self.inner.registry.agent_identity_records()
    }

    fn import_legacy_external_triggers(&self) -> Result<()> {
        let mut records = Vec::new();
        for identity in self
            .agent_identity_records()?
            .into_iter()
            .filter(|record| record.status == AgentRegistryStatus::Active)
        {
            let agent_home = self.agent_data_dir(&identity.agent_id);
            if !agent_home.exists() {
                continue;
            }
            let storage = AppStorage::new_for_agent(
                agent_home,
                identity.agent_id.clone(),
                self.runtime_db().clone(),
            )?;
            records.extend(storage.read_recent_external_triggers(usize::MAX)?);
        }
        self.inner
            .runtime_db
            .external_triggers()
            .import_legacy(records)
    }

    #[cfg(test)]
    pub(crate) fn append_agent_identity(&self, record: &AgentIdentityRecord) -> Result<()> {
        self.inner.registry.append_agent_identity(record)
    }

    pub(crate) fn cache_agent_identity(&self, record: &AgentIdentityRecord) -> Result<()> {
        self.inner.registry.cache_agent_identity(record)
    }

    fn workspace_occupancy_by_id(
        &self,
        occupancy_id: &str,
    ) -> Result<Option<WorkspaceOccupancyRecord>> {
        self.inner.registry.workspace_occupancy_by_id(occupancy_id)
    }

    fn acquire_workspace_occupancy(
        &self,
        workspace_id: &str,
        execution_root_id: &str,
        holder_agent_id: &str,
        access_mode: WorkspaceAccessMode,
    ) -> Result<Option<WorkspaceOccupancyRecord>> {
        self.inner.registry.acquire_workspace_occupancy(
            workspace_id,
            execution_root_id,
            holder_agent_id,
            access_mode,
        )
    }

    fn release_workspace_occupancy(
        &self,
        occupancy_id: &str,
    ) -> Result<Option<WorkspaceOccupancyRecord>> {
        self.inner
            .registry
            .release_workspace_occupancy(occupancy_id)
    }

    fn acquire_workspace_cleanup_lease(
        &self,
        execution_root_id: &str,
    ) -> Result<WorkspaceCleanupLeaseGuard> {
        self.inner
            .registry
            .acquire_workspace_cleanup_lease(execution_root_id)
    }

    pub fn ensure_workspace_entry(&self, workspace_anchor: PathBuf) -> Result<WorkspaceEntry> {
        self.inner.registry.ensure_workspace_entry(workspace_anchor)
    }

    pub async fn list_agents(&self) -> Result<Vec<AgentSummary>> {
        self.ensure_default_agent_identity()?;
        let mut summaries = Vec::new();
        for identity in self.agent_identity_records()?.into_iter().filter(|record| {
            record.status == AgentRegistryStatus::Active
                && record.visibility == AgentVisibility::Public
        }) {
            let runtime = self.get_or_create_agent(&identity.agent_id).await?;
            summaries.push(runtime.agent_summary().await?);
        }
        summaries.sort_by(|left, right| left.agent.id.cmp(&right.agent.id));
        Ok(summaries)
    }

    pub async fn list_agent_entries(&self) -> Result<Vec<AgentListEntry>> {
        self.ensure_default_agent_identity()?;
        let mut entries = Vec::new();
        for identity in self.agent_identity_records()?.into_iter().filter(|record| {
            record.status == AgentRegistryStatus::Active
                && record.visibility == AgentVisibility::Public
        }) {
            let runtime = {
                let registry = self.inner.runtimes.read().await;
                registry
                    .agents
                    .get(&identity.agent_id)
                    .filter(|entry| !entry.task.is_finished())
                    .map(|entry| entry.runtime.clone())
            };
            let entry = if let Some(runtime) = runtime {
                runtime.agent_list_entry().await?
            } else {
                self.agent_list_entry_from_storage(&identity)?
            };
            entries.push(entry);
        }
        entries.sort_by(|left, right| left.identity.agent_id.cmp(&right.identity.agent_id));
        Ok(entries)
    }

    pub async fn operator_agent_tree(&self) -> Result<AgentTreeProjection> {
        self.ensure_default_agent_identity()?;
        let identities = self
            .agent_identity_records()?
            .into_iter()
            .filter(|identity| identity.status == AgentRegistryStatus::Active)
            .collect::<Vec<_>>();
        let active_agent_ids = identities
            .iter()
            .map(|identity| identity.agent_id.clone())
            .collect::<HashSet<_>>();
        let mut nodes = HashMap::new();
        let mut children_by_parent = HashMap::<String, Vec<String>>::new();
        let mut root_ids = Vec::new();

        for identity in identities {
            let runtime = {
                let registry = self.inner.runtimes.read().await;
                registry
                    .agents
                    .get(&identity.agent_id)
                    .filter(|entry| !entry.task.is_finished())
                    .map(|entry| entry.runtime.clone())
            };
            let agent = if let Some(runtime) = runtime {
                runtime.agent_list_entry().await?
            } else {
                self.agent_list_entry_from_storage(&identity)?
            };
            let canonical_relations = self
                .runtime_db()
                .agent_canonical_relations()
                .latest(&identity.agent_id)?
                .ok_or_else(|| {
                    anyhow!(
                        "missing canonical relations for active agent {}",
                        identity.agent_id
                    )
                })?;
            let parent_agent_id = canonical_relations
                .lineage
                .as_ref()
                .map(|lineage| lineage.parent_agent_id.clone())
                .filter(|parent_agent_id| active_agent_ids.contains(parent_agent_id));
            if let Some(parent_agent_id) = parent_agent_id {
                children_by_parent
                    .entry(parent_agent_id)
                    .or_default()
                    .push(identity.agent_id.clone());
            } else {
                root_ids.push(identity.agent_id.clone());
            }
            nodes.insert(
                identity.agent_id,
                AgentTreeNode {
                    agent,
                    canonical_relations,
                    children: Vec::new(),
                },
            );
        }

        root_ids.sort();
        for child_ids in children_by_parent.values_mut() {
            child_ids.sort();
        }
        let mut roots = Vec::new();
        for root_id in root_ids {
            if let Some(root) = build_agent_tree_node(
                &root_id,
                &mut nodes,
                &children_by_parent,
                &mut HashSet::new(),
            ) {
                roots.push(root);
            }
        }
        let mut remaining_ids = nodes.keys().cloned().collect::<Vec<_>>();
        remaining_ids.sort();
        for agent_id in remaining_ids {
            if let Some(root) = build_agent_tree_node(
                &agent_id,
                &mut nodes,
                &children_by_parent,
                &mut HashSet::new(),
            ) {
                roots.push(root);
            }
        }
        Ok(AgentTreeProjection { roots })
    }

    fn agent_list_entry_from_storage(
        &self,
        identity: &AgentIdentityRecord,
    ) -> Result<AgentListEntry> {
        let storage = self.agent_storage(&identity.agent_id)?;
        let agent = match storage.read_agent() {
            Ok(Some(agent)) => agent,
            Ok(None) => stopped_unloaded_agent(&identity.agent_id),
            Err(error) => {
                warn!(
                    agent_id = %identity.agent_id,
                    error = %error,
                    "failed to read agent state for /agents/list; using stopped placeholder"
                );
                stopped_unloaded_agent(&identity.agent_id)
            }
        };
        let model = crate::runtime::agent_model_state_for_catalog(
            &RuntimeModelCatalog::from_config(&self.config()),
            &self.runtime_context_config(),
            &agent,
        );
        let scheduling_posture = match storage.agent_posture_projection(&agent) {
            Ok(posture) => posture,
            Err(error) => {
                warn!(
                    agent_id = %identity.agent_id,
                    error = %error,
                    "failed to read agent posture for /agents/list; using unknown placeholder"
                );
                crate::types::AgentPostureProjection::default()
            }
        };
        let waiting_reason = crate::runtime::lightweight_agent_list_waiting_reason(&agent);
        Ok(AgentListEntry {
            identity: AgentIdentityView::from_record(identity, &self.config().default_agent_id),
            lifecycle: AgentLifecycleHint::from_status(&agent.id, agent.status.clone()),
            status: agent.status,
            scheduling_posture,
            pending: agent.pending,
            current_run_id: agent.current_run_id,
            waiting_reason,
            model: (&model).into(),
            active_workspace_entry: agent
                .active_workspace_entry
                .map(crate::types::ActiveWorkspaceEntry::without_projection_metadata),
        })
    }

    /// Authoritative roster snapshot data (S4): membership, per-Agent
    /// committed event windows, and latest canonical Brief anchors from one
    /// committed database read view. All-or-nothing: any per-Agent assembly
    /// failure fails the whole snapshot, and entries reflect committed
    /// canonical state instead of in-memory runtime watchers.
    pub(crate) fn agent_roster_snapshot(&self) -> Result<AgentRosterSnapshotData> {
        let rows = self.runtime_db().agent_roster_snapshot_rows()?;
        let catalog = RuntimeModelCatalog::from_config(&self.config());
        let mut agents = Vec::with_capacity(rows.rows.len());
        for row in rows.rows {
            let identity: AgentIdentityRecord =
                serde_json::from_str(&row.identity_json).map_err(|error| {
                    anyhow!(
                        "unreadable roster identity payload for {}: {error}",
                        row.agent_id
                    )
                })?;
            let agent_state = row
                .agent_state_json
                .as_deref()
                .map(|payload| {
                    serde_json::from_str::<AgentState>(payload).map_err(|error| {
                        anyhow!(
                            "unreadable roster agent state payload for {}: {error}",
                            row.agent_id
                        )
                    })
                })
                .transpose()?;
            let latest_brief = row
                .latest_brief
                .map(|brief| -> Result<AgentRosterLatestBriefData> {
                    Ok(AgentRosterLatestBriefData {
                        brief_id: brief.brief_id,
                        created_event_seq: brief
                            .created_event_seq
                            .map(|seq| {
                                u64::try_from(seq).map_err(|_| {
                                    anyhow!("negative latest brief linkage for {}", row.agent_id)
                                })
                            })
                            .transpose()?,
                        created_at: chrono::DateTime::parse_from_rfc3339(&brief.created_at)
                            .map(|parsed| parsed.with_timezone(&Utc))
                            .map_err(|error| {
                                anyhow!(
                                    "unreadable latest brief timestamp for {}: {error}",
                                    row.agent_id
                                )
                            })?,
                        preview: brief_preview(&brief.preview),
                    })
                })
                .transpose()?;
            let agent =
                self.agent_list_entry_from_committed_state(&identity, agent_state, &catalog)?;
            agents.push(AgentRosterEntryData {
                agent,
                event_head_seq: row.event_head_seq,
                oldest_retained_seq: row.oldest_retained_seq,
                latest_brief,
            });
        }
        Ok(AgentRosterSnapshotData {
            runtime_id: rows.runtime_id,
            event_log_epoch: rows.event_log_epoch,
            visibility_policy_generation: rows.visibility_policy_generation,
            agents,
        })
    }

    /// Per-Agent canonical projection snapshot data (S5) assembled from one
    /// committed read view. Returns `None` when the Agent is not an active
    /// public member, so unknown, private, and deleted identities stay
    /// indistinguishable. Assembly is all-or-nothing: an unreadable anchor
    /// fails the whole request instead of substituting placeholder facts.
    pub(crate) fn agent_projection_snapshot(
        &self,
        agent_id: &str,
    ) -> Result<Option<AgentProjectionSnapshotData>> {
        let rows = self.runtime_db().agent_projection_snapshot_rows(agent_id)?;
        let row = match rows.row {
            Some(row) => row,
            None => return Ok(None),
        };
        let identity: AgentIdentityRecord =
            serde_json::from_str(&row.identity_json).map_err(|error| {
                anyhow!(
                    "unreadable projection identity payload for {}: {error}",
                    row.agent_id
                )
            })?;
        let agent_state = row
            .agent_state_json
            .as_deref()
            .map(|payload| {
                serde_json::from_str::<AgentState>(payload).map_err(|error| {
                    anyhow!(
                        "unreadable projection agent state payload for {}: {error}",
                        row.agent_id
                    )
                })
            })
            .transpose()?;
        let catalog = RuntimeModelCatalog::from_config(&self.config());
        let agent = self.agent_list_entry_from_committed_state(&identity, agent_state, &catalog)?;
        let current_work_item = row
            .current_work_item
            .map(|work_item| -> Result<AgentWorkItemAnchorData> {
                Ok(AgentWorkItemAnchorData {
                    work_item_id: work_item.work_item_id,
                    state: crate::runtime_db::observer_sync::parse_work_item_state(
                        &work_item.state,
                    )?,
                    // A stored NULL is the pre-plan state; the wire anchor
                    // is non-optional and maps it to the draft baseline.
                    plan_status: work_item
                        .plan_status
                        .as_deref()
                        .map(crate::runtime_db::observer_sync::parse_work_item_plan_status)
                        .transpose()?
                        .unwrap_or(crate::types::WorkItemPlanStatus::Draft),
                    revision: u64::try_from(work_item.revision)
                        .map_err(|_| anyhow!("negative work item revision for {}", row.agent_id))?,
                    updated_at: chrono::DateTime::parse_from_rfc3339(&work_item.updated_at)
                        .map(|parsed| parsed.with_timezone(&Utc))
                        .map_err(|error| {
                            anyhow!(
                                "unreadable work item timestamp for {}: {error}",
                                row.agent_id
                            )
                        })?,
                })
            })
            .transpose()?;
        let latest_brief = row
            .latest_brief
            .map(|brief| -> Result<AgentRosterLatestBriefData> {
                Ok(AgentRosterLatestBriefData {
                    brief_id: brief.brief_id,
                    created_event_seq: brief
                        .created_event_seq
                        .map(|seq| {
                            u64::try_from(seq).map_err(|_| {
                                anyhow!("negative latest brief linkage for {}", row.agent_id)
                            })
                        })
                        .transpose()?,
                    created_at: chrono::DateTime::parse_from_rfc3339(&brief.created_at)
                        .map(|parsed| parsed.with_timezone(&Utc))
                        .map_err(|error| {
                            anyhow!(
                                "unreadable latest brief timestamp for {}: {error}",
                                row.agent_id
                            )
                        })?,
                    preview: brief_preview(&brief.preview),
                })
            })
            .transpose()?;
        // Hydration references mirror the anchors the projection itself
        // names; each id resolves through the per-family batch record API.
        let mut hydration_references = Vec::new();
        if let Some(message_id) = row.latest_message_id.clone() {
            hydration_references.push(AgentHydrationReferenceData {
                record_kind: ObserverSyncRecordKindData::Message,
                record_id: message_id,
            });
        }
        if let Some(transcript_entry_id) = row.latest_transcript_entry_id.clone() {
            hydration_references.push(AgentHydrationReferenceData {
                record_kind: ObserverSyncRecordKindData::TranscriptEntry,
                record_id: transcript_entry_id,
            });
        }
        if let Some(brief) = latest_brief.as_ref() {
            hydration_references.push(AgentHydrationReferenceData {
                record_kind: ObserverSyncRecordKindData::Brief,
                record_id: brief.brief_id.clone(),
            });
        }
        Ok(Some(AgentProjectionSnapshotData {
            runtime_id: rows.runtime_id,
            event_log_epoch: rows.event_log_epoch,
            visibility_policy_generation: rows.visibility_policy_generation,
            agent_id: row.agent_id,
            snapshot_through_seq: row.event_head_seq,
            event_head_seq: row.event_head_seq,
            oldest_retained_seq: row.oldest_retained_seq,
            agent,
            canonical_relations: row.canonical_relations,
            current_work_item,
            conversation: ConversationRevisionAnchorsData {
                latest_message_id: row.latest_message_id,
                latest_transcript_entry_id: row.latest_transcript_entry_id,
            },
            latest_brief,
            hydration_references,
        }))
    }

    /// Builds one roster `AgentListEntry` from the committed identity and
    /// AgentState captured by the snapshot read view. Unlike
    /// `agent_list_entry_from_storage`, read failures propagate: the roster
    /// is all-or-nothing and never substitutes placeholder facts. A member
    /// with no committed state yet keeps the stopped placeholder semantics
    /// it would also get from `/agents/list`.
    fn agent_list_entry_from_committed_state(
        &self,
        identity: &AgentIdentityRecord,
        agent_state: Option<AgentState>,
        catalog: &RuntimeModelCatalog,
    ) -> Result<AgentListEntry> {
        let storage = self.agent_storage_read_only(&identity.agent_id)?;
        let agent = match agent_state {
            Some(agent) => agent,
            None => stopped_unloaded_agent(&identity.agent_id),
        };
        let model = crate::runtime::agent_model_state_for_catalog(
            catalog,
            &self.runtime_context_config(),
            &agent,
        );
        let scheduling_posture = storage.agent_posture_projection(&agent)?;
        let waiting_reason = crate::runtime::lightweight_agent_list_waiting_reason(&agent);
        Ok(AgentListEntry {
            identity: AgentIdentityView::from_record(identity, &self.config().default_agent_id),
            lifecycle: AgentLifecycleHint::from_status(&agent.id, agent.status.clone()),
            status: agent.status,
            scheduling_posture,
            pending: agent.pending,
            current_run_id: agent.current_run_id,
            waiting_reason,
            model: (&model).into(),
            active_workspace_entry: agent
                .active_workspace_entry
                .map(crate::types::ActiveWorkspaceEntry::without_projection_metadata),
        })
    }

    pub async fn public_agent_activity_snapshots(
        &self,
    ) -> Result<Vec<PublicAgentActivitySnapshot>> {
        self.ensure_default_agent_identity()?;
        let mut snapshots = Vec::new();
        for identity in self.agent_identity_records()?.into_iter().filter(|record| {
            record.status == AgentRegistryStatus::Active
                && record.visibility == AgentVisibility::Public
        }) {
            if let Some(runtime) = self.try_get_loaded_runtime(&identity.agent_id).await {
                let state = runtime.agent_state().await?;
                let active_task_count = runtime.active_tasks(usize::MAX).await?.len();
                snapshots.push(PublicAgentActivitySnapshot {
                    agent_id: identity.agent_id,
                    status: state.status.clone(),
                    active_task_count,
                    last_runtime_failure: state.last_runtime_failure,
                });
                continue;
            }
            let storage = self.agent_storage(&identity.agent_id)?;
            let state = storage
                .read_agent()?
                .unwrap_or_else(|| AgentState::new(identity.agent_id.clone()));
            let active_task_count = storage.active_task_count_for_agent(&identity.agent_id)?;
            snapshots.push(PublicAgentActivitySnapshot {
                agent_id: identity.agent_id,
                status: state.status.clone(),
                active_task_count,
                last_runtime_failure: state.last_runtime_failure,
            });
        }
        snapshots.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        Ok(snapshots)
    }

    pub async fn preview_public_agent_prompt(
        &self,
        agent_id: &str,
        text: String,
        authority_class: AuthorityClass,
        budget_override: Option<usize>,
    ) -> std::result::Result<EffectivePrompt, PublicAgentError> {
        let identity = self.public_agent_identity(agent_id)?;
        self.preview_agent_prompt_from_storage(&identity, text, authority_class, budget_override)
            .await
            .map_err(PublicAgentError::Runtime)
    }

    pub async fn preview_agent_prompt(
        &self,
        agent_id: &str,
        text: String,
        authority_class: AuthorityClass,
    ) -> Result<EffectivePrompt> {
        self.validate_agent_id(agent_id)?;
        let identity = if agent_id == self.config().default_agent_id {
            self.ensure_default_agent_identity()?;
            self.agent_identity_record(agent_id)?.ok_or_else(|| {
                anyhow!(
                    "default agent {} identity missing after initialization",
                    agent_id
                )
            })?
        } else {
            self.agent_identity_record(agent_id)?.ok_or_else(|| {
                anyhow!(
                    "agent {} not found; create it first with 'holon agent create {}'",
                    agent_id,
                    agent_id
                )
            })?
        };
        self.active_agent_identity(agent_id)
            .map_err(anyhow::Error::new)?;
        self.preview_agent_prompt_from_storage(&identity, text, authority_class, None)
            .await
    }

    pub async fn preview_agent_prompt_with_budget(
        &self,
        agent_id: &str,
        text: String,
        authority_class: AuthorityClass,
        budget: usize,
    ) -> Result<EffectivePrompt> {
        self.validate_agent_id(agent_id)?;
        let identity = self.agent_identity_record(agent_id)?.ok_or_else(|| {
            anyhow!(
                "agent {agent_id} not found; create it first with 'holon agent create {agent_id}'"
            )
        })?;
        self.active_agent_identity(agent_id)
            .map_err(anyhow::Error::new)?;
        self.preview_agent_prompt_from_storage(&identity, text, authority_class, Some(budget))
            .await
    }

    pub fn public_agent_boundary_metadata(
        &self,
        agent_id: &str,
    ) -> std::result::Result<Value, PublicAgentError> {
        let identity = self.public_agent_identity(agent_id)?;
        self.agent_boundary_metadata_from_storage(&identity)
            .map_err(PublicAgentError::Runtime)
    }

    async fn preview_agent_prompt_from_storage(
        &self,
        identity: &AgentIdentityRecord,
        text: String,
        authority_class: AuthorityClass,
        budget_override: Option<usize>,
    ) -> Result<EffectivePrompt> {
        let storage = self.agent_storage_read_only(&identity.agent_id)?;
        let state = storage
            .read_agent()?
            .unwrap_or_else(|| AgentState::new(identity.agent_id.clone()));
        let identity_view =
            AgentIdentityView::from_record(identity, &self.config().default_agent_id);
        let message = MessageEnvelope::new(
            identity.agent_id.clone(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("debug_prompt".into()),
                actor_display_name: None,
            },
            authority_class,
            Priority::Normal,
            MessageBody::Text { text },
        )
        .with_admission(
            MessageDeliverySurface::CliPrompt,
            AdmissionContext::LocalProcess,
        );
        let workspace = crate::runtime::workspace::workspace_view_from_state(
            &state,
            storage.data_dir().to_path_buf(),
        )?;
        let execution = crate::runtime::workspace::build_effective_execution(
            &storage,
            ExecutionScopeKind::AgentTurn,
            state.execution_profile.clone(),
            workspace,
            &state.attached_workspaces,
        )
        .snapshot();
        let agent_home = self.agent_data_dir(&identity.agent_id);
        let config = self.config();
        let loaded_agents_md = load_agents_md(
            config.user_home_dir.as_deref(),
            agent_home.as_path(),
            crate::runtime::workspace::workspace_anchor_for_state_ref(&state),
        )?;
        let loaded_agent_memory = load_agent_memory(agent_home.as_path())?;
        let skill_visibility = skill_visibility(&identity_view);
        let workspace_skill_root = state
            .active_workspace_entry
            .as_ref()
            .map(|entry| entry.execution_root.as_path());
        let skill_roots = effective_skill_root_registrations(
            skill_visibility,
            config.user_home_dir.as_deref(),
            &state.id,
            agent_home.as_path(),
            workspace_skill_root,
        );
        let mut skill_registry = self.inner.skills_registry.write().await;
        skill_registry.sync_effective_roots(skill_roots.clone())?;
        let mut skills = skills_runtime_view_from_catalog(
            skill_registry.catalog_for_roots(&skill_roots, None),
            &skill_roots,
            &state.active_skills,
        );
        skills.agent_templates_catalog =
            discover_agent_templates_catalog(config.user_home_dir.as_deref(), agent_home.as_path());
        let model_catalog = RuntimeModelCatalog::from_config(&config);
        let model_ref = model_catalog
            .provider_chain(state.model_override.as_ref())
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                crate::runtime::agent_model_state_for_catalog(
                    &model_catalog,
                    &self.runtime_context_config(),
                    &state,
                )
                .effective_model
            });
        let provider = self
            .inner
            .static_provider
            .clone()
            .map(Ok)
            .unwrap_or_else(|| build_provider_from_config(&config))?;
        let apply_patch_surface = ApplyPatchSurface::for_model_route_ref(&model_ref.as_string());
        let registry = ToolRegistry::new(execution.execution_root.clone());
        let capability_policy = self
            .runtime_db()
            .agent_canonical_relations()
            .latest(&identity.agent_id)?
            .and_then(|relations| relations.capability_policy);
        let available_tools = registry
            .tool_specs_with_families_for_apply_patch_surface(apply_patch_surface)?
            .into_iter()
            .filter(|(family, _)| {
                capability_policy
                    .as_ref()
                    .is_none_or(|policy| policy.allows(*family))
            })
            .map(|(_, tool)| tool)
            .collect::<Vec<_>>();
        let prompt_tools = provider.prompt_tool_specs(&available_tools);
        let mut context_config = self.runtime_context_config();
        if let Some(budget) = budget_override {
            context_config.prompt_budget_estimated_tokens = budget;
        }
        build_effective_prompt_with_apply_patch_surface(
            &storage,
            &state,
            &execution,
            &message,
            &context_config,
            &execution.execution_root,
            agent_home.as_path(),
            &identity_view,
            loaded_agents_md,
            loaded_agent_memory,
            &skills,
            &prompt_tools,
            apply_patch_surface,
            None,
        )
    }

    fn agent_boundary_metadata_from_storage(
        &self,
        identity: &AgentIdentityRecord,
    ) -> Result<Value> {
        let storage = self.agent_storage_read_only(&identity.agent_id)?;
        let state = storage
            .read_agent()?
            .unwrap_or_else(|| AgentState::new(identity.agent_id.clone()));
        let workspace = crate::runtime::workspace::workspace_view_from_state(
            &state,
            storage.data_dir().to_path_buf(),
        )?;
        let execution = crate::runtime::workspace::build_effective_execution(
            &storage,
            ExecutionScopeKind::AgentTurn,
            state.execution_profile.clone(),
            workspace,
            &state.attached_workspaces,
        );
        Ok(HostLocalBoundary::from_snapshot(&execution.snapshot()).audit_metadata())
    }

    pub async fn child_agent_summaries(
        &self,
        parent_agent_id: &str,
    ) -> Result<Vec<ChildAgentSummary>> {
        let mut children = Vec::new();
        for identity in self.agent_identity_records()?.into_iter().filter(|record| {
            record.status == AgentRegistryStatus::Active
                && record.kind == AgentKind::Child
                && record.parent_agent_id.as_deref() == Some(parent_agent_id)
        }) {
            let storage = self.agent_storage_read_only(&identity.agent_id)?;
            let state = storage
                .read_agent()?
                .unwrap_or_else(|| AgentState::new(identity.agent_id.clone()));
            let active_task_count = storage.active_task_count_for_agent(&identity.agent_id)?;
            children.push(ChildAgentSummary {
                identity: AgentIdentityView::from_record(
                    &identity,
                    &self.config().default_agent_id,
                ),
                status: state.status.clone(),
                current_run_id: state.current_run_id.clone(),
                pending: state.pending,
                active_task_count,
                observability: self
                    .child_agent_observability_snapshot(&identity.agent_id, &storage, &state)
                    .await?,
            });
        }
        children.sort_by(|left, right| left.identity.agent_id.cmp(&right.identity.agent_id));
        Ok(children)
    }

    async fn child_agent_observability_snapshot(
        &self,
        agent_id: &str,
        storage: &AppStorage,
        state: &AgentState,
    ) -> Result<crate::types::ChildAgentObservabilitySnapshot> {
        if let Some(runtime) = self.try_get_loaded_runtime(agent_id).await {
            return runtime.child_agent_observability().await;
        }
        RuntimeHandle::child_agent_observability_from_storage(storage, state)
    }

    pub async fn resolve_external_trigger(
        &self,
        callback_token: &str,
    ) -> Result<Option<(RuntimeHandle, ExternalTriggerRecord)>> {
        let Some((agent_id, descriptor)) =
            self.resolve_external_trigger_record(callback_token).await?
        else {
            return Ok(None);
        };
        let runtime = self.get_or_create_agent(&agent_id).await?;
        Ok(Some((runtime, descriptor)))
    }

    pub async fn resolve_external_trigger_record(
        &self,
        callback_token: &str,
    ) -> Result<Option<(String, ExternalTriggerRecord)>> {
        let token_hash = hash_callback_token(callback_token);
        if let Some(descriptor) = self
            .inner
            .runtime_db
            .external_triggers()
            .active_by_token_hash(&token_hash)?
        {
            return Ok(Some((descriptor.target_agent_id.clone(), descriptor)));
        }
        Ok(None)
    }

    fn ensure_default_agent_identity(&self) -> Result<AgentIdentityRecord> {
        self.inner.registry.ensure_default_agent_identity()
    }

    fn ensure_legacy_public_agent_bootstraps(&self) -> Result<()> {
        for identity in self.inner.registry.agent_identity_records()? {
            if identity.kind != AgentKind::Named
                || identity.visibility != AgentVisibility::Public
                || identity.ownership() != AgentOwnership::SelfOwned
                || self
                    .runtime_db()
                    .agent_bootstraps()
                    .latest(&identity.agent_id)?
                    .is_some()
            {
                continue;
            }
            self.runtime_db()
                .agent_bootstraps()
                .upsert(&AgentBootstrapRecord::legacy_ready(&identity.agent_id))?;
        }
        Ok(())
    }

    async fn ensure_default_agent_home_initialized(&self) -> Result<()> {
        let config = self.config();
        let agent_home = self.agent_data_dir(&config.default_agent_id);
        let template_home = config
            .user_home_dir
            .as_deref()
            .unwrap_or(config.home_dir.as_path());
        let _ = ensure_agent_home_agents_md_without_template_with_home(&agent_home, template_home)
            .await?;
        Ok(())
    }

    async fn create_child_identity(
        &self,
        parent_agent_id: &str,
        task: &TaskRecord,
        template: Option<&str>,
        catalog_agent_home: &Path,
    ) -> Result<AgentIdentityRecord> {
        let child_agent_id = ids::runtime_id(TEMP_CHILD_AGENT_PREFIX.trim_end_matches('_'));
        self.validate_agent_id(&child_agent_id)?;
        let config = self.config();
        let template_home = config
            .user_home_dir
            .as_deref()
            .unwrap_or(config.home_dir.as_path());
        if let Some(template) = template {
            initialize_agent_home_from_template_with_catalog(
                &self.agent_data_dir(&child_agent_id),
                template_home,
                catalog_agent_home,
                template,
            )
            .await?;
        } else {
            initialize_agent_home_without_template_with_home(
                &self.agent_data_dir(&child_agent_id),
                template_home,
            )
            .await?;
        }
        let mut record = AgentIdentityRecord::new(
            child_agent_id,
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(parent_agent_id.to_string()),
            Some(task.id.clone()),
        )
        .with_lineage_parent_agent_id(Some(parent_agent_id.to_string()));
        record.durability = Some(AgentDurability::Ephemeral);
        let relations = supervised_creation_records(
            &record,
            parent_agent_id,
            &task.id,
            task.effective_work_item_id(),
        );
        self.runtime_db()
            .agent_identities()
            .create_with_relations(&record, &relations)?;
        self.inner.registry.cache_agent_identity(&record)?;
        Ok(record)
    }

    async fn archive_private_agent(&self, agent_id: &str) -> Result<()> {
        if let Some(identity) = self.agent_identity_record(agent_id)? {
            if identity.status != AgentRegistryStatus::Deleted {
                let identity = self
                    .runtime_db()
                    .agent_identities()
                    .tombstone_with_closed_supervision(agent_id)?;
                self.cache_agent_identity(&identity)?;
            }
        }

        let entry = self.inner.runtimes.write().await.agents.remove(agent_id);
        if let Some(entry) = entry {
            let _ = entry
                .runtime
                .control(crate::types::ControlAction::Stop)
                .await;
            let _ = entry.task.await;
        }
        // Cancel any active wait conditions before removing the data directory.
        // This produces audit events and avoids orphaned active waits.
        let now = chrono::Utc::now();
        if let Ok(storage) = self.agent_storage(agent_id) {
            if let Ok(active) = storage.active_wait_conditions_for_agent(agent_id) {
                let mut cancelled_ids = Vec::new();
                for condition in active {
                    let mut cancelled = condition.clone();
                    cancelled.status = crate::types::WaitConditionStatus::Cancelled;
                    cancelled.updated_at = now;
                    cancelled.cancelled_at = Some(now);
                    if storage.append_wait_condition(&cancelled).is_ok() {
                        cancelled_ids.push(condition.id);
                    }
                }
                if !cancelled_ids.is_empty() {
                    let _ = storage.append_event(&crate::types::AuditEvent::legacy(
                        "wait_conditions_cancelled",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "reason": "agent_archived",
                            "wait_condition_ids": cancelled_ids,
                        }),
                    ));
                }
            }
        }
        // Abort pending queue entries to prevent orphaned queued messages
        // that would never be consumed after the agent is removed.
        if let Ok(aborted_count) = self
            .runtime_db()
            .queue_entries()
            .abort_pending_for_agent(agent_id)
        {
            if aborted_count > 0 {
                if let Ok(storage) = self.agent_storage(agent_id) {
                    let _ = storage.append_event(&crate::types::AuditEvent::legacy(
                        "queue_entries_aborted",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "reason": "agent_archived",
                            "count": aborted_count,
                        }),
                    ));
                }
            }
        }

        let data_dir = self.agent_data_dir(agent_id);
        if data_dir.exists() {
            fs::remove_dir_all(&data_dir)?;
        }
        Ok(())
    }

    fn converge_private_child_identities(&self) -> Result<()> {
        for identity in self.agent_identity_records()?.into_iter() {
            if !self.should_archive_private_child_identity(&identity)? {
                continue;
            }
            self.archive_private_agent_identity_record(&identity.agent_id)?;
        }
        Ok(())
    }

    fn should_archive_private_child_identity(
        &self,
        identity: &AgentIdentityRecord,
    ) -> Result<bool> {
        if identity.status != AgentRegistryStatus::Active
            || identity.visibility != AgentVisibility::Private
            || identity.ownership() != AgentOwnership::ParentSupervised
            || identity.kind != AgentKind::Child
        {
            return Ok(false);
        }

        let data_dir = self.agent_data_dir(&identity.agent_id);
        if !data_dir.exists() {
            return Ok(true);
        }

        let Some(parent_agent_id) = identity.parent_agent_id.as_deref() else {
            return Ok(true);
        };
        let Some(parent_identity) = self.agent_identity_record(parent_agent_id)? else {
            return Ok(true);
        };
        if parent_identity.status != AgentRegistryStatus::Active {
            return Ok(true);
        }

        let Some(task_id) = identity.delegated_from_task_id.as_deref() else {
            return Ok(true);
        };
        if !self.agent_data_dir(parent_agent_id).exists() {
            return Ok(true);
        }
        let parent_storage = self.agent_storage(parent_agent_id)?;
        let Some(task) = parent_storage.latest_task_record(task_id)? else {
            return Ok(true);
        };

        if task.kind == TaskKind::ActorInvocation {
            return Ok(false);
        }

        Ok(matches!(
            task.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        ))
    }

    fn archive_private_agent_identity_record(&self, agent_id: &str) -> Result<()> {
        if let Some(identity) = self.agent_identity_record(agent_id)? {
            if identity.status != AgentRegistryStatus::Deleted {
                let identity = self
                    .runtime_db()
                    .agent_identities()
                    .tombstone_with_closed_supervision(agent_id)?;
                self.cache_agent_identity(&identity)?;
            }
        }
        // Abort pending queue entries as a safety net for agents that were
        // stopped through a different path (e.g. data dir already removed).
        let _ = self
            .runtime_db()
            .queue_entries()
            .abort_pending_for_agent(agent_id)?;

        let data_dir = self.agent_data_dir(agent_id);
        if data_dir.exists() {
            fs::remove_dir_all(&data_dir)?;
        }
        Ok(())
    }

    async fn stop_private_agent(&self, agent_id: &str) -> Result<()> {
        self.archive_private_agent(agent_id).await
    }

    async fn spawn_child_task(
        &self,
        parent_runtime: RuntimeHandle,
        task: &TaskRecord,
        prompt: String,
        authority_class: AuthorityClass,
        worktree: bool,
        template: Option<String>,
        model_resolution: AgentModelResolution,
    ) -> Result<ChildTaskSpawn> {
        let parent_state = parent_runtime.agent_state().await?;
        let parent_agent_home = self.agent_data_dir(&parent_state.id);
        let child_identity = self
            .create_child_identity(
                &parent_state.id,
                task,
                template.as_deref(),
                &parent_agent_home,
            )
            .await?;
        let child_runtime = self.get_or_create_agent(&child_identity.agent_id).await?;
        child_runtime
            .inherit_from_parent_state(&parent_state)
            .await?;
        apply_spawn_model_resolution(&child_runtime, &model_resolution).await?;
        let child_turn_baseline = child_runtime.agent_state().await?.turn_index;

        let mut task_detail = json!({
            "child_agent_id": child_identity.agent_id,
            "child_turn_baseline": child_turn_baseline,
            "child_kind": AgentKind::Child,
            "child_visibility": AgentVisibility::Private,
            "child_ownership": AgentOwnership::ParentSupervised,
            "child_profile_preset": AgentProfilePreset::PrivateChild,
            "wait_policy": task.wait_policy(),
            "workspace_mode": if worktree { "worktree" } else { "inherit" },
            "model_resolution": model_resolution,
        });

        if worktree {
            let seed = parent_runtime
                .prepare_managed_worktree_for_task(&task.id)
                .await?;
            parent_runtime
                .storage()
                .append_transcript_entry(&TranscriptEntry::new(
                    parent_state.id.clone(),
                    TranscriptEntryKind::SubagentPrompt,
                    None,
                    None,
                    json!({
                        "prompt": prompt,
                        "authority_class": authority_class,
                        "task_id": task.id,
                        "workspace_root": seed.worktree_path,
                    }),
                ))?;
            child_runtime
                .enter_worktree(
                    seed.original_cwd.clone(),
                    seed.original_branch.clone(),
                    seed.worktree_path.clone(),
                    seed.worktree_branch.clone(),
                )
                .await?;
            task_detail["worktree"] = json!({
                "worktree_path": seed.worktree_path,
                "worktree_branch": seed.worktree_branch,
            });
        }

        let mut message = crate::types::MessageEnvelope::new(
            child_identity.agent_id.clone(),
            crate::types::MessageKind::InternalFollowup,
            crate::types::MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            authority_class.clone(),
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text { text: prompt },
        )
        .with_admission(
            crate::types::MessageDeliverySurface::RuntimeSystem,
            crate::types::AdmissionContext::RuntimeOwned,
        );
        message.metadata = Some(json!({
            "spawn_preset": AgentProfilePreset::PrivateChild,
            "delegated_task_id": task.id,
            "supervision_task_id": task.id,
            "parent_agent_id": parent_state.id,
            "child_agent_id": child_identity.agent_id,
            "parent_supervised": true,
            "delegated_authority_class": authority_class,
        }));
        child_runtime.enqueue(message).await?;

        Ok(ChildTaskSpawn {
            child_agent_id: child_identity.agent_id,
            child_turn_baseline,
            delivery_id: None,
            task_detail,
        })
    }

    async fn invoke_existing_agent(
        &self,
        task: &TaskRecord,
        target_agent_id: &str,
        message_text: String,
        authority_class: AuthorityClass,
    ) -> Result<ChildTaskSpawn> {
        let canonical_relations = self
            .runtime_db()
            .agent_canonical_relations()
            .latest(target_agent_id)?;
        let active_supervision = canonical_relations
            .as_ref()
            .and_then(|relations| relations.supervision.as_ref())
            .filter(|supervision| {
                supervision.supervisor_agent_id == task.agent_id
                    && matches!(
                        supervision.state,
                        AgentSupervisionState::Active | AgentSupervisionState::CleanupRequired
                    )
            });
        let (principal_kind, route) = if active_supervision.is_some() {
            (
                AgentMessagePrincipalKind::SupervisingParent,
                "supervision_follow_up",
            )
        } else {
            (AgentMessagePrincipalKind::PeerAgent, "agent_invocation")
        };
        let caller = AgentMessageCallerContext {
            caller_principal: format!("agent:{}", task.agent_id),
            caller_agent_id: Some(task.agent_id.clone()),
            principal_kind,
            route: route.into(),
            origin: MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            authority_class,
            delivery_surface: MessageDeliverySurface::RuntimeSystem,
            admission_context: AdmissionContext::RuntimeOwned,
            current_turn_id: task
                .detail
                .as_ref()
                .and_then(|detail| detail.get("parent_turn_id"))
                .and_then(Value::as_str)
                .map(str::to_string),
            current_task_id: Some(task.id.clone()),
            current_work_item_id: task.work_item_id.clone(),
        };
        let prepared = crate::runtime::AgentMessageDeliveryService::prepare(
            AgentMessageSendRequest {
                target_agent_id: target_agent_id.to_string(),
                content: MessageBody::Text { text: message_text },
                client_idempotency_key: task.id.clone(),
                correlation_id: Some(task.id.clone()),
                causation_id: task.parent_message_id.clone(),
                requested_priority: Some(Priority::Normal),
            },
            caller,
        )?;
        let identity = self.agent_identity_record(target_agent_id)?;
        let (identity, runtime, child_turn_baseline, receipt) = match identity {
            Some(identity) if identity.status == AgentRegistryStatus::Active => {
                match self.get_or_create_agent(target_agent_id).await {
                    Ok(runtime) => {
                        let child_turn_baseline = runtime.agent_state().await?.turn_index;
                        let receipt = runtime
                            .agent_message_delivery_service()
                            .deliver(&prepared)
                            .await?;
                        (
                            Some(identity),
                            Some(runtime),
                            Some(child_turn_baseline),
                            receipt,
                        )
                    }
                    Err(activation_error) => {
                        let latest_identity = self.agent_identity_record(target_agent_id)?;
                        if latest_identity
                            .as_ref()
                            .is_none_or(|latest| latest.status != AgentRegistryStatus::Active)
                        {
                            let receipt = self
                                .runtime_db()
                                .agent_message_deliveries()
                                .admit_without_queue(&prepared.record)?;
                            (
                                Some(latest_identity.unwrap_or(identity)),
                                None,
                                None,
                                receipt,
                            )
                        } else {
                            return Err(activation_error);
                        }
                    }
                }
            }
            _ => {
                let receipt = self
                    .runtime_db()
                    .agent_message_deliveries()
                    .admit_without_queue(&prepared.record)?;
                (identity, None, None, receipt)
            }
        };
        if receipt.outcome != AgentMessageDeliveryOutcome::Accepted {
            if matches!(
                receipt.rejection_code,
                Some(
                    AgentMessageDeliveryRejectionCode::TargetNotFound
                        | AgentMessageDeliveryRejectionCode::MessageNotAuthorized
                )
            ) {
                return Err(anyhow!(RuntimeError::not_found(
                    "agent_target_unavailable",
                    "agent target was not found or is not available to this caller",
                )
                .with_safe_context("task_id", &task.id)
                .with_recovery_hint(
                    "use an agent id already available through the caller's authorized agent context",
                )));
            }
            return Err(anyhow!(
                "agent message delivery {} was rejected: {:?}",
                receipt.delivery_id,
                receipt.rejection_code
            ));
        }
        let identity = identity.ok_or_else(|| {
            anyhow!(RuntimeError::not_found(
                "agent_target_unavailable",
                "agent target was not found or is not available to this caller",
            )
            .with_safe_context("task_id", &task.id)
            .with_recovery_hint(
                "use an agent id already available through the caller's authorized agent context",
            ))
        })?;
        runtime.ok_or_else(|| {
            anyhow!(
                "accepted delivery {} targets an inactive agent",
                receipt.delivery_id
            )
        })?;
        let child_turn_baseline = child_turn_baseline.ok_or_else(|| {
            anyhow!(
                "accepted delivery {} is missing its pre-delivery turn baseline",
                receipt.delivery_id
            )
        })?;

        let mut task_detail = json!({
            "target_agent_id": target_agent_id,
            "target_agent_kind": identity.kind,
            "target_agent_visibility": identity.visibility,
            "target_agent_ownership": identity.ownership(),
            "target_agent_profile_preset": identity.profile_preset(),
            "child_turn_baseline": child_turn_baseline,
            "created_new_subagent": false,
            "delivery_id": receipt.delivery_id,
            "delivery_state": receipt.state,
        });
        if identity.kind == AgentKind::Child {
            task_detail["child_agent_id"] = json!(target_agent_id);
        }
        Ok(ChildTaskSpawn {
            child_agent_id: target_agent_id.to_string(),
            child_turn_baseline,
            delivery_id: Some(receipt.delivery_id),
            task_detail,
        })
    }

    #[cfg(test)]
    async fn spawn_public_named_agent(
        &self,
        parent_runtime: RuntimeHandle,
        agent_id: &str,
        initial_message: Option<String>,
        authority_class: AuthorityClass,
        template: Option<String>,
        model_resolution: AgentModelResolution,
    ) -> Result<AgentCreateResult> {
        let parent_state = parent_runtime.agent_state().await?;
        let parent_agent_home = self.agent_data_dir(&parent_state.id);
        let lineage_parent_agent_id = parent_state.id.clone();
        let desired = AgentBootstrapDesiredState {
            template,
            catalog_agent_home: Some(parent_agent_home),
            workspace: Some(AgentBootstrapWorkspaceState {
                attached_workspaces:
                    crate::runtime::workspace::inherited_attached_workspaces_for_agent(
                        &parent_state,
                        agent_id,
                    ),
                execution_profile: parent_state.execution_profile.clone(),
                inherited_model_override: parent_state.model_override.clone(),
                inherited_model_override_reasoning_effort: parent_state
                    .model_override_reasoning_effort
                    .clone(),
            }),
            model_resolution: Some(model_resolution),
            initial_message: initial_message.map(|text| AgentBootstrapInitialMessage {
                message_id: format!("agent_bootstrap_message:{agent_id}"),
                text,
                authority_class,
                creator_agent_id: parent_state.id,
            }),
        };
        self.create_public_named_agent_with_bootstrap(
            agent_id,
            Some(&lineage_parent_agent_id),
            None,
            desired,
        )
        .await
    }

    async fn await_child_terminal_result(
        &self,
        child_agent_id: &str,
        child_turn_baseline: u64,
        worktree: bool,
        cleanup_agent_on_terminal: bool,
    ) -> Result<ChildTaskTerminalResult> {
        let storage = self.agent_storage(child_agent_id)?;
        let identity = self
            .active_agent_identity(child_agent_id)
            .map_err(anyhow::Error::from)?;
        if let Some(result) = self
            .completed_child_terminal_from_storage(&storage, &identity, child_turn_baseline)
            .await?
        {
            if cleanup_agent_on_terminal {
                self.archive_private_agent(child_agent_id).await?;
            }
            return Ok(result);
        }
        let runtime = self.get_or_create_agent(child_agent_id).await?;
        loop {
            let state = runtime.agent_state().await?;
            let events = storage.read_recent_events(32)?;
            let runtime_error = events
                .iter()
                .rev()
                .find(|event| event.kind == "runtime_error")
                .cloned();
            let terminal = state
                .last_turn_terminal
                .as_ref()
                .filter(|record| record.turn_index == state.turn_index)
                .cloned();
            let closure = runtime.current_closure_decision().await?;
            let observed_new_turn = state.turn_index > child_turn_baseline;
            let quiescent = state.current_run_id.is_none() && state.pending == 0;
            let terminal_signal = terminal.is_some()
                || runtime_error.is_some()
                || state.status == AgentStatus::Stopped;
            let is_terminal = observed_new_turn
                && quiescent
                && !child_has_active_lifecycle_blockers(&storage, child_agent_id)?
                && !matches!(
                    closure.outcome,
                    ClosureOutcome::Waiting | ClosureOutcome::Continuable
                )
                && terminal_signal;
            if !is_terminal {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            let mut status = if state.status == AgentStatus::Stopped {
                TaskStatus::Cancelled
            } else if closure.outcome == ClosureOutcome::Failed {
                TaskStatus::Failed
            } else {
                TaskStatus::Completed
            };

            let text = if let Some(terminal) = terminal {
                if terminal.kind.is_failure() {
                    status = TaskStatus::Failed;
                }
                terminal
                    .last_assistant_message
                    .or_else(|| {
                        if status == TaskStatus::Failed {
                            state
                                .last_runtime_failure
                                .as_ref()
                                .map(|failure| failure.summary.clone())
                                .or_else(|| {
                                    runtime_error.as_ref().and_then(|error| {
                                        error
                                            .data
                                            .get("error")
                                            .and_then(|value| value.as_str())
                                            .map(ToString::to_string)
                                    })
                                })
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default()
            } else if let Some(error) = runtime_error {
                status = TaskStatus::Failed;
                error
                    .data
                    .get("error")
                    .and_then(|value| value.as_str())
                    .unwrap_or("child agent failed")
                    .to_string()
            } else {
                "child agent completed without additional output".to_string()
            };

            let mut metadata = json!({
                "target_agent_id": child_agent_id,
                "target_agent_kind": identity.kind,
                "target_agent_visibility": identity.visibility,
                "target_agent_ownership": identity.ownership(),
                "target_agent_profile_preset": identity.profile_preset(),
                "token_usage": json!({
                    "total": crate::types::TokenUsage::new(state.total_input_tokens, state.total_output_tokens),
                    "last_turn": state.last_turn_token_usage.clone(),
                    "total_model_rounds": state.total_model_rounds,
                }),
            });
            if identity.kind == AgentKind::Child {
                metadata["child_agent_id"] = json!(child_agent_id);
                metadata["child_kind"] = json!(identity.kind);
                metadata["child_visibility"] = json!(identity.visibility);
                metadata["child_ownership"] = json!(identity.ownership());
                metadata["child_profile_preset"] = json!(identity.profile_preset());
                metadata["child_observability"] = json!(runtime.child_agent_observability().await?);
            }
            if worktree {
                if let Some(worktree) = state.worktree_session.as_ref() {
                    let changed_files =
                        Self::detect_changed_files_for_worktree(&worktree.worktree_path)
                            .await
                            .unwrap_or_default();
                    metadata["worktree"] = json!({
                        "worktree_path": worktree.worktree_path,
                        "worktree_branch": worktree.worktree_branch,
                        "changed_files": changed_files,
                    });
                }
            }
            let task_detail = Some(metadata);

            if cleanup_agent_on_terminal {
                self.archive_private_agent(child_agent_id).await?;
            }
            return Ok(ChildTaskTerminalResult {
                status,
                text,
                task_detail,
            });
        }
    }

    async fn await_invocation_delivery_terminal_result(
        &self,
        child_agent_id: &str,
        delivery_id: &str,
        invocation_task_id: &str,
        worktree: bool,
    ) -> Result<ChildTaskTerminalResult> {
        let storage = self.agent_storage(child_agent_id)?;
        let identity = self
            .active_agent_identity(child_agent_id)
            .map_err(anyhow::Error::from)?;
        if let Some(evidence) = self.invocation_delivery_terminal_evidence(
            &storage,
            child_agent_id,
            delivery_id,
            invocation_task_id,
        )? {
            return self
                .invocation_terminal_result(&storage, &identity, delivery_id, evidence, worktree)
                .await;
        }

        let _runtime = self.get_or_create_agent(child_agent_id).await?;
        loop {
            if let Some(evidence) = self.invocation_delivery_terminal_evidence(
                &storage,
                child_agent_id,
                delivery_id,
                invocation_task_id,
            )? {
                return self
                    .invocation_terminal_result(
                        &storage,
                        &identity,
                        delivery_id,
                        evidence,
                        worktree,
                    )
                    .await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn invocation_delivery_terminal_evidence(
        &self,
        storage: &AppStorage,
        child_agent_id: &str,
        delivery_id: &str,
        invocation_task_id: &str,
    ) -> Result<Option<InvocationTerminalEvidence>> {
        use crate::domain::execution_protocol::{
            ConversationOutcome, ExecutionAttemptState, ExecutionBinding, ExecutionOutcome,
            ExecutionSourceIdentity, WorkItemExecutionState, WorkItemOutcome,
        };

        let Some(delivery) = self
            .runtime_db()
            .agent_message_deliveries()
            .latest(delivery_id)?
        else {
            return Ok(Some(InvocationTerminalEvidence {
                status: TaskStatus::Failed,
                text: format!(
                    "agent invocation failed: delivery {delivery_id} is missing from the durable ledger"
                ),
                activation_id: None,
                turn_id: None,
                completion_ref: None,
            }));
        };
        anyhow::ensure!(
            delivery.target_agent_id == child_agent_id,
            "delivery {delivery_id} targets {}, not {child_agent_id}",
            delivery.target_agent_id
        );
        anyhow::ensure!(
            delivery.caller.current_task_id.as_deref() == Some(invocation_task_id)
                && delivery.correlation_id.as_deref() == Some(invocation_task_id),
            "delivery {delivery_id} does not belong to invocation task {invocation_task_id}"
        );
        match delivery.state {
            AgentMessageDeliveryState::Failed | AgentMessageDeliveryState::Rejected => {
                return Ok(Some(InvocationTerminalEvidence {
                    status: TaskStatus::Failed,
                    text: format!(
                        "agent invocation delivery {delivery_id} failed: {}",
                        delivery
                            .diagnostic
                            .as_deref()
                            .unwrap_or("target execution was rejected before completion")
                    ),
                    activation_id: delivery.activation_id,
                    turn_id: delivery.turn_id,
                    completion_ref: None,
                }));
            }
            AgentMessageDeliveryState::CancelledByDeletion => {
                return Ok(Some(InvocationTerminalEvidence {
                    status: TaskStatus::Cancelled,
                    text: format!(
                        "agent invocation delivery {delivery_id} was cancelled because the target agent was deleted"
                    ),
                    activation_id: delivery.activation_id,
                    turn_id: delivery.turn_id,
                    completion_ref: None,
                }));
            }
            AgentMessageDeliveryState::Queued | AgentMessageDeliveryState::Dispatched
                if delivery.activation_id.is_none() =>
            {
                return Ok(None);
            }
            AgentMessageDeliveryState::Queued
            | AgentMessageDeliveryState::Dispatched
            | AgentMessageDeliveryState::Consumed => {}
        }
        let Some(root_activation_id) = delivery.activation_id.clone() else {
            return Ok(Some(InvocationTerminalEvidence {
                status: TaskStatus::Failed,
                text: format!(
                    "agent invocation delivery {delivery_id} was consumed without a canonical execution binding"
                ),
                activation_id: None,
                turn_id: delivery.turn_id,
                completion_ref: None,
            }));
        };
        let Some(execution) = self
            .runtime_db()
            .transitions()
            .load_execution_protocol_state_if_initialized(child_agent_id)?
        else {
            return Ok(None);
        };
        let mut cursor = InvocationSettlementCursor::Attempt(root_activation_id.clone());
        let mut visited = HashSet::new();
        for _ in 0..128 {
            let cursor_key = match &cursor {
                InvocationSettlementCursor::Attempt(attempt_id) => {
                    format!("attempt:{attempt_id}")
                }
                InvocationSettlementCursor::WorkItem(work_item_id) => {
                    format!("work_item:{work_item_id}")
                }
            };
            if !visited.insert(cursor_key) {
                return Ok(Some(InvocationTerminalEvidence {
                    status: TaskStatus::Failed,
                    text: format!(
                        "agent invocation delivery {delivery_id} has a cyclic execution continuation"
                    ),
                    activation_id: Some(root_activation_id),
                    turn_id: delivery.turn_id,
                    completion_ref: None,
                }));
            }
            match cursor.clone() {
                InvocationSettlementCursor::Attempt(attempt_id) => {
                    let Some(attempt) = execution.attempts.get(&attempt_id).cloned() else {
                        return Ok(Some(InvocationTerminalEvidence {
                            status: TaskStatus::Failed,
                            text: format!(
                                "agent invocation delivery {delivery_id} references missing activation {attempt_id}"
                            ),
                            activation_id: Some(attempt_id),
                            turn_id: delivery.turn_id,
                            completion_ref: None,
                        }));
                    };
                    match attempt.state {
                        ExecutionAttemptState::Open => return Ok(None),
                        ExecutionAttemptState::Interrupted => {
                            if let Some(recovery) = execution
                                .attempts
                                .values()
                                .filter(|candidate| {
                                    candidate.recovery_of_attempt_id.as_deref()
                                        == Some(attempt.attempt_id.as_str())
                                })
                                .max_by_key(|candidate| candidate.source.generation)
                            {
                                cursor = InvocationSettlementCursor::Attempt(
                                    recovery.attempt_id.clone(),
                                );
                                continue;
                            }
                            if matches!(
                                delivery.state,
                                AgentMessageDeliveryState::Queued
                                    | AgentMessageDeliveryState::Dispatched
                            ) {
                                return Ok(None);
                            }
                            return Ok(Some(InvocationTerminalEvidence {
                                status: TaskStatus::Failed,
                                text: format!(
                                    "agent invocation activation {} was interrupted before recovery",
                                    attempt.attempt_id
                                ),
                                activation_id: Some(attempt.attempt_id),
                                turn_id: attempt.turn_id,
                                completion_ref: None,
                            }));
                        }
                        ExecutionAttemptState::ProtocolViolation => {
                            return Ok(Some(InvocationTerminalEvidence {
                                status: TaskStatus::Failed,
                                text: format!(
                                    "agent invocation activation {} ended in a protocol violation",
                                    attempt.attempt_id
                                ),
                                activation_id: Some(attempt.attempt_id),
                                turn_id: attempt.turn_id,
                                completion_ref: None,
                            }));
                        }
                        ExecutionAttemptState::Settled => {}
                    }
                    let Some(outcome_id) = attempt.terminal_outcome_id.as_deref() else {
                        return Ok(None);
                    };
                    let Some(outcome) = execution.outcomes.get(outcome_id) else {
                        return Ok(None);
                    };
                    match &outcome.outcome {
                        ExecutionOutcome::Conversation(ConversationOutcome::Replied)
                        | ExecutionOutcome::Command(_) => {
                            let Some(turn_id) = attempt.turn_id.as_deref() else {
                                return Ok(Some(InvocationTerminalEvidence {
                                    status: TaskStatus::Failed,
                                    text: format!(
                                        "agent invocation activation {} settled without a turn identity",
                                        attempt.attempt_id
                                    ),
                                    activation_id: Some(attempt.attempt_id),
                                    turn_id: None,
                                    completion_ref: None,
                                }));
                            };
                            let tasks = self
                                .runtime_db()
                                .tasks()
                                .latest_for_agent(child_agent_id, usize::MAX)?
                                .into_iter()
                                .filter(|task| {
                                    task.detail
                                        .as_ref()
                                        .and_then(|detail| detail.get("parent_turn_id"))
                                        .and_then(Value::as_str)
                                        == Some(turn_id)
                                })
                                .collect::<Vec<_>>();
                            if tasks.iter().any(|task| {
                                matches!(
                                    task.status,
                                    TaskStatus::Queued
                                        | TaskStatus::Running
                                        | TaskStatus::Cancelling
                                )
                            }) {
                                return Ok(None);
                            }
                            if !tasks.is_empty() {
                                let task_ids = tasks
                                    .iter()
                                    .map(|task| task.id.as_str())
                                    .collect::<HashSet<_>>();
                                let continuations = execution
                                    .attempts
                                    .values()
                                    .filter(|candidate| {
                                        matches!(
                                            &candidate.source.identity,
                                            ExecutionSourceIdentity::TaskResult {
                                                task_id,
                                                ..
                                            } if task_ids.contains(task_id.as_str())
                                        )
                                    })
                                    .collect::<Vec<_>>();
                                if tasks.iter().any(|task| {
                                    !continuations.iter().any(|candidate| {
                                        matches!(
                                            &candidate.source.identity,
                                            ExecutionSourceIdentity::TaskResult {
                                                task_id,
                                                ..
                                            } if task_id == &task.id
                                        )
                                    })
                                }) {
                                    return Ok(None);
                                }
                                if let Some(continuation) = continuations
                                    .into_iter()
                                    .max_by_key(|candidate| candidate.source.generation)
                                {
                                    cursor = InvocationSettlementCursor::Attempt(
                                        continuation.attempt_id.clone(),
                                    );
                                    continue;
                                }
                            }
                            return self.invocation_terminal_evidence_for_turn(
                                child_agent_id,
                                delivery_id,
                                &attempt,
                            );
                        }
                        ExecutionOutcome::Conversation(ConversationOutcome::Wait { wait }) => {
                            let Some(next) = execution
                                .attempts
                                .values()
                                .filter(|candidate| {
                                    matches!(
                                        &candidate.source.identity,
                                        ExecutionSourceIdentity::TriggeredWait {
                                            wait_id,
                                            ..
                                        } if wait_id == &wait.wait_id
                                    )
                                })
                                .max_by_key(|candidate| candidate.source.generation)
                            else {
                                return Ok(None);
                            };
                            cursor = InvocationSettlementCursor::Attempt(next.attempt_id.clone());
                        }
                        ExecutionOutcome::Conversation(
                            ConversationOutcome::HandoffToWorkItemWait { work_item_id, .. },
                        ) => {
                            cursor = InvocationSettlementCursor::WorkItem(work_item_id.clone());
                        }
                        ExecutionOutcome::Conversation(ConversationOutcome::Paused { reason }) => {
                            return Ok(Some(InvocationTerminalEvidence {
                                status: TaskStatus::Failed,
                                text: format!(
                                    "agent invocation paused without a wake path: {reason}"
                                ),
                                activation_id: Some(attempt.attempt_id),
                                turn_id: attempt.turn_id,
                                completion_ref: None,
                            }));
                        }
                        ExecutionOutcome::Conversation(ConversationOutcome::Interrupted {
                            reason,
                        }) => {
                            return Ok(Some(InvocationTerminalEvidence {
                                status: TaskStatus::Interrupted,
                                text: format!("agent invocation was interrupted: {reason}"),
                                activation_id: Some(attempt.attempt_id),
                                turn_id: attempt.turn_id,
                                completion_ref: None,
                            }));
                        }
                        ExecutionOutcome::Conversation(ConversationOutcome::Failed { policy }) => {
                            return Ok(Some(InvocationTerminalEvidence {
                                status: TaskStatus::Failed,
                                text: format!("agent invocation failed under policy {policy}"),
                                activation_id: Some(attempt.attempt_id),
                                turn_id: attempt.turn_id,
                                completion_ref: None,
                            }));
                        }
                        ExecutionOutcome::WorkItem(WorkItemOutcome::Complete { completion }) => {
                            return Ok(Some(self.invocation_terminal_evidence_for_completion(
                                storage, &attempt, completion,
                            )?));
                        }
                        ExecutionOutcome::WorkItem(
                            WorkItemOutcome::Continue
                            | WorkItemOutcome::Wait { .. }
                            | WorkItemOutcome::Yield { .. },
                        ) => {
                            let ExecutionBinding::WorkItem { work_item_id } = &attempt.binding
                            else {
                                return Ok(Some(InvocationTerminalEvidence {
                                    status: TaskStatus::Failed,
                                    text: "agent invocation WorkItem outcome lost its execution binding"
                                        .into(),
                                    activation_id: Some(attempt.attempt_id),
                                    turn_id: attempt.turn_id,
                                    completion_ref: None,
                                }));
                            };
                            cursor = InvocationSettlementCursor::WorkItem(work_item_id.clone());
                        }
                        ExecutionOutcome::WorkItem(WorkItemOutcome::Pause { reason }) => {
                            return Ok(Some(InvocationTerminalEvidence {
                                status: TaskStatus::Failed,
                                text: format!("agent invocation WorkItem paused: {reason}"),
                                activation_id: Some(attempt.attempt_id),
                                turn_id: attempt.turn_id,
                                completion_ref: None,
                            }));
                        }
                        ExecutionOutcome::WorkItem(WorkItemOutcome::Failed { policy }) => {
                            return Ok(Some(InvocationTerminalEvidence {
                                status: TaskStatus::Failed,
                                text: format!(
                                    "agent invocation WorkItem failed under policy {policy}"
                                ),
                                activation_id: Some(attempt.attempt_id),
                                turn_id: attempt.turn_id,
                                completion_ref: None,
                            }));
                        }
                        ExecutionOutcome::WorkItem(WorkItemOutcome::Interrupted { reason }) => {
                            return Ok(Some(InvocationTerminalEvidence {
                                status: TaskStatus::Interrupted,
                                text: format!(
                                    "agent invocation WorkItem was interrupted: {reason}"
                                ),
                                activation_id: Some(attempt.attempt_id),
                                turn_id: attempt.turn_id,
                                completion_ref: None,
                            }));
                        }
                        ExecutionOutcome::WorkItem(WorkItemOutcome::NeedsRepair { repair_id }) => {
                            return Ok(Some(InvocationTerminalEvidence {
                                status: TaskStatus::Failed,
                                text: format!(
                                    "agent invocation WorkItem requires repair: {repair_id}"
                                ),
                                activation_id: Some(attempt.attempt_id),
                                turn_id: attempt.turn_id,
                                completion_ref: None,
                            }));
                        }
                    }
                }
                InvocationSettlementCursor::WorkItem(work_item_id) => {
                    let Some(work_item) = execution.work_items.get(&work_item_id) else {
                        return Ok(Some(InvocationTerminalEvidence {
                            status: TaskStatus::Failed,
                            text: format!(
                                "agent invocation references missing WorkItem execution {work_item_id}"
                            ),
                            activation_id: Some(root_activation_id),
                            turn_id: delivery.turn_id,
                            completion_ref: None,
                        }));
                    };
                    match &work_item.state {
                        WorkItemExecutionState::InFlight { attempt_id, .. } => {
                            cursor = InvocationSettlementCursor::Attempt(attempt_id.clone());
                        }
                        WorkItemExecutionState::Waiting { wait, .. } => {
                            let Some(next) = execution
                                .attempts
                                .values()
                                .filter(|candidate| {
                                    matches!(
                                        &candidate.source.identity,
                                        ExecutionSourceIdentity::TriggeredWait {
                                            wait_id,
                                            ..
                                        } if wait_id == &wait.wait_id
                                    ) && matches!(
                                        &candidate.binding,
                                        ExecutionBinding::WorkItem {
                                            work_item_id: candidate_work_item_id
                                        } if candidate_work_item_id == &work_item_id
                                    )
                                })
                                .max_by_key(|candidate| candidate.source.generation)
                            else {
                                return Ok(None);
                            };
                            cursor = InvocationSettlementCursor::Attempt(next.attempt_id.clone());
                        }
                        WorkItemExecutionState::Terminal { completion, .. } => {
                            let attempt = execution
                                .attempts
                                .values()
                                .filter(|candidate| {
                                    matches!(
                                        &candidate.binding,
                                        ExecutionBinding::WorkItem {
                                            work_item_id: candidate_work_item_id
                                        } if candidate_work_item_id == &work_item_id
                                    )
                                })
                                .max_by_key(|candidate| candidate.source.generation);
                            return Ok(Some(if let Some(attempt) = attempt {
                                self.invocation_terminal_evidence_for_completion(
                                    storage, attempt, completion,
                                )?
                            } else {
                                InvocationTerminalEvidence {
                                    status: TaskStatus::Completed,
                                    text: storage
                                        .read_brief_by_id(completion)?
                                        .map(|brief| brief.text)
                                        .unwrap_or_default(),
                                    activation_id: Some(root_activation_id),
                                    turn_id: delivery.turn_id,
                                    completion_ref: Some(completion.clone()),
                                }
                            }));
                        }
                        WorkItemExecutionState::NeedsRepair { repair_id, .. } => {
                            return Ok(Some(InvocationTerminalEvidence {
                                status: TaskStatus::Failed,
                                text: format!(
                                    "agent invocation WorkItem requires repair: {repair_id}"
                                ),
                                activation_id: Some(root_activation_id),
                                turn_id: delivery.turn_id,
                                completion_ref: None,
                            }));
                        }
                        WorkItemExecutionState::Runnable { .. }
                        | WorkItemExecutionState::Paused { .. } => return Ok(None),
                    }
                }
            }
        }
        Ok(Some(InvocationTerminalEvidence {
            status: TaskStatus::Failed,
            text: format!(
                "agent invocation delivery {delivery_id} exceeded the bounded continuation chain"
            ),
            activation_id: Some(root_activation_id),
            turn_id: delivery.turn_id,
            completion_ref: None,
        }))
    }

    fn invocation_terminal_evidence_for_turn(
        &self,
        child_agent_id: &str,
        delivery_id: &str,
        attempt: &crate::domain::execution_protocol::ExecutionAttempt,
    ) -> Result<Option<InvocationTerminalEvidence>> {
        let Some(turn_id) = attempt.turn_id.as_deref() else {
            return Ok(None);
        };
        let Some(turn) = self
            .runtime_db()
            .turn_records()
            .by_id(Some(child_agent_id), turn_id)?
        else {
            return Ok(None);
        };
        let Some(terminal) = turn.terminal else {
            return Ok(None);
        };
        let status = if terminal.kind.is_failure() {
            TaskStatus::Failed
        } else {
            TaskStatus::Completed
        };
        let text = self
            .assistant_text_for_turn(child_agent_id, turn_id)?
            .or(terminal.reason)
            .unwrap_or_else(|| {
                format!(
                    "agent invocation delivery {delivery_id} completed without additional output"
                )
            });
        Ok(Some(InvocationTerminalEvidence {
            status,
            text,
            activation_id: Some(attempt.attempt_id.clone()),
            turn_id: Some(turn_id.to_string()),
            completion_ref: None,
        }))
    }

    fn invocation_terminal_evidence_for_completion(
        &self,
        storage: &AppStorage,
        attempt: &crate::domain::execution_protocol::ExecutionAttempt,
        completion: &str,
    ) -> Result<InvocationTerminalEvidence> {
        Ok(InvocationTerminalEvidence {
            status: TaskStatus::Completed,
            text: storage
                .read_brief_by_id(completion)?
                .map(|brief| brief.text)
                .unwrap_or_default(),
            activation_id: Some(attempt.attempt_id.clone()),
            turn_id: attempt.turn_id.clone(),
            completion_ref: Some(completion.to_string()),
        })
    }

    fn assistant_text_for_turn(&self, agent_id: &str, turn_id: &str) -> Result<Option<String>> {
        let text = self
            .runtime_db()
            .transcript_entries()
            .for_turn_ids(agent_id, &[turn_id.to_string()])?
            .into_iter()
            .filter(|entry| {
                entry.kind == TranscriptEntryKind::AssistantRound
                    && entry.data.get("round_purpose").and_then(Value::as_str)
                        != Some("runtime_checkpoint")
            })
            .flat_map(|entry| {
                entry
                    .data
                    .get("blocks")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })
            .filter_map(|block| {
                (block.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| block.get("text").and_then(Value::as_str))
                    .flatten()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(ToString::to_string)
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok((!text.is_empty()).then_some(text))
    }

    async fn invocation_terminal_result(
        &self,
        storage: &AppStorage,
        identity: &AgentIdentityRecord,
        delivery_id: &str,
        evidence: InvocationTerminalEvidence,
        worktree: bool,
    ) -> Result<ChildTaskTerminalResult> {
        let state = storage
            .read_agent()?
            .unwrap_or_else(|| stopped_unloaded_agent(&identity.agent_id));
        let mut metadata = json!({
            "target_agent_id": identity.agent_id,
            "target_agent_kind": identity.kind,
            "target_agent_visibility": identity.visibility,
            "target_agent_ownership": identity.ownership(),
            "target_agent_profile_preset": identity.profile_preset(),
            "delivery_id": delivery_id,
            "activation_id": evidence.activation_id,
            "turn_id": evidence.turn_id,
            "completion_ref": evidence.completion_ref,
            "token_usage": json!({
                "total": crate::types::TokenUsage::new(state.total_input_tokens, state.total_output_tokens),
                "last_turn": state.last_turn_token_usage.clone(),
                "total_model_rounds": state.total_model_rounds,
            }),
        });
        if identity.kind == AgentKind::Child {
            metadata["child_agent_id"] = json!(identity.agent_id);
            metadata["child_kind"] = json!(identity.kind);
            metadata["child_visibility"] = json!(identity.visibility);
            metadata["child_ownership"] = json!(identity.ownership());
            metadata["child_profile_preset"] = json!(identity.profile_preset());
        }
        if worktree {
            if let Some(worktree) = state.worktree_session.as_ref() {
                let changed_files =
                    Self::detect_changed_files_for_worktree(&worktree.worktree_path)
                        .await
                        .unwrap_or_default();
                metadata["worktree"] = json!({
                    "worktree_path": worktree.worktree_path,
                    "worktree_branch": worktree.worktree_branch,
                    "changed_files": changed_files,
                });
            }
        }
        Ok(ChildTaskTerminalResult {
            status: evidence.status,
            text: evidence.text,
            task_detail: Some(metadata),
        })
    }

    async fn completed_child_terminal_from_storage(
        &self,
        storage: &AppStorage,
        identity: &AgentIdentityRecord,
        child_turn_baseline: u64,
    ) -> Result<Option<ChildTaskTerminalResult>> {
        let Some(state) = storage.read_agent()? else {
            return Ok(None);
        };
        let Some(terminal) = state
            .last_turn_terminal
            .as_ref()
            .filter(|record| record.turn_index == state.turn_index)
            .filter(|record| record.turn_index > child_turn_baseline)
            .cloned()
        else {
            return Ok(None);
        };
        if state.current_run_id.is_some()
            || state.pending > 0
            || child_has_active_lifecycle_blockers(storage, &identity.agent_id)?
        {
            return Ok(None);
        }
        let closure = RuntimeHandle::closure_decision_from_storage(storage, &state)?;
        if matches!(
            closure.outcome,
            ClosureOutcome::Waiting | ClosureOutcome::Continuable
        ) {
            return Ok(None);
        }

        let mut status = if terminal.kind.is_failure() {
            TaskStatus::Failed
        } else {
            TaskStatus::Completed
        };
        if state.status == AgentStatus::Stopped {
            status = TaskStatus::Cancelled;
        }
        let text = terminal
            .last_assistant_message
            .or_else(|| {
                if status == TaskStatus::Failed {
                    state
                        .last_runtime_failure
                        .as_ref()
                        .map(|failure| failure.summary.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default();

        Ok(Some(ChildTaskTerminalResult {
            status,
            text,
            task_detail: {
                let mut detail = json!( {
                    "target_agent_id": identity.agent_id,
                    "target_agent_kind": identity.kind,
                    "target_agent_visibility": identity.visibility,
                    "target_agent_ownership": identity.ownership(),
                    "target_agent_profile_preset": identity.profile_preset(),
                });
                if identity.kind == AgentKind::Child {
                    detail["child_agent_id"] = json!(identity.agent_id);
                    detail["child_kind"] = json!(identity.kind);
                    detail["child_visibility"] = json!(identity.visibility);
                    detail["child_ownership"] = json!(identity.ownership());
                    detail["child_profile_preset"] = json!(identity.profile_preset());
                }
                detail["token_usage"] = json!({
                    "total": crate::types::TokenUsage::new(state.total_input_tokens, state.total_output_tokens),
                    "last_turn": state.last_turn_token_usage.clone(),
                    "total_model_rounds": state.total_model_rounds,
                });
                Some(detail)
            },
        }))
    }

    pub(crate) fn agent_data_dir(&self, agent_id: &str) -> PathBuf {
        self.config().data_dir.join("agents").join(agent_id)
    }

    pub(crate) fn is_temporary_agent_id(agent_id: &str) -> bool {
        agent_id.starts_with(TEMP_AGENT_PREFIX)
    }

    fn validate_agent_id(&self, agent_id: &str) -> Result<()> {
        self.inner.registry.validate_agent_id(agent_id)
    }

    fn runtime_context_config(&self) -> ContextConfig {
        let config = self.config();
        let base = ContextConfig {
            recent_messages: config.context_window_messages,
            recent_briefs: config.context_window_briefs,
            compaction_trigger_messages: config.compaction_trigger_messages,
            compaction_keep_recent_messages: config.compaction_keep_recent_messages,
            prompt_budget_estimated_tokens: config.prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens: config.compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens: config.compaction_keep_recent_estimated_tokens,
            recent_episode_candidates: config.recent_episode_candidates,
            max_relevant_episodes: config.max_relevant_episodes,
            ..ContextConfig::default()
        };
        RuntimeModelCatalog::from_config(&config).resolved_context_config(&base, None)
    }

    fn spawn_runtime(
        &self,
        agent_id: &str,
        recovery_generation: Option<u64>,
    ) -> Result<(
        RuntimeHandle,
        JoinHandle<()>,
        watch::Receiver<AgentRuntimePhase>,
    )> {
        let config = self.config();
        let runtime = if let Some(provider) = self.inner.static_provider.as_ref() {
            RuntimeHandle::new_static_with_host_bridge(
                agent_id.to_string(),
                self.agent_data_dir(agent_id),
                InitialWorkspaceBinding::Detached,
                config.callback_base_url.clone(),
                provider.clone(),
                config.default_agent_id.clone(),
                self.runtime_context_config(),
                config.user_home_dir.clone(),
                self.inner.runtime_db.clone(),
                self.bridge(),
                RuntimeModelCatalog::from_config(&config),
                self.inner.event_bus.clone(),
            )?
        } else {
            RuntimeHandle::new_reconfigurable_with_host_bridge(
                agent_id.to_string(),
                self.agent_data_dir(agent_id),
                InitialWorkspaceBinding::Detached,
                config.callback_base_url.clone(),
                (*config).clone(),
                config.default_agent_id.clone(),
                self.runtime_context_config(),
                self.inner.runtime_db.clone(),
                self.bridge(),
                self.inner.event_bus.clone(),
            )?
        };
        runtime.enable_memory_index_notify(self.inner.memory_index_notify.clone());
        let (phase_tx, phase_rx) = watch::channel(AgentRuntimePhase::Bootstrapping);
        let runtime_recovery_tx = self.inner.runtime_recovery_tx.clone();
        let runtime_task = tokio::spawn({
            let runtime = runtime.clone();
            let agent_id = agent_id.to_string();
            async move {
                let mut run = Box::pin(runtime.clone().run());
                let result = tokio::select! {
                    result = &mut run => result,
                    bootstrap = runtime.wait_for_bootstrap() => {
                        match bootstrap {
                            Ok(()) => {
                                phase_tx.send_replace(AgentRuntimePhase::Running);
                            }
                            Err(_) => {
                                phase_tx.send_replace(AgentRuntimePhase::FailedCleaning);
                            }
                        }
                        run.await
                    }
                };
                let retryable_failure = result
                    .as_ref()
                    .err()
                    .map(|error| describe_runtime_error(error).retryable);
                if let Err(error) = result {
                    phase_tx.send_replace(AgentRuntimePhase::FailedCleaning);
                    runtime.record_runtime_loop_failure(&error).await;
                    tracing::warn!(
                        agent_id,
                        "agent runtime loop stopped; host recovery will rebuild it when safe"
                    );
                }
                phase_tx.send_replace(AgentRuntimePhase::Terminated);
                if let (Some(generation), Some(retryable)) =
                    (recovery_generation, retryable_failure)
                {
                    let _ = runtime_recovery_tx.send(RuntimeRecoveryNotice {
                        agent_id,
                        generation,
                        retryable,
                    });
                }
            }
        });
        Ok((runtime, runtime_task, phase_rx))
    }

    async fn detect_changed_files_for_worktree(
        worktree_path: &std::path::Path,
    ) -> Result<Vec<String>> {
        let worktree_path = worktree_path.to_path_buf();
        spawn_blocking(move || -> Result<Vec<String>> {
            let output = std::process::Command::new("git")
                .arg("status")
                .arg("--porcelain")
                .current_dir(&worktree_path)
                .output()?;
            if !output.status.success() {
                return Ok(Vec::new());
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut changed_files = stdout
                .lines()
                .filter(|line| !line.is_empty())
                .map(|line| {
                    let parts = line.trim().splitn(2, ' ').collect::<Vec<_>>();
                    if parts.len() > 1 {
                        parts[1].to_string()
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>();
            changed_files.sort();
            Ok(changed_files)
        })
        .await?
    }
}

fn build_agent_tree_node(
    agent_id: &str,
    nodes: &mut HashMap<String, AgentTreeNode>,
    children_by_parent: &HashMap<String, Vec<String>>,
    visiting: &mut HashSet<String>,
) -> Option<AgentTreeNode> {
    if !visiting.insert(agent_id.to_string()) {
        return None;
    }
    let mut node = nodes.remove(agent_id)?;
    if let Some(child_ids) = children_by_parent.get(agent_id) {
        for child_id in child_ids {
            if let Some(child) =
                build_agent_tree_node(child_id, nodes, children_by_parent, visiting)
            {
                node.children.push(child);
            }
        }
    }
    visiting.remove(agent_id);
    Some(node)
}

impl RuntimeHostBridge {
    fn host(&self) -> Result<RuntimeHost> {
        let inner = self
            .inner
            .upgrade()
            .ok_or_else(|| anyhow!("runtime host is no longer available"))?;
        Ok(RuntimeHost { inner })
    }

    pub(crate) fn agent_storage(&self, agent_id: &str) -> Result<AppStorage> {
        self.host()?.agent_storage(agent_id)
    }

    pub(crate) fn skills_registry(&self) -> Result<Arc<RwLock<SkillsRegistry>>> {
        Ok(self.host()?.skills_registry())
    }

    pub(crate) async fn identity_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Option<AgentIdentityRecord>> {
        self.host()?.agent_identity_record(agent_id)
    }

    pub(crate) async fn canonical_relations_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Option<crate::types::AgentCanonicalRelationsProjection>> {
        self.host()?
            .runtime_db()
            .agent_canonical_relations()
            .latest(agent_id)
    }

    pub(crate) async fn child_summaries(
        &self,
        parent_agent_id: &str,
    ) -> Result<Vec<ChildAgentSummary>> {
        self.host()?.child_agent_summaries(parent_agent_id).await
    }

    /// Get a full AgentSummary for a given agent_id without starting an
    /// unloaded target runtime.
    pub(crate) async fn agent_summary_for(
        &self,
        agent_id: &str,
    ) -> Result<crate::types::AgentSummary> {
        self.host()?
            .local_agent_summary(agent_id)
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn child_observability(
        &self,
        child_agent_id: &str,
    ) -> Result<Option<crate::types::ChildAgentObservabilitySnapshot>> {
        let host = self.host()?;
        let Some(identity) = host.agent_identity_record(child_agent_id)? else {
            return Ok(None);
        };
        if identity.status != AgentRegistryStatus::Active
            || !host.agent_data_dir(child_agent_id).exists()
        {
            return Ok(None);
        }
        let storage = host.agent_storage(child_agent_id)?;
        let state = storage
            .read_agent()?
            .unwrap_or_else(|| AgentState::new(child_agent_id.to_string()));
        Ok(Some(
            host.child_agent_observability_snapshot(child_agent_id, &storage, &state)
                .await?,
        ))
    }

    pub(crate) async fn reusable_agent_exists(&self, agent_id: &str) -> Result<bool> {
        let host = self.host()?;
        let Some(identity) = host.agent_identity_record(agent_id)? else {
            return Ok(false);
        };
        Ok(
            identity.status == AgentRegistryStatus::Active
                && host.agent_data_dir(agent_id).exists(),
        )
    }

    pub(crate) async fn spawn_child_task(
        &self,
        parent_runtime: RuntimeHandle,
        task: &TaskRecord,
        prompt: String,
        authority_class: AuthorityClass,
        worktree: bool,
        template: Option<String>,
        model_resolution: AgentModelResolution,
    ) -> Result<ChildTaskSpawn> {
        self.host()?
            .spawn_child_task(
                parent_runtime,
                task,
                prompt,
                authority_class,
                worktree,
                template,
                model_resolution,
            )
            .await
    }

    pub(crate) async fn invoke_existing_agent(
        &self,
        task: &TaskRecord,
        target_agent_id: &str,
        message_text: String,
        authority_class: AuthorityClass,
    ) -> Result<ChildTaskSpawn> {
        Box::pin(self.host()?.invoke_existing_agent(
            task,
            target_agent_id,
            message_text,
            authority_class,
        ))
        .await
    }

    pub(crate) async fn create_agent(
        &self,
        parent_runtime: RuntimeHandle,
        request: CreateAgentRequest,
    ) -> Result<AgentCreateResult> {
        self.host()?.create_agent(parent_runtime, request).await
    }

    pub(crate) async fn child_turn_index(&self, agent_id: &str) -> Result<u64> {
        let runtime = self.host()?.get_or_create_agent(agent_id).await?;
        Ok(runtime.agent_state().await?.turn_index)
    }

    pub(crate) async fn record_operator_notification(
        &self,
        agent_id: &str,
        record: &OperatorNotificationRecord,
    ) -> Result<()> {
        let runtime = self.host()?.get_or_create_agent(agent_id).await?;
        runtime.persist_operator_notification(record)
    }

    pub(crate) async fn submit_operator_notification_delivery(
        &self,
        agent_id: &str,
        record: &OperatorNotificationRecord,
    ) -> Result<()> {
        let runtime = self.host()?.get_or_create_agent(agent_id).await?;
        let _ = runtime
            .submit_operator_notification_delivery(record)
            .await?;
        Ok(())
    }

    pub(crate) async fn await_agent_invocation_terminal_result(
        &self,
        child_agent_id: &str,
        child_turn_baseline: u64,
        delivery_id: Option<&str>,
        invocation_task_id: &str,
        worktree: bool,
        cleanup_agent_on_terminal: bool,
    ) -> Result<ChildTaskTerminalResult> {
        if let Some(delivery_id) = delivery_id {
            return self
                .host()?
                .await_invocation_delivery_terminal_result(
                    child_agent_id,
                    delivery_id,
                    invocation_task_id,
                    worktree,
                )
                .await;
        }
        self.host()?
            .await_child_terminal_result(
                child_agent_id,
                child_turn_baseline,
                worktree,
                cleanup_agent_on_terminal,
            )
            .await
    }

    pub(crate) async fn stop_private_agent(&self, agent_id: &str) -> Result<()> {
        self.host()?.stop_private_agent(agent_id).await
    }

    pub(crate) async fn deliver_child_followup(
        &self,
        parent_agent_id: &str,
        task_id: &str,
        child_agent_id: &str,
        input: &str,
        authority_class: AuthorityClass,
    ) -> Result<bool> {
        if !self.reusable_agent_exists(child_agent_id).await? {
            return Ok(false);
        }
        let runtime = match self.host()?.get_or_create_agent(child_agent_id).await {
            Ok(runtime) => runtime,
            Err(error) => {
                if !self.reusable_agent_exists(child_agent_id).await? {
                    return Ok(false);
                }
                return Err(error);
            }
        };
        let mut message = crate::types::MessageEnvelope::new(
            child_agent_id.to_string(),
            crate::types::MessageKind::InternalFollowup,
            crate::types::MessageOrigin::Task {
                task_id: task_id.to_string(),
            },
            authority_class.clone(),
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text {
                text: input.to_string(),
            },
        )
        .with_admission(
            crate::types::MessageDeliverySurface::RuntimeSystem,
            crate::types::AdmissionContext::RuntimeOwned,
        );
        message.metadata = Some(json!({
            "delegated_task_id": task_id,
            "parent_agent_id": parent_agent_id,
            "child_agent_id": child_agent_id,
            "followup_via": "task_input",
            "delegated_authority_class": authority_class,
        }));
        runtime.enqueue(message).await.map(|_| true)
    }

    pub(crate) async fn acquire_workspace_occupancy(
        &self,
        workspace_id: &str,
        execution_root_id: &str,
        holder_agent_id: &str,
        access_mode: WorkspaceAccessMode,
    ) -> Result<Option<WorkspaceOccupancyRecord>> {
        self.host()?.acquire_workspace_occupancy(
            workspace_id,
            execution_root_id,
            holder_agent_id,
            access_mode,
        )
    }

    pub(crate) async fn release_workspace_occupancy(
        &self,
        occupancy_id: &str,
    ) -> Result<Option<WorkspaceOccupancyRecord>> {
        self.host()?.release_workspace_occupancy(occupancy_id)
    }

    pub(crate) async fn workspace_occupancy_by_id(
        &self,
        occupancy_id: &str,
    ) -> Result<Option<WorkspaceOccupancyRecord>> {
        self.host()?.workspace_occupancy_by_id(occupancy_id)
    }

    pub(crate) async fn workspace_occupancies(&self) -> Result<Vec<WorkspaceOccupancyRecord>> {
        self.host()?.workspace_occupancies()
    }

    pub(crate) async fn acquire_workspace_cleanup_lease(
        &self,
        execution_root_id: &str,
    ) -> Result<WorkspaceCleanupLeaseGuard> {
        self.host()?
            .acquire_workspace_cleanup_lease(execution_root_id)
    }

    pub(crate) async fn workspace_entry_by_id(
        &self,
        workspace_id: &str,
    ) -> Result<Option<WorkspaceEntry>> {
        Ok(self
            .host()?
            .workspace_entries()?
            .into_iter()
            .find(|entry| entry.workspace_id == workspace_id))
    }

    pub(crate) async fn ensure_workspace_entry(
        &self,
        workspace_anchor: PathBuf,
    ) -> Result<WorkspaceEntry> {
        self.host()?.ensure_workspace_entry(workspace_anchor)
    }
}

fn child_has_active_lifecycle_blockers(storage: &AppStorage, child_agent_id: &str) -> Result<bool> {
    // Check if the child agent has any active tasks other than its own ChildAgentTask.
    // A child's ChildAgentTask reflects the parent-child supervision relationship, not
    // an independent lifecycle blocker. Only other tasks (command tasks, etc.) block recovery.
    let has_active_tasks = storage
        .latest_active_task_records_for_agent(child_agent_id, usize::MAX)?
        .into_iter()
        .filter(|task| !matches!(task.kind, TaskKind::ChildAgentTask))
        .any(|task| {
            matches!(
                task.status,
                TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
            )
        });
    if has_active_tasks {
        return Ok(true);
    }

    Ok(storage
        .active_wait_conditions_for_agent(child_agent_id)?
        .into_iter()
        .any(|condition| {
            condition.status == crate::types::WaitConditionStatus::Active
                && condition.kind == crate::types::WaitConditionKind::Task
        }))
}

#[cfg(test)]
mod memory_indexer_retry_tests {
    use super::*;

    #[test]
    fn memory_indexer_retry_delay_stays_within_jittered_bounds() {
        for attempts in 0..12u32 {
            let delay = RuntimeHost::memory_indexer_retry_delay(attempts);
            let exponential_ms =
                500u64.saturating_mul(1u64.checked_shl(attempts).unwrap_or(u64::MAX));
            let expected_ms = exponential_ms.min(30_000);
            assert!(
                delay.as_millis() as u64 >= expected_ms * 3 / 4,
                "attempt {attempts}: delay {:?} below lower bound {expected_ms}ms",
                delay
            );
            assert!(
                delay.as_millis() as u64 <= expected_ms * 5 / 4 + 1,
                "attempt {attempts}: delay {:?} above upper bound {expected_ms}ms",
                delay
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };

    use async_trait::async_trait;
    use chrono::Utc;
    use tempfile::tempdir;
    use tokio::sync::Notify;

    use crate::{
        config::{provider_registry_for_tests, ControlAuthMode, ModelRouteRef},
        domain::execution_protocol::{
            AdmittedFences, ExecutionAttempt, ExecutionAttemptState, ExecutionBinding,
            ExecutionOrigin, ExecutionPriority, ExecutionProvenance, ExecutionSource,
            ExecutionSourceIdentity, ExecutionTrust,
        },
        provider::{AgentProvider, ProviderTurnRequest, ProviderTurnResponse, StubProvider},
        runtime::RuntimeHandle,
        runtime_db::RuntimeDb,
        storage::AppStorage,
        system::WorkspaceProjectionKind,
        types::{
            AgentDeletionPhase, AgentDeletionStatus, AgentKind, AgentOwnership, AgentProfilePreset,
            AgentRegistryStatus, AgentStatus, AgentVisibility, AuthorityClass, BriefKind,
            BriefRecord, ChildAgentWorkspaceMode, ControlAction, DeliverySummaryRecord,
            InvokeAgentRequest, InvokeAgentTarget, MessageBody, MessageEnvelope, MessageKind,
            MessageOrigin, Priority, QueueEntryRecord, QueueEntryStatus, TaskRecord,
            TaskRecoverySpec, TaskStatus, TimerRecord, TimerStatus, TurnTerminalKind,
            WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource,
            WorkItemRecord, WorkItemState, ACTOR_INVOCATION_TASK_KIND,
        },
    };

    use super::*;

    fn write_test_model_config(home: &Path) {
        fs::write(
            home.join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
    }

    struct ProviderConfigFixture {
        _home: tempfile::TempDir,
        _workspace: tempfile::TempDir,
        config: AppConfig,
    }

    fn test_host() -> (tempfile::TempDir, RuntimeHost) {
        let home = tempdir().unwrap();
        write_test_model_config(home.path());
        let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        config.user_home_dir = Some(home.path().to_path_buf());
        let host =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        (home, host)
    }

    fn canonical_test_host() -> (tempfile::TempDir, RuntimeHost) {
        let home = tempdir().unwrap();
        write_test_model_config(home.path());
        let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        config.user_home_dir = Some(home.path().to_path_buf());
        let host =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        (home, host)
    }

    #[tokio::test]
    async fn debug_prompt_preview_is_storage_only_and_leaves_queued_input_unchanged() {
        let (_home, host) = test_host();
        let agent_id = host.config().default_agent_id.clone();
        let storage = AppStorage::new_for_agent(
            host.agent_data_dir(&agent_id),
            agent_id.clone(),
            host.runtime_db().clone(),
        )
        .expect("storage");
        let mut message = MessageEnvelope::new(
            &agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "queued user input".into(),
            },
        );
        message.turn_id = Some("turn_queued".into());
        storage.append_message(&message).unwrap();
        storage
            .append_queue_entry(&QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: agent_id.clone(),
                priority: message.priority.clone(),
                status: QueueEntryStatus::Queued,
                created_at: message.created_at,
                updated_at: Utc::now(),
            })
            .unwrap();

        let prompt = host
            .preview_agent_prompt(
                &agent_id,
                "inspect prompt".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await
            .unwrap();

        assert!(prompt.render_dump().contains("inspect prompt"));
        assert!(host.inner.runtimes.read().await.agents.is_empty());
        assert_eq!(storage.read_agent().unwrap(), None);
        let entries = storage.latest_queue_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message_id, message.id);
        assert_eq!(entries[0].status, QueueEntryStatus::Queued);
    }

    async fn assert_runtime_prompt_uses_configured_user_home(
        mut config: AppConfig,
        static_provider: bool,
    ) {
        let user_home = tempdir().unwrap();
        let user_agents_dir = user_home.path().join(".agents");
        fs::create_dir_all(&user_agents_dir).unwrap();
        let user_agents_md = user_agents_dir.join("AGENTS.md");
        fs::write(&user_agents_md, "configured user-global marker").unwrap();
        assert_ne!(config.home_dir, user_home.path());
        config.user_home_dir = Some(user_home.path().to_path_buf());

        let host = if static_provider {
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap()
        } else {
            RuntimeHost::new(config).unwrap()
        };
        let runtime = host.default_runtime().await.unwrap();
        let prompt = runtime
            .preview_prompt(
                "inspect configured user guidance".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await
            .unwrap();

        assert_eq!(
            prompt.loaded_agents_md.user_global_status,
            crate::types::AgentsMdLoadStatus::Loaded
        );
        assert_eq!(
            prompt
                .loaded_agents_md
                .user_global_source
                .as_ref()
                .map(|source| source.path.as_path()),
            Some(user_agents_md.as_path())
        );
        assert!(prompt
            .system_sections
            .iter()
            .any(|section| section.name == "user_global_agents_md"
                && section.content.contains("configured user-global marker")));

        host.unload_runtime(&host.config().default_agent_id).await;
        let recovered = host.default_runtime().await.unwrap();
        let recovered_prompt = recovered
            .preview_prompt(
                "inspect recovered user guidance".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await
            .unwrap();
        assert_eq!(
            recovered_prompt
                .loaded_agents_md
                .user_global_source
                .as_ref()
                .map(|source| source.path.as_path()),
            Some(user_agents_md.as_path())
        );

        host.create_named_agent("configured-home-named", None)
            .await
            .unwrap();
        let named = host
            .get_public_agent("configured-home-named")
            .await
            .unwrap();
        let named_prompt = named
            .preview_prompt(
                "inspect named agent user guidance".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await
            .unwrap();
        assert_eq!(
            named_prompt
                .loaded_agents_md
                .user_global_source
                .as_ref()
                .map(|source| source.path.as_path()),
            Some(user_agents_md.as_path())
        );

        let parent_state = recovered.agent_state().await.unwrap();
        let task = test_child_supervision_task(&parent_state.id, "configured-home-task");
        let child_identity = host
            .create_child_identity(
                &parent_state.id,
                &task,
                None,
                recovered.agent_home().as_path(),
            )
            .await
            .unwrap();
        let child = host
            .get_or_create_agent(&child_identity.agent_id)
            .await
            .unwrap();
        let child_prompt = child
            .preview_prompt(
                "inspect private child user guidance".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await
            .unwrap();
        assert_eq!(
            child_prompt
                .loaded_agents_md
                .user_global_source
                .as_ref()
                .map(|source| source.path.as_path()),
            Some(user_agents_md.as_path())
        );
    }

    #[tokio::test]
    async fn static_runtime_prompt_uses_configured_user_home() {
        let fixture = provider_test_config(Some("dummy-token"));
        assert_runtime_prompt_uses_configured_user_home(fixture.config, true).await;
    }

    #[tokio::test]
    async fn reconfigurable_runtime_prompt_uses_configured_user_home() {
        let fixture = provider_test_config(Some("dummy-token"));
        assert_runtime_prompt_uses_configured_user_home(fixture.config, false).await;
    }

    #[tokio::test]
    async fn config_reload_preserves_runtime_user_home() {
        let mut fixture = provider_test_config(Some("dummy-token"));
        let original_user_home = tempdir().unwrap();
        let replacement_user_home = tempdir().unwrap();
        fs::create_dir_all(original_user_home.path().join(".agents")).unwrap();
        fs::create_dir_all(replacement_user_home.path().join(".agents")).unwrap();
        let original_agents_md = original_user_home.path().join(".agents/AGENTS.md");
        fs::write(&original_agents_md, "original stable guidance").unwrap();
        fs::write(
            replacement_user_home.path().join(".agents/AGENTS.md"),
            "replacement guidance",
        )
        .unwrap();
        fixture.config.user_home_dir = Some(original_user_home.path().to_path_buf());
        let host = RuntimeHost::new(fixture.config.clone()).unwrap();
        let runtime = host.default_runtime().await.unwrap();

        let mut reloaded = fixture.config;
        reloaded.user_home_dir = Some(replacement_user_home.path().to_path_buf());
        runtime.reload_config(&reloaded).await.unwrap();

        let prompt = runtime
            .preview_prompt(
                "inspect stable reloaded user guidance".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await
            .unwrap();
        assert_eq!(
            prompt
                .loaded_agents_md
                .user_global_source
                .as_ref()
                .map(|source| source.path.as_path()),
            Some(original_agents_md.as_path())
        );
        assert!(prompt
            .system_sections
            .iter()
            .any(|section| section.content.contains("original stable guidance")));
        assert!(!prompt
            .system_sections
            .iter()
            .any(|section| section.content.contains("replacement guidance")));
    }

    #[tokio::test]
    async fn debug_prompt_preview_uses_model_apply_patch_surface_even_when_tools_are_lowered() {
        let home = tempdir().unwrap();
        fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai-codex/gpt-5.3-codex-spark"}}"#,
        )
        .unwrap();
        let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        let host =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        let agent_id = host.config().default_agent_id.clone();

        let prompt = host
            .preview_agent_prompt(
                &agent_id,
                "inspect prompt".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await
            .unwrap();
        let rendered = prompt.render_dump();

        assert!(rendered.contains("Current ApplyPatch surface is Codex DSL freeform"));
        assert!(rendered.contains("send raw `*** Begin Patch` / `*** End Patch` text directly"));
        assert!(!rendered.contains("Current ApplyPatch surface is a JSON/function tool"));
    }

    fn inherited_model_resolution(provider: &str, model: &str) -> AgentModelResolution {
        AgentModelResolution {
            requested: None,
            resolved_provider: provider.to_string(),
            resolved_model: model.to_string(),
            resolved_parameters: None,
            resolution_status: AgentModelResolutionStatus::Inherited,
            policy_notes: Vec::new(),
        }
    }

    fn test_child_supervision_task(agent_id: &str, task_id: &str) -> TaskRecord {
        let now = chrono::Utc::now();
        TaskRecord {
            id: task_id.to_string(),
            agent_id: agent_id.to_string(),
            kind: crate::types::TaskKind::ChildAgentTask,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        }
    }

    async fn wait_for_terminal_task(runtime: &RuntimeHandle, task_id: &str) -> TaskRecord {
        for _ in 0..100 {
            let task = runtime
                .storage()
                .latest_task_record(task_id)
                .unwrap()
                .expect("task should remain persisted");
            if matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
            ) {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for task {task_id} to become terminal");
    }

    async fn invoke_new_subagent(
        runtime: &RuntimeHandle,
        message: String,
        authority_class: AuthorityClass,
        template: Option<String>,
        model_request: Option<crate::types::AgentModelRequest>,
    ) -> anyhow::Result<crate::types::AgentInvocationReceipt> {
        let model_resolution = runtime
            .resolve_agent_model_request("InvokeAgent", model_request)
            .await?;
        runtime
            .agent_invocation_service()
            .invoke(InvokeAgentRequest {
                target: InvokeAgentTarget::NewSubagent {
                    template,
                    workspace_mode: ChildAgentWorkspaceMode::Inherit,
                    model_resolution: Some(model_resolution),
                },
                message,
                authority_class,
            })
            .await
    }

    struct BlockingProvider {
        started: Arc<Notify>,
    }

    struct AbortObserved(Arc<AtomicBool>);

    impl Drop for AbortObserved {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl AgentProvider for BlockingProvider {
        async fn complete_turn(
            &self,
            _request: ProviderTurnRequest,
        ) -> anyhow::Result<ProviderTurnResponse> {
            self.started.notify_waiters();
            std::future::pending::<anyhow::Result<ProviderTurnResponse>>().await
        }
    }

    fn provider_test_config(anthropic_token: Option<&str>) -> ProviderConfigFixture {
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let home_path = home.path().to_path_buf();
        let workspace_path = workspace.path().to_path_buf();
        let config = AppConfig {
            default_agent_id: "default".into(),
            http_addr: "127.0.0.1:0".into(),
            callback_base_url: "http://127.0.0.1:0".into(),
            user_home_dir: None,
            home_dir: home_path.clone(),
            data_dir: home_path.clone(),
            socket_path: home_path.join("run").join("holon.sock"),
            workspace_dir: workspace_path,
            context_window_messages: 8,
            context_window_briefs: 8,
            compaction_trigger_messages: 10,
            compaction_keep_recent_messages: 4,
            prompt_budget_estimated_tokens: 4096,
            compaction_trigger_estimated_tokens: 2048,
            compaction_keep_recent_estimated_tokens: 768,
            recent_episode_candidates: 12,
            max_relevant_episodes: 3,
            control_token: Some("secret".into()),
            control_auth_mode: ControlAuthMode::Auto,
            auth: Default::default(),
            api_cors: Default::default(),
            api_projection: Default::default(),
            config_file_path: home_path.join("config.json"),
            stored_config: Default::default(),
            default_model: ModelRouteRef::parse_compatible("anthropic/claude-sonnet-5").unwrap(),
            fallback_models: Vec::new(),
            vision_model: None,
            image_generation_model: None,
            vision_candidate_models: Vec::new(),
            runtime_max_output_tokens: 8192,
            default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
            max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
            disable_provider_fallback: false,
            tui_alternate_screen: crate::config::AltScreenMode::Auto,
            validated_model_overrides: std::collections::HashMap::new(),
            validated_unknown_model_fallback: None,
            model_discovery_cache: Default::default(),
            providers: provider_registry_for_tests(
                None,
                anthropic_token,
                PathBuf::from("/tmp/missing-codex-home"),
            ),
            web_config: crate::web::WebConfig::default(),
        };
        ProviderConfigFixture {
            _home: home,
            _workspace: workspace,
            config,
        }
    }

    async fn wait_for_brief_count(runtime: &RuntimeHandle, expected: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if runtime.storage().read_recent_briefs(16).unwrap().len() >= expected {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {expected} briefs"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn public_named_agents_require_explicit_creation() {
        let (_home, host) = test_host();

        let error = host
            .get_public_agent("release-bot")
            .await
            .err()
            .expect("missing public agent should not be auto-created");
        assert!(error.to_string().contains("not found"));

        let created = host.create_named_agent("release-bot", None).await.unwrap();
        assert_eq!(created.kind, AgentKind::Named);
        assert_eq!(created.visibility, AgentVisibility::Public);
        assert_eq!(created.ownership(), AgentOwnership::SelfOwned);
        assert_eq!(created.profile_preset(), AgentProfilePreset::PublicNamed);
        let relations = host
            .runtime_db()
            .agent_canonical_relations()
            .latest("release-bot")
            .unwrap()
            .unwrap();
        assert_eq!(
            relations.sources.durability,
            Some(crate::types::AgentCanonicalValueSource::Canonical)
        );
        assert_eq!(
            relations.sources.lifecycle_attachment,
            Some(crate::types::AgentCanonicalValueSource::Canonical)
        );
        assert_eq!(
            relations.sources.capability_policy,
            Some(crate::types::AgentCanonicalValueSource::Canonical)
        );
        assert_eq!(
            relations.sources.message_policy,
            Some(crate::types::AgentCanonicalValueSource::Canonical)
        );
        let agent_home = host.agent_data_dir("release-bot");
        assert!(agent_home.join("AGENTS.md").is_file());
        assert!(std::fs::read_to_string(agent_home.join("AGENTS.md"))
            .unwrap()
            .contains("## Holon Agent Home"));
        assert!(agent_home.join("memory/self.md").is_file());
        assert!(agent_home.join("memory/operator.md").is_file());
        assert!(agent_home.join("notes").is_dir());
        assert!(agent_home.join("work").is_dir());
        assert!(agent_home.join("tmp").is_dir());
        assert!(agent_home.join("skills").is_dir());
        assert!(agent_home.join(".holon/state").is_dir());
        assert!(agent_home.join(".holon/ledger").is_dir());
        assert!(agent_home.join(".holon/indexes").is_dir());
        assert!(agent_home.join(".holon/cache").is_dir());

        let runtime = host.get_public_agent("release-bot").await.unwrap();
        assert_eq!(
            runtime.agent_summary().await.unwrap().identity.agent_id,
            "release-bot"
        );

        let listed = host
            .list_agents()
            .await
            .unwrap()
            .into_iter()
            .map(|summary| summary.identity.agent_id)
            .collect::<Vec<_>>();
        assert!(listed.contains(&host.config().default_agent_id));
        assert!(listed.contains(&"release-bot".to_string()));
    }

    #[tokio::test]
    async fn named_agent_template_resolution_uses_user_home_not_config_home() {
        struct HomeGuard(Option<String>);

        impl Drop for HomeGuard {
            fn drop(&mut self) {
                match &self.0 {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
            }
        }

        let config_home = tempdir().unwrap();
        let os_home = tempdir().unwrap();
        write_test_model_config(config_home.path());

        let worker = os_home
            .path()
            .join(".agents")
            .join("agent_templates")
            .join("worker");
        fs::create_dir_all(&worker).unwrap();
        fs::write(
            worker.join("AGENTS.md"),
            "# User worker\n\nfrom user home\n",
        )
        .unwrap();

        let _home_guard = HomeGuard(std::env::var("HOME").ok());
        std::env::set_var("HOME", os_home.path());
        let config = AppConfig::load_with_home(Some(config_home.path().to_path_buf())).unwrap();
        let host =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();

        host.create_named_agent("worker-bot", Some("worker"))
            .await
            .unwrap();

        let agents_md =
            fs::read_to_string(host.agent_data_dir("worker-bot").join("AGENTS.md")).unwrap();
        assert!(agents_md.starts_with("# User worker\n\nfrom user home\n"));
    }

    #[tokio::test]
    async fn public_named_agent_degraded_template_can_be_repaired_without_recreation() {
        let (home, host) = test_host();

        let created = host
            .create_public_named_agent("late-template", Some("late"), None, None)
            .await
            .unwrap();
        assert_eq!(created.receipt.stage, AgentCreateStage::Degraded);
        assert_eq!(
            created.receipt.bootstrap.template.status,
            AgentBootstrapStepStatus::Failed
        );
        assert_eq!(created.identity.agent_id, "late-template");
        assert!(host.public_agent_detail("late-template").is_ok());
        assert!(
            host.get_public_agent("late-template").await.is_ok(),
            "a template failure must not make the committed Agent unusable"
        );

        let template_dir = home.path().join(".agents/agent_templates/late");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(
            template_dir.join("AGENTS.md"),
            "# Late template\n\nrepaired\n",
        )
        .unwrap();

        let repaired = host.repair_public_agent("late-template").await.unwrap();
        assert_eq!(
            repaired.bootstrap.as_ref().unwrap().status,
            AgentBootstrapStatus::Ready
        );
        assert_eq!(repaired.identity.agent_id, created.identity.agent_id);
        assert!(
            fs::read_to_string(host.agent_data_dir("late-template").join("AGENTS.md"))
                .unwrap()
                .starts_with("# Late template\n\nrepaired\n")
        );
    }

    #[tokio::test]
    async fn public_named_agent_repair_does_not_overwrite_user_agents_md() {
        let (home, host) = test_host();
        let created = host
            .create_public_named_agent("dirty-template", Some("late"), None, None)
            .await
            .unwrap();
        assert_eq!(created.receipt.stage, AgentCreateStage::Degraded);

        let agents_md = host.agent_data_dir("dirty-template").join("AGENTS.md");
        fs::write(&agents_md, "# User instructions\n\nkeep me\n").unwrap();
        let template_dir = home.path().join(".agents/agent_templates/late");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "# Runtime template\n").unwrap();

        let repaired = host.repair_public_agent("dirty-template").await.unwrap();
        let bootstrap = repaired.bootstrap.as_ref().unwrap();
        assert_eq!(bootstrap.status, AgentBootstrapStatus::Degraded);
        assert_eq!(bootstrap.template.status, AgentBootstrapStepStatus::Failed);
        assert!(bootstrap
            .template
            .last_error
            .as_deref()
            .unwrap()
            .contains("refuses to overwrite user content"));
        assert_eq!(
            fs::read_to_string(agents_md).unwrap(),
            "# User instructions\n\nkeep me\n"
        );
    }

    #[tokio::test]
    async fn unloaded_list_agent_entries_reads_agent_state_from_db_without_agent_json() {
        let home = tempdir().unwrap();
        write_test_model_config(home.path());
        let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        let runtime_db =
            RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())
                .unwrap();
        let identity = AgentIdentityRecord::new(
            "release-bot",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        runtime_db.agent_identities().upsert(&identity).unwrap();
        let mut state = AgentState::new("release-bot");
        state.status = AgentStatus::Asleep;
        runtime_db.agent_states().upsert(&state).unwrap();

        let host =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("ok"))).unwrap();
        assert!(!host
            .agent_data_dir("release-bot")
            .join(".holon/state/agent.json")
            .exists());

        let entry = host
            .list_agent_entries()
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.identity.agent_id == "release-bot")
            .expect("release-bot should be listed from DB-only state");
        assert_eq!(entry.status, AgentStatus::Asleep);
    }

    #[tokio::test]
    async fn unloaded_operator_agent_state_projection_reads_storage_without_starting_runtime() {
        let (_home, host) = test_host();
        let agent_id = host.config().default_agent_id.clone();
        let mut state = AgentState::new(&agent_id);
        state.status = AgentStatus::Asleep;
        host.runtime_db().agent_states().upsert(&state).unwrap();

        assert!(host.inner.runtimes.read().await.agents.is_empty());
        assert!(!host
            .agent_data_dir(&agent_id)
            .join(".holon/state/agent.json")
            .exists());

        let projection = host
            .operator_agent_state_projection(&agent_id, 10, 10)
            .await
            .unwrap();

        assert_eq!(projection.source, AgentStateProjectionSource::Storage);
        assert_eq!(projection.agent.agent.status, AgentStatus::Asleep);
        assert!(host.inner.runtimes.read().await.agents.is_empty());
        assert!(!host
            .agent_data_dir(&agent_id)
            .join(".holon/state/agent.json")
            .exists());
    }

    #[tokio::test]
    async fn loaded_operator_agent_state_projection_prefers_runtime() {
        let (_home, host) = test_host();
        let agent_id = host.config().default_agent_id.clone();
        host.default_runtime().await.unwrap();

        let projection = host
            .operator_agent_state_projection(&agent_id, 10, 10)
            .await
            .unwrap();

        assert_eq!(projection.source, AgentStateProjectionSource::Loaded);
    }

    #[tokio::test]
    async fn external_ingress_stopped_gate_reads_agent_state_from_db_without_agent_json() {
        let home = tempdir().unwrap();
        write_test_model_config(home.path());
        let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        let runtime_db =
            RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())
                .unwrap();
        let identity = AgentIdentityRecord::new(
            "release-bot",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        runtime_db.agent_identities().upsert(&identity).unwrap();
        let mut state = AgentState::new("release-bot");
        state.status = AgentStatus::Stopped;
        runtime_db.agent_states().upsert(&state).unwrap();

        let host =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("ok"))).unwrap();
        assert!(!host
            .agent_data_dir("release-bot")
            .join(".holon/state/agent.json")
            .exists());

        let error = match host
            .get_public_agent_for_external_ingress("release-bot")
            .await
        {
            Ok(_) => panic!("stopped DB agent state should reject external ingress"),
            Err(error) => error,
        };
        assert!(matches!(error, PublicAgentError::Stopped { .. }));
    }

    #[tokio::test]
    async fn default_runtime_materializes_default_agent_template() {
        let (_home, host) = test_host();
        let runtime = host.default_runtime().await.unwrap();
        let agent_home = host.agent_data_dir(&host.config().default_agent_id);

        assert!(agent_home.join("AGENTS.md").is_file());
        assert!(std::fs::read_to_string(agent_home.join("AGENTS.md"))
            .unwrap()
            .contains("Holon Default Agent"));
        assert!(std::fs::read_to_string(agent_home.join("AGENTS.md"))
            .unwrap()
            .contains("## Holon Agent Home"));
        assert!(agent_home.join("memory/self.md").is_file());
        assert!(agent_home.join("memory/operator.md").is_file());
        assert!(agent_home.join("notes").is_dir());
        assert!(agent_home.join("work").is_dir());
        assert!(agent_home.join("tmp").is_dir());
        assert!(agent_home.join("skills").is_dir());
        assert!(!agent_home.join(".holon/state/agent.json").exists());
        assert!(agent_home.join(".holon/ledger").is_dir());
        assert!(!agent_home.join("agent.json").exists());
        let provenance: crate::agent_template::TemplateProvenanceRecord = serde_json::from_slice(
            &std::fs::read(crate::agent_template::template_provenance_path(&agent_home)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            provenance.selector,
            crate::agent_template::DEFAULT_AGENT_TEMPLATE_ID
        );
        assert_eq!(
            runtime.agent_summary().await.unwrap().identity.agent_id,
            host.config().default_agent_id
        );
    }

    #[tokio::test]
    async fn default_runtime_does_not_overwrite_existing_default_agents_md() {
        let (_home, host) = test_host();
        let agent_home = host.agent_data_dir(&host.config().default_agent_id);
        std::fs::create_dir_all(&agent_home).unwrap();
        std::fs::write(agent_home.join("AGENTS.md"), "custom default").unwrap();

        let _runtime = host.default_runtime().await.unwrap();

        assert_eq!(
            std::fs::read_to_string(agent_home.join("AGENTS.md")).unwrap(),
            "custom default"
        );
    }

    #[test]
    fn agent_data_dir_uses_agents_directory_without_sessions_compat() {
        let (_home, host) = test_host();
        let legacy = host
            .config()
            .data_dir
            .join("sessions")
            .join(&host.config().default_agent_id);
        std::fs::create_dir_all(&legacy).unwrap();

        assert_eq!(
            host.agent_data_dir(&host.config().default_agent_id),
            host.config()
                .data_dir
                .join("agents")
                .join(&host.config().default_agent_id)
        );
    }

    #[tokio::test]
    async fn public_named_create_returns_receipt_and_rejects_duplicate() {
        let (_home, host) = test_host();

        let created = host
            .create_public_named_agent_with_name(
                "receipt-bot",
                None,
                None,
                None,
                Some("Receipt Bot"),
            )
            .await
            .unwrap();
        assert_eq!(created.identity.agent_id, "receipt-bot");
        assert_eq!(created.identity.name.as_deref(), Some("Receipt Bot"));
        assert_eq!(created.receipt.name.as_deref(), Some("Receipt Bot"));
        assert_eq!(created.receipt.display_name, "Receipt Bot");
        assert_eq!(created.receipt.agent_id, "receipt-bot");
        assert_eq!(created.receipt.stage, AgentCreateStage::Bootstrapped);
        assert_eq!(created.receipt.lifecycle, AgentRegistryStatus::Active);
        assert!(created.receipt.created);
        assert!(!created.receipt.receipt_id.is_empty());

        let detail = host.public_agent_detail("receipt-bot").unwrap();
        assert_eq!(detail.display_name, "Receipt Bot");
        assert_eq!(detail.name.as_deref(), Some("Receipt Bot"));

        let renamed = host
            .rename_public_agent("receipt-bot", "Receipt Worker", "test-operator")
            .unwrap();
        assert_eq!(renamed.identity.agent_id, "receipt-bot");
        assert_eq!(renamed.name.as_deref(), Some("Receipt Worker"));
        assert_eq!(renamed.display_name, "Receipt Worker");
        assert_eq!(
            host.agent_identity_record("receipt-bot")
                .unwrap()
                .unwrap()
                .name
                .as_deref(),
            Some("Receipt Worker")
        );

        let duplicate = host
            .create_public_named_agent_with_name(
                "other-receipt-bot",
                None,
                None,
                None,
                Some("Receipt Worker"),
            )
            .await
            .expect_err("duplicate display names must fail closed");
        assert!(duplicate.to_string().contains("already_exists"));

        host.create_public_named_agent_with_name("unicode-name", None, None, None, Some("Straße"))
            .await
            .unwrap();
        let unicode_duplicate = host
            .create_public_named_agent_with_name(
                "other-unicode-name",
                None,
                None,
                None,
                Some("STRASSE"),
            )
            .await
            .expect_err("Unicode-equivalent display names must fail closed");
        assert!(unicode_duplicate.to_string().contains("already_exists"));

        let error = host
            .create_public_named_agent("receipt-bot", None, None, None)
            .await
            .expect_err("duplicate public named creation must fail closed");
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "already_exists");
        assert_eq!(
            tool_error.domain,
            Some(crate::runtime_error::RuntimeErrorDomain::Conflict)
        );
    }

    #[tokio::test]
    async fn concurrent_public_named_create_has_one_core_commit_winner() {
        let (_home, host) = test_host();
        let left_host = host.clone();
        let right_host = host.clone();
        let (left, right) = tokio::join!(
            left_host.create_public_named_agent("create-race", None, None, None),
            right_host.create_public_named_agent("create-race", None, None, None)
        );

        let successes = usize::from(left.is_ok()) + usize::from(right.is_ok());
        assert_eq!(successes, 1);
        let error = left.err().or_else(|| right.err()).unwrap();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "already_exists");
        assert_eq!(
            tool_error.domain,
            Some(crate::runtime_error::RuntimeErrorDomain::Conflict)
        );
        let bootstrap = host
            .runtime_db()
            .agent_bootstraps()
            .latest("create-race")
            .unwrap()
            .unwrap();
        assert_eq!(bootstrap.summary().status, AgentBootstrapStatus::Ready);
    }

    #[tokio::test]
    async fn deleted_public_agent_detail_retains_name_and_rename_is_rejected() {
        let (_home, host) = test_host();
        let _created = host
            .create_public_named_agent_with_name(
                "delete-named",
                None,
                None,
                None,
                Some("Retained Name"),
            )
            .await
            .unwrap();

        let (_identity, job, created_deletion) = host
            .begin_public_agent_deletion("delete-named", false, "test-operator")
            .await
            .unwrap();
        assert!(created_deletion);

        let deleting_detail = host.public_agent_detail("delete-named").unwrap();
        assert_eq!(deleting_detail.name.as_deref(), Some("Retained Name"));
        assert_eq!(deleting_detail.display_name, "Retained Name");
        assert!(deleting_detail.deletion.is_some());

        let rename_error = host
            .rename_public_agent("delete-named", "New Name", "test-operator")
            .expect_err("deleting agent must not be renamed");
        assert!(matches!(
            rename_error,
            PublicAgentError::Deleting { ref agent_id } if agent_id == "delete-named"
        ));
        let repair_error = host
            .repair_public_agent("delete-named")
            .await
            .expect_err("deleting agent must not accept bootstrap repair");
        assert!(matches!(
            repair_error,
            PublicAgentError::Deleting { ref agent_id } if agent_id == "delete-named"
        ));

        host.execute_deletion_job(job).await.unwrap();
        let deleted_detail = host.public_agent_detail("delete-named").unwrap();
        assert_eq!(deleted_detail.name.as_deref(), Some("Retained Name"));
        assert_eq!(deleted_detail.display_name, "Retained Name");
        assert!(matches!(
            deleted_detail.identity.status,
            AgentRegistryStatus::Deleted
        ));

        let rename_error = host
            .rename_public_agent("delete-named", "New Name", "test-operator")
            .expect_err("deleted agent must not be renamed");
        assert!(matches!(
            rename_error,
            PublicAgentError::Deleted { ref agent_id } if agent_id == "delete-named"
        ));
    }

    #[tokio::test]
    async fn agent_face_create_reincarnates_fully_deleted_agent_id() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();

        let created = parent
            .agent_creation_service()
            .create(CreateAgentRequest {
                agent_id: "reborn-tool".into(),
                name: Some("First Incarnation".into()),
                template: None,
                initial_message: None,
                authority_class: AuthorityClass::OperatorInstruction,
                model_resolution: None,
                lineage_parent_agent_id: None,
                inherit_parent_runtime: false,
            })
            .await
            .unwrap();
        assert!(created.receipt.created);
        assert_eq!(created.identity.incarnation, 1);

        let (_identity, job, created_deletion) = host
            .begin_public_agent_deletion("reborn-tool", false, "test-operator")
            .await
            .unwrap();
        assert!(created_deletion);
        host.execute_deletion_job(job).await.unwrap();
        assert!(matches!(
            host.public_agent_detail("reborn-tool")
                .unwrap()
                .identity
                .status,
            AgentRegistryStatus::Deleted
        ));

        // While the deletion job is still pending (not executed), create
        // must fail closed with the typed deletion_incomplete error. Drive
        // this on a second agent whose job is never executed.
        host.create_public_named_agent_with_name("reborn-pending", None, None, None, None)
            .await
            .unwrap();
        host.begin_public_agent_deletion("reborn-pending", false, "test-operator")
            .await
            .unwrap();
        let pending_error = parent
            .agent_creation_service()
            .create(CreateAgentRequest {
                agent_id: "reborn-pending".into(),
                name: None,
                template: None,
                initial_message: None,
                authority_class: AuthorityClass::OperatorInstruction,
                model_resolution: None,
                lineage_parent_agent_id: None,
                inherit_parent_runtime: false,
            })
            .await
            .expect_err("create must fail closed while deletion is incomplete");
        assert!(
            pending_error.to_string().contains("deletion_incomplete"),
            "unexpected error: {pending_error:#}"
        );

        // The released id creates a brand-new incarnation.
        let recreated = parent
            .agent_creation_service()
            .create(CreateAgentRequest {
                agent_id: "reborn-tool".into(),
                name: Some("Second Incarnation".into()),
                template: None,
                initial_message: None,
                authority_class: AuthorityClass::OperatorInstruction,
                model_resolution: None,
                lineage_parent_agent_id: None,
                inherit_parent_runtime: false,
            })
            .await
            .unwrap();
        assert!(recreated.receipt.created);
        assert_eq!(recreated.identity.incarnation, 2);
        assert_eq!(
            recreated.identity.name.as_deref(),
            Some("Second Incarnation")
        );
    }

    #[tokio::test]
    async fn spawn_public_named_rejects_existing_agent_without_side_effects() {
        let fixture = provider_test_config(Some("dummy-token"));
        let host = RuntimeHost::new(fixture.config).unwrap();
        let parent = host.default_runtime().await.unwrap();
        parent
            .set_model_override(
                ModelRouteRef::parse_compatible("anthropic/claude-haiku-4-5").unwrap(),
                None,
            )
            .await
            .unwrap();

        host.create_named_agent("release-bot", None).await.unwrap();
        let named = host.get_public_agent("release-bot").await.unwrap();
        let before = named.agent_summary().await.unwrap();
        assert!(before.agent.model_override.is_none());

        let error = host
            .spawn_public_named_agent(
                parent,
                "release-bot",
                Some("continue release work".into()),
                AuthorityClass::OperatorInstruction,
                None,
                inherited_model_resolution("anthropic", "claude-sonnet-4-6"),
            )
            .await
            .expect_err("public named spawn must not reuse an existing agent");

        let after = named.agent_summary().await.unwrap();
        assert!(
            after.agent.model_override.is_none(),
            "existing public named agent should keep its own runtime state"
        );
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "already_exists");
        assert_eq!(
            tool_error.details,
            Some(json!({
                "agent_id": "release-bot",
                "preset": AgentProfilePreset::PublicNamed,
            }))
        );
        assert!(
            named.storage().read_recent_messages(10).unwrap().is_empty(),
            "duplicate creation must not inject a follow-up message"
        );
    }

    #[tokio::test]
    async fn spawn_public_named_rejects_existing_default_agent_without_side_effects() {
        let fixture = provider_test_config(Some("dummy-token"));
        let host = RuntimeHost::new(fixture.config).unwrap();
        let parent = host.default_runtime().await.unwrap();
        let before = parent.agent_summary().await.unwrap();
        assert!(before.agent.model_override.is_none());

        let error = host
            .spawn_public_named_agent(
                parent.clone(),
                &host.config().default_agent_id,
                Some("do not inject this".into()),
                AuthorityClass::OperatorInstruction,
                None,
                inherited_model_resolution("anthropic", "claude-sonnet-4-6"),
            )
            .await
            .expect_err("public named spawn must not reuse the default agent");

        let after = parent.agent_summary().await.unwrap();
        assert!(after.agent.model_override.is_none());
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "already_exists");
        assert_eq!(
            tool_error.details,
            Some(json!({
                "agent_id": host.config().default_agent_id,
                "preset": AgentProfilePreset::PublicNamed,
            }))
        );
        assert!(
            parent
                .storage()
                .read_recent_messages(10)
                .unwrap()
                .is_empty(),
            "duplicate creation must not inject a follow-up message into the default agent"
        );
    }

    #[tokio::test]
    async fn canonical_create_reuses_committed_agent_without_supervision_or_reconfiguration() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
        let parent_agent_id = parent.agent_summary().await.unwrap().identity.agent_id;

        let created = parent
            .agent_creation_service()
            .create(CreateAgentRequest {
                agent_id: "canonical-create".into(),
                name: Some("Canonical Create".into()),
                template: None,
                initial_message: Some("initial work".into()),
                authority_class: AuthorityClass::OperatorInstruction,
                model_resolution: Some(inherited_model_resolution(
                    "anthropic",
                    "claude-sonnet-4-6",
                )),
                lineage_parent_agent_id: Some(parent_agent_id),
                inherit_parent_runtime: true,
            })
            .await
            .unwrap();
        assert!(created.receipt.created);
        assert!(
            parent.storage().latest_task_records().unwrap().is_empty(),
            "independent create must not create a supervision task"
        );

        let target = host.get_public_agent("canonical-create").await.unwrap();
        let before = target.agent_summary().await.unwrap();
        let before_bootstrap = host
            .runtime_db()
            .agent_bootstraps()
            .latest("canonical-create")
            .unwrap()
            .unwrap();
        let before_messages = target.storage().read_recent_messages(100).unwrap().len();

        let duplicate = parent
            .agent_creation_service()
            .create(CreateAgentRequest {
                agent_id: "canonical-create".into(),
                name: Some("Ignored Rename".into()),
                template: Some("ignored-template".into()),
                initial_message: Some("must not be delivered".into()),
                authority_class: AuthorityClass::ExternalEvidence,
                model_resolution: Some(inherited_model_resolution("openai", "gpt-5.4")),
                lineage_parent_agent_id: None,
                inherit_parent_runtime: false,
            })
            .await
            .unwrap();
        assert!(!duplicate.receipt.created);

        let after = target.agent_summary().await.unwrap();
        let after_bootstrap = host
            .runtime_db()
            .agent_bootstraps()
            .latest("canonical-create")
            .unwrap()
            .unwrap();
        assert_eq!(after.identity, before.identity);
        assert_eq!(after.agent.model_override, before.agent.model_override);
        assert_eq!(
            after.agent.model_override_reasoning_effort,
            before.agent.model_override_reasoning_effort
        );
        assert_eq!(
            after.agent.attached_workspaces,
            before.agent.attached_workspaces
        );
        assert_eq!(after_bootstrap.desired, before_bootstrap.desired);
        assert_eq!(
            target.storage().read_recent_messages(100).unwrap().len(),
            before_messages,
            "duplicate create must not deliver its initial message"
        );
        assert!(
            parent.storage().latest_task_records().unwrap().is_empty(),
            "duplicate create must not create an invocation task"
        );
    }

    #[tokio::test]
    async fn canonical_new_subagent_rejects_missing_model_before_task_creation() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
        let tasks_before = parent.storage().latest_task_records().unwrap();

        let error = parent
            .agent_invocation_service()
            .invoke(InvokeAgentRequest {
                target: InvokeAgentTarget::NewSubagent {
                    template: None,
                    workspace_mode: ChildAgentWorkspaceMode::Inherit,
                    model_resolution: None,
                },
                message: "invalid invocation".into(),
                authority_class: AuthorityClass::OperatorInstruction,
            })
            .await
            .expect_err("missing model resolution must reject the invocation");

        assert_eq!(
            error.to_string(),
            "new subagent invocation requires model resolution"
        );
        assert_eq!(
            parent.storage().latest_task_records().unwrap(),
            tasks_before,
            "invalid invocation must not persist an orphaned queued task"
        );
    }

    #[tokio::test]
    async fn canonical_existing_invocation_preserves_target_configuration_and_relations() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
        let parent_agent_id = parent.agent_summary().await.unwrap().identity.agent_id;
        parent
            .agent_creation_service()
            .create(CreateAgentRequest {
                agent_id: "canonical-existing".into(),
                name: None,
                template: None,
                initial_message: None,
                authority_class: AuthorityClass::OperatorInstruction,
                model_resolution: Some(inherited_model_resolution(
                    "anthropic",
                    "claude-sonnet-4-6",
                )),
                lineage_parent_agent_id: Some(parent_agent_id.clone()),
                inherit_parent_runtime: true,
            })
            .await
            .unwrap();
        let target = host.get_public_agent("canonical-existing").await.unwrap();
        let before_summary = target.agent_summary().await.unwrap();
        let before_relations = host
            .operator_agent_detail("canonical-existing")
            .unwrap()
            .canonical_relations;
        let receipt = parent
            .agent_invocation_service()
            .invoke(InvokeAgentRequest {
                target: InvokeAgentTarget::ExistingAgent {
                    agent_id: "canonical-existing".into(),
                },
                message: "continue existing work".into(),
                authority_class: AuthorityClass::ExternalEvidence,
            })
            .await
            .unwrap();

        assert!(!receipt.created);
        assert_eq!(receipt.agent_id, "canonical-existing");
        assert_eq!(receipt.task_handle.task_kind, ACTOR_INVOCATION_TASK_KIND);
        let task = parent
            .storage()
            .latest_task_record(&receipt.task_handle.task_id)
            .unwrap()
            .unwrap();
        assert_eq!(task.kind, TaskKind::ActorInvocation);

        let after_summary = target.agent_summary().await.unwrap();
        let after_relations = host
            .operator_agent_detail("canonical-existing")
            .unwrap()
            .canonical_relations;
        assert_eq!(after_summary.identity, before_summary.identity);
        assert_eq!(
            after_summary.agent.model_override,
            before_summary.agent.model_override
        );
        assert_eq!(
            after_summary.agent.model_override_reasoning_effort,
            before_summary.agent.model_override_reasoning_effort
        );
        assert_eq!(
            after_summary.agent.attached_workspaces,
            before_summary.agent.attached_workspaces
        );
        assert_eq!(
            after_summary.agent.worktree_session,
            before_summary.agent.worktree_session
        );
        assert_eq!(after_relations, before_relations);

        let terminal = wait_for_terminal_task(&parent, &receipt.task_handle.task_id).await;
        assert_eq!(terminal.status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn canonical_new_subagent_remains_reusable_after_invocation_and_restart() {
        let (_home, host) = test_host();
        let config = host.config().as_ref().clone();
        let parent = host.default_runtime().await.unwrap();

        let created = parent
            .agent_invocation_service()
            .invoke(InvokeAgentRequest {
                target: InvokeAgentTarget::NewSubagent {
                    template: None,
                    workspace_mode: ChildAgentWorkspaceMode::Inherit,
                    model_resolution: Some(inherited_model_resolution("openai", "gpt-5.4")),
                },
                message: "first invocation".into(),
                authority_class: AuthorityClass::OperatorInstruction,
            })
            .await
            .unwrap();
        assert!(created.created);
        assert_eq!(created.task_handle.task_kind, ACTOR_INVOCATION_TASK_KIND);
        let first_terminal = wait_for_terminal_task(&parent, &created.task_handle.task_id).await;
        assert_eq!(first_terminal.status, TaskStatus::Completed);

        let child_identity = host
            .agent_identity_record(&created.agent_id)
            .unwrap()
            .expect("new subagent identity should remain recorded");
        assert_eq!(child_identity.status, AgentRegistryStatus::Active);
        assert_eq!(child_identity.durability, Some(AgentDurability::Ephemeral));
        assert!(host.agent_data_dir(&created.agent_id).exists());

        let reused = parent
            .agent_invocation_service()
            .invoke(InvokeAgentRequest {
                target: InvokeAgentTarget::ExistingAgent {
                    agent_id: created.agent_id.clone(),
                },
                message: "second invocation".into(),
                authority_class: AuthorityClass::OperatorInstruction,
            })
            .await
            .unwrap();
        assert!(!reused.created);
        assert_eq!(reused.agent_id, created.agent_id);
        let second_terminal = wait_for_terminal_task(&parent, &reused.task_handle.task_id).await;
        assert_eq!(second_terminal.status, TaskStatus::Completed);

        host.shutdown().await.unwrap();
        drop(host);
        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        let restarted_identity = restarted
            .agent_identity_record(&created.agent_id)
            .unwrap()
            .expect("terminal invocation must not delete its target during restart convergence");
        assert_eq!(restarted_identity.status, AgentRegistryStatus::Active);
        assert!(restarted.agent_data_dir(&created.agent_id).exists());
    }

    #[tokio::test]
    async fn existing_agent_samples_child_turn_before_delivery_admission() {
        struct DeliveryCheckpointGuard;

        impl Drop for DeliveryCheckpointGuard {
            fn drop(&mut self) {
                crate::runtime::release_delivery_checkpoint();
            }
        }

        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
        let created = parent
            .agent_invocation_service()
            .invoke(InvokeAgentRequest {
                target: InvokeAgentTarget::NewSubagent {
                    template: None,
                    workspace_mode: ChildAgentWorkspaceMode::Inherit,
                    model_resolution: Some(inherited_model_resolution("openai", "gpt-5.4")),
                },
                message: "first invocation".into(),
                authority_class: AuthorityClass::OperatorInstruction,
            })
            .await
            .unwrap();
        let first_terminal = wait_for_terminal_task(&parent, &created.task_handle.task_id).await;
        assert_eq!(first_terminal.status, TaskStatus::Completed);

        let child = host.get_or_create_agent(&created.agent_id).await.unwrap();
        for _ in 0..100 {
            if child.agent_state().await.unwrap().status == AgentStatus::Asleep {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let child_turn_baseline = child.agent_state().await.unwrap().turn_index;

        crate::runtime::enable_delivery_checkpoint(created.agent_id.clone());
        let _checkpoint_guard = DeliveryCheckpointGuard;
        let parent_for_invoke = parent.clone();
        let child_agent_id = created.agent_id.clone();
        let invocation = tokio::spawn(async move {
            parent_for_invoke
                .agent_invocation_service()
                .invoke(InvokeAgentRequest {
                    target: InvokeAgentTarget::ExistingAgent {
                        agent_id: child_agent_id,
                    },
                    message: "second invocation".into(),
                    authority_class: AuthorityClass::OperatorInstruction,
                })
                .await
        });

        crate::runtime::wait_for_delivery_checkpoint().await;
        for _ in 0..100 {
            let state = child.agent_state().await.unwrap();
            if state.turn_index > child_turn_baseline && state.status == AgentStatus::Asleep {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let child_state = child.agent_state().await.unwrap();
        assert!(child_state.turn_index > child_turn_baseline);
        assert_eq!(child_state.status, AgentStatus::Asleep);
        let invocation_turn_index = child_state.turn_index;

        child
            .enqueue(
                MessageEnvelope::new(
                    created.agent_id.clone(),
                    MessageKind::InternalFollowup,
                    MessageOrigin::System {
                        subsystem: "unrelated-test-message".into(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Normal,
                    MessageBody::Text {
                        text: "unrelated turn after invocation".into(),
                    },
                )
                .with_admission(
                    MessageDeliverySurface::RuntimeSystem,
                    AdmissionContext::RuntimeOwned,
                ),
            )
            .await
            .unwrap();
        for _ in 0..100 {
            let state = child.agent_state().await.unwrap();
            if state.turn_index > invocation_turn_index && state.status == AgentStatus::Asleep {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let unrelated_turn_id = child
            .agent_state()
            .await
            .unwrap()
            .last_turn_terminal
            .expect("unrelated turn should complete")
            .turn_id;

        crate::runtime::release_delivery_checkpoint();
        let receipt = invocation.await.unwrap().unwrap();
        let terminal = wait_for_terminal_task(&parent, &receipt.task_handle.task_id).await;
        assert_eq!(terminal.status, TaskStatus::Completed);
        assert_eq!(
            terminal
                .detail
                .as_ref()
                .and_then(|detail| detail.get("child_turn_baseline"))
                .and_then(Value::as_u64),
            Some(child_turn_baseline)
        );
        let detail = terminal.detail.as_ref().unwrap();
        let delivery_id = detail["delivery_id"].as_str().unwrap();
        let delivery = host
            .runtime_db()
            .agent_message_deliveries()
            .latest(delivery_id)
            .unwrap()
            .expect("invocation delivery should remain persisted");
        assert!(delivery.activation_id.is_some());
        assert_eq!(detail["activation_id"], delivery.activation_id.unwrap());
        assert_eq!(detail["turn_id"], delivery.turn_id.unwrap());
        assert_ne!(detail["turn_id"], unrelated_turn_id);
    }

    #[tokio::test]
    async fn concurrent_existing_agent_invocations_keep_distinct_execution_results() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
        let created = parent
            .agent_invocation_service()
            .invoke(InvokeAgentRequest {
                target: InvokeAgentTarget::NewSubagent {
                    template: None,
                    workspace_mode: ChildAgentWorkspaceMode::Inherit,
                    model_resolution: Some(inherited_model_resolution("openai", "gpt-5.4")),
                },
                message: "bootstrap invocation target".into(),
                authority_class: AuthorityClass::OperatorInstruction,
            })
            .await
            .unwrap();
        let bootstrap = wait_for_terminal_task(&parent, &created.task_handle.task_id).await;
        assert_eq!(bootstrap.status, TaskStatus::Completed);

        let first_service = parent.agent_invocation_service();
        let second_service = parent.agent_invocation_service();
        let first = first_service.invoke(InvokeAgentRequest {
            target: InvokeAgentTarget::ExistingAgent {
                agent_id: created.agent_id.clone(),
            },
            message: "first concurrent invocation".into(),
            authority_class: AuthorityClass::OperatorInstruction,
        });
        let second = second_service.invoke(InvokeAgentRequest {
            target: InvokeAgentTarget::ExistingAgent {
                agent_id: created.agent_id.clone(),
            },
            message: "second concurrent invocation".into(),
            authority_class: AuthorityClass::OperatorInstruction,
        });
        let (first, second) = tokio::join!(first, second);
        let first = first.unwrap();
        let second = second.unwrap();
        let first_task = wait_for_terminal_task(&parent, &first.task_handle.task_id).await;
        let second_task = wait_for_terminal_task(&parent, &second.task_handle.task_id).await;
        assert_eq!(first_task.status, TaskStatus::Completed);
        assert_eq!(second_task.status, TaskStatus::Completed);

        let first_detail = first_task.detail.as_ref().unwrap();
        let second_detail = second_task.detail.as_ref().unwrap();
        let first_delivery = host
            .runtime_db()
            .agent_message_deliveries()
            .latest(first_detail["delivery_id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        let second_delivery = host
            .runtime_db()
            .agent_message_deliveries()
            .latest(second_detail["delivery_id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_ne!(first_delivery.delivery_id, second_delivery.delivery_id);
        assert_ne!(first_delivery.activation_id, second_delivery.activation_id);
        assert_ne!(first_delivery.turn_id, second_delivery.turn_id);
        assert_eq!(
            first_detail["activation_id"],
            first_delivery.activation_id.unwrap()
        );
        assert_eq!(first_detail["turn_id"], first_delivery.turn_id.unwrap());
        assert_eq!(
            second_detail["activation_id"],
            second_delivery.activation_id.unwrap()
        );
        assert_eq!(second_detail["turn_id"], second_delivery.turn_id.unwrap());
    }

    #[tokio::test]
    async fn failed_existing_agent_delivery_terminates_without_a_target_turn() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
        let created = parent
            .agent_invocation_service()
            .invoke(InvokeAgentRequest {
                target: InvokeAgentTarget::NewSubagent {
                    template: None,
                    workspace_mode: ChildAgentWorkspaceMode::Inherit,
                    model_resolution: Some(inherited_model_resolution("openai", "gpt-5.4")),
                },
                message: "bootstrap invocation target".into(),
                authority_class: AuthorityClass::OperatorInstruction,
            })
            .await
            .unwrap();
        let bootstrap = wait_for_terminal_task(&parent, &created.task_handle.task_id).await;
        assert_eq!(bootstrap.status, TaskStatus::Completed);
        let child = host.get_or_create_agent(&created.agent_id).await.unwrap();
        let child_turn_baseline = child.agent_state().await.unwrap().turn_index;
        let parent_agent_id = host.config().default_agent_id.clone();

        let task_id = "task-failed-existing-delivery";
        let prepared = crate::runtime::AgentMessageDeliveryService::prepare(
            AgentMessageSendRequest {
                target_agent_id: created.agent_id.clone(),
                content: MessageBody::Text {
                    text: "must fail before execution".into(),
                },
                client_idempotency_key: task_id.into(),
                correlation_id: Some(task_id.into()),
                causation_id: None,
                requested_priority: Some(Priority::Normal),
            },
            AgentMessageCallerContext {
                caller_principal: format!("agent:{parent_agent_id}"),
                caller_agent_id: Some(parent_agent_id),
                principal_kind: AgentMessagePrincipalKind::SupervisingParent,
                route: "supervision_follow_up".into(),
                origin: MessageOrigin::Task {
                    task_id: task_id.into(),
                },
                authority_class: AuthorityClass::OperatorInstruction,
                delivery_surface: MessageDeliverySurface::RuntimeSystem,
                admission_context: AdmissionContext::RuntimeOwned,
                current_turn_id: None,
                current_task_id: Some(task_id.into()),
                current_work_item_id: None,
            },
        )
        .unwrap();
        let now = Utc::now();
        let admitted = host
            .runtime_db()
            .transitions()
            .commit_delivery_admission(
                &crate::runtime_db::transitions::QueueTransitionCommand {
                    agent_id: created.agent_id.clone(),
                    operation: crate::runtime_db::transitions::QueueOperation::Admit,
                    mutation: crate::runtime_db::transitions::QueueMutation::Upsert(
                        crate::types::QueueEntryRecord {
                            message_id: prepared.message.id.clone(),
                            agent_id: created.agent_id.clone(),
                            priority: Priority::Normal,
                            status: QueueEntryStatus::Queued,
                            created_at: now,
                            updated_at: now,
                        },
                    ),
                    scheduler_claim_work_item: None,
                    agent_state: None,
                    message_evidence: vec![prepared.message.clone()],
                    transcript_entries: Vec::new(),
                    turn_record: None,
                    audit_events: Vec::new(),
                    notify_scheduler: false,
                    fault: None,
                    brief_evidence: Vec::new(),
                },
                None,
                &prepared.record,
            )
            .unwrap()
            .delivery_receipt
            .unwrap();
        let queued = host
            .runtime_db()
            .queue_entries()
            .latest(&prepared.message.id)
            .unwrap()
            .unwrap();
        let mut dropped = queued.clone();
        dropped.status = QueueEntryStatus::Dropped;
        dropped.updated_at = Utc::now();
        host.runtime_db()
            .transitions()
            .commit_queue(&crate::runtime_db::transitions::QueueTransitionCommand {
                agent_id: created.agent_id.clone(),
                operation: crate::runtime_db::transitions::QueueOperation::RepairDrop,
                mutation: crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                    expected: queued,
                    record: dropped,
                },
                scheduler_claim_work_item: None,
                agent_state: None,
                message_evidence: Vec::new(),
                transcript_entries: Vec::new(),
                turn_record: None,
                audit_events: Vec::new(),
                notify_scheduler: false,
                fault: None,
                brief_evidence: Vec::new(),
            })
            .unwrap();

        let result = host
            .await_invocation_delivery_terminal_result(
                &created.agent_id,
                &admitted.delivery_id,
                task_id,
                false,
            )
            .await
            .unwrap();
        assert_eq!(result.status, TaskStatus::Failed);
        assert!(result.text.contains("queue processing terminated"));
        assert_eq!(
            child.agent_state().await.unwrap().turn_index,
            child_turn_baseline
        );
        let detail = result.task_detail.unwrap();
        assert_eq!(detail["delivery_id"], admitted.delivery_id);
        assert!(detail["activation_id"].is_null());
        assert!(detail["turn_id"].is_null());
    }

    #[tokio::test]
    async fn spawn_public_named_records_lineage_without_supervision() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();

        let created = host
            .spawn_public_named_agent(
                parent,
                "release-bot",
                Some("coordinate release work".into()),
                AuthorityClass::OperatorInstruction,
                None,
                inherited_model_resolution("anthropic", "claude-sonnet-4-6"),
            )
            .await
            .unwrap();
        assert_eq!(created.identity.agent_id, "release-bot");
        assert_eq!(created.receipt.agent_id, "release-bot");
        assert_eq!(created.receipt.stage, AgentCreateStage::Bootstrapped);
        assert!(created.receipt.created);

        let identity = host
            .agent_identity_record("release-bot")
            .unwrap()
            .expect("public named identity should exist");
        assert_eq!(identity.parent_agent_id, None);
        assert_eq!(identity.delegated_from_task_id, None);
        assert_eq!(
            identity.lineage_parent_agent_id.as_deref(),
            Some(host.config().default_agent_id.as_str())
        );

        let summary = host
            .get_public_agent("release-bot")
            .await
            .unwrap()
            .agent_summary()
            .await
            .unwrap();
        assert_eq!(
            summary.identity.lineage_parent_agent_id.as_deref(),
            Some(host.config().default_agent_id.as_str())
        );
    }

    #[tokio::test]
    async fn operator_agent_tree_nests_supervised_child_and_allows_detail_navigation() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
        let parent_agent_id = host.config().default_agent_id.clone();

        let spawned = invoke_new_subagent(
            &parent,
            "tree navigation work".into(),
            AuthorityClass::OperatorInstruction,
            None,
            None,
        )
        .await
        .unwrap();
        let child_agent_id = spawned.agent_id.clone();
        assert!(!spawned.task_handle.task_id.is_empty());

        let tree = host.operator_agent_tree().await.unwrap();
        assert_eq!(
            tree.roots.len(),
            1,
            "only the default agent should root the operator tree"
        );
        let root = &tree.roots[0];
        assert_eq!(root.agent.identity.agent_id, parent_agent_id);
        assert_eq!(root.children.len(), 1);
        let child = &root.children[0];
        assert_eq!(child.agent.identity.agent_id, child_agent_id);
        assert_eq!(
            child
                .canonical_relations
                .lineage
                .as_ref()
                .expect("child lineage")
                .parent_agent_id,
            parent_agent_id
        );
        assert!(
            child.canonical_relations.supervision.is_some(),
            "private child keeps its supervision attachment in the tree"
        );
        assert!(child.children.is_empty());
        assert_eq!(
            tree.into_agent_entries()
                .iter()
                .filter(|entry| entry.identity.agent_id == child_agent_id)
                .count(),
            1,
            "each active child appears exactly once in the operator tree"
        );

        // The operator can open the private child directly even though peers cannot enumerate it.
        let child_detail = host.operator_agent_detail(&child_agent_id).unwrap();
        assert_eq!(
            child_detail
                .canonical_relations
                .lineage
                .as_ref()
                .unwrap()
                .parent_agent_id,
            parent_agent_id
        );
        let parent_detail = host.operator_agent_detail(&parent_agent_id).unwrap();
        assert!(parent_detail
            .lineage_children
            .iter()
            .any(|record| record.child_agent_id == child_agent_id));
    }

    #[tokio::test]
    async fn operator_agent_tree_keeps_unsupervised_public_named_lineage() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
        let parent_agent_id = host.config().default_agent_id.clone();

        host.spawn_public_named_agent(
            parent,
            "release-bot",
            Some("coordinate release work".into()),
            AuthorityClass::OperatorInstruction,
            None,
            inherited_model_resolution("anthropic", "claude-sonnet-4-6"),
        )
        .await
        .unwrap();

        let tree = host.operator_agent_tree().await.unwrap();
        assert_eq!(tree.roots.len(), 1);
        let root = &tree.roots[0];
        assert_eq!(root.agent.identity.agent_id, parent_agent_id);
        let child = root
            .children
            .iter()
            .find(|node| node.agent.identity.agent_id == "release-bot")
            .expect("detached public named agent should stay nested under its lineage parent");
        assert!(
            child.canonical_relations.supervision.is_none(),
            "public named agents keep lineage without a supervision attachment"
        );
    }

    #[tokio::test]
    async fn operator_agent_tree_hides_deleted_agent_but_keeps_history() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
        host.spawn_public_named_agent(
            parent,
            "gone-bot",
            Some("temporary work".into()),
            AuthorityClass::OperatorInstruction,
            None,
            inherited_model_resolution("anthropic", "claude-sonnet-4-6"),
        )
        .await
        .unwrap();

        let (_, job, _) = host
            .begin_public_agent_deletion("gone-bot", false, "operator")
            .await
            .unwrap();
        host.execute_deletion_job(job).await.unwrap();

        let tree = host.operator_agent_tree().await.unwrap();
        assert!(
            !tree
                .into_agent_entries()
                .iter()
                .any(|entry| entry.identity.agent_id == "gone-bot"),
            "deleted identities must be hidden from the operator tree"
        );
        assert!(
            host.runtime_db()
                .agent_canonical_relations()
                .latest("gone-bot")
                .unwrap()
                .is_some(),
            "canonical relation history must remain queryable after deletion"
        );
    }

    #[tokio::test]
    async fn private_child_initial_message_sets_task_label_and_supervision_provenance() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
        let parent_agent_id = parent.agent_state().await.unwrap().id;
        let initial_message = "  investigate   remote\nTUI  access ".to_string();

        let spawned = invoke_new_subagent(
            &parent,
            initial_message.clone(),
            AuthorityClass::ExternalEvidence,
            None,
            None,
        )
        .await
        .unwrap();
        let task_id = spawned.task_handle.task_id.clone();
        let task = parent
            .storage()
            .latest_task_record(&task_id)
            .unwrap()
            .expect("supervision task should be persisted");
        assert_eq!(
            task.summary.as_deref(),
            Some("investigate remote TUI access")
        );

        let child = host.get_or_create_agent(&spawned.agent_id).await.unwrap();
        let messages = child.storage().read_recent_messages(10).unwrap();
        let delegated = messages
            .iter()
            .find(|message| {
                message
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("delegated_task_id"))
                    .and_then(|value| value.as_str())
                    == Some(task_id.as_str())
            })
            .expect("child should receive the initial delegation message");
        assert_eq!(
            delegated.origin,
            MessageOrigin::Task {
                task_id: task_id.clone()
            }
        );
        assert_eq!(delegated.authority_class, AuthorityClass::ExternalEvidence);
        assert_eq!(
            delegated.metadata.as_ref().unwrap()["parent_supervised"],
            true
        );
        assert_eq!(
            delegated.metadata.as_ref().unwrap()["supervision_task_id"],
            task_id
        );
        assert_eq!(
            delegated.body,
            MessageBody::Text {
                text: initial_message
            }
        );

        assert!(host
            .bridge()
            .deliver_child_followup(
                &parent_agent_id,
                &task_id,
                &spawned.agent_id,
                "additional untrusted evidence",
                AuthorityClass::ExternalEvidence,
            )
            .await
            .unwrap());
        let messages = child.storage().read_recent_messages(10).unwrap();
        let followup = messages
            .iter()
            .find(|message| {
                message
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("followup_via"))
                    .and_then(|value| value.as_str())
                    == Some("task_input")
            })
            .expect("child should receive the task input follow-up");
        assert_eq!(followup.authority_class, AuthorityClass::ExternalEvidence);
        assert_eq!(
            followup.metadata.as_ref().unwrap()["delegated_authority_class"],
            serde_json::json!("external_evidence")
        );
    }

    #[tokio::test]
    async fn private_child_spawn_accepts_user_global_catalog_id_with_install_suffix() {
        let (home, host) = test_host();
        let template_dir = home
            .path()
            .join(".agents/agent_templates/holon-reviewer@official");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(
            template_dir.join("AGENTS.md"),
            "# Official reviewer\n\nSynced reviewer template\n",
        )
        .unwrap();
        let parent = host.default_runtime().await.unwrap();

        let spawned = invoke_new_subagent(
            &parent,
            "review the implementation".into(),
            AuthorityClass::OperatorInstruction,
            Some("user_global:holon-reviewer@official".into()),
            None,
        )
        .await
        .unwrap();

        let child_home = host.agent_data_dir(&spawned.agent_id);
        assert!(fs::read_to_string(child_home.join("AGENTS.md"))
            .unwrap()
            .starts_with("# Official reviewer\n\nSynced reviewer template\n"));
        assert!(!spawned.task_handle.task_id.is_empty());
    }

    #[tokio::test]
    async fn private_child_spawn_failure_preserves_safe_cause_in_tool_and_task_diagnostics() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();

        let error = invoke_new_subagent(
            &parent,
            "review the implementation".into(),
            AuthorityClass::OperatorInstruction,
            Some("user_global:reviewer%official".into()),
            None,
        )
        .await
        .expect_err("invalid template selector should fail after task creation");
        let tool_error = crate::tool::ToolError::from_anyhow(&error);

        assert_eq!(tool_error.kind, "agent_invocation_failed");
        assert_eq!(
            tool_error.domain,
            Some(crate::runtime_error::RuntimeErrorDomain::Task)
        );
        assert!(tool_error
            .message
            .contains("template install_id contains unsupported characters"));
        assert!(tool_error
            .source_chain
            .iter()
            .any(|cause| cause.contains("template install_id contains unsupported characters")));
        let task_id = tool_error
            .details
            .as_ref()
            .and_then(|details| details.get("task_id"))
            .and_then(Value::as_str)
            .expect("tool error should identify the failed supervision task");
        let rendered = tool_error.render_for_model(Some("InvokeAgent"));
        assert!(rendered.contains("template install_id contains unsupported characters"));

        let task = parent
            .storage()
            .latest_task_record(task_id)
            .unwrap()
            .expect("failed supervision task should remain persisted");
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(
            task.detail.as_ref().unwrap()["error"],
            "template install_id contains unsupported characters"
        );

        let output = parent.task_output(task_id, false, 0).await.unwrap();
        let failure = output
            .task
            .failure_artifact
            .expect("failed supervision task should expose a failure artifact");
        assert!(failure
            .source_chain
            .iter()
            .any(|cause| cause.contains("template install_id contains unsupported characters")));
    }

    #[tokio::test]
    async fn private_child_spawn_accepts_explicit_model_selection() {
        let fixture = provider_test_config(Some("dummy-token"));
        let host = RuntimeHost::new(fixture.config).unwrap();
        let parent = host.default_runtime().await.unwrap();

        let spawned = invoke_new_subagent(
            &parent,
            "compare implementation".into(),
            AuthorityClass::OperatorInstruction,
            None,
            Some(crate::types::AgentModelRequest {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
                reasoning_effort: Some("high".into()),
                temperature: None,
                max_output_tokens: None,
                allow_fallback: Some(false),
            }),
        )
        .await
        .unwrap();

        let child = host.get_or_create_agent(&spawned.agent_id).await.unwrap();
        let child_summary = child.agent_summary().await.unwrap();
        assert_eq!(
            child_summary.model.override_model.unwrap().as_string(),
            "anthropic@default/claude-haiku-4-5"
        );
        assert_eq!(
            child_summary.model.override_reasoning_effort.as_deref(),
            Some("high")
        );
    }

    #[tokio::test]
    async fn private_child_spawn_rejects_unavailable_explicit_model_without_fallback() {
        let fixture = provider_test_config(Some("dummy-token"));
        let host = RuntimeHost::new(fixture.config).unwrap();
        let parent = host.default_runtime().await.unwrap();

        let error = invoke_new_subagent(
            &parent,
            "compare implementation".into(),
            AuthorityClass::OperatorInstruction,
            None,
            Some(crate::types::AgentModelRequest {
                provider: "openai".into(),
                model: "gpt-5.4".into(),
                reasoning_effort: None,
                temperature: None,
                max_output_tokens: None,
                allow_fallback: Some(false),
            }),
        )
        .await
        .expect_err("unavailable explicit model should be rejected before child creation");

        assert!(error.to_string().contains("requested model"));
        assert!(error.to_string().contains("unavailable"));
    }

    #[tokio::test]
    async fn private_child_runtime_spawn_rejects_blank_initial_message() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();

        let error = invoke_new_subagent(
            &parent,
            "   \n\t  ".into(),
            AuthorityClass::OperatorInstruction,
            None,
            None,
        )
        .await
        .expect_err("blank private child initial_message should be rejected");

        assert!(error
            .to_string()
            .contains("agent invocation requires a non-empty message"));
    }

    #[tokio::test]
    async fn independent_agent_initial_message_is_optional_and_inherits_only_attached_workspaces() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
        let named_agent_id = "independent-no-bootstrap".to_string();
        let bootstrap_agent_id = "independent-bootstrap".to_string();
        let bootstrap_message_id = format!("agent_bootstrap_message:{bootstrap_agent_id}");
        let workspace_home = tempdir().unwrap();
        let workspace_path = workspace_home.path().to_path_buf();
        let workspace = host.ensure_workspace_entry(workspace_path.clone()).unwrap();
        parent.attach_workspace(&workspace).await.unwrap();
        parent
            .enter_workspace(
                &workspace,
                WorkspaceProjectionKind::CanonicalRoot,
                WorkspaceAccessMode::SharedRead,
                Some(workspace_path.clone()),
                None,
            )
            .await
            .unwrap();
        let worktree_home = tempdir().unwrap();
        parent
            .enter_worktree(
                workspace_path,
                "main".into(),
                worktree_home.path().to_path_buf(),
                "feature/bootstrap".into(),
            )
            .await
            .unwrap();
        let parent_state = parent.agent_state().await.unwrap();
        assert!(parent_state.active_workspace_entry.is_some());
        assert!(parent_state.worktree_session.is_some());
        let parent_home_id = crate::types::agent_home_workspace_id(&parent_state.id);
        assert!(
            parent_state.attached_workspaces.contains(&parent_home_id),
            "parent state should contain its own agent home"
        );

        host.spawn_public_named_agent(
            parent.clone(),
            &named_agent_id,
            None,
            AuthorityClass::OperatorInstruction,
            None,
            inherited_model_resolution("anthropic", "claude-sonnet-4-6"),
        )
        .await
        .unwrap();

        let named = host.get_public_agent(&named_agent_id).await.unwrap();
        let named_state = named.agent_state().await.unwrap();
        let named_home_id = crate::types::agent_home_workspace_id(&named_agent_id);
        assert_eq!(
            named_state.attached_workspaces,
            vec![named_home_id, workspace.workspace_id.clone()]
        );
        assert!(
            !named_state.attached_workspaces.contains(&parent_home_id),
            "public named agent should not inherit the caller's agent home"
        );
        assert!(
            named_state.active_workspace_entry.is_none(),
            "public named creation should not inherit the caller's active workspace entry"
        );
        assert!(
            named_state.worktree_session.is_none(),
            "public named creation should not inherit the caller's worktree session"
        );
        assert!(
            named.storage().read_recent_messages(10).unwrap().is_empty(),
            "omitted initial_message should not enqueue a bootstrap message"
        );

        host.spawn_public_named_agent(
            parent,
            &bootstrap_agent_id,
            Some("bootstrap release lane".into()),
            AuthorityClass::OperatorInstruction,
            None,
            inherited_model_resolution("anthropic", "claude-sonnet-4-6"),
        )
        .await
        .unwrap();
        let bootstrap_named = host.get_public_agent(&bootstrap_agent_id).await.unwrap();
        let messages = bootstrap_named.storage().read_recent_messages(10).unwrap();
        let bootstrap = messages
            .iter()
            .find(|message| {
                message
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("bootstrap"))
                    .and_then(|value| value.as_bool())
                    == Some(true)
            })
            .expect("public named initial_message should enqueue bootstrap input");
        assert_eq!(
            bootstrap.origin,
            MessageOrigin::System {
                subsystem: "spawn_agent".into()
            }
        );
        assert_eq!(
            bootstrap.metadata.as_ref().unwrap()["creator_agent_id"],
            host.config().default_agent_id
        );
        assert!(bootstrap
            .metadata
            .as_ref()
            .unwrap()
            .get("delegated_task_id")
            .is_none());
        assert_eq!(
            bootstrap.body,
            MessageBody::Text {
                text: "bootstrap release lane".into()
            }
        );

        let mut bootstrap_record = host
            .runtime_db()
            .agent_bootstraps()
            .latest(&bootstrap_agent_id)
            .unwrap()
            .unwrap();
        bootstrap_record.desired.initial_message = Some(AgentBootstrapInitialMessage {
            message_id: bootstrap_message_id.clone(),
            text: "bootstrap release lane".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            creator_agent_id: host.config().default_agent_id.clone(),
        });
        bootstrap_record.initial_message.status = AgentBootstrapStepStatus::Failed;
        bootstrap_record.initial_message.last_error = Some("injected retry".into());
        bootstrap_record.revision = bootstrap_record.revision.saturating_add(1);
        bootstrap_record.updated_at = Utc::now();
        host.runtime_db()
            .agent_bootstraps()
            .upsert(&bootstrap_record)
            .unwrap();

        let (left, right) = tokio::join!(
            host.repair_public_agent(&bootstrap_agent_id),
            host.repair_public_agent(&bootstrap_agent_id)
        );
        assert_eq!(
            left.unwrap().bootstrap.unwrap().status,
            AgentBootstrapStatus::Ready
        );
        assert_eq!(
            right.unwrap().bootstrap.unwrap().status,
            AgentBootstrapStatus::Ready
        );
        let messages = bootstrap_named.storage().read_recent_messages(10).unwrap();
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.id == bootstrap_message_id)
                .count(),
            1,
            "concurrent repair must not duplicate the bootstrap message"
        );
    }

    #[tokio::test]
    async fn spawn_public_named_resolves_parent_agent_template_catalog() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
        let parent_state = parent.agent_state().await.unwrap();
        let parent_agent_home = host.agent_data_dir(&parent_state.id);
        let template_dir = parent_agent_home.join("agent_templates").join("worker");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(
            template_dir.join("AGENTS.md"),
            "# Parent worker\n\nParent catalog worker\n",
        )
        .unwrap();

        host.spawn_public_named_agent(
            parent,
            "worker-bot",
            None,
            AuthorityClass::OperatorInstruction,
            Some("worker".into()),
            inherited_model_resolution("anthropic", "claude-sonnet-4-6"),
        )
        .await
        .unwrap();

        let named_home = host.agent_data_dir("worker-bot");
        let agents_md = fs::read_to_string(named_home.join("AGENTS.md")).unwrap();
        assert!(agents_md.starts_with("# Parent worker\n\nParent catalog worker\n"));
        assert!(agents_md.contains("## Holon Agent Home"));
        assert!(agents_md.contains("`agent_home` is this agent's default workspace"));
    }

    #[tokio::test]
    async fn agent_summary_reports_runtime_default_then_override_and_clear() {
        let fixture = provider_test_config(Some("dummy-token"));
        let host = RuntimeHost::new(fixture.config).unwrap();
        let runtime = host.default_runtime().await.unwrap();

        let inherited = runtime.agent_summary().await.unwrap();
        assert_eq!(
            inherited.model.source,
            crate::types::AgentModelSource::RuntimeDefault
        );
        assert_eq!(
            inherited.model.effective_model.as_string(),
            "anthropic@default/claude-sonnet-5"
        );
        assert!(inherited.model.override_model.is_none());
        assert_eq!(
            inherited
                .model
                .resolved_policy
                .prompt_budget_estimated_tokens,
            900_000
        );

        let updated = runtime
            .set_model_override(
                ModelRouteRef::parse_compatible("openai@default/gpt-5.4").unwrap(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            updated.source,
            crate::types::AgentModelSource::AgentOverride
        );
        assert_eq!(
            updated.effective_model.as_string(),
            "openai@default/gpt-5.4"
        );
        assert_eq!(
            updated.runtime_default_model.as_string(),
            "anthropic@default/claude-sonnet-5"
        );
        assert_eq!(
            updated
                .effective_fallback_models
                .iter()
                .map(|model| model.as_string())
                .collect::<Vec<_>>(),
            vec!["anthropic@default/claude-sonnet-5"]
        );
        assert_eq!(
            updated.resolved_policy.prompt_budget_estimated_tokens,
            258_400
        );
        assert_eq!(
            updated.resolved_policy.source,
            crate::model_catalog::ModelMetadataSource::ConservativeBuiltin
        );

        let summary = runtime.agent_summary().await.unwrap();
        assert_eq!(summary.agent.model_override, updated.override_model);
        assert_eq!(summary.model, updated);

        let cleared = runtime.clear_model_override().await.unwrap();
        assert_eq!(
            cleared.source,
            crate::types::AgentModelSource::RuntimeDefault
        );
        assert!(cleared.override_model.is_none());
        assert_eq!(
            cleared.effective_model.as_string(),
            "anthropic@default/claude-sonnet-5"
        );
        assert_eq!(
            cleared.resolved_policy.prompt_budget_estimated_tokens,
            900_000
        );
    }

    #[tokio::test]
    async fn recovered_runtime_reapplies_persisted_model_override_to_provider_chain() {
        let fixture = provider_test_config(Some("dummy-token"));
        let host = RuntimeHost::new(fixture.config.clone()).unwrap();
        let bridge = RuntimeHostBridge {
            inner: Arc::downgrade(&host.inner),
        };
        let storage = AppStorage::new_for_agent(
            fixture.config.data_dir.clone(),
            "default",
            host.runtime_db().clone(),
        )
        .unwrap();
        let mut state = AgentState::new("default");
        state.model_override =
            Some(ModelRouteRef::parse_compatible("anthropic/claude-haiku-4-5").unwrap());
        storage.write_agent(&state).unwrap();

        let runtime = RuntimeHandle::new_reconfigurable_with_host_bridge(
            "default",
            fixture.config.data_dir.clone(),
            fixture.config.workspace_dir.clone(),
            fixture.config.callback_base_url.clone(),
            fixture.config.clone(),
            fixture.config.default_agent_id.clone(),
            host.runtime_context_config(),
            host.runtime_db().clone(),
            bridge,
            host.inner.event_bus.clone(),
        )
        .unwrap();

        assert_eq!(
            runtime.current_provider().await.configured_model_refs(),
            vec![
                "anthropic@default/claude-haiku-4-5".to_string(),
                "anthropic@default/claude-sonnet-5".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn child_runtime_reconfigures_provider_when_inheriting_model_override() {
        let fixture = provider_test_config(Some("dummy-token"));
        let host = RuntimeHost::new(fixture.config).unwrap();
        let parent = host.default_runtime().await.unwrap();
        parent
            .set_model_override(
                ModelRouteRef::parse_compatible("anthropic@default/claude-haiku-4-5").unwrap(),
                None,
            )
            .await
            .unwrap();
        let parent_state = parent.agent_state().await.unwrap();
        let parent_agent_home = host.agent_data_dir(&parent_state.id);
        let task = test_child_supervision_task(&parent_state.id, "task-1");
        let child_identity = host
            .create_child_identity(&parent_state.id, &task, None, &parent_agent_home)
            .await
            .unwrap();
        let relations = host
            .runtime_db()
            .agent_canonical_relations()
            .latest(&child_identity.agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            relations.sources.lineage,
            Some(crate::types::AgentCanonicalValueSource::Canonical)
        );
        assert_eq!(
            relations.sources.supervision,
            Some(crate::types::AgentCanonicalValueSource::Canonical)
        );
        assert_eq!(
            relations.sources.durability,
            Some(crate::types::AgentCanonicalValueSource::Canonical)
        );
        assert_eq!(
            relations.sources.lifecycle_attachment,
            Some(crate::types::AgentCanonicalValueSource::Canonical)
        );
        assert_eq!(
            relations.sources.capability_policy,
            Some(crate::types::AgentCanonicalValueSource::Canonical)
        );
        assert_eq!(
            relations.sources.message_policy,
            Some(crate::types::AgentCanonicalValueSource::Canonical)
        );
        let child_home = host.agent_data_dir(&child_identity.agent_id);
        assert!(child_home.join("AGENTS.md").is_file());
        assert!(std::fs::read_to_string(child_home.join("AGENTS.md"))
            .unwrap()
            .contains("## Holon Agent Home"));
        assert!(child_home.join("memory/self.md").is_file());
        assert!(child_home.join("memory/operator.md").is_file());
        assert!(child_home.join("notes").is_dir());
        assert!(child_home.join("work").is_dir());
        assert!(child_home.join("skills").is_dir());
        assert!(child_home.join(".holon/state").is_dir());
        assert!(child_home.join(".holon/ledger").is_dir());
        assert!(child_home.join(".holon/indexes").is_dir());
        assert!(child_home.join(".holon/cache").is_dir());
        let child = host
            .get_or_create_agent(&child_identity.agent_id)
            .await
            .unwrap();

        child
            .inherit_from_parent_state(&parent_state)
            .await
            .unwrap();

        assert_eq!(
            child.current_provider().await.configured_model_refs(),
            vec![
                "anthropic@default/claude-haiku-4-5".to_string(),
                "anthropic@default/claude-sonnet-5".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn child_identity_template_resolves_parent_agent_template_catalog() {
        let fixture = provider_test_config(Some("dummy-token"));
        let host = RuntimeHost::new(fixture.config).unwrap();
        let parent = host.default_runtime().await.unwrap();
        let parent_state = parent.agent_state().await.unwrap();
        let parent_agent_home = host.agent_data_dir(&parent_state.id);
        let template_dir = parent_agent_home.join("agent_templates").join("worker");
        fs::create_dir_all(&template_dir).unwrap();
        fs::write(template_dir.join("AGENTS.md"), "parent catalog worker").unwrap();

        let task = test_child_supervision_task(&parent_state.id, "task-1");
        let child_identity = host
            .create_child_identity(&parent_state.id, &task, Some("worker"), &parent_agent_home)
            .await
            .unwrap();
        let child_home = host.agent_data_dir(&child_identity.agent_id);
        let agents_md = fs::read_to_string(child_home.join("AGENTS.md")).unwrap();

        assert!(agents_md.contains("parent catalog worker"));
        assert!(child_home.join("memory/self.md").is_file());
        assert!(child_home.join("memory/operator.md").is_file());
        assert!(child_home.join(".holon/state").is_dir());
    }

    #[tokio::test]
    async fn create_named_agent_rejects_conflicting_existing_identity() {
        let (_home, host) = test_host();
        let agent_id = "conflicting-release-bot";

        let child = AgentIdentityRecord::new(
            agent_id,
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(host.config().default_agent_id.clone()),
            Some("task-1".into()),
        );
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();

        let error = host
            .create_named_agent(agent_id, None)
            .await
            .err()
            .expect("conflicting identity should fail named-agent creation");
        assert!(error.to_string().contains("different identity type"));
    }

    #[tokio::test]
    async fn create_named_agent_rejects_temporary_prefix() {
        let (_home, host) = test_host();

        let error = host
            .create_named_agent("tmp_release_bot", None)
            .await
            .err()
            .expect("temporary prefix should be reserved");
        assert!(error.to_string().contains("reserved temporary prefix"));
    }

    #[tokio::test]
    async fn unknown_named_agents_are_not_auto_created() {
        let (_home, host) = test_host();

        let error = host
            .get_or_create_agent("release-bot")
            .await
            .err()
            .expect("unknown named agent should fail");
        assert!(error.to_string().contains("create it first"));
    }

    #[tokio::test]
    async fn parent_summary_shows_private_children_but_public_listing_hides_them() {
        let (_home, host) = test_host();
        let default_runtime = host.default_runtime().await.unwrap();

        let child = AgentIdentityRecord::new(
            "child_test",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(host.config().default_agent_id.clone()),
            Some("task-1".into()),
        );
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();

        let child_storage = host.agent_storage("child_test").unwrap();
        let mut child_state = AgentState::new("child_test");
        child_state.status = AgentStatus::AwakeRunning;
        child_state.pending = 1;
        child_state.current_run_id = Some("run-1".into());
        child_storage.write_agent(&child_state).unwrap();
        child_storage
            .append_task(&TaskRecord {
                id: "task-1".into(),
                agent_id: "child_test".into(),
                kind: crate::types::TaskKind::CommandTask,
                status: TaskStatus::Running,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                parent_message_id: None,
                work_item_id: None,
                summary: Some("child task".into()),
                detail: Some(serde_json::json!({ "wait_policy": "background" })),
                recovery: None,
            })
            .unwrap();

        let summary = default_runtime.agent_summary().await.unwrap();
        assert_eq!(summary.identity.kind, AgentKind::Default);
        assert_eq!(summary.active_children.len(), 1);
        assert_eq!(summary.active_children[0].identity.agent_id, "child_test");
        assert_eq!(summary.active_children[0].identity.kind, AgentKind::Child);
        assert_eq!(
            summary.active_children[0].identity.visibility,
            AgentVisibility::Private
        );
        assert_eq!(
            summary.active_children[0].observability.phase,
            crate::types::ChildAgentPhase::Running
        );
        {
            let registry = host.inner.runtimes.read().await;
            assert!(
                !registry.agents.contains_key("child_test"),
                "child summary inspection should not start the child runtime"
            );
        }

        let listed = host
            .list_agents()
            .await
            .unwrap()
            .into_iter()
            .map(|summary| summary.identity.agent_id)
            .collect::<Vec<_>>();
        assert!(listed.contains(&host.config().default_agent_id));
        assert!(!listed.contains(&"child_test".to_string()));
    }

    #[tokio::test]
    async fn host_bootstrap_keeps_interrupted_supervised_child_active() {
        let (_home, host) = test_host();
        let config = host.config().as_ref().clone();
        let parent_agent_id = config.default_agent_id.clone();
        let parent_storage = host.agent_storage(&parent_agent_id).unwrap();
        parent_storage
            .append_task(&TaskRecord {
                id: "task-1".into(),
                agent_id: parent_agent_id.clone(),
                kind: crate::types::TaskKind::SubagentTask,
                status: TaskStatus::Interrupted,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                parent_message_id: None,
                work_item_id: None,
                summary: Some("delegated child".into()),
                detail: Some(serde_json::json!({
                    "child_agent_id": "child_test",
                    "task_status": "interrupted",
                })),
                recovery: None,
            })
            .unwrap();

        let child = AgentIdentityRecord::new(
            "child_test",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(parent_agent_id.clone()),
            Some("task-1".into()),
        );
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();

        let child_storage = host.agent_storage("child_test").unwrap();
        let mut child_state = AgentState::new("child_test");
        child_state.status = AgentStatus::AwaitingTask;
        child_storage.write_agent(&child_state).unwrap();
        child_storage
            .append_task(&TaskRecord {
                id: "child-task-1".into(),
                agent_id: "child_test".into(),
                kind: crate::types::TaskKind::CommandTask,
                status: TaskStatus::Running,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                parent_message_id: None,
                work_item_id: None,
                summary: Some("child task".into()),
                detail: Some(serde_json::json!({ "wait_policy": "blocking" })),
                recovery: None,
            })
            .unwrap();

        drop(host);

        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        let runtime = restarted.default_runtime().await.unwrap();
        let identity = restarted
            .agent_identity_record("child_test")
            .unwrap()
            .expect("child identity should remain present");
        assert_eq!(identity.status, AgentRegistryStatus::Active);
        assert!(restarted.agent_data_dir("child_test").exists());
        let snapshot = runtime.task_status_snapshot("task-1").await.unwrap();
        assert_eq!(snapshot.status, TaskStatus::Interrupted);
        assert_eq!(snapshot.child_agent_id.as_deref(), Some("child_test"));
        assert!(snapshot.child_observability.is_some());
        let summary = runtime.agent_summary().await.unwrap();
        assert_eq!(summary.active_children.len(), 1);
        assert_eq!(summary.active_children[0].identity.agent_id, "child_test");
    }

    #[tokio::test]
    async fn recovered_runtime_reattaches_supervised_child_monitor() {
        let (_home, host) = test_host();
        let config = host.config().as_ref().clone();
        let parent_agent_id = config.default_agent_id.clone();
        let parent_storage = host.agent_storage(&parent_agent_id).unwrap();
        parent_storage
            .append_task(&TaskRecord {
                id: "task-recover-child".into(),
                agent_id: parent_agent_id.clone(),
                kind: crate::types::TaskKind::ChildAgentTask,
                status: TaskStatus::Running,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                parent_message_id: None,
                work_item_id: None,
                summary: Some("delegated child".into()),
                detail: Some(serde_json::json!({
                    "child_agent_id": "child_recover",
                    "child_turn_baseline": 0,
                    "task_status": "running",
                })),
                recovery: Some(TaskRecoverySpec::ChildAgentTask {
                    summary: "delegated child".into(),
                    prompt: "continue delegated child".into(),
                    authority_class: AuthorityClass::OperatorInstruction,
                    workspace_mode: crate::types::ChildAgentWorkspaceMode::Inherit,
                }),
            })
            .unwrap();

        let child = AgentIdentityRecord::new(
            "child_recover",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(parent_agent_id.clone()),
            Some("task-recover-child".into()),
        )
        .with_lineage_parent_agent_id(Some(parent_agent_id.clone()));
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();

        let child_storage = host.agent_storage("child_recover").unwrap();
        let mut child_state = AgentState::new("child_recover");
        child_state.turn_index = 1;
        child_state.status = AgentStatus::AwakeIdle;
        child_state.last_turn_terminal = Some(crate::types::TurnTerminalRecord {
            turn_index: 1,
            turn_id: "test".into(),
            kind: crate::types::TurnTerminalKind::Completed,
            reason: None,
            last_assistant_message: Some("child finished after restart".into()),
            no_brief_reason: None,
            checkpoint: None,
            completed_at: chrono::Utc::now(),
            duration_ms: 1,
        });
        child_storage.write_agent(&child_state).unwrap();

        drop(host);

        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        let runtime = restarted.default_runtime().await.unwrap();
        let runtime_task = tokio::spawn(runtime.clone().run());

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let task = runtime
                .storage()
                .latest_task_record("task-recover-child")
                .unwrap()
                .expect("recovered task should remain recorded");
            if task.status == TaskStatus::Completed {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for recovered child monitor to converge"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let task = runtime
            .storage()
            .latest_task_record("task-recover-child")
            .unwrap()
            .expect("completed task should remain recorded");
        assert_eq!(task.status, TaskStatus::Completed);
        let output = runtime
            .task_output("task-recover-child", false, 0)
            .await
            .unwrap();
        assert!(output
            .task
            .output_preview
            .contains("child finished after restart"));
        let events = runtime.storage().read_recent_events(100).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "supervised_child_task_monitor_reattached"
                && event
                    .data
                    .get("task_ids")
                    .and_then(|value| value.as_array())
                    .is_some_and(|ids| ids.iter().any(|value| value == "task-recover-child"))
        }));
        assert!(!events.iter().any(|event| {
            event.kind == "task_interrupted_on_restart"
                && event.data.get("id").and_then(|value| value.as_str())
                    == Some("task-recover-child")
        }));
        let child_identity = restarted
            .agent_identity_record("child_recover")
            .unwrap()
            .expect("child identity should remain recorded");
        assert_eq!(child_identity.status, AgentRegistryStatus::Deleted);

        runtime_task.abort();
    }

    #[tokio::test]
    async fn recovered_child_monitor_waits_for_active_child_tasks() {
        let (_home, host) = test_host();
        let config = host.config().as_ref().clone();
        let parent_agent_id = config.default_agent_id.clone();
        let parent_storage = host.agent_storage(&parent_agent_id).unwrap();
        parent_storage
            .append_task(&TaskRecord {
                id: "task-recover-active-child-task".into(),
                agent_id: parent_agent_id.clone(),
                kind: crate::types::TaskKind::ChildAgentTask,
                status: TaskStatus::Running,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                parent_message_id: None,
                work_item_id: None,
                summary: Some("delegated child".into()),
                detail: Some(serde_json::json!({
                    "child_agent_id": "child_recover_active_task",
                    "child_turn_baseline": 0,
                    "task_status": "running",
                })),
                recovery: Some(TaskRecoverySpec::ChildAgentTask {
                    summary: "delegated child".into(),
                    prompt: "continue delegated child".into(),
                    authority_class: AuthorityClass::OperatorInstruction,
                    workspace_mode: crate::types::ChildAgentWorkspaceMode::Inherit,
                }),
            })
            .unwrap();

        let child = AgentIdentityRecord::new(
            "child_recover_active_task",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(parent_agent_id.clone()),
            Some("task-recover-active-child-task".into()),
        )
        .with_lineage_parent_agent_id(Some(parent_agent_id.clone()));
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();

        let child_storage = host.agent_storage("child_recover_active_task").unwrap();
        let mut child_state = AgentState::new("child_recover_active_task");
        child_state.turn_index = 1;
        child_state.status = AgentStatus::AwakeIdle;
        child_state.last_turn_terminal = Some(crate::types::TurnTerminalRecord {
            turn_index: 1,
            turn_id: "test".into(),
            kind: crate::types::TurnTerminalKind::Completed,
            reason: None,
            last_assistant_message: Some("child says done before command finished".into()),
            no_brief_reason: None,
            checkpoint: None,
            completed_at: chrono::Utc::now(),
            duration_ms: 1,
        });
        child_storage.write_agent(&child_state).unwrap();
        child_storage
            .append_task(&TaskRecord {
                id: "child-command-still-running".into(),
                agent_id: "child_recover_active_task".into(),
                kind: crate::types::TaskKind::CommandTask,
                status: TaskStatus::Running,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                parent_message_id: None,
                work_item_id: None,
                summary: Some("verification".into()),
                detail: None,
                recovery: None,
            })
            .unwrap();

        // Also add an active Task wait condition so the lifecycle blocker
        // persists through recovery. The orphaned command task above is
        // interrupted by the child's own bootstrap_recovery, but a wait
        // condition survives restart, keeping the parent monitor running
        // deterministically rather than racing with task cleanup.
        let now = chrono::Utc::now();
        child_storage
            .append_wait_condition(&crate::types::WaitConditionRecord {
                id: "wait-child-command".into(),
                agent_id: "child_recover_active_task".into(),
                work_item_id: None,
                status: crate::types::WaitConditionStatus::Active,
                kind: crate::types::WaitConditionKind::Task,
                source: None,
                subject_ref: Some("child-command-still-running".into()),
                waiting_for: "command result".into(),
                wake_sources: vec![crate::types::WakeSource::TaskResult {
                    task_id: "child-command-still-running".into(),
                }],
                continuation: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,
                turn_id: None,
                trigger_message_id: None,
                triggered_at: None,
            })
            .unwrap();

        drop(host);

        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        let runtime = restarted.default_runtime().await.unwrap();
        let runtime_task = tokio::spawn(runtime.clone().run());
        tokio::time::sleep(Duration::from_millis(250)).await;

        let task = runtime
            .storage()
            .latest_task_record("task-recover-active-child-task")
            .unwrap()
            .expect("recovered task should remain recorded");
        assert_eq!(task.status, TaskStatus::Running);
        let child_identity = restarted
            .agent_identity_record("child_recover_active_task")
            .unwrap()
            .expect("child identity should remain recorded");
        assert_eq!(child_identity.status, AgentRegistryStatus::Active);

        runtime_task.abort();
    }

    #[tokio::test]
    async fn recovered_child_monitor_waits_for_active_child_task_result_wait() {
        let (_home, host) = test_host();
        let config = host.config().as_ref().clone();
        let parent_agent_id = config.default_agent_id.clone();
        let parent_storage = host.agent_storage(&parent_agent_id).unwrap();
        parent_storage
            .append_task(&TaskRecord {
                id: "task-recover-active-child-wait".into(),
                agent_id: parent_agent_id.clone(),
                kind: crate::types::TaskKind::ChildAgentTask,
                status: TaskStatus::Running,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                parent_message_id: None,
                work_item_id: None,
                summary: Some("delegated child".into()),
                detail: Some(serde_json::json!({
                    "child_agent_id": "child_recover_active_wait",
                    "child_turn_baseline": 0,
                    "task_status": "running",
                })),
                recovery: Some(TaskRecoverySpec::ChildAgentTask {
                    summary: "delegated child".into(),
                    prompt: "continue delegated child".into(),
                    authority_class: AuthorityClass::OperatorInstruction,
                    workspace_mode: crate::types::ChildAgentWorkspaceMode::Inherit,
                }),
            })
            .unwrap();

        let child = AgentIdentityRecord::new(
            "child_recover_active_wait",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(parent_agent_id.clone()),
            Some("task-recover-active-child-wait".into()),
        )
        .with_lineage_parent_agent_id(Some(parent_agent_id.clone()));
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();

        let child_storage = host.agent_storage("child_recover_active_wait").unwrap();
        let mut child_state = AgentState::new("child_recover_active_wait");
        child_state.turn_index = 1;
        child_state.status = AgentStatus::AwakeIdle;
        child_state.last_turn_terminal = Some(crate::types::TurnTerminalRecord {
            turn_index: 1,
            turn_id: "test".into(),
            kind: crate::types::TurnTerminalKind::Completed,
            reason: None,
            last_assistant_message: Some("child says done before wait resolved".into()),
            no_brief_reason: None,
            checkpoint: None,
            completed_at: chrono::Utc::now(),
            duration_ms: 1,
        });
        child_storage.write_agent(&child_state).unwrap();
        let now = chrono::Utc::now();
        child_storage
            .append_wait_condition(&crate::types::WaitConditionRecord {
                id: "wait-child-task-result".into(),
                agent_id: "child_recover_active_wait".into(),
                work_item_id: None,
                status: crate::types::WaitConditionStatus::Active,
                kind: crate::types::WaitConditionKind::Task,
                source: None,
                subject_ref: Some("child-command".into()),
                waiting_for: "task result".into(),
                wake_sources: vec![crate::types::WakeSource::TaskResult {
                    task_id: "child-command".into(),
                }],
                continuation: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,
                turn_id: None,
                trigger_message_id: None,
                triggered_at: None,
            })
            .unwrap();

        drop(host);

        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        let runtime = restarted.default_runtime().await.unwrap();
        let runtime_task = tokio::spawn(runtime.clone().run());
        tokio::time::sleep(Duration::from_millis(250)).await;

        let task = runtime
            .storage()
            .latest_task_record("task-recover-active-child-wait")
            .unwrap()
            .expect("recovered task should remain recorded");
        assert_eq!(task.status, TaskStatus::Running);
        let child_identity = restarted
            .agent_identity_record("child_recover_active_wait")
            .unwrap()
            .expect("child identity should remain recorded");
        assert_eq!(child_identity.status, AgentRegistryStatus::Active);

        runtime_task.abort();
    }

    #[tokio::test]
    async fn host_bootstrap_archives_orphaned_private_child_identity() {
        let (_home, host) = test_host();
        let config = host.config().as_ref().clone();
        let parent_agent_id = config.default_agent_id.clone();

        let child = AgentIdentityRecord::new(
            "child_orphan",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(parent_agent_id),
            Some("missing-task".into()),
        );
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();

        let child_storage =
            AppStorage::new_for_agent_for_test(host.agent_data_dir("child_orphan"), "child_orphan")
                .unwrap();
        child_storage
            .write_agent(&AgentState::new("child_orphan"))
            .unwrap();

        drop(host);

        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        let identity = restarted
            .agent_identity_record("child_orphan")
            .unwrap()
            .expect("child identity should still be recorded after archive");
        assert_eq!(identity.status, AgentRegistryStatus::Deleted);
        assert!(!restarted.agent_data_dir("child_orphan").exists());
    }

    #[tokio::test]
    async fn archived_private_child_identity_cannot_restart_runtime() {
        let (_home, host) = test_host();
        let config = host.config().as_ref().clone();
        let parent_agent_id = config.default_agent_id.clone();

        let child = AgentIdentityRecord::new(
            "child_archived",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(parent_agent_id),
            Some("task-1".into()),
        );
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();
        host.archive_private_agent("child_archived").await.unwrap();

        drop(host);

        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        let err = restarted.get_or_create_agent("child_archived").await;
        assert!(err.is_err(), "archived child should not restart");
        let err = err.err().unwrap();
        assert!(err.to_string().contains("archived"));
        assert!(!restarted.agent_data_dir("child_archived").exists());
    }

    #[tokio::test]
    async fn host_bootstrap_does_not_recreate_missing_parent_storage_when_archiving_child() {
        let (_home, host) = test_host();
        let config = host.config().as_ref().clone();

        let parent = AgentIdentityRecord::new(
            "parent_missing",
            AgentKind::Named,
            AgentVisibility::Private,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PrivateChild,
            None,
            None,
        );
        host.append_agent_identity(&parent).unwrap();
        host.runtime_db()
            .agent_identities()
            .upsert(&parent)
            .unwrap();

        let child = AgentIdentityRecord::new(
            "child_parent_missing",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some("parent_missing".into()),
            Some("task-1".into()),
        );
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();

        let child_storage = AppStorage::new_for_agent_for_test(
            host.agent_data_dir("child_parent_missing"),
            "child_parent_missing",
        )
        .unwrap();
        child_storage
            .write_agent(&AgentState::new("child_parent_missing"))
            .unwrap();

        drop(host);

        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        let identity = restarted
            .agent_identity_record("child_parent_missing")
            .unwrap()
            .expect("child identity should remain recorded after archive");
        assert_eq!(identity.status, AgentRegistryStatus::Deleted);
        assert!(!restarted.agent_data_dir("child_parent_missing").exists());
        assert!(!restarted.agent_data_dir("parent_missing").exists());
    }

    #[tokio::test]
    async fn stop_task_cleans_up_interrupted_supervised_child_after_restart() {
        let (_home, host) = test_host();
        let config = host.config().as_ref().clone();
        let parent_agent_id = config.default_agent_id.clone();
        let parent_storage = AppStorage::new_for_agent(
            host.agent_data_dir(&parent_agent_id),
            &parent_agent_id,
            host.runtime_db().clone(),
        )
        .unwrap();
        parent_storage
            .append_task(&TaskRecord {
                id: "task-stop".into(),
                agent_id: parent_agent_id.clone(),
                kind: crate::types::TaskKind::SubagentTask,
                status: TaskStatus::Interrupted,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                parent_message_id: None,
                work_item_id: None,
                summary: Some("delegated child".into()),
                detail: Some(serde_json::json!({
                    "child_agent_id": "child_stop",
                    "task_status": "interrupted",
                })),
                recovery: None,
            })
            .unwrap();

        let child = AgentIdentityRecord::new(
            "child_stop",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(parent_agent_id),
            Some("task-stop".into()),
        );
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();

        let child_storage =
            AppStorage::new_for_agent_for_test(host.agent_data_dir("child_stop"), "child_stop")
                .unwrap();
        child_storage
            .write_agent(&AgentState::new("child_stop"))
            .unwrap();

        drop(host);

        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        let runtime = restarted.default_runtime().await.unwrap();

        let stopped = runtime
            .stop_task("task-stop", &AuthorityClass::OperatorInstruction)
            .await
            .unwrap();
        assert_eq!(stopped.status, TaskStatus::Cancelled);

        let identity = restarted
            .agent_identity_record("child_stop")
            .unwrap()
            .expect("child identity should remain recorded after archive");
        assert_eq!(identity.status, AgentRegistryStatus::Deleted);
        assert!(!restarted.agent_data_dir("child_stop").exists());
    }

    #[tokio::test]
    async fn unload_runtime_only_removes_targeted_agent() {
        let (_home, host) = test_host();
        host.create_named_agent("alpha", None).await.unwrap();
        host.create_named_agent("beta", None).await.unwrap();

        let _alpha = host.get_public_agent("alpha").await.unwrap();
        let _beta = host.get_public_agent("beta").await.unwrap();

        {
            let registry = host.inner.runtimes.read().await;
            assert!(registry.agents.contains_key("alpha"));
            assert!(registry.agents.contains_key("beta"));
        }

        host.unload_runtime("alpha").await;

        {
            let registry = host.inner.runtimes.read().await;
            assert!(!registry.agents.contains_key("alpha"));
            assert!(registry.agents.contains_key("beta"));
        }

        let beta_runtime = host.get_public_agent("beta").await.unwrap();
        assert_eq!(
            beta_runtime
                .agent_summary()
                .await
                .unwrap()
                .identity
                .agent_id,
            "beta"
        );
    }

    #[tokio::test]
    async fn host_shutdown_preserves_public_agent_durable_status() {
        let (_home, host) = test_host();
        let storage = AppStorage::new_for_agent(
            host.agent_data_dir(&host.config().default_agent_id),
            &host.config().default_agent_id.clone(),
            host.runtime_db().clone(),
        )
        .unwrap();
        let mut state = AgentState::new(&host.config().default_agent_id);
        state.status = AgentStatus::Stopped;
        storage.write_agent(&state).unwrap();

        let _runtime = host.default_runtime().await.unwrap();
        host.shutdown().await.unwrap();

        let persisted = storage.read_agent().unwrap().unwrap();
        assert_eq!(persisted.status, AgentStatus::Stopped);
        let events = storage.read_recent_events(16).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "runtime_service_shutdown_requested"));
    }

    #[tokio::test]
    async fn host_shutdown_aborts_active_run_with_daemon_shutdown_reason() {
        let home = tempdir().unwrap();
        write_test_model_config(home.path());
        let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        let agent_id = config.default_agent_id.clone();
        let started = Arc::new(Notify::new());
        let provider = Arc::new(BlockingProvider {
            started: started.clone(),
        });
        let host = RuntimeHost::new_with_provider(config, provider).unwrap();
        let runtime = host.default_runtime().await.unwrap();
        let started_wait = started.notified();

        runtime
            .enqueue(MessageEnvelope::new(
                &agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "block until daemon shutdown".into(),
                },
            ))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), started_wait)
            .await
            .expect("provider turn should start");

        tokio::time::timeout(Duration::from_secs(5), host.shutdown())
            .await
            .expect("host shutdown should be bounded")
            .unwrap();

        let storage = host.agent_storage(&agent_id).unwrap();
        let persisted = storage.read_agent().unwrap().unwrap();
        assert_eq!(persisted.status, AgentStatus::AwakeIdle);
        assert_eq!(persisted.current_run_id, None);
        let terminal = persisted
            .last_turn_terminal
            .expect("aborted run should persist a terminal record");
        assert_eq!(terminal.kind, TurnTerminalKind::Aborted);
        assert_eq!(terminal.reason.as_deref(), Some("daemon_shutdown"));
        let events = storage.read_recent_events(32).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "runtime_service_shutdown_requested"
                && event.data.get("aborted_run_id").is_some()
        }));
        assert!(events.iter().any(|event| {
            event.kind == "current_run_aborted"
                && event.data.get("reason").and_then(Value::as_str) == Some("daemon_shutdown")
        }));
        assert!(events.iter().any(|event| {
            event.kind == "message_processing_aborted"
                && event.data.get("reason").and_then(Value::as_str) == Some("daemon_shutdown")
        }));
    }

    #[tokio::test]
    async fn host_shutdown_awaits_runtime_task_after_abort() {
        let (_home, host) = test_host();
        let agent_id = host.config().default_agent_id.clone();
        let _runtime = host.default_runtime().await.unwrap();
        let aborted = Arc::new(AtomicBool::new(false));
        let replacement_task = {
            let aborted = aborted.clone();
            tokio::spawn(async move {
                let _abort_observed = AbortObserved(aborted);
                std::future::pending::<()>().await;
            })
        };

        let old_task = {
            let mut registry = host.inner.runtimes.write().await;
            let entry = registry
                .agents
                .get_mut(&agent_id)
                .expect("default runtime should be loaded");
            std::mem::replace(&mut entry.task, replacement_task)
        };
        old_task.abort();
        let _ = old_task.await;

        host.shutdown().await.unwrap();

        assert!(
            aborted.load(Ordering::SeqCst),
            "host shutdown should await the aborted runtime task"
        );
    }

    #[tokio::test]
    async fn daemon_style_shutdown_does_not_strand_public_agent_on_restart() {
        let (_home, host) = test_host();
        let config = host.config().as_ref().clone();
        let agent_id = config.default_agent_id.clone();
        let runtime = host.default_runtime().await.unwrap();

        runtime
            .enqueue(MessageEnvelope::new(
                &agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "before shutdown".into(),
                },
            ))
            .await
            .unwrap();
        wait_for_brief_count(&runtime, 1).await;

        host.shutdown().await.unwrap();

        let persisted = host
            .agent_storage(&agent_id)
            .unwrap()
            .read_agent()
            .unwrap()
            .unwrap();
        assert_ne!(persisted.status, AgentStatus::Stopped);

        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("ok"))).unwrap();
        let runtime2 = restarted.default_runtime().await.unwrap();
        runtime2
            .enqueue(MessageEnvelope::new(
                &agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "after restart".into(),
                },
            ))
            .await
            .unwrap();
        wait_for_brief_count(&runtime2, 2).await;

        let final_state = runtime2.agent_state().await.unwrap();
        assert_ne!(final_state.status, AgentStatus::Stopped);
    }

    #[tokio::test]
    async fn explicit_agent_stop_remains_durable_across_restart() {
        let (_home, host) = test_host();
        let config = host.config().as_ref().clone();
        let runtime = host.default_runtime().await.unwrap();
        runtime.control(ControlAction::Stop).await.unwrap();
        host.unload_runtime(&config.default_agent_id).await;

        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("ok"))).unwrap();
        let stopped = restarted.default_runtime().await.unwrap();
        assert_eq!(
            stopped.agent_state().await.unwrap().status,
            AgentStatus::Stopped
        );
    }

    #[tokio::test]
    async fn stop_releases_active_workspace_occupancy() {
        let (_home, host) = test_host();
        let workspace_home = tempdir().unwrap();
        let workspace_path = workspace_home.path().to_path_buf();
        let workspace = host.ensure_workspace_entry(workspace_path.clone()).unwrap();
        let runtime = host.default_runtime().await.unwrap();
        runtime.attach_workspace(&workspace).await.unwrap();
        runtime
            .enter_workspace(
                &workspace,
                WorkspaceProjectionKind::CanonicalRoot,
                WorkspaceAccessMode::ExclusiveWrite,
                Some(workspace_path.clone()),
                None,
            )
            .await
            .unwrap();

        let occupancy_id = runtime
            .agent_state()
            .await
            .unwrap()
            .active_workspace_entry
            .as_ref()
            .and_then(|entry| entry.occupancy_id.clone())
            .expect("exclusive workspace should acquire occupancy");
        runtime.control(ControlAction::Stop).await.unwrap();

        let stopped = runtime.agent_state().await.unwrap();
        assert_eq!(stopped.status, AgentStatus::Stopped);
        assert!(
            stopped.active_workspace_entry.is_none(),
            "stopped agents should not keep an active workspace entry"
        );
        let released = host
            .workspace_occupancy_by_id(&occupancy_id)
            .unwrap()
            .expect("occupancy record should remain queryable");
        assert!(
            released.released_at.is_some(),
            "stop should release the workspace occupancy"
        );

        host.create_named_agent("peer", None).await.unwrap();
        let peer = host.get_public_agent("peer").await.unwrap();
        peer.attach_workspace(&workspace).await.unwrap();
        peer.enter_workspace(
            &workspace,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::ExclusiveWrite,
            Some(workspace_path),
            None,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn start_respawns_stopped_persistent_agent_runtime_loop() {
        let (_home, host) = test_host();
        let agent_id = host.config().default_agent_id.clone();
        let runtime = host.default_runtime().await.unwrap();
        runtime.control(ControlAction::Stop).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let finished = {
                let registry = host.inner.runtimes.read().await;
                registry
                    .agents
                    .get(&agent_id)
                    .map(|entry| entry.task.is_finished())
                    .unwrap_or(false)
            };
            if finished {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for stopped runtime task to exit"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let started = host
            .control_public_agent(&agent_id, ControlAction::Start)
            .await
            .unwrap();
        started
            .enqueue(MessageEnvelope::new(
                &agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "start me".into(),
                },
            ))
            .await
            .unwrap();
        wait_for_brief_count(&started, 1).await;

        let registry = host.inner.runtimes.read().await;
        let entry = registry
            .agents
            .get(&agent_id)
            .expect("expected live runtime entry");
        assert!(
            !entry.task.is_finished(),
            "start should restore a live runtime loop"
        );
        drop(registry);

        let briefs = started.storage().read_recent_briefs(10).unwrap();
        assert!(briefs.iter().any(|brief| brief.text.contains("done")));
        let events = started.storage().read_recent_events(100).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "message_acknowledged"
                && event.data["summary"].as_str() == Some("Queued work: start me")
        }));
    }

    #[tokio::test]
    async fn runtime_loop_failure_recovers_and_replays_canonical_claim_without_host_access() {
        let (_home, host) = canonical_test_host();
        let agent_id = host.config().default_agent_id.clone();
        let runtime = host.default_runtime().await.unwrap();
        runtime.inject_runtime_loop_failure_after_next_claim();
        let message = runtime
            .enqueue(MessageEnvelope::new(
                &agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "recover claimed message".into(),
                },
            ))
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let events = runtime.storage().read_recent_events(32).unwrap();
            let task_finished = host
                .inner
                .runtimes
                .read()
                .await
                .agents
                .get(&agent_id)
                .is_some_and(|entry| entry.task.is_finished());
            if task_finished
                && events
                    .iter()
                    .any(|event| event.kind == "agent_runtime_loop_failed")
                && runtime
                    .storage()
                    .latest_queue_entries()
                    .unwrap()
                    .iter()
                    .any(|entry| {
                        entry.message_id == message.id && entry.status == QueueEntryStatus::Dequeued
                    })
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for runtime loop failure event"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let state = runtime.agent_state().await.unwrap();
        let failure = state
            .last_runtime_failure
            .expect("runtime loop failure should be visible in agent state");
        assert!(failure
            .summary
            .contains("injected agent runtime loop failure after queue claim"));
        assert!(runtime
            .storage()
            .read_recent_events(32)
            .unwrap()
            .iter()
            .all(|event| {
                event.kind != "queue_claim_released_for_runtime_restart"
                    || event.data["message_id"] != message.id
            }));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let queue_replayed = runtime
                .storage()
                .latest_queue_entries()
                .unwrap()
                .iter()
                .any(|entry| {
                    entry.message_id == message.id && entry.status == QueueEntryStatus::Processed
                });
            let events = runtime.storage().read_recent_events(64).unwrap();
            let recovery_recorded = events.iter().any(|event| {
                event.kind == "scheduler_bootstrap_claim_recovered"
                    && event.data["message_id"].as_str() == Some(message.id.as_str())
                    && event.data["recovery_outcome"].as_str()
                        == Some("attempt_interrupted_for_reentry")
            });
            let replay_started = events.iter().any(|event| {
                event.kind == "turn_replay_started"
                    && event.data["message_id"].as_str() == Some(message.id.as_str())
            });
            let recovered = events.iter().any(|event| {
                event.kind == "runtime_loop_recovered"
                    && event.data["failed_generation"].as_u64() == Some(1)
                    && event.data["recovered_generation"].as_u64() == Some(2)
            });
            let brief_delivered = runtime
                .storage()
                .read_recent_briefs(10)
                .unwrap()
                .iter()
                .any(|brief| brief.related_message_id.as_deref() == Some(message.id.as_str()));
            if queue_replayed && recovery_recorded && replay_started && recovered && brief_delivered
            {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                let queue = runtime.storage().latest_queue_entries().unwrap();
                let briefs = runtime.storage().read_recent_briefs(10).unwrap();
                let task_finished = host
                    .inner
                    .runtimes
                    .read()
                    .await
                    .agents
                    .get(&agent_id)
                    .is_none_or(|entry| entry.task.is_finished());
                panic!(
                    "timed out waiting for automatic canonical claim reconciliation and replay: \
                     queue={queue:?}, events={events:?}, briefs={briefs:?}, \
                     task_finished={task_finished}"
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let registry = host.inner.runtimes.read().await;
        let entry = registry
            .agents
            .get(&agent_id)
            .expect("rebuilt runtime should be registered");
        assert!(
            !entry.task.is_finished(),
            "automatic recovery should start a fresh runtime loop"
        );
    }

    #[tokio::test]
    async fn non_retryable_runtime_loop_failure_does_not_trigger_automatic_recovery() {
        let (_home, host) = canonical_test_host();
        let agent_id = host.config().default_agent_id.clone();
        let runtime = host.default_runtime().await.unwrap();
        runtime.inject_non_retryable_runtime_loop_failure_after_next_claim();
        runtime
            .enqueue(MessageEnvelope::new(
                &agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "do not automatically recover".into(),
                },
            ))
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let finished = host
                .inner
                .runtimes
                .read()
                .await
                .agents
                .get(&agent_id)
                .is_some_and(|entry| entry.task.is_finished());
            if finished {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for non-retryable runtime failure"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
        let registry = host.inner.runtimes.read().await;
        let entry = registry
            .agents
            .get(&agent_id)
            .expect("failed runtime entry should remain registered");
        assert_eq!(entry.generation, 1);
        assert!(
            entry.task.is_finished(),
            "non-retryable failures should not spawn a fresh runtime"
        );
        assert!(!registry.recovering.contains_key(&agent_id));
        drop(registry);
        assert!(runtime
            .storage()
            .read_recent_events(64)
            .unwrap()
            .iter()
            .all(|event| event.kind != "runtime_loop_recovered"));
    }

    #[test]
    fn runtime_host_new_builds_provider_from_valid_config() {
        let fixture = provider_test_config(Some("anthropic-token"));
        let host = RuntimeHost::new(fixture.config);
        assert!(host.is_ok());
    }

    #[test]
    fn runtime_host_new_fails_when_no_configured_provider_is_available() {
        let fixture = provider_test_config(None);
        let err = RuntimeHost::new(fixture.config)
            .err()
            .expect("missing provider auth should fail host construction");
        assert!(err
            .to_string()
            .contains("no available providers for configured model chain"));
        assert!(err
            .to_string()
            .contains("anthropic@default/claude-sonnet-5"));
    }

    #[tokio::test]
    async fn get_public_agent_rejects_private_child() {
        let (_home, host) = test_host();

        let child = AgentIdentityRecord::new(
            "child_private_1",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(host.config().default_agent_id.clone()),
            Some("task-1".into()),
        );
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();

        let err = host
            .get_public_agent("child_private_1")
            .await
            .err()
            .expect("private child should be rejected by get_public_agent");
        match err {
            PublicAgentError::Private { agent_id } => {
                assert_eq!(agent_id, "child_private_1");
            }
            other => panic!("expected Private error, got: {other}"),
        }
    }

    #[tokio::test]
    async fn get_agent_for_local_status_accepts_private_child() {
        let (_home, host) = test_host();

        let child = AgentIdentityRecord::new(
            "child_local_1",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(host.config().default_agent_id.clone()),
            Some("task-1".into()),
        );
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();

        let child_storage = host.agent_storage("child_local_1").unwrap();
        let mut child_state = AgentState::new("child_local_1");
        child_state.status = AgentStatus::AwakeRunning;
        child_storage.write_agent(&child_state).unwrap();

        let runtime = host
            .get_agent_for_local_status("child_local_1")
            .await
            .expect("private child should be accessible through local status API");
        let summary = runtime.agent_summary().await.unwrap();
        assert_eq!(summary.identity.agent_id, "child_local_1");
        assert_eq!(summary.identity.visibility, AgentVisibility::Private);
        assert_eq!(summary.identity.kind, AgentKind::Child);
    }

    #[tokio::test]
    async fn get_agent_for_local_status_rejects_archived() {
        let (_home, host) = test_host();

        let mut child = AgentIdentityRecord::new(
            "child_archived_1",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(host.config().default_agent_id.clone()),
            Some("task-1".into()),
        );
        child.status = AgentRegistryStatus::Deleted;
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();

        let err = host
            .get_agent_for_local_status("child_archived_1")
            .await
            .err()
            .expect("archived agent should be rejected");
        match err {
            PublicAgentError::Deleted { agent_id } => {
                assert_eq!(agent_id, "child_archived_1");
            }
            other => panic!("expected Archived error, got: {other}"),
        }
    }

    #[tokio::test]
    async fn get_agent_for_local_status_rejects_unknown() {
        let (_home, host) = test_host();

        let err = host
            .get_agent_for_local_status("nonexistent_agent")
            .await
            .err()
            .expect("unknown agent should be rejected");
        match err {
            PublicAgentError::NotFound { agent_id } => {
                assert_eq!(agent_id, "nonexistent_agent");
            }
            other => panic!("expected NotFound error, got: {other}"),
        }
    }

    #[tokio::test]
    async fn get_agent_for_local_status_accepts_public_agent() {
        let (_home, host) = test_host();

        let default_id = host.config().default_agent_id.clone();
        let runtime = host
            .get_agent_for_local_status(&default_id)
            .await
            .expect("public agent should be accessible through local status API");
        let summary = runtime.agent_summary().await.unwrap();
        assert_eq!(summary.identity.agent_id, default_id);
        assert_eq!(summary.identity.visibility, AgentVisibility::Public);
    }

    #[tokio::test]
    async fn deletion_coordinator_drives_job_to_completed() {
        let (_home, host) = test_host();

        // Create a public self-owned agent to delete.
        let agent = AgentIdentityRecord::new(
            "delete-me",
            AgentKind::Default,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.append_agent_identity(&agent).unwrap();
        host.runtime_db().agent_identities().upsert(&agent).unwrap();

        // Begin deletion.
        let (identity, job, created) = host
            .begin_public_agent_deletion("delete-me", false, "operator")
            .await
            .unwrap();
        assert!(created);
        assert_eq!(identity.status, AgentRegistryStatus::Deleting);
        assert_eq!(job.status, AgentDeletionStatus::Pending);

        // Execute the deletion job.
        match host.execute_deletion_job(job).await {
            Ok(_) => {}
            Err(e) => panic!("deletion failed: {e:#}"),
        }

        // Verify identity is Deleted.
        let final_identity = host
            .agent_identity_record("delete-me")
            .unwrap()
            .expect("identity should still exist");
        assert_eq!(final_identity.status, AgentRegistryStatus::Deleted);
        assert!(final_identity.deleted_at.is_some());

        // Verify job is Completed.
        let final_job = host
            .runtime_db()
            .agent_deletions()
            .latest_for_agent("delete-me")
            .unwrap()
            .expect("job should exist");
        assert_eq!(final_job.status, AgentDeletionStatus::Completed);
        assert!(final_job.completed_at.is_some());

        // Agent home should be removed.
        assert!(!host.agent_data_dir("delete-me").exists());
    }

    #[tokio::test]
    async fn deletion_coordinator_terminalizes_open_work_items() {
        let (_home, host) = test_host();
        let agent = AgentIdentityRecord::new(
            "delete-work",
            AgentKind::Default,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.append_agent_identity(&agent).unwrap();
        host.runtime_db().agent_identities().upsert(&agent).unwrap();
        let mut work_item =
            WorkItemRecord::new("delete-work", "unfinished work", WorkItemState::Open);
        work_item.blocked_by = Some("operator input".into());
        host.runtime_db()
            .work_items()
            .insert_new(&work_item)
            .unwrap();

        let (_, job, _) = host
            .begin_public_agent_deletion("delete-work", false, "operator")
            .await
            .unwrap();
        host.execute_deletion_job(job).await.unwrap();

        let completed = host
            .runtime_db()
            .work_items()
            .latest(&work_item.id)
            .unwrap()
            .unwrap();
        assert_eq!(completed.state, WorkItemState::Completed);
        assert_eq!(completed.blocked_by, None);
        assert_eq!(
            completed.result_summary.as_deref(),
            Some("Agent deleted before work completed")
        );
    }

    #[tokio::test]
    async fn deletion_coordinator_resumes_from_persisted_phase() {
        let (_home, host) = test_host();

        // Create a public self-owned agent.
        let agent = AgentIdentityRecord::new(
            "resume-me",
            AgentKind::Default,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.append_agent_identity(&agent).unwrap();
        host.runtime_db().agent_identities().upsert(&agent).unwrap();

        // Begin deletion.
        let (_, job, _) = host
            .begin_public_agent_deletion("resume-me", false, "operator")
            .await
            .unwrap();

        // Simulate a crash after Quiesce by manually advancing the job phase.
        let mut advanced_job = job.clone();
        advanced_job.status = AgentDeletionStatus::Running;
        advanced_job.phase = AgentDeletionPhase::Ingress;
        advanced_job.updated_at = Utc::now();
        host.runtime_db()
            .agent_deletions()
            .update(&advanced_job)
            .unwrap();

        // Execute should resume from Ingress.
        host.execute_deletion_job(advanced_job).await.unwrap();

        // Verify completion.
        let final_job = host
            .runtime_db()
            .agent_deletions()
            .latest_for_agent("resume-me")
            .unwrap()
            .expect("job should exist");
        assert_eq!(final_job.status, AgentDeletionStatus::Completed);
        assert_eq!(
            host.agent_identity_record("resume-me")
                .unwrap()
                .unwrap()
                .status,
            AgentRegistryStatus::Deleted
        );
    }

    #[tokio::test]
    async fn deletion_cascade_private_children() {
        let (_home, host) = test_host();
        let parent_id = "cascade-parent";

        // Create parent agent.
        let parent = AgentIdentityRecord::new(
            parent_id,
            AgentKind::Default,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.append_agent_identity(&parent).unwrap();
        host.runtime_db()
            .agent_identities()
            .upsert(&parent)
            .unwrap();

        // Create a private child.
        let child = AgentIdentityRecord::new(
            "cascade-child",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(parent_id.to_string()),
            Some("task-1".to_string()),
        );
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();

        // Begin deletion with cascade.
        let (_, job, _) = host
            .begin_public_agent_deletion(parent_id, true, "operator")
            .await
            .unwrap();

        match host.execute_deletion_job(job).await {
            Ok(_) => {}
            Err(e) => panic!("deletion failed: {e:#}"),
        }

        // Both parent and child should be Deleted.
        assert_eq!(
            host.agent_identity_record(parent_id)
                .unwrap()
                .unwrap()
                .status,
            AgentRegistryStatus::Deleted
        );
        assert_eq!(
            host.agent_identity_record("cascade-child")
                .unwrap()
                .unwrap()
                .status,
            AgentRegistryStatus::Deleted
        );
    }

    #[tokio::test]
    async fn archive_private_agent_aborts_pending_queue_entries() {
        let (_home, host) = test_host();
        let parent_id = "archive-queue-parent";

        // Create parent agent.
        let parent = AgentIdentityRecord::new(
            parent_id,
            AgentKind::Default,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.append_agent_identity(&parent).unwrap();
        host.runtime_db()
            .agent_identities()
            .upsert(&parent)
            .unwrap();

        // Create a private child with queued entries.
        let child_id = "archive-queue-child";
        let child = AgentIdentityRecord::new(
            child_id,
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(parent_id.to_string()),
            Some("task-queue-1".to_string()),
        );
        host.append_agent_identity(&child).unwrap();
        host.runtime_db().agent_identities().upsert(&child).unwrap();
        host.runtime_db()
            .agent_canonical_relations()
            .upsert_supervision(
                &supervised_creation_records(&child, parent_id, "task-queue-1", None)
                    .supervision
                    .unwrap(),
            )
            .unwrap();

        let now = Utc::now();
        host.runtime_db()
            .queue_entries()
            .upsert(&QueueEntryRecord {
                message_id: "msg-queued-orphan-1".into(),
                agent_id: child_id.into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Queued,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        host.runtime_db()
            .queue_entries()
            .upsert(&QueueEntryRecord {
                message_id: "msg-interrupted-orphan-1".into(),
                agent_id: child_id.into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Interrupted,
                created_at: now,
                updated_at: now,
            })
            .unwrap();

        // Archive the private child agent.
        host.archive_private_agent(child_id).await.unwrap();

        // Verify queued entries were aborted.
        assert_eq!(
            host.runtime_db()
                .queue_entries()
                .latest("msg-queued-orphan-1")
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Aborted
        );
        assert_eq!(
            host.runtime_db()
                .queue_entries()
                .latest("msg-interrupted-orphan-1")
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Aborted
        );
        assert_eq!(
            host.runtime_db()
                .agent_canonical_relations()
                .latest(child_id)
                .unwrap()
                .unwrap()
                .supervision
                .unwrap()
                .state,
            AgentSupervisionState::Closed
        );
    }

    #[tokio::test]
    async fn deletion_pipeline_aborts_pending_queue_entries() {
        let (_home, host) = test_host();

        // Create a public self-owned agent with queued entries.
        let agent = AgentIdentityRecord::new(
            "delete-queue-me",
            AgentKind::Default,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.append_agent_identity(&agent).unwrap();
        host.runtime_db().agent_identities().upsert(&agent).unwrap();

        let now = Utc::now();
        host.runtime_db()
            .queue_entries()
            .upsert(&QueueEntryRecord {
                message_id: "msg-queued-delete-1".into(),
                agent_id: "delete-queue-me".into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Queued,
                created_at: now,
                updated_at: now,
            })
            .unwrap();

        // Begin deletion and execute the full pipeline.
        let (_, job, _) = host
            .begin_public_agent_deletion("delete-queue-me", false, "operator")
            .await
            .unwrap();
        host.execute_deletion_job(job).await.unwrap();

        // Verify the queued entry was aborted.
        assert_eq!(
            host.runtime_db()
                .queue_entries()
                .latest("msg-queued-delete-1")
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Aborted
        );
    }

    #[tokio::test]
    async fn read_only_host_methods_do_not_activate_runtime() {
        let (_home, host) = test_host();
        let agent_id = host.config().default_agent_id.clone();

        let _storage = host
            .operator_agent_read_storage(&agent_id)
            .expect("read storage");
        assert!(host.inner.runtimes.read().await.agents.is_empty());

        let loaded = host.try_get_loaded_runtime(&agent_id).await;
        assert!(loaded.is_none());
        assert!(host.inner.runtimes.read().await.agents.is_empty());

        let summary = host
            .local_agent_summary(&agent_id)
            .await
            .expect("local agent summary");
        assert_eq!(summary.agent.id, agent_id);
        assert_eq!(
            summary.loaded_agents_md.user_global_status,
            crate::types::AgentsMdLoadStatus::NotEvaluated
        );
        assert_eq!(
            summary.loaded_agents_md.agent_status,
            crate::types::AgentsMdLoadStatus::NotEvaluated
        );
        assert_eq!(
            summary.loaded_agents_md.workspace_status,
            crate::types::AgentsMdLoadStatus::NotEvaluated
        );
        assert!(host.inner.runtimes.read().await.agents.is_empty());

        let _worktree_summary = host
            .public_agent_worktree_summary(&agent_id)
            .expect("worktree summary");
        assert!(host.inner.runtimes.read().await.agents.is_empty());

        let _inspection = host
            .public_agent_scheduler_repair_inspection(&agent_id)
            .expect("scheduler repair inspection");
        assert!(host.inner.runtimes.read().await.agents.is_empty());
    }

    #[tokio::test]
    async fn startup_recovery_activates_active_task_owners_and_waits_for_bootstrap() {
        let (_home, host) = canonical_test_host();
        let agent_id = "startup-task-owner";
        host.create_named_agent(agent_id, None).await.unwrap();
        host.unload_runtime(agent_id).await;
        let storage = host.agent_storage(agent_id).unwrap();
        storage
            .append_task(&TaskRecord {
                id: "task-startup-owner".into(),
                agent_id: agent_id.into(),
                kind: crate::types::TaskKind::CommandTask,
                status: TaskStatus::Running,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                parent_message_id: None,
                work_item_id: None,
                summary: Some("startup owner task".into()),
                detail: None,
                recovery: None,
            })
            .unwrap();

        assert!(host.try_get_loaded_runtime(agent_id).await.is_none());
        host.recover_orphaned_queue_claims_at_startup()
            .await
            .unwrap();

        let task = storage
            .latest_task_record("task-startup-owner")
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Interrupted);
        let message_id = format!("message:task-restart:{}", task.id);
        assert!(storage.read_message_by_id(&message_id).unwrap().is_some());
        assert!(host.try_get_loaded_runtime(agent_id).await.is_some());
    }

    #[tokio::test]
    async fn startup_recovery_replays_interrupted_operator_prompt_while_work_item_waits_external() {
        let (_home, host) = canonical_test_host();
        let config = host.config().as_ref().clone();
        let agent_id = "startup-interrupted-prompt";
        host.create_named_agent(agent_id, None).await.unwrap();
        host.unload_runtime(agent_id).await;
        let storage = host.agent_storage(agent_id).unwrap();
        let mut work_item = WorkItemRecord::new(agent_id, "wait for review", WorkItemState::Open);
        work_item.id = "work-startup-external".into();
        storage.append_work_item(&work_item).unwrap();
        let now = Utc::now();
        storage
            .append_wait_condition(&WaitConditionRecord {
                id: "wait-startup-external".into(),
                agent_id: agent_id.into(),
                work_item_id: Some(work_item.id.clone()),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::External,
                source: Some("github".into()),
                subject_ref: Some("github:holon-run/holon#2528".into()),
                waiting_for: "review".into(),
                wake_sources: vec![WakeSource::ExternalIngress {
                    external_trigger_id: Some("trigger-startup-external".into()),
                }],
                continuation: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,
                turn_id: None,
                trigger_message_id: None,
                triggered_at: None,
            })
            .unwrap();
        let mut state = storage.read_agent().unwrap().unwrap();
        state.current_work_item_id = Some(work_item.id.clone());
        storage.write_agent(&state).unwrap();
        let mut message = MessageEnvelope::new(
            agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "resume after restart".into(),
            },
        );
        message.id = "msg-startup-interrupted-prompt".into();
        message.turn_id = Some("turn-startup-interrupted-prompt".into());
        storage.append_message(&message).unwrap();
        storage
            .append_queue_entry(&QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: agent_id.into(),
                priority: message.priority,
                status: QueueEntryStatus::Interrupted,
                created_at: message.created_at,
                updated_at: now,
            })
            .unwrap();

        assert!(storage.latest_active_task_records(10).unwrap().is_empty());
        assert!(host.try_get_loaded_runtime(agent_id).await.is_none());
        drop(storage);
        drop(host);

        let started = Arc::new(Notify::new());
        let started_wait = started.notified();
        tokio::pin!(started_wait);
        started_wait.as_mut().enable();
        let restarted = RuntimeHost::new_with_provider(
            config,
            Arc::new(BlockingProvider {
                started: started.clone(),
            }),
        )
        .unwrap();
        assert!(restarted
            .recover_orphaned_queue_claims_at_startup()
            .await
            .unwrap()
            .is_empty());
        tokio::time::timeout(Duration::from_secs(5), &mut started_wait)
            .await
            .expect("startup recovery should replay the interrupted prompt");

        let runtime = restarted
            .try_get_loaded_runtime(agent_id)
            .await
            .expect("startup recovery should keep the interrupted prompt owner active");
        assert!(runtime
            .storage()
            .read_recent_events(100)
            .unwrap()
            .iter()
            .any(|event| {
                event.kind == "turn_replay_started"
                    && event.data["source_turn_id"] == "turn-startup-interrupted-prompt"
            }));
    }

    #[tokio::test]
    async fn startup_recovery_converges_stopped_task_owner_without_reentry() {
        let (_home, host) = canonical_test_host();
        let agent_id = "stopped-task-owner";
        host.create_named_agent(agent_id, None).await.unwrap();
        host.unload_runtime(agent_id).await;
        let storage = host.agent_storage(agent_id).unwrap();
        let mut state = storage.read_agent().unwrap().unwrap();
        state.status = AgentStatus::Stopped;
        storage.write_agent(&state).unwrap();
        storage
            .append_task(&TaskRecord {
                id: "task-stopped-owner".into(),
                agent_id: agent_id.into(),
                kind: crate::types::TaskKind::CommandTask,
                status: TaskStatus::Running,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                parent_message_id: None,
                work_item_id: None,
                summary: Some("stopped owner task".into()),
                detail: None,
                recovery: None,
            })
            .unwrap();

        host.recover_orphaned_queue_claims_at_startup()
            .await
            .unwrap();

        let task = storage
            .latest_task_record("task-stopped-owner")
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Interrupted);
        assert_eq!(
            task.detail
                .as_ref()
                .and_then(|detail| detail["interrupted_reason"].as_str()),
            Some("agent_stopped")
        );
        let result_message_id = task
            .parent_message_id
            .as_deref()
            .expect("agent stop should atomically persist a TaskResult");
        assert_eq!(
            storage
                .read_message_by_id(result_message_id)
                .unwrap()
                .expect("agent stop TaskResult")
                .kind,
            MessageKind::TaskResult
        );
        assert!(storage
            .read_message_by_id("message:task-restart:task-stopped-owner")
            .unwrap()
            .is_none());
    }

    fn startup_timer_fixture(
        id: &str,
        agent_id: &str,
        status: TimerStatus,
        interval_ms: Option<u64>,
        next_fire_at: Option<chrono::DateTime<Utc>>,
    ) -> TimerRecord {
        TimerRecord {
            id: id.into(),
            agent_id: agent_id.into(),
            created_at: Utc::now(),
            duration_ms: 60_000,
            interval_ms,
            repeat: interval_ms.is_some(),
            status,
            summary: Some("startup timer summary".into()),
            next_fire_at,
            last_fired_at: None,
            fire_count: 0,
        }
    }

    fn assert_startup_timer_tick_count(storage: &AppStorage, timer_id: &str, expected: usize) {
        let ticks = storage
            .read_recent_messages(200)
            .unwrap()
            .iter()
            .filter(|message| {
                message.kind == MessageKind::TimerTick
                    && matches!(&message.origin, MessageOrigin::Timer { timer_id: origin } if origin == timer_id)
            })
            .count();
        assert_eq!(
            ticks, expected,
            "timer {timer_id} should have produced exactly {expected} TimerTick message(s)"
        );
    }

    #[tokio::test]
    async fn startup_recovery_activates_timer_only_owner_and_resolves_waiting_timer() {
        let (_home, host) = canonical_test_host();
        let config = host.config().as_ref().clone();
        let agent_id = "startup-timer-owner";
        host.create_named_agent(agent_id, None).await.unwrap();
        let storage = host.agent_storage(agent_id).unwrap();
        let mut work_item = WorkItemRecord::new(agent_id, "wait for timer", WorkItemState::Open);
        work_item.id = "work-startup-timer".into();
        storage.append_work_item(&work_item).unwrap();
        let now = Utc::now();
        // The timer is created already overdue. Overdue state comes from
        // wall-clock time passing while the agent is unloaded, not from a
        // record rewrite: the timers upsert guard rejects writes whose
        // effective updated_at (next_fire_at) moves backwards.
        host.runtime_db()
            .timers()
            .upsert(&startup_timer_fixture(
                "timer-startup-owner-1",
                agent_id,
                TimerStatus::Active,
                None,
                Some(now - chrono::Duration::minutes(30)),
            ))
            .unwrap();
        // Register the wait through the live runtime so the durable wait
        // condition and its timer wake binding are real, rather than a
        // hand-written condition that skips wake admission state.
        // Execution-protocol Waiting authority for the registered
        // condition is seeded below.
        let registration = host
            .try_get_loaded_runtime(agent_id)
            .await
            .expect("created agent runtime should stay loaded")
            .register_wait_for(
                agent_id,
                Some(work_item.id.clone()),
                crate::runtime::WaitForWakeKind::Timer,
                Some("timer-startup-owner-1".into()),
                "timer catch-up".into(),
                None,
            )
            .await
            .unwrap();
        // register_wait_for only records execution-protocol Waiting
        // authority when a live execution attempt settles with a Wait
        // outcome. Seed the same protocol state for the registered
        // condition, mirroring what a waiting turn persists, so the
        // recovered TimerTick can claim this work item back into an
        // execution.
        let registered_work_item = host
            .runtime_db()
            .work_items()
            .latest(&work_item.id)
            .unwrap()
            .unwrap();
        crate::runtime::tests::support::seed_waiting_work_execution(
            &storage,
            &registered_work_item,
            &registration.condition.id,
        );
        host.unload_runtime(agent_id).await;

        // The timer is the only recovery signal: no queue or task candidates exist.
        assert!(storage.latest_queue_entries().unwrap().is_empty());
        assert!(storage.latest_active_task_records(10).unwrap().is_empty());
        assert!(host
            .runtime_db()
            .queue_entries()
            .recovery_candidate_agent_ids()
            .unwrap()
            .is_empty());
        assert!(host
            .runtime_db()
            .tasks()
            .active_owner_agent_ids()
            .unwrap()
            .is_empty());
        assert!(host.try_get_loaded_runtime(agent_id).await.is_none());
        drop(storage);
        drop(host);

        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        assert!(restarted
            .recover_orphaned_queue_claims_at_startup()
            .await
            .unwrap()
            .is_empty());

        restarted
            .try_get_loaded_runtime(agent_id)
            .await
            .expect("startup recovery should activate the timer-only owner");
        let storage = restarted.agent_storage(agent_id).unwrap();
        let timer = storage
            .latest_timer_record("timer-startup-owner-1")
            .unwrap()
            .unwrap();
        assert_eq!(timer.status, TimerStatus::Completed);
        assert_eq!(timer.fire_count, 1);
        assert_startup_timer_tick_count(&storage, "timer-startup-owner-1", 1);

        let mut resolved = false;
        for _ in 0..100 {
            if storage
                .active_wait_conditions_for_agent(agent_id)
                .unwrap()
                .is_empty()
            {
                resolved = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            resolved,
            "the recovered TimerTick should resolve the waiting timer condition"
        );
    }

    #[tokio::test]
    async fn startup_recovery_discovers_timer_only_owner_without_wait_records() {
        let (_home, host) = canonical_test_host();
        let agent_id = "startup-timer-no-wait";
        host.create_named_agent(agent_id, None).await.unwrap();
        host.unload_runtime(agent_id).await;
        let storage = host.agent_storage(agent_id).unwrap();
        let overdue = Utc::now() - chrono::Duration::minutes(5);
        host.runtime_db()
            .timers()
            .upsert(&startup_timer_fixture(
                "timer-startup-no-wait-1",
                agent_id,
                TimerStatus::Active,
                None,
                Some(overdue),
            ))
            .unwrap();

        assert!(host.try_get_loaded_runtime(agent_id).await.is_none());
        host.recover_orphaned_queue_claims_at_startup()
            .await
            .unwrap();

        assert!(host.try_get_loaded_runtime(agent_id).await.is_some());
        let timer = storage
            .latest_timer_record("timer-startup-no-wait-1")
            .unwrap()
            .unwrap();
        assert_eq!(timer.status, TimerStatus::Completed);
        assert_eq!(timer.fire_count, 1);
        assert_startup_timer_tick_count(&storage, "timer-startup-no-wait-1", 1);
    }

    #[tokio::test]
    async fn startup_recovery_overdue_repeating_timer_fires_once_and_keeps_cadence() {
        let (_home, host) = canonical_test_host();
        let agent_id = "startup-timer-repeating";
        host.create_named_agent(agent_id, None).await.unwrap();
        host.unload_runtime(agent_id).await;
        let storage = host.agent_storage(agent_id).unwrap();
        // Overdue by five hourly periods; recovery must catch up once, not replay all.
        let overdue = Utc::now() - chrono::Duration::hours(5);
        host.runtime_db()
            .timers()
            .upsert(&startup_timer_fixture(
                "timer-startup-repeating-1",
                agent_id,
                TimerStatus::Active,
                Some(3_600_000),
                Some(overdue),
            ))
            .unwrap();

        assert!(host.try_get_loaded_runtime(agent_id).await.is_none());
        host.recover_orphaned_queue_claims_at_startup()
            .await
            .unwrap();

        let timer = storage
            .latest_timer_record("timer-startup-repeating-1")
            .unwrap()
            .unwrap();
        assert_eq!(timer.status, TimerStatus::Active);
        assert_eq!(timer.fire_count, 1);
        assert!(timer
            .next_fire_at
            .is_some_and(|next_fire_at| next_fire_at > Utc::now()));
        assert_startup_timer_tick_count(&storage, "timer-startup-repeating-1", 1);
    }

    #[tokio::test]
    async fn startup_recovery_keeps_terminal_timer_owner_and_unrelated_agents_unloaded() {
        let (_home, host) = canonical_test_host();
        let terminal_agent_id = "startup-timer-terminal";
        let unrelated_agent_id = "startup-agent-no-timer";
        for agent_id in [terminal_agent_id, unrelated_agent_id] {
            host.create_named_agent(agent_id, None).await.unwrap();
            host.unload_runtime(agent_id).await;
        }
        let storage = host.agent_storage(terminal_agent_id).unwrap();
        let future = Utc::now() + chrono::Duration::hours(1);
        host.runtime_db()
            .timers()
            .upsert(&startup_timer_fixture(
                "timer-terminal-completed",
                terminal_agent_id,
                TimerStatus::Completed,
                None,
                None,
            ))
            .unwrap();
        host.runtime_db()
            .timers()
            .upsert(&startup_timer_fixture(
                "timer-terminal-cancelled",
                terminal_agent_id,
                TimerStatus::Cancelled,
                None,
                Some(future),
            ))
            .unwrap();

        host.recover_orphaned_queue_claims_at_startup()
            .await
            .unwrap();

        assert!(host
            .try_get_loaded_runtime(terminal_agent_id)
            .await
            .is_none());
        assert!(host
            .try_get_loaded_runtime(unrelated_agent_id)
            .await
            .is_none());
        assert_startup_timer_tick_count(&storage, "timer-terminal-completed", 0);
        assert_startup_timer_tick_count(&storage, "timer-terminal-cancelled", 0);
    }

    #[tokio::test]
    async fn startup_recovery_skips_stopped_timer_only_owner() {
        let (_home, host) = canonical_test_host();
        let agent_id = "startup-timer-stopped";
        host.create_named_agent(agent_id, None).await.unwrap();
        host.unload_runtime(agent_id).await;
        let storage = host.agent_storage(agent_id).unwrap();
        let mut state = storage.read_agent().unwrap().unwrap();
        state.status = AgentStatus::Stopped;
        storage.write_agent(&state).unwrap();
        let overdue = Utc::now() - chrono::Duration::minutes(15);
        host.runtime_db()
            .timers()
            .upsert(&startup_timer_fixture(
                "timer-startup-stopped-1",
                agent_id,
                TimerStatus::Active,
                None,
                Some(overdue),
            ))
            .unwrap();

        host.recover_orphaned_queue_claims_at_startup()
            .await
            .unwrap();

        assert!(host.try_get_loaded_runtime(agent_id).await.is_none());
        let timer = storage
            .latest_timer_record("timer-startup-stopped-1")
            .unwrap()
            .unwrap();
        assert_eq!(timer.status, TimerStatus::Active);
        assert_eq!(timer.fire_count, 0);
        assert_startup_timer_tick_count(&storage, "timer-startup-stopped-1", 0);
        let state = storage.read_agent().unwrap().unwrap();
        assert_eq!(state.status, AgentStatus::Stopped);
    }

    #[tokio::test]
    async fn startup_recovery_activates_timer_and_task_owner_once() {
        let (_home, host) = canonical_test_host();
        let agent_id = "startup-timer-and-task";
        host.create_named_agent(agent_id, None).await.unwrap();
        host.unload_runtime(agent_id).await;
        let storage = host.agent_storage(agent_id).unwrap();
        storage
            .append_task(&TaskRecord {
                id: "task-startup-timer-owner".into(),
                agent_id: agent_id.into(),
                kind: crate::types::TaskKind::CommandTask,
                status: TaskStatus::Running,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                parent_message_id: None,
                work_item_id: None,
                summary: Some("startup timer owner task".into()),
                detail: None,
                recovery: None,
            })
            .unwrap();
        let overdue = Utc::now() - chrono::Duration::minutes(10);
        host.runtime_db()
            .timers()
            .upsert(&startup_timer_fixture(
                "timer-startup-and-task-1",
                agent_id,
                TimerStatus::Active,
                None,
                Some(overdue),
            ))
            .unwrap();

        assert!(host.try_get_loaded_runtime(agent_id).await.is_none());
        host.recover_orphaned_queue_claims_at_startup()
            .await
            .unwrap();

        let task = storage
            .latest_task_record("task-startup-timer-owner")
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Interrupted);
        let timer = storage
            .latest_timer_record("timer-startup-and-task-1")
            .unwrap()
            .unwrap();
        assert_eq!(timer.status, TimerStatus::Completed);
        assert_eq!(timer.fire_count, 1);
        assert_startup_timer_tick_count(&storage, "timer-startup-and-task-1", 1);
        assert!(host.try_get_loaded_runtime(agent_id).await.is_some());
    }

    #[tokio::test]
    async fn startup_recovery_does_not_refire_persisted_one_shot_timer() {
        let (_home, host) = canonical_test_host();
        let config = host.config().as_ref().clone();
        let agent_id = "startup-timer-once";
        host.create_named_agent(agent_id, None).await.unwrap();
        host.unload_runtime(agent_id).await;
        let storage = host.agent_storage(agent_id).unwrap();
        let overdue = Utc::now() - chrono::Duration::minutes(20);
        host.runtime_db()
            .timers()
            .upsert(&startup_timer_fixture(
                "timer-startup-once-1",
                agent_id,
                TimerStatus::Active,
                None,
                Some(overdue),
            ))
            .unwrap();
        drop(storage);
        drop(host);

        let second_host =
            RuntimeHost::new_with_provider(config.clone(), Arc::new(StubProvider::new("done")))
                .unwrap();
        second_host
            .recover_orphaned_queue_claims_at_startup()
            .await
            .unwrap();
        let storage = second_host.agent_storage(agent_id).unwrap();
        let timer = storage
            .latest_timer_record("timer-startup-once-1")
            .unwrap()
            .unwrap();
        assert_eq!(timer.status, TimerStatus::Completed);
        assert_eq!(timer.fire_count, 1);
        assert_startup_timer_tick_count(&storage, "timer-startup-once-1", 1);
        drop(storage);
        tokio::time::timeout(Duration::from_secs(5), second_host.shutdown())
            .await
            .expect("second host shutdown")
            .unwrap();
        drop(second_host);

        let third_host =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        third_host
            .recover_orphaned_queue_claims_at_startup()
            .await
            .unwrap();
        assert!(third_host.try_get_loaded_runtime(agent_id).await.is_none());
        let storage = third_host.agent_storage(agent_id).unwrap();
        let timer = storage
            .latest_timer_record("timer-startup-once-1")
            .unwrap()
            .unwrap();
        assert_eq!(timer.status, TimerStatus::Completed);
        assert_eq!(timer.fire_count, 1);
        assert_startup_timer_tick_count(&storage, "timer-startup-once-1", 1);
    }

    #[tokio::test]
    async fn startup_recovery_ignores_interrupted_queue_for_deleted_private_child() {
        let (_home, host) = canonical_test_host();
        let agent_id = "tmp_child_deleted_recovery";
        let now = Utc::now();
        let mut identity = AgentIdentityRecord::new(
            agent_id,
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(host.config().default_agent_id.clone()),
            Some("task-deleted-child".into()),
        );
        identity.status = AgentRegistryStatus::Deleted;
        identity.deleted_at = Some(now);
        host.append_agent_identity(&identity).unwrap();
        host.runtime_db()
            .queue_entries()
            .upsert(&QueueEntryRecord {
                message_id: "msg-deleted-child".into(),
                agent_id: agent_id.into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Interrupted,
                created_at: now,
                updated_at: now,
            })
            .unwrap();

        assert!(host
            .recover_orphaned_queue_claims_at_startup()
            .await
            .unwrap()
            .is_empty());
        assert!(host.try_get_loaded_runtime(agent_id).await.is_none());
        assert_eq!(
            host.runtime_db()
                .queue_entries()
                .latest("msg-deleted-child")
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Interrupted
        );
    }

    #[tokio::test]
    async fn shutdown_rejects_new_runtime_activation() {
        let (_home, host) = test_host();
        let agent_id = host.config().default_agent_id.clone();

        host.shutdown().await.expect("shutdown");

        match host.get_public_agent(&agent_id).await {
            Err(PublicAgentError::ShuttingDown) => {}
            Err(err) => panic!("expected ShuttingDown, got {err:?}"),
            Ok(_) => panic!("activation should be rejected after shutdown"),
        }
    }

    #[tokio::test]
    async fn recover_orphaned_dequeued_claims_recovers_only_orphaned() {
        let (_home, host) = test_host();
        let agent_id = host.config().default_agent_id.clone();
        let runtime_db = host.runtime_db().clone();
        let storage = AppStorage::new_for_agent(
            host.agent_data_dir(&agent_id),
            agent_id.clone(),
            runtime_db.clone(),
        )
        .unwrap();

        let make_message = |id: &str| {
            let mut msg = MessageEnvelope::new(
                &agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: format!("test {id}"),
                },
            );
            msg.id = id.to_string();
            msg
        };
        let make_queue_entry = |msg: &MessageEnvelope, status: QueueEntryStatus| QueueEntryRecord {
            message_id: msg.id.clone(),
            agent_id: agent_id.clone(),
            priority: msg.priority.clone(),
            status,
            created_at: msg.created_at,
            updated_at: Utc::now(),
        };

        // Orphaned: no activation, no terminal turn
        let msg_a = make_message("msg-orphaned");
        storage.append_message(&msg_a).unwrap();
        storage
            .append_queue_entry(&make_queue_entry(&msg_a, QueueEntryStatus::Dequeued))
            .unwrap();

        // Has unified execution attempt: should NOT be recovered.
        let msg_b = make_message("msg-activated");
        storage.append_message(&msg_b).unwrap();
        storage
            .append_queue_entry(&make_queue_entry(&msg_b, QueueEntryStatus::Dequeued))
            .unwrap();
        let attempt = ExecutionAttempt {
            attempt_id: format!("activation:message:{}", msg_b.id),
            agent_id: agent_id.clone(),
            source_message_id: Some(msg_b.id.clone()),
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::QueueMessage {
                    message_id: msg_b.id.clone(),
                },
                generation: 1,
            },
            binding: ExecutionBinding::AgentLifecycle {
                agent_id: agent_id.clone(),
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
            admitted_at: Utc::now().to_rfc3339(),
            terminal_at: None,
        };
        runtime_db
            .transaction(|tx| {
                tx.execute(
                    "INSERT INTO execution_protocol_attempts (
                       agent_id, attempt_id, lifecycle_state,
                       source_identity_json, source_generation,
                       recovery_of_attempt_id, terminal_outcome_id, payload_json
                     ) VALUES (?1, ?2, 'open', ?3, ?4, NULL, NULL, ?5)",
                    rusqlite::params![
                        &agent_id,
                        &attempt.attempt_id,
                        serde_json::to_string(&attempt.source.identity)?,
                        attempt.source.generation as i64,
                        serde_json::to_string(&attempt)?,
                    ],
                )?;
                Ok(())
            })
            .expect("insert execution attempt");

        // A legacy activation without a unified attempt no longer protects a claim.
        let msg_legacy = make_message("msg-legacy-activation");
        storage.append_message(&msg_legacy).unwrap();
        storage
            .append_queue_entry(&make_queue_entry(&msg_legacy, QueueEntryStatus::Dequeued))
            .unwrap();
        runtime_db
            .transaction(|tx| {
                tx.execute(
                    "INSERT INTO scheduler_activations
                       (agent_id, activation_id, authority_id, owner_kind, owner_id,
                        work_item_id, admitted_generation, admission_kind,
                        recovery_for_activation_id, wait_id, wait_generation,
                        lifecycle_state, idempotency_key, payload_json, created_at, updated_at)
                     VALUES (?, ?, '', 'work_item', 'test-work', 'test-work', 0, 'scheduling', NULL, NULL, NULL, 'running', '', '{}', ?, ?)",
                    rusqlite::params![
                        &agent_id,
                        format!("activation:message:{}", msg_legacy.id),
                        Utc::now().timestamp_millis(),
                        Utc::now().timestamp_millis(),
                    ],
                )?;
                Ok(())
            })
            .expect("insert legacy activation");

        // Has terminal turn: completion evidence settles the stale claim.
        let msg_c = make_message("msg-terminal");
        storage.append_message(&msg_c).unwrap();
        storage
            .append_queue_entry(&make_queue_entry(&msg_c, QueueEntryStatus::Dequeued))
            .unwrap();
        let mut turn = crate::types::TurnRecord::new(&agent_id, "turn-terminal", 0);
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&msg_c));
        turn.terminal = Some(crate::types::TurnTerminalSummary {
            kind: crate::types::TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason: None,
            completed_at: Utc::now(),
            duration_ms: 100,
        });
        runtime_db
            .turn_records()
            .upsert(&turn)
            .expect("upsert turn");

        // Has a result brief without a terminal turn: should also settle, not replay.
        let msg_result = make_message("msg-result-brief");
        storage.append_message(&msg_result).unwrap();
        storage
            .append_queue_entry(&make_queue_entry(&msg_result, QueueEntryStatus::Dequeued))
            .unwrap();
        let result_brief = BriefRecord::new(
            &agent_id,
            BriefKind::Result,
            "completed before restart",
            Some(msg_result.id.clone()),
            None,
        );
        storage.append_brief(&result_brief).unwrap();

        // Has failure evidence: should settle as aborted, not replay.
        let msg_failure = make_message("msg-failure-brief");
        storage.append_message(&msg_failure).unwrap();
        storage
            .append_queue_entry(&make_queue_entry(&msg_failure, QueueEntryStatus::Dequeued))
            .unwrap();
        let failure_brief = BriefRecord::new(
            &agent_id,
            BriefKind::Failure,
            "failed before restart",
            Some(msg_failure.id.clone()),
            None,
        );
        storage.append_brief(&failure_brief).unwrap();

        // Has delivery evidence attached to its trigger turn: should settle, not replay.
        let msg_delivery = make_message("msg-delivery");
        storage.append_message(&msg_delivery).unwrap();
        storage
            .append_queue_entry(&make_queue_entry(&msg_delivery, QueueEntryStatus::Dequeued))
            .unwrap();
        let mut delivery_turn = crate::types::TurnRecord::new(&agent_id, "turn-delivery", 1);
        delivery_turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(
            &msg_delivery,
        ));
        storage.append_turn(&delivery_turn).unwrap();
        let mut delivery = DeliverySummaryRecord::new(
            &agent_id,
            "work-delivery",
            "delivered before restart",
            Some(1),
            None,
        );
        delivery.turn_id = Some(delivery_turn.turn_id);
        storage.append_delivery_summary(&delivery).unwrap();

        let recovered = host
            .recover_orphaned_queue_claims_at_startup()
            .await
            .expect("recovery");
        assert_eq!(recovered, vec![agent_id.clone()]);

        let entries = storage.latest_queue_entries().unwrap();
        for entry in &entries {
            match entry.message_id.as_str() {
                "msg-orphaned" | "msg-legacy-activation" => assert_eq!(
                    entry.status,
                    QueueEntryStatus::Interrupted,
                    "orphaned should be recovered"
                ),
                "msg-activated" => assert_eq!(
                    entry.status,
                    QueueEntryStatus::Dequeued,
                    "active execution should retain its claim"
                ),
                "msg-terminal" | "msg-result-brief" | "msg-delivery" => assert_eq!(
                    entry.status,
                    QueueEntryStatus::Processed,
                    "successful completion evidence should prevent replay"
                ),
                "msg-failure-brief" => assert_eq!(
                    entry.status,
                    QueueEntryStatus::Aborted,
                    "failure completion evidence should prevent replay"
                ),
                _ => {}
            }
        }

        let events = storage.read_recent_events(32).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "orphaned_queue_claim_recovered"
                && event.data["message_id"] == "msg-result-brief"
                && event.data["next_status"] == "processed"
                && event.data["reason"] == "terminal_or_result_completion_evidence"
        }));
        assert!(events.iter().any(|event| {
            event.kind == "orphaned_queue_claim_recovered"
                && event.data["message_id"] == "msg-failure-brief"
                && event.data["next_status"] == "aborted"
                && event.data["reason"] == "terminal_failure_completion_evidence"
        }));
    }
}
