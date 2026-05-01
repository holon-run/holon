use std::{
    collections::HashMap,
    fs,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Weak},
    time::Duration,
};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio::{
    sync::RwLock,
    task::{spawn_blocking, JoinHandle},
};

use crate::{
    agent_template::{
        ensure_agent_home_agents_md_from_template_with_home,
        initialize_agent_home_from_template_with_home, initialize_agent_home_without_template,
        seed_builtin_templates_for_home, DEFAULT_AGENT_TEMPLATE_ID,
    },
    callbacks::hash_callback_token,
    config::{AppConfig, RuntimeModelCatalog},
    context::ContextConfig,
    host_registry::RuntimeRegistry,
    provider::{build_provider_from_config, AgentProvider},
    runtime::{InitialWorkspaceBinding, RuntimeHandle},
    storage::AppStorage,
    system::WorkspaceAccessMode,
    types::{
        AgentIdentityRecord, AgentIdentityView, AgentKind, AgentOwnership, AgentProfilePreset,
        AgentRegistryStatus, AgentState, AgentStatus, AgentSummary, AgentVisibility,
        ChildAgentSummary, ClosureOutcome, ExternalTriggerRecord, ExternalTriggerStatus,
        OperatorNotificationRecord, RuntimeFailureSummary, TaskRecord, TaskStatus, TranscriptEntry,
        TranscriptEntryKind, TrustLevel, WorkItemRecord, WorkPlanItem, WorkspaceEntry,
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
                write!(f, "agent {} is stopped; resume first", agent_id)
            }
            Self::Runtime(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PublicAgentError {}

struct HostInner {
    registry: RuntimeRegistry,
    static_provider: Option<Arc<dyn AgentProvider>>,
    agents: RwLock<HashMap<String, AgentEntry>>,
}

struct AgentEntry {
    runtime: RuntimeHandle,
    task: JoinHandle<()>,
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

impl RuntimeHost {
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
        seed_builtin_templates_for_home(&config.home_dir)?;
        let registry = RuntimeRegistry::new(config)?;
        let host = Self {
            inner: Arc::new(HostInner {
                registry,
                static_provider,
                agents: RwLock::new(HashMap::new()),
            }),
        };
        host.ensure_default_agent_identity()?;
        host.converge_private_child_identities()?;
        Ok(host)
    }

    pub fn config(&self) -> &AppConfig {
        self.inner.registry.config()
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
        for entry in entries {
            let _ = entry.runtime.request_service_shutdown().await;
            let _ = entry.task.await;
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

    pub async fn get_public_agent_for_external_ingress(
        &self,
        agent_id: &str,
    ) -> std::result::Result<RuntimeHandle, PublicAgentError> {
        self.public_agent_identity(agent_id)?;
        let state = AppStorage::new(self.agent_data_dir(agent_id))
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
        if action == crate::types::ControlAction::Resume && was_stopped {
            self.unload_runtime(agent_id).await;
        }
        runtime
            .control(action.clone())
            .await
            .map_err(PublicAgentError::Runtime)?;
        if action == crate::types::ControlAction::Resume && was_stopped {
            return self.get_public_agent(agent_id).await;
        }
        Ok(runtime)
    }

    pub async fn enqueue_public_work_item(
        &self,
        agent_id: &str,
        delivery_target: String,
    ) -> std::result::Result<(RuntimeHandle, crate::types::WorkItemRecord), PublicAgentError> {
        let runtime = self.get_public_agent(agent_id).await?;
        let (record, _) = runtime
            .create_work_item(delivery_target, None)
            .await
            .map_err(PublicAgentError::Runtime)?;
        Ok((runtime, record))
    }

    pub async fn create_named_agent(
        &self,
        agent_id: &str,
        template: Option<&str>,
    ) -> Result<AgentIdentityRecord> {
        self.ensure_named_agent(agent_id, template, None)
            .await
            .map(|(record, _)| record)
    }

    async fn ensure_named_agent(
        &self,
        agent_id: &str,
        template: Option<&str>,
        lineage_parent_agent_id: Option<&str>,
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
            initialize_agent_home_from_template_with_home(
                &self.agent_data_dir(agent_id),
                &self.config().home_dir,
                template,
            )
            .await?;
        } else {
            initialize_agent_home_without_template(&self.agent_data_dir(agent_id))?;
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
                            "agent {} not found; create it first with 'holon agents create {}'",
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
            "run" => format!("{TEMP_RUN_AGENT_PREFIX}{}", uuid::Uuid::new_v4().simple()),
            other => format!(
                "{TEMP_AGENT_PREFIX}{other}_{}",
                uuid::Uuid::new_v4().simple()
            ),
        };
        self.validate_agent_id(&agent_id)?;
        let (runtime, runtime_task) = self.spawn_runtime(&agent_id)?;
        Ok((agent_id, runtime, runtime_task))
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

    fn agent_identity_records(&self) -> Result<Vec<AgentIdentityRecord>> {
        self.inner.registry.agent_identity_records()
    }

    fn append_agent_identity(&self, record: &AgentIdentityRecord) -> Result<()> {
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

    pub fn public_agent_activity_snapshots(&self) -> Result<Vec<PublicAgentActivitySnapshot>> {
        self.ensure_default_agent_identity()?;
        let mut snapshots = Vec::new();
        for identity in self.agent_identity_records()?.into_iter().filter(|record| {
            record.status == AgentRegistryStatus::Active
                && record.visibility == AgentVisibility::Public
        }) {
            let state = AppStorage::new(self.agent_data_dir(&identity.agent_id))?
                .read_agent()?
                .unwrap_or_else(|| AgentState::new(identity.agent_id.clone()));
            snapshots.push(PublicAgentActivitySnapshot {
                agent_id: identity.agent_id,
                status: state.status.clone(),
                active_task_count: state.active_task_ids.len(),
                last_runtime_failure: state.last_runtime_failure,
            });
        }
        snapshots.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        Ok(snapshots)
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
            let storage = AppStorage::new(self.agent_data_dir(&identity.agent_id))?;
            let state = storage
                .read_agent()?
                .unwrap_or_else(|| AgentState::new(identity.agent_id.clone()));
            children.push(ChildAgentSummary {
                identity: AgentIdentityView::from_record(
                    &identity,
                    &self.config().default_agent_id,
                ),
                status: state.status.clone(),
                current_run_id: state.current_run_id.clone(),
                pending: state.pending,
                active_task_count: state.active_task_ids.len(),
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
        for agent_id in self.known_agent_ids().await? {
            let storage = AppStorage::new(self.agent_data_dir(&agent_id))?;
            if let Some(descriptor) =
                storage
                    .latest_external_triggers()?
                    .into_iter()
                    .find(|record| {
                        record.token_hash == token_hash
                            && record.status == ExternalTriggerStatus::Active
                    })
            {
                return Ok(Some((agent_id, descriptor)));
            }
        }
        Ok(None)
    }

    fn ensure_default_agent_identity(&self) -> Result<AgentIdentityRecord> {
        self.inner.registry.ensure_default_agent_identity()
    }

    async fn ensure_default_agent_home_initialized(&self) -> Result<()> {
        let agent_home = self.agent_data_dir(&self.config().default_agent_id);
        let _ = ensure_agent_home_agents_md_from_template_with_home(
            &agent_home,
            &self.config().home_dir,
            DEFAULT_AGENT_TEMPLATE_ID,
        )
        .await?;
        Ok(())
    }

    async fn create_child_identity(
        &self,
        parent_agent_id: &str,
        task_id: &str,
        template: Option<&str>,
    ) -> Result<AgentIdentityRecord> {
        let child_agent_id = format!("{TEMP_CHILD_AGENT_PREFIX}{}", uuid::Uuid::new_v4().simple());
        self.validate_agent_id(&child_agent_id)?;
        if let Some(template) = template {
            initialize_agent_home_from_template_with_home(
                &self.agent_data_dir(&child_agent_id),
                &self.config().home_dir,
                template,
            )
            .await?;
        } else {
            initialize_agent_home_without_template(&self.agent_data_dir(&child_agent_id))?;
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
        let parent_data_dir = self.agent_data_dir(parent_agent_id);
        if !parent_data_dir.exists() {
            return Ok(true);
        }
        let parent_storage = AppStorage::new(parent_data_dir)?;
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
        trust: TrustLevel,
        worktree: bool,
        template: Option<String>,
    ) -> Result<ChildTaskSpawn> {
        let parent_state = parent_runtime.agent_state().await?;
        let child_identity = self
            .create_child_identity(&parent_state.id, &task.id, template.as_deref())
            .await?;
        let child_runtime = self.get_or_create_agent(&child_identity.agent_id).await?;
        child_runtime
            .inherit_from_parent_state(&parent_state)
            .await?;
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
                        "trust": trust,
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
            trust,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text { text: prompt },
        )
        .with_admission(
            crate::types::MessageDeliverySurface::RuntimeSystem,
            crate::types::AdmissionContext::RuntimeOwned,
        );
        message.metadata = Some(json!({
            "delegated_task_id": task.id,
            "parent_agent_id": parent_state.id,
            "child_agent_id": child_identity.agent_id,
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
        prompt: String,
        trust: TrustLevel,
        template: Option<String>,
    ) -> Result<String> {
        let parent_state = parent_runtime.agent_state().await?;
        let (named_identity, created) = self
            .ensure_named_agent(
                agent_id,
                template.as_deref(),
                Some(parent_state.id.as_str()),
            )
            .await?;
        let named_runtime = self.get_or_create_agent(&named_identity.agent_id).await?;
        if created {
            named_runtime
                .inherit_from_parent_state(&parent_state)
                .await?;
        }

        let mut message = crate::types::MessageEnvelope::new(
            named_identity.agent_id.clone(),
            crate::types::MessageKind::InternalFollowup,
            crate::types::MessageOrigin::System {
                subsystem: "spawn_agent".into(),
            },
            trust,
            crate::types::Priority::Normal,
            crate::types::MessageBody::Text { text: prompt },
        )
        .with_admission(
            crate::types::MessageDeliverySurface::RuntimeSystem,
            crate::types::AdmissionContext::RuntimeOwned,
        );
        message.metadata = Some(json!({
            "spawn_preset": AgentProfilePreset::PublicNamed,
            "parent_agent_id": parent_state.id,
            "child_agent_id": named_identity.agent_id,
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
        let runtime = self.get_or_create_agent(child_agent_id).await?;
        let storage = AppStorage::new(self.agent_data_dir(child_agent_id))?;
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

    pub(crate) fn agent_data_dir(&self, agent_id: &str) -> PathBuf {
        let primary = self.config().data_dir.join("agents").join(agent_id);
        let legacy = self.config().data_dir.join("sessions").join(agent_id);
        if primary.exists() || !legacy.exists() {
            primary
        } else {
            legacy
        }
    }

    pub(crate) fn is_temporary_agent_id(agent_id: &str) -> bool {
        agent_id.starts_with(TEMP_AGENT_PREFIX)
    }

    fn validate_agent_id(&self, agent_id: &str) -> Result<()> {
        self.inner.registry.validate_agent_id(agent_id)
    }

    async fn known_agent_ids(&self) -> Result<Vec<String>> {
        let mut ids = {
            let agents = self.inner.agents.read().await;
            agents.keys().cloned().collect::<Vec<_>>()
        };
        ids.extend(
            self.agent_identity_records()?
                .into_iter()
                .filter(|record| record.status == AgentRegistryStatus::Active)
                .map(|record| record.agent_id),
        );
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    fn runtime_context_config(&self) -> ContextConfig {
        let base = ContextConfig {
            recent_messages: self.config().context_window_messages,
            recent_briefs: self.config().context_window_briefs,
            compaction_trigger_messages: self.config().compaction_trigger_messages,
            compaction_keep_recent_messages: self.config().compaction_keep_recent_messages,
            prompt_budget_estimated_tokens: self.config().prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens: self.config().compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens: self
                .config()
                .compaction_keep_recent_estimated_tokens,
            recent_episode_candidates: self.config().recent_episode_candidates,
            max_relevant_episodes: self.config().max_relevant_episodes,
        };
        RuntimeModelCatalog::from_config(self.config()).resolved_context_config(&base, None)
    }

    fn spawn_runtime(&self, agent_id: &str) -> Result<(RuntimeHandle, JoinHandle<()>)> {
        let runtime = if let Some(provider) = self.inner.static_provider.as_ref() {
            RuntimeHandle::new_static_with_host_bridge(
                agent_id.to_string(),
                self.agent_data_dir(agent_id),
                InitialWorkspaceBinding::Detached,
                self.config().callback_base_url.clone(),
                provider.clone(),
                self.config().default_agent_id.clone(),
                self.runtime_context_config(),
                self.bridge(),
                RuntimeModelCatalog::from_config(self.config()),
            )?
        } else {
            RuntimeHandle::new_reconfigurable_with_host_bridge(
                agent_id.to_string(),
                self.agent_data_dir(agent_id),
                InitialWorkspaceBinding::Detached,
                self.config().callback_base_url.clone(),
                self.config().clone(),
                self.config().default_agent_id.clone(),
                self.runtime_context_config(),
                self.bridge(),
            )?
        };
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
        let storage = AppStorage::new(host.agent_data_dir(child_agent_id))?;
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
        trust: TrustLevel,
        worktree: bool,
        template: Option<String>,
    ) -> Result<ChildTaskSpawn> {
        self.host()?
            .spawn_child_task(parent_runtime, task, prompt, trust, worktree, template)
            .await
    }

    pub(crate) async fn spawn_public_named_agent(
        &self,
        parent_runtime: RuntimeHandle,
        agent_id: &str,
        prompt: String,
        trust: TrustLevel,
        template: Option<String>,
    ) -> Result<String> {
        self.host()?
            .spawn_public_named_agent(parent_runtime, agent_id, prompt, trust, template)
            .await
    }

    pub(crate) async fn child_turn_index(&self, agent_id: &str) -> Result<u64> {
        let runtime = self.host()?.get_or_create_agent(agent_id).await?;
        Ok(runtime.agent_state().await?.turn_index)
    }

    pub(crate) async fn create_child_work_item(
        &self,
        agent_id: &str,
        delivery_target: String,
        plan: Option<Vec<WorkPlanItem>>,
    ) -> Result<WorkItemRecord> {
        let runtime = self.host()?.get_or_create_agent(agent_id).await?;
        let (work_item, _) = runtime.create_work_item(delivery_target, plan).await?;
        let (_, current) = runtime.pick_work_item(work_item.id.clone()).await?;
        Ok(current)
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
        trust: TrustLevel,
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
            trust,
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

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    use tempfile::tempdir;

    use crate::{
        config::{provider_registry_for_tests, ControlAuthMode, ModelRef},
        provider::StubProvider,
        runtime::RuntimeHandle,
        storage::AppStorage,
        types::{
            AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentStatus,
            AgentVisibility, ControlAction, MessageBody, MessageEnvelope, MessageKind,
            MessageOrigin, Priority, TaskRecord, TaskRecoverySpec, TaskStatus, TrustLevel,
        },
    };

    use super::*;

    struct ProviderConfigFixture {
        _home: tempfile::TempDir,
        _workspace: tempfile::TempDir,
        config: AppConfig,
    }

    fn test_host() -> (tempfile::TempDir, RuntimeHost) {
        let home = tempdir().unwrap();
        let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        let host =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        (home, host)
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
            config_file_path: home_path.join("config.json"),
            stored_config: Default::default(),
            default_model: ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
            fallback_models: Vec::new(),
            runtime_max_output_tokens: 8192,
            default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
            max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
            disable_provider_fallback: false,
            tui_alternate_screen: crate::config::AltScreenMode::Auto,
            validated_model_overrides: std::collections::HashMap::new(),
            validated_unknown_model_fallback: None,
            providers: provider_registry_for_tests(
                None,
                anthropic_token,
                PathBuf::from("/tmp/missing-codex-home"),
            ),
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
        assert!(agent_home.join(".holon/state/agent.json").is_file());
        assert!(agent_home.join(".holon/ledger").is_dir());
        assert!(!agent_home.join("agent.json").exists());
        let provenance: crate::agent_template::TemplateProvenanceRecord = serde_json::from_slice(
            &std::fs::read(crate::agent_template::template_provenance_path(&agent_home)).unwrap(),
        )
        .unwrap();
        assert_eq!(provenance.selector, DEFAULT_AGENT_TEMPLATE_ID);
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

    #[tokio::test]
    async fn spawn_public_named_preserves_existing_runtime_state() {
        let fixture = provider_test_config(Some("dummy-token"));
        let host = RuntimeHost::new(fixture.config).unwrap();
        let parent = host.default_runtime().await.unwrap();
        parent
            .set_model_override(ModelRef::parse("anthropic/claude-haiku-4-5").unwrap())
            .await
            .unwrap();

        host.create_named_agent("release-bot", None).await.unwrap();
        let named = host.get_public_agent("release-bot").await.unwrap();
        let before = named.agent_summary().await.unwrap();
        assert!(before.agent.model_override.is_none());

        host.spawn_public_named_agent(
            parent,
            "release-bot",
            "continue release work".into(),
            TrustLevel::TrustedOperator,
            None,
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
            "coordinate release work".into(),
            TrustLevel::TrustedOperator,
            None,
        )
        .await
        .unwrap();

        let identity = host
            .agent_identity_record("release-bot")
            .unwrap()
            .expect("public named identity should exist");
        assert_eq!(identity.parent_agent_id, None);
        assert_eq!(identity.delegated_from_task_id, None);
        assert_eq!(identity.lineage_parent_agent_id.as_deref(), Some("default"));

        let summary = host
            .get_public_agent("release-bot")
            .await
            .unwrap()
            .agent_summary()
            .await
            .unwrap();
        assert_eq!(
            summary.identity.lineage_parent_agent_id.as_deref(),
            Some("default")
        );
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
            "anthropic/claude-sonnet-4-6"
        );
        assert!(inherited.model.override_model.is_none());
        assert_eq!(
            inherited
                .model
                .resolved_policy
                .prompt_budget_estimated_tokens,
            180_000
        );
        assert!(inherited
            .model
            .available_models
            .iter()
            .any(|entry| entry.model_ref.as_string() == "openai/gpt-5.4"));

        let updated = runtime
            .set_model_override(ModelRef::parse("openai/gpt-5.4").unwrap())
            .await
            .unwrap();
        assert_eq!(
            updated.source,
            crate::types::AgentModelSource::AgentOverride
        );
        assert_eq!(updated.effective_model.as_string(), "openai/gpt-5.4");
        assert_eq!(
            updated.runtime_default_model.as_string(),
            "anthropic/claude-sonnet-4-6"
        );
        assert_eq!(
            updated
                .effective_fallback_models
                .iter()
                .map(|model| model.as_string())
                .collect::<Vec<_>>(),
            vec!["anthropic/claude-sonnet-4-6"]
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
            "anthropic/claude-sonnet-4-6"
        );
        assert_eq!(
            cleared.resolved_policy.prompt_budget_estimated_tokens,
            180_000
        );
    }

    #[tokio::test]
    async fn recovered_runtime_reapplies_persisted_model_override_to_provider_chain() {
        let fixture = provider_test_config(Some("dummy-token"));
        let host = RuntimeHost::new(fixture.config.clone()).unwrap();
        let bridge = RuntimeHostBridge {
            inner: Arc::downgrade(&host.inner),
        };
        let storage = AppStorage::new(fixture.config.data_dir.clone()).unwrap();
        let mut state = AgentState::new("default");
        state.model_override = Some(ModelRef::parse("anthropic/claude-haiku-4-5").unwrap());
        storage.write_agent(&state).unwrap();

        let runtime = RuntimeHandle::new_reconfigurable_with_host_bridge(
            "default",
            fixture.config.data_dir.clone(),
            fixture.config.workspace_dir.clone(),
            fixture.config.callback_base_url.clone(),
            fixture.config.clone(),
            fixture.config.default_agent_id.clone(),
            host.runtime_context_config(),
            bridge,
        )
        .unwrap();

        assert_eq!(
            runtime.current_provider().await.configured_model_refs(),
            vec![
                "anthropic/claude-haiku-4-5".to_string(),
                "anthropic/claude-sonnet-4-6".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn child_runtime_reconfigures_provider_when_inheriting_model_override() {
        let fixture = provider_test_config(Some("dummy-token"));
        let host = RuntimeHost::new(fixture.config).unwrap();
        let parent = host.default_runtime().await.unwrap();
        parent
            .set_model_override(ModelRef::parse("anthropic/claude-haiku-4-5").unwrap())
            .await
            .unwrap();
        let parent_state = parent.agent_state().await.unwrap();
        let child_identity = host
            .create_child_identity(&parent_state.id, "task-1", None)
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
                "anthropic/claude-haiku-4-5".to_string(),
                "anthropic/claude-sonnet-4-6".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn create_named_agent_rejects_conflicting_existing_identity() {
        let (_home, host) = test_host();

        let child = AgentIdentityRecord::new(
            "release-bot",
            AgentKind::Child,
            AgentVisibility::Private,
            AgentOwnership::ParentSupervised,
            AgentProfilePreset::PrivateChild,
            Some(host.config().default_agent_id.clone()),
            Some("task-1".into()),
        );
        host.append_agent_identity(&child).unwrap();

        let error = host
            .create_named_agent("release-bot", None)
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

        let child_storage = AppStorage::new(host.agent_data_dir("child_test")).unwrap();
        let mut child_state = AgentState::new("child_test");
        child_state.status = AgentStatus::AwakeRunning;
        child_state.pending = 1;
        child_state.current_run_id = Some("run-1".into());
        child_state.active_task_ids = vec!["task-1".into()];
        child_storage.write_agent(&child_state).unwrap();

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
        let config = host.config().clone();
        let parent_agent_id = config.default_agent_id.clone();
        let parent_storage = AppStorage::new(host.agent_data_dir(&parent_agent_id)).unwrap();
        parent_storage
            .append_task(&TaskRecord {
                id: "task-1".into(),
                agent_id: parent_agent_id.clone(),
                kind: crate::types::TaskKind::SubagentTask,
                status: TaskStatus::Interrupted,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                parent_message_id: None,
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

        let child_storage = AppStorage::new(host.agent_data_dir("child_test")).unwrap();
        let mut child_state = AgentState::new("child_test");
        child_state.status = AgentStatus::AwaitingTask;
        child_state.active_task_ids = vec!["child-task-1".into()];
        child_storage.write_agent(&child_state).unwrap();

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
        let config = host.config().clone();
        let parent_agent_id = config.default_agent_id.clone();
        let parent_storage = AppStorage::new(host.agent_data_dir(&parent_agent_id)).unwrap();
        parent_storage
            .append_task(&TaskRecord {
                id: "task-recover-child".into(),
                agent_id: parent_agent_id.clone(),
                kind: crate::types::TaskKind::ChildAgentTask,
                status: TaskStatus::Running,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                parent_message_id: None,
                summary: Some("delegated child".into()),
                detail: Some(serde_json::json!({
                    "child_agent_id": "child_recover",
                    "child_turn_baseline": 0,
                    "task_status": "running",
                })),
                recovery: Some(TaskRecoverySpec::ChildAgentTask {
                    summary: "delegated child".into(),
                    prompt: "continue delegated child".into(),
                    trust: TrustLevel::TrustedOperator,
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

        let child_storage = AppStorage::new(host.agent_data_dir("child_recover")).unwrap();
        let mut child_state = AgentState::new("child_recover");
        child_state.turn_index = 1;
        child_state.status = AgentStatus::AwakeIdle;
        child_state.last_turn_terminal = Some(crate::types::TurnTerminalRecord {
            turn_index: 1,
            kind: crate::types::TurnTerminalKind::Completed,
            last_assistant_message: Some("child finished after restart".into()),
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
    async fn host_bootstrap_archives_orphaned_private_child_identity() {
        let (_home, host) = test_host();
        let config = host.config().clone();
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

        let child_storage = AppStorage::new(host.agent_data_dir("child_orphan")).unwrap();
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
        let config = host.config().clone();
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
        let config = host.config().clone();

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

        let child_storage = AppStorage::new(host.agent_data_dir("child_parent_missing")).unwrap();
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
        let config = host.config().clone();
        let parent_agent_id = config.default_agent_id.clone();
        let parent_storage = AppStorage::new(host.agent_data_dir(&parent_agent_id)).unwrap();
        parent_storage
            .append_task(&TaskRecord {
                id: "task-stop".into(),
                agent_id: parent_agent_id.clone(),
                kind: crate::types::TaskKind::SubagentTask,
                status: TaskStatus::Interrupted,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                parent_message_id: None,
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

        let child_storage = AppStorage::new(host.agent_data_dir("child_stop")).unwrap();
        child_storage
            .write_agent(&AgentState::new("child_stop"))
            .unwrap();

        drop(host);

        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        let runtime = restarted.default_runtime().await.unwrap();

        let stopped = runtime
            .stop_task("task-stop", &TrustLevel::TrustedOperator)
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
        let storage =
            AppStorage::new(host.agent_data_dir(&host.config().default_agent_id)).unwrap();
        let mut state = AgentState::new(&host.config().default_agent_id);
        state.status = AgentStatus::Paused;
        storage.write_agent(&state).unwrap();

        let _runtime = host.default_runtime().await.unwrap();
        host.shutdown().await.unwrap();

        let persisted = storage.read_agent().unwrap().unwrap();
        assert_eq!(persisted.status, AgentStatus::Paused);
        let events = storage.read_recent_events(16).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "runtime_service_shutdown_requested"));
    }

    #[tokio::test]
    async fn daemon_style_shutdown_does_not_strand_public_agent_on_restart() {
        let (_home, host) = test_host();
        let config = host.config().clone();
        let agent_id = config.default_agent_id.clone();
        let runtime = host.default_runtime().await.unwrap();

        runtime
            .enqueue(MessageEnvelope::new(
                &agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                TrustLevel::TrustedOperator,
                Priority::Normal,
                MessageBody::Text {
                    text: "before shutdown".into(),
                },
            ))
            .await
            .unwrap();
        wait_for_brief_count(&runtime, 2).await;

        host.shutdown().await.unwrap();

        let persisted = AppStorage::new(host.agent_data_dir(&agent_id))
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
                TrustLevel::TrustedOperator,
                Priority::Normal,
                MessageBody::Text {
                    text: "after restart".into(),
                },
            ))
            .await
            .unwrap();
        wait_for_brief_count(&runtime2, 4).await;

        let final_state = runtime2.agent_state().await.unwrap();
        assert_ne!(final_state.status, AgentStatus::Stopped);
    }

    #[tokio::test]
    async fn explicit_agent_stop_remains_durable_across_restart() {
        let (_home, host) = test_host();
        let config = host.config().clone();
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
    async fn resume_respawns_stopped_persistent_agent_runtime_loop() {
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

        let resumed = host
            .control_public_agent(&agent_id, ControlAction::Resume)
            .await
            .unwrap();
        resumed
            .enqueue(MessageEnvelope::new(
                &agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                TrustLevel::TrustedOperator,
                Priority::Normal,
                MessageBody::Text {
                    text: "resume me".into(),
                },
            ))
            .await
            .unwrap();
        wait_for_brief_count(&resumed, 2).await;

        let agents = host.inner.agents.read().await;
        let entry = agents.get(&agent_id).expect("expected live runtime entry");
        assert!(
            !entry.task.is_finished(),
            "resume should restore a live runtime loop"
        );
        drop(agents);

        let briefs = resumed.storage().read_recent_briefs(10).unwrap();
        assert!(briefs.iter().any(|brief| brief.text.contains("resume me")));
        assert!(briefs.iter().any(|brief| brief.text.contains("done")));
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
        assert!(err.to_string().contains("anthropic/claude-sonnet-4-6"));
    }
}
