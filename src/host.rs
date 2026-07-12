use std::{
    collections::HashMap,
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio::{
    sync::{Notify, RwLock},
    task::{spawn_blocking, JoinHandle},
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::{
    agent_memory::load_agent_memory,
    agent_template::{
        discover_agent_templates_catalog, ensure_agent_home_agents_md_without_template_with_home,
        initialize_agent_home_from_template_with_catalog,
        initialize_agent_home_from_template_with_home,
        initialize_agent_home_without_template_with_home,
    },
    agents_md::load_agents_md,
    callbacks::hash_callback_token,
    config::{AppConfig, RuntimeModelCatalog},
    context::ContextConfig,
    host_registry::RuntimeRegistry,
    ids,
    prompt::{build_effective_prompt_with_apply_patch_surface, EffectivePrompt},
    provider::{build_provider_from_config, AgentProvider},
    runtime::{InitialWorkspaceBinding, RuntimeHandle},
    runtime_db::RuntimeDb,
    skills::{
        effective_skill_root_registrations, skills_runtime_view_from_catalog, SkillVisibility,
        SkillsRegistry,
    },
    storage::{AppStorage, EventBus, PublishedAuditEvent},
    system::{ExecutionScopeKind, HostLocalBoundary, WorkspaceAccessMode},
    tool::{apply_patch::ApplyPatchSurface, ToolRegistry},
    types::{
        AdmissionContext, AgentIdentityRecord, AgentIdentityView, AgentKind, AgentLifecycleHint,
        AgentListEntry, AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentState,
        AgentStatus, AgentSummary, AgentVisibility, AuthorityClass, ChildAgentSummary,
        ClosureOutcome, ExternalTriggerRecord, MessageBody, MessageDeliverySurface,
        MessageEnvelope, MessageKind, MessageOrigin, OperatorNotificationRecord, Priority,
        RuntimeFailureSummary, SpawnAgentModelResolution, SpawnAgentModelResolutionStatus,
        TaskKind, TaskRecord, TaskStatus, TranscriptEntry, TranscriptEntryKind, WorkspaceEntry,
        WorkspaceOccupancyRecord,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicAgentActivitySnapshot {
    pub agent_id: String,
    pub status: AgentStatus,
    pub active_task_count: usize,
    pub last_runtime_failure: Option<RuntimeFailureSummary>,
}

#[derive(Clone)]
pub struct RuntimeHost {
    inner: Arc<HostInner>,
}

pub(crate) const TEMP_AGENT_PREFIX: &str = "tmp_";
const TEMP_RUN_AGENT_PREFIX: &str = "tmp_run_";
const TEMP_CHILD_AGENT_PREFIX: &str = "tmp_child_";
// Give runtime loops a short cleanup window while keeping daemon stop bounded.
#[cfg(not(test))]
const HOST_SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
#[cfg(test)]
const HOST_SHUTDOWN_GRACE: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub enum PublicAgentError {
    NotFound { agent_id: String },
    Archived { agent_id: String },
    Private { agent_id: String },
    Stopped { agent_id: String },
    Runtime(anyhow::Error),
}

impl std::fmt::Display for PublicAgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { agent_id } => write!(f, "agent {} not found", agent_id),
            Self::Archived { agent_id } => write!(f, "agent {} is archived", agent_id),
            Self::Private { agent_id } => write!(f, "agent {} is private", agent_id),
            Self::Stopped { agent_id } => {
                write!(f, "agent {} is stopped; start first", agent_id)
            }
            Self::Runtime(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PublicAgentError {}

struct HostInner {
    registry: RuntimeRegistry,
    runtime_db: RuntimeDb,
    event_bus: EventBus,
    memory_index_notify: Arc<Notify>,
    daemon_indexer_token: CancellationToken,
    daemon_indexer_handle: Mutex<Option<JoinHandle<()>>>,
    skills_registry: Arc<RwLock<SkillsRegistry>>,
    static_provider: Option<Arc<dyn AgentProvider>>,
    agents: RwLock<HashMap<String, AgentEntry>>,
}

struct AgentEntry {
    runtime: RuntimeHandle,
    task: JoinHandle<()>,
}

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
    pub task_detail: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct ChildTaskTerminalResult {
    pub status: TaskStatus,
    pub text: String,
    pub task_detail: Option<Value>,
}

async fn apply_spawn_model_resolution(
    runtime: &RuntimeHandle,
    resolution: &SpawnAgentModelResolution,
) -> Result<()> {
    if resolution.resolution_status == SpawnAgentModelResolutionStatus::Inherited {
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

    fn new_inner(
        config: AppConfig,
        static_provider: Option<Arc<dyn AgentProvider>>,
    ) -> Result<Self> {
        let runtime_db =
            RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())?;
        let registry = RuntimeRegistry::new(config, runtime_db.clone())?;
        let host = Self {
            inner: Arc::new(HostInner {
                registry,
                runtime_db,
                event_bus: EventBus::new(1024),
                memory_index_notify: Arc::new(Notify::new()),
                daemon_indexer_token: CancellationToken::new(),
                daemon_indexer_handle: Mutex::new(None),
                skills_registry: Arc::new(RwLock::new(SkillsRegistry::new())),
                static_provider,
                agents: RwLock::new(HashMap::new()),
            }),
        };
        host.ensure_default_agent_identity()?;
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
            let agents = self.inner.agents.read().await;
            agents.values().map(|entry| entry.runtime.clone()).collect()
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

    const DAEMON_INDEXER_BATCH: usize = 500;
    const DAEMON_INDEXER_FALLBACK_POLL: Duration = Duration::from_secs(60);

    async fn run_daemon_memory_indexer(self) {
        use crate::memory::refresh_memory_index_bounded;
        loop {
            if self.inner.daemon_indexer_token.is_cancelled() {
                break;
            }
            let agent_ids = match self
                .inner
                .runtime_db
                .runtime_index_outbox()
                .agent_ids_with_pending()
            {
                Ok(ids) => ids,
                Err(error) => {
                    tracing::warn!(error = %error, "daemon memory indexer: failed to query pending agents");
                    self.wait_daemon_indexer_round().await;
                    continue;
                }
            };

            let mut did_work = false;
            for agent_id in &agent_ids {
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
                        if status.lag > 0 || status.consumption_was_limited {
                            did_work = true;
                        }
                        tracing::debug!(
                            agent_id = %agent_id,
                            freshness = %status.freshness,
                            lag = status.lag,
                            "daemon memory indexer: processed agent"
                        );
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(
                            agent_id = %agent_id,
                            error = %error,
                            "daemon memory indexer: refresh failed"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            agent_id = %agent_id,
                            error = %error,
                            "daemon memory indexer: task failed"
                        );
                    }
                }
            }

            if did_work {
                tokio::task::yield_now().await;
            } else {
                self.wait_daemon_indexer_round().await;
            }
        }
    }

    async fn wait_daemon_indexer_round(&self) {
        tokio::select! {
            _ = self.inner.daemon_indexer_token.cancelled() => {}
            _ = self.inner.memory_index_notify.notified() => {}
            _ = tokio::time::sleep(Self::DAEMON_INDEXER_FALLBACK_POLL) => {}
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
            let mut agents = self.inner.agents.write().await;
            agents.drain().map(|(_, entry)| entry).collect::<Vec<_>>()
        };
        if entries.is_empty() {
            return Ok(());
        }
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
        Ok(())
    }

    pub(crate) async fn unload_runtime(&self, agent_id: &str) {
        let entry = self.inner.agents.write().await.remove(agent_id);
        if let Some(entry) = entry {
            entry.task.abort();
            let _ = entry.task.await;
        }
    }

    pub async fn default_runtime(&self) -> Result<RuntimeHandle> {
        self.ensure_default_agent_identity()?;
        self.ensure_default_agent_home_initialized().await?;
        self.get_or_create_agent(&self.config().default_agent_id)
            .await
    }

    fn public_agent_identity(
        &self,
        agent_id: &str,
    ) -> std::result::Result<AgentIdentityRecord, PublicAgentError> {
        let identity = self
            .agent_identity_record(agent_id)
            .map_err(PublicAgentError::Runtime)?
            .ok_or_else(|| PublicAgentError::NotFound {
                agent_id: agent_id.to_string(),
            })?;
        if identity.status != AgentRegistryStatus::Active {
            return Err(PublicAgentError::Archived {
                agent_id: agent_id.to_string(),
            });
        }
        if identity.visibility != AgentVisibility::Public {
            return Err(PublicAgentError::Private {
                agent_id: agent_id.to_string(),
            });
        }
        Ok(identity)
    }

    pub async fn get_public_agent(
        &self,
        agent_id: &str,
    ) -> std::result::Result<RuntimeHandle, PublicAgentError> {
        self.public_agent_identity(agent_id)?;
        self.get_or_create_agent(agent_id)
            .await
            .map_err(PublicAgentError::Runtime)
    }

    /// Get an agent for the local control/status API, allowing private child
    /// agents in addition to public ones. This relies on the current local
    /// trusted control API boundary rather than hiding `agent_id`.
    pub async fn get_agent_for_local_status(
        &self,
        agent_id: &str,
    ) -> std::result::Result<RuntimeHandle, PublicAgentError> {
        let identity = self
            .agent_identity_record(agent_id)
            .map_err(PublicAgentError::Runtime)?
            .ok_or_else(|| PublicAgentError::NotFound {
                agent_id: agent_id.to_string(),
            })?;
        if identity.status != AgentRegistryStatus::Active {
            return Err(PublicAgentError::Archived {
                agent_id: agent_id.to_string(),
            });
        }
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
        self.get_or_create_agent(agent_id)
            .await
            .map_err(PublicAgentError::Runtime)
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
        self.ensure_named_agent(agent_id, template, None, None)
            .await
            .map(|(record, _)| record)
    }

    async fn ensure_named_agent(
        &self,
        agent_id: &str,
        template: Option<&str>,
        lineage_parent_agent_id: Option<&str>,
        catalog_agent_home: Option<&Path>,
    ) -> Result<(AgentIdentityRecord, bool)> {
        self.validate_agent_id(agent_id)?;
        if agent_id == self.config().default_agent_id {
            if template.is_some() {
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
            if existing.status == AgentRegistryStatus::Archived {
                return Err(anyhow!("agent {} is archived", agent_id));
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
            if template.is_some() {
                return Err(anyhow!(
                    "agent {} already exists; template initialization only applies when creating a new agent",
                    agent_id
                ));
            }
            return Ok((existing, false));
        }
        if let Some(template) = template {
            let agent_home = self.agent_data_dir(agent_id);
            let user_home = self.config().home_dir.clone();
            if let Some(catalog_agent_home) = catalog_agent_home {
                initialize_agent_home_from_template_with_catalog(
                    &agent_home,
                    &user_home,
                    catalog_agent_home,
                    template,
                )
                .await?;
            } else {
                initialize_agent_home_from_template_with_home(&agent_home, &user_home, template)
                    .await?;
            }
        } else {
            let user_home = self.config().home_dir.clone();
            initialize_agent_home_without_template_with_home(
                &self.agent_data_dir(agent_id),
                &user_home,
            )
            .await?;
        }
        let record = AgentIdentityRecord::new(
            agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        )
        .with_lineage_parent_agent_id(lineage_parent_agent_id.map(ToString::to_string));
        self.append_agent_identity(&record)?;
        let _ = self.get_or_create_agent(agent_id).await?;
        Ok((record, true))
    }

    pub fn get_or_create_agent<'a>(
        &'a self,
        agent_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<RuntimeHandle>> + Send + 'a>> {
        Box::pin(async move {
            self.validate_agent_id(agent_id)?;
            {
                let agents = self.inner.agents.read().await;
                if let Some(entry) = agents.get(agent_id) {
                    if !entry.task.is_finished() {
                        return Ok(entry.runtime.clone());
                    }
                }
            }

            let stale_entry = {
                let mut agents = self.inner.agents.write().await;
                if agents
                    .get(agent_id)
                    .is_some_and(|entry| entry.task.is_finished())
                {
                    agents.remove(agent_id)
                } else {
                    None
                }
            };
            if let Some(entry) = stale_entry {
                let _ = entry.task.await;
            }

            if agent_id == self.config().default_agent_id {
                self.ensure_default_agent_identity()?;
                self.ensure_default_agent_home_initialized().await?;
            } else {
                match self.agent_identity_record(agent_id)? {
                    Some(identity) if identity.status == AgentRegistryStatus::Archived => {
                        return Err(anyhow!("agent {} is archived", agent_id));
                    }
                    Some(_) => {}
                    None => {
                        return Err(anyhow!(
                            "agent {} not found; create it first with 'holon agent create {}'",
                            agent_id,
                            agent_id
                        ));
                    }
                }
            }

            let (runtime, runtime_task) = self.spawn_runtime(agent_id)?;

            let mut stale_entry = None;
            let mut agents = self.inner.agents.write().await;
            if let Some(entry) = agents.get(agent_id) {
                if !entry.task.is_finished() {
                    runtime_task.abort();
                    return Ok(entry.runtime.clone());
                }
                stale_entry = agents.remove(agent_id);
            }
            agents.insert(
                agent_id.to_string(),
                AgentEntry {
                    runtime: runtime.clone(),
                    task: runtime_task,
                },
            );
            drop(agents);
            if let Some(entry) = stale_entry {
                let _ = entry.task.await;
            }
            Ok(runtime)
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
        let (runtime, runtime_task) = self.spawn_runtime(&agent_id)?;
        Ok((agent_id, runtime, runtime_task))
    }

    pub fn workspace_entries(&self) -> Result<Vec<WorkspaceEntry>> {
        self.inner.registry.workspace_entries()
    }

    pub fn resolve_workspace_aliases(
        &self,
        workspace_ids: &[String],
    ) -> Result<std::collections::HashMap<String, String>> {
        self.inner.registry.resolve_workspace_aliases(workspace_ids)
    }

    pub fn workspace_occupancies(&self) -> Result<Vec<WorkspaceOccupancyRecord>> {
        self.inner.registry.workspace_occupancies()
    }

    pub fn agent_identity_record(&self, agent_id: &str) -> Result<Option<AgentIdentityRecord>> {
        self.inner.registry.agent_identity_record(agent_id)
    }

    fn agent_identity_records(&self) -> Result<Vec<AgentIdentityRecord>> {
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

    pub(crate) fn append_agent_identity(&self, record: &AgentIdentityRecord) -> Result<()> {
        self.inner.registry.append_agent_identity(record)
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
                let agents = self.inner.agents.read().await;
                agents
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

    pub async fn public_agent_activity_snapshots(
        &self,
    ) -> Result<Vec<PublicAgentActivitySnapshot>> {
        self.ensure_default_agent_identity()?;
        let mut snapshots = Vec::new();
        for identity in self.agent_identity_records()?.into_iter().filter(|record| {
            record.status == AgentRegistryStatus::Active
                && record.visibility == AgentVisibility::Public
        }) {
            if let Some(runtime) = self.loaded_runtime(&identity.agent_id).await {
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
    ) -> std::result::Result<EffectivePrompt, PublicAgentError> {
        let identity = self.public_agent_identity(agent_id)?;
        self.preview_agent_prompt_from_storage(&identity, text, authority_class)
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
        if identity.status == AgentRegistryStatus::Archived {
            return Err(anyhow!("agent {} is archived", agent_id));
        }
        self.preview_agent_prompt_from_storage(&identity, text, authority_class)
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
        let loaded_agents_md = load_agents_md(
            Some(self.config().home_dir.as_path()),
            agent_home.as_path(),
            crate::runtime::workspace::workspace_anchor_for_state_ref(&state),
        )?;
        let loaded_agent_memory = load_agent_memory(agent_home.as_path())?;
        let skill_visibility = skill_visibility(&identity_view);
        let user_home = self.config().home_dir.clone();
        let workspace_anchor = state
            .active_workspace_entry
            .as_ref()
            .map(|entry| entry.workspace_anchor.as_path());
        let skill_roots = effective_skill_root_registrations(
            skill_visibility,
            Some(user_home.as_path()),
            &state.id,
            agent_home.as_path(),
            workspace_anchor,
        );
        let mut skill_registry = self.inner.skills_registry.write().await;
        skill_registry.sync_effective_roots(skill_roots.clone())?;
        let mut skills = skills_runtime_view_from_catalog(
            skill_registry.catalog_for_roots(&skill_roots, None),
            &skill_roots,
            &state.active_skills,
        );
        skills.agent_templates_catalog = discover_agent_templates_catalog(
            Some(self.config().home_dir.as_path()),
            agent_home.as_path(),
        );
        let config = self.config();
        let model_catalog = RuntimeModelCatalog::from_config(&config);
        let model_ref = model_catalog
            .provider_chain_for_turn(
                state.model_override.as_ref(),
                state.pending_fallback_model.as_ref(),
            )
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
        let apply_patch_surface =
            ApplyPatchSurface::for_model_ref(&model_ref.model_ref().as_string());
        let registry = ToolRegistry::new(execution.execution_root.clone());
        let available_tools = registry
            .tool_specs_with_families_for_apply_patch_surface(apply_patch_surface)?
            .into_iter()
            .filter(|(family, _)| {
                identity_view
                    .profile_preset
                    .allows_tool_capability_family(*family)
            })
            .map(|(_, tool)| tool)
            .collect::<Vec<_>>();
        let prompt_tools = provider.prompt_tool_specs(&available_tools);
        build_effective_prompt_with_apply_patch_surface(
            &storage,
            &state,
            &execution,
            &message,
            &self.runtime_context_config(),
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
            let storage = self.agent_storage(&identity.agent_id)?;
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
        if let Some(runtime) = self.loaded_runtime(agent_id).await {
            return runtime.child_agent_observability().await;
        }
        RuntimeHandle::child_agent_observability_from_storage(storage, state)
    }

    async fn loaded_runtime(&self, agent_id: &str) -> Option<RuntimeHandle> {
        let agents = self.inner.agents.read().await;
        agents.get(agent_id).map(|entry| entry.runtime.clone())
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

    async fn ensure_default_agent_home_initialized(&self) -> Result<()> {
        let agent_home = self.agent_data_dir(&self.config().default_agent_id);
        let user_home = self.config().home_dir.clone();
        let _ =
            ensure_agent_home_agents_md_without_template_with_home(&agent_home, &user_home).await?;
        Ok(())
    }

    async fn create_child_identity(
        &self,
        parent_agent_id: &str,
        task_id: &str,
        template: Option<&str>,
        catalog_agent_home: &Path,
    ) -> Result<AgentIdentityRecord> {
        let child_agent_id = ids::runtime_id(TEMP_CHILD_AGENT_PREFIX.trim_end_matches('_'));
        self.validate_agent_id(&child_agent_id)?;
        if let Some(template) = template {
            let user_home = self.config().home_dir.clone();
            initialize_agent_home_from_template_with_catalog(
                &self.agent_data_dir(&child_agent_id),
                &user_home,
                catalog_agent_home,
                template,
            )
            .await?;
        } else {
            let user_home = self.config().home_dir.clone();
            initialize_agent_home_without_template_with_home(
                &self.agent_data_dir(&child_agent_id),
                &user_home,
            )
            .await?;
        }
        let record = AgentIdentityRecord::new(
            child_agent_id,
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(parent_agent_id.to_string()),
            Some(task_id.to_string()),
        )
        .with_lineage_parent_agent_id(Some(parent_agent_id.to_string()));
        self.append_agent_identity(&record)?;
        Ok(record)
    }

    async fn archive_private_agent(&self, agent_id: &str) -> Result<()> {
        if let Some(mut identity) = self.agent_identity_record(agent_id)? {
            if identity.status != AgentRegistryStatus::Archived {
                identity.status = AgentRegistryStatus::Archived;
                identity.archived_at = Some(chrono::Utc::now());
                identity.updated_at = chrono::Utc::now();
                self.append_agent_identity(&identity)?;
            }
        }

        let entry = self.inner.agents.write().await.remove(agent_id);
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
                    let _ = storage.append_event(&crate::types::AuditEvent::new(
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

        Ok(matches!(
            task.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        ))
    }

    fn archive_private_agent_identity_record(&self, agent_id: &str) -> Result<()> {
        if let Some(mut identity) = self.agent_identity_record(agent_id)? {
            if identity.status != AgentRegistryStatus::Archived {
                identity.status = AgentRegistryStatus::Archived;
                identity.archived_at = Some(chrono::Utc::now());
                identity.updated_at = chrono::Utc::now();
                self.append_agent_identity(&identity)?;
            }
        }

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
        model_resolution: SpawnAgentModelResolution,
    ) -> Result<ChildTaskSpawn> {
        let parent_state = parent_runtime.agent_state().await?;
        let parent_agent_home = self.agent_data_dir(&parent_state.id);
        let child_identity = self
            .create_child_identity(
                &parent_state.id,
                &task.id,
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
            authority_class,
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
        }));
        child_runtime.enqueue(message).await?;

        Ok(ChildTaskSpawn {
            child_agent_id: child_identity.agent_id,
            child_turn_baseline,
            task_detail,
        })
    }

    async fn spawn_public_named_agent(
        &self,
        parent_runtime: RuntimeHandle,
        agent_id: &str,
        initial_message: Option<String>,
        authority_class: AuthorityClass,
        template: Option<String>,
        model_resolution: SpawnAgentModelResolution,
    ) -> Result<String> {
        let parent_state = parent_runtime.agent_state().await?;
        let parent_agent_home = self.agent_data_dir(&parent_state.id);
        let (named_identity, created) = self
            .ensure_named_agent(
                agent_id,
                template.as_deref(),
                Some(parent_state.id.as_str()),
                Some(&parent_agent_home),
            )
            .await?;
        let named_runtime = self.get_or_create_agent(&named_identity.agent_id).await?;
        if created {
            named_runtime
                .inherit_attached_workspaces_from_parent_state(&parent_state)
                .await?;
        }
        apply_spawn_model_resolution(&named_runtime, &model_resolution).await?;

        let Some(initial_message) = initial_message else {
            return Ok(named_identity.agent_id);
        };

        let mut message = crate::types::MessageEnvelope::new(
            named_identity.agent_id.clone(),
            crate::types::MessageKind::InternalFollowup,
            crate::types::MessageOrigin::System {
                subsystem: "spawn_agent".into(),
            },
            authority_class,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text {
                text: initial_message,
            },
        )
        .with_admission(
            crate::types::MessageDeliverySurface::RuntimeSystem,
            crate::types::AdmissionContext::RuntimeOwned,
        );
        message.metadata = Some(json!({
            "spawn_preset": AgentProfilePreset::PublicNamed,
            "creator_agent_id": parent_state.id,
            "spawned_agent_id": named_identity.agent_id,
            "bootstrap": true,
        }));
        named_runtime.enqueue(message).await?;
        Ok(named_identity.agent_id)
    }

    async fn await_child_terminal_result(
        &self,
        child_agent_id: &str,
        child_turn_baseline: u64,
        worktree: bool,
    ) -> Result<ChildTaskTerminalResult> {
        let storage = self.agent_storage(child_agent_id)?;
        if let Some(result) = self
            .completed_child_terminal_from_storage(&storage, child_agent_id, child_turn_baseline)
            .await?
        {
            self.archive_private_agent(child_agent_id).await?;
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
                "child_agent_id": child_agent_id,
                "child_kind": AgentKind::Child,
                "child_visibility": AgentVisibility::Private,
                "child_ownership": AgentOwnership::ParentSupervised,
                "child_profile_preset": AgentProfilePreset::PrivateChild,
                "child_observability": runtime.child_agent_observability().await?,
                "token_usage": json!({
                    "total": crate::types::TokenUsage::new(state.total_input_tokens, state.total_output_tokens),
                    "last_turn": state.last_turn_token_usage.clone(),
                    "total_model_rounds": state.total_model_rounds,
                }),
            });
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

            self.archive_private_agent(child_agent_id).await?;
            return Ok(ChildTaskTerminalResult {
                status,
                text,
                task_detail,
            });
        }
    }

    async fn completed_child_terminal_from_storage(
        &self,
        storage: &AppStorage,
        child_agent_id: &str,
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
            || child_has_active_lifecycle_blockers(storage, child_agent_id)?
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
                    "child_agent_id": child_agent_id,
                    "child_kind": AgentKind::Child,
                    "child_visibility": AgentVisibility::Private,
                    "child_ownership": AgentOwnership::ParentSupervised,
                    "child_profile_preset": AgentProfilePreset::PrivateChild,
                });
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

    fn spawn_runtime(&self, agent_id: &str) -> Result<(RuntimeHandle, JoinHandle<()>)> {
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
        let runtime_task = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                let _ = runtime.run().await;
            }
        });
        Ok((runtime, runtime_task))
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

    pub(crate) async fn child_summaries(
        &self,
        parent_agent_id: &str,
    ) -> Result<Vec<ChildAgentSummary>> {
        self.host()?.child_agent_summaries(parent_agent_id).await
    }

    /// Get a full AgentSummary for a given agent_id through the local trusted
    /// control boundary. This allows private child agent observation.
    pub(crate) async fn agent_summary_for(
        &self,
        agent_id: &str,
    ) -> Result<crate::types::AgentSummary> {
        let runtime = self.host()?.get_agent_for_local_status(agent_id).await?;
        runtime.agent_summary().await
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
        model_resolution: SpawnAgentModelResolution,
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

    pub(crate) async fn spawn_public_named_agent(
        &self,
        parent_runtime: RuntimeHandle,
        agent_id: &str,
        initial_message: Option<String>,
        authority_class: AuthorityClass,
        template: Option<String>,
        model_resolution: SpawnAgentModelResolution,
    ) -> Result<String> {
        self.host()?
            .spawn_public_named_agent(
                parent_runtime,
                agent_id,
                initial_message,
                authority_class,
                template,
                model_resolution,
            )
            .await
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

    pub(crate) async fn await_child_terminal_result(
        &self,
        child_agent_id: &str,
        child_turn_baseline: u64,
        worktree: bool,
    ) -> Result<ChildTaskTerminalResult> {
        self.host()?
            .await_child_terminal_result(child_agent_id, child_turn_baseline, worktree)
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
            authority_class,
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
        provider::{AgentProvider, ProviderTurnRequest, ProviderTurnResponse, StubProvider},
        runtime::RuntimeHandle,
        runtime_db::RuntimeDb,
        storage::AppStorage,
        system::WorkspaceProjectionKind,
        types::{
            AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentStatus,
            AgentVisibility, AuthorityClass, ControlAction, MessageBody, MessageEnvelope,
            MessageKind, MessageOrigin, Priority, QueueEntryRecord, QueueEntryStatus, TaskRecord,
            TaskRecoverySpec, TaskStatus, TurnTerminalKind,
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
        let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
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
            MessageOrigin::Operator { actor_id: None },
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
        assert!(host.inner.agents.read().await.is_empty());
        assert_eq!(storage.read_agent().unwrap(), None);
        let entries = storage.latest_queue_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message_id, message.id);
        assert_eq!(entries[0].status, QueueEntryStatus::Queued);
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

    fn inherited_model_resolution(provider: &str, model: &str) -> SpawnAgentModelResolution {
        SpawnAgentModelResolution {
            requested: None,
            resolved_provider: provider.to_string(),
            resolved_model: model.to_string(),
            resolved_parameters: None,
            resolution_status: SpawnAgentModelResolutionStatus::Inherited,
            policy_notes: Vec::new(),
        }
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
            api_cors: Default::default(),
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
        let agent_home = host.agent_data_dir("release-bot");
        assert!(agent_home.join("AGENTS.md").is_file());
        assert!(std::fs::read_to_string(agent_home.join("AGENTS.md"))
            .unwrap()
            .contains("## Holon Agent Home"));
        assert!(agent_home.join("memory/self.md").is_file());
        assert!(agent_home.join("memory/operator.md").is_file());
        assert!(agent_home.join("notes").is_dir());
        assert!(agent_home.join("work").is_dir());
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
    async fn named_agent_template_resolution_uses_config_home_not_os_home() {
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

        let worker = config_home
            .path()
            .join(".agents")
            .join("agent_templates")
            .join("worker");
        fs::create_dir_all(&worker).unwrap();
        fs::write(
            worker.join("AGENTS.md"),
            "# Config worker\n\nfrom config home\n",
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
        assert!(agents_md.starts_with("# Config worker\n\nfrom config home\n"));
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
    async fn spawn_public_named_preserves_existing_runtime_state() {
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

        host.spawn_public_named_agent(
            parent,
            "release-bot",
            Some("continue release work".into()),
            AuthorityClass::OperatorInstruction,
            None,
            inherited_model_resolution("anthropic", "claude-sonnet-4-6"),
        )
        .await
        .unwrap();

        let after = named.agent_summary().await.unwrap();
        assert!(
            after.agent.model_override.is_none(),
            "existing public named agent should keep its own runtime state"
        );
    }

    #[tokio::test]
    async fn spawn_public_named_records_lineage_without_supervision() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();

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
    async fn private_child_initial_message_sets_task_label_and_supervision_provenance() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
        let initial_message = "  investigate   remote\nTUI  access ".to_string();

        let spawned = parent
            .spawn_agent(
                Some(initial_message.clone()),
                AuthorityClass::OperatorInstruction,
                AgentProfilePreset::PrivateChild,
                None,
                false,
                None,
                None,
            )
            .await
            .unwrap();
        let task_id = spawned
            .supervision_task_id
            .clone()
            .expect("private child should return a supervision task");
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
    }

    #[tokio::test]
    async fn private_child_spawn_accepts_explicit_model_selection() {
        let fixture = provider_test_config(Some("dummy-token"));
        let host = RuntimeHost::new(fixture.config).unwrap();
        let parent = host.default_runtime().await.unwrap();

        let spawned = parent
            .spawn_agent(
                Some("compare implementation".into()),
                AuthorityClass::OperatorInstruction,
                AgentProfilePreset::PrivateChild,
                None,
                false,
                None,
                Some(crate::types::SpawnAgentModelRequest {
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

        let resolution = spawned
            .model_resolution
            .as_ref()
            .expect("spawn should return model resolution");
        assert_eq!(
            resolution.resolution_status,
            SpawnAgentModelResolutionStatus::Accepted
        );
        assert_eq!(resolution.resolved_provider, "anthropic");
        assert_eq!(resolution.resolved_model, "claude-haiku-4-5");

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

        let error = parent
            .spawn_agent(
                Some("compare implementation".into()),
                AuthorityClass::OperatorInstruction,
                AgentProfilePreset::PrivateChild,
                None,
                false,
                None,
                Some(crate::types::SpawnAgentModelRequest {
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

        let error = parent
            .spawn_agent(
                Some("   \n\t  ".into()),
                AuthorityClass::OperatorInstruction,
                AgentProfilePreset::PrivateChild,
                None,
                false,
                None,
                None,
            )
            .await
            .expect_err("blank private child initial_message should be rejected");

        assert!(error
            .to_string()
            .contains("private_child spawn requires non-empty initial_message"));
    }

    #[tokio::test]
    async fn public_named_initial_message_is_optional_and_inherits_only_attached_workspaces() {
        let (_home, host) = test_host();
        let parent = host.default_runtime().await.unwrap();
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
            "release-bot",
            None,
            AuthorityClass::OperatorInstruction,
            None,
            inherited_model_resolution("anthropic", "claude-sonnet-4-6"),
        )
        .await
        .unwrap();

        let named = host.get_public_agent("release-bot").await.unwrap();
        let named_state = named.agent_state().await.unwrap();
        let named_home_id = crate::types::agent_home_workspace_id("release-bot");
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
            "release-bot",
            Some("bootstrap release lane".into()),
            AuthorityClass::OperatorInstruction,
            None,
            inherited_model_resolution("anthropic", "claude-sonnet-4-6"),
        )
        .await
        .unwrap();
        let messages = named.storage().read_recent_messages(10).unwrap();
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
        let child_identity = host
            .create_child_identity(&parent_state.id, "task-1", None, &parent_agent_home)
            .await
            .unwrap();
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

        let child_identity = host
            .create_child_identity(
                &parent_state.id,
                "task-1",
                Some("worker"),
                &parent_agent_home,
            )
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
            let agents = host.inner.agents.read().await;
            assert!(
                !agents.contains_key("child_test"),
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
        assert_eq!(child_identity.status, AgentRegistryStatus::Archived);

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
        assert_eq!(identity.status, AgentRegistryStatus::Archived);
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
        assert_eq!(identity.status, AgentRegistryStatus::Archived);
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
        assert_eq!(identity.status, AgentRegistryStatus::Archived);
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
            let agents = host.inner.agents.read().await;
            assert!(agents.contains_key("alpha"));
            assert!(agents.contains_key("beta"));
        }

        host.unload_runtime("alpha").await;

        {
            let agents = host.inner.agents.read().await;
            assert!(!agents.contains_key("alpha"));
            assert!(agents.contains_key("beta"));
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
                MessageOrigin::Operator { actor_id: None },
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
            let mut agents = host.inner.agents.write().await;
            let entry = agents
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
                MessageOrigin::Operator { actor_id: None },
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
                MessageOrigin::Operator { actor_id: None },
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
                let agents = host.inner.agents.read().await;
                agents
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
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "start me".into(),
                },
            ))
            .await
            .unwrap();
        wait_for_brief_count(&started, 1).await;

        let agents = host.inner.agents.read().await;
        let entry = agents.get(&agent_id).expect("expected live runtime entry");
        assert!(
            !entry.task.is_finished(),
            "start should restore a live runtime loop"
        );
        drop(agents);

        let briefs = started.storage().read_recent_briefs(10).unwrap();
        assert!(briefs.iter().any(|brief| brief.text.contains("done")));
        let events = started.storage().read_recent_events(100).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "message_acknowledged"
                && event.data["summary"].as_str() == Some("Queued work: start me")
        }));
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
        child.status = AgentRegistryStatus::Archived;
        host.append_agent_identity(&child).unwrap();

        let err = host
            .get_agent_for_local_status("child_archived_1")
            .await
            .err()
            .expect("archived agent should be rejected");
        match err {
            PublicAgentError::Archived { agent_id } => {
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
}
