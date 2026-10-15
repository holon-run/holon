//! Explicit external observation I/O; capture never invokes this coordinator.
mod work_plans;
mod work_queries;
mod work_results;
use super::resource_observation::{
    CapturedResourceObservations, ContextPlanSource, ContextResourceBody, ContextResourceSource,
    ObservedPlan, ResourceInvalidation, ResourceObservationError,
};
use super::*;
use crate::diagnostics::resource_io::ResourceReadBudget;
use crate::runtime::input_evidence_budget::{
    serialized_bytes, InputMemoryCategory, OwnedBytesEstimate,
};
use std::{collections::BTreeMap, sync::Weak};
pub(super) use work_plans::{
    borrowed_resource_record_count, ObservedWorkPlanRead, WorkPlanFilePreparationRequired,
    WorkPlanReadPrior, WorkPlanReplanRequired, WorkResourceReadStamp,
};
pub(crate) use work_queries::WorkItemPlanQueryRead;
pub(in crate::runtime) use work_queries::WorkQueryPlanSource;

pub(crate) use work_results::{
    OriginalWorkReportContinuation, OriginalWorkReportHandoff, OwnedCreatedWorkItemRead,
    OwnedWorkItemMutationRead, WorkItemMutationMaterial,
};

const RECONCILE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
#[derive(Default)]
pub(super) struct ContextResourceObserver {
    closed: AtomicBool,
    cancel: CancellationToken,
    notify: Arc<Notify>,
    refresh: Mutex<()>,
    task: StdMutex<Option<tokio::task::JoinHandle<()>>>,
}
struct ResourceSourceRead {
    source: ContextResourceSource,
    bytes: u64,
    records: usize,
    _material: Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
}
impl RuntimeHandle {
    pub(in crate::runtime) fn resource_material_limits(&self) -> Result<(usize, u64)> {
        let limits = self.inner.input_evidence_budget.limits();
        Ok((
            usize::try_from(limits.record_limit.min(65_536))?,
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("resource byte limit overflow")?
                .min(64 * 1024 * 1024),
        ))
    }
    fn context_resource_source(
        &self,
        state: &AgentState,
        work: Option<&super::resident_work::ResidentWorkDomain>,
    ) -> Result<ResourceSourceRead> {
        let (records, bytes) = self.resource_material_limits()?;
        self.context_resource_source_with_limits(state, work, records, bytes)
    }
    fn context_resource_source_with_limits(
        &self,
        state: &AgentState,
        work: Option<&super::resident_work::ResidentWorkDomain>,
        records: usize,
        bytes: u64,
    ) -> Result<ResourceSourceRead> {
        self.context_resource_source_with_material(state, work, (records, bytes))
    }
    fn context_resource_source_with_material(
        &self,
        state: &AgentState,
        work: Option<&super::resident_work::ResidentWorkDomain>,
        remaining: (usize, u64),
    ) -> Result<ResourceSourceRead> {
        let host_write_generation = self
            .inner
            .host_bridge
            .as_ref()
            .map(RuntimeHostBridge::context_resource_generation)
            .transpose()?;
        let workspace = state.active_workspace_entry.as_ref();
        let config = self.inner.config_snapshot.load_full();
        let source_bytes = serialized_bytes(&(
            &state.id,
            self.inner.storage.data_dir(),
            &config.user_home_dir,
            workspace.map(|entry| {
                (
                    &entry.workspace_id,
                    &entry.workspace_anchor,
                    &entry.execution_root_id,
                    &entry.execution_root,
                )
            }),
        ))?
        .checked_add(1024)
        .and_then(|n| n.checked_mul(8))
        .context("resource source material overflow")?;
        let mut plan_bytes = 0u64;
        let mut plan_records = 0usize;
        if let Some(work) = work {
            anyhow::ensure!(
                work.scope().agent_id == state.id
                    && work.scope().identity_incarnation == self.inner.identity_incarnation,
                "resource Work source scope differs"
            );
            for item in work.context_plan_sources() {
                plan_bytes = plan_bytes
                    .checked_add(
                        serialized_bytes(&(&item.id, item.plan_artifact.is_some()))?
                            .checked_add(128)
                            .and_then(|n| n.checked_mul(8))
                            .context("resource plan source bytes overflow")?,
                    )
                    .context("resource plan source total overflow")?;
                plan_records = plan_records
                    .checked_add(32)
                    .context("resource plan source records overflow")?;
            }
        }
        let source_bytes = source_bytes
            .checked_add(plan_bytes)
            .context("resource source bytes overflow")?;
        let source_records = 128usize
            .checked_add(plan_records)
            .context("resource source records overflow")?;
        let (records, bytes) = self.resource_material_limits()?;
        anyhow::ensure!(
            source_bytes <= bytes.min(remaining.1) && source_records <= records.min(remaining.0),
            ResourceObservationError::ResourceExhausted("source descriptor")
        );
        let material = self.inner.input_evidence_budget.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: source_bytes,
            },
        )?;
        Ok(ResourceSourceRead {
            source: ContextResourceSource {
                scope: super::resident_input_evidence::InputEvidenceScope {
                    agent_id: state.id.clone(),
                    identity_incarnation: self.inner.identity_incarnation,
                },
                agent_home: self.agent_home(),
                user_home: config.user_home_dir.clone(),
                workspace_id: workspace.map(|entry| entry.workspace_id.clone()),
                workspace_anchor: self
                    .workspace_anchor_for_state_ref(state)
                    .map(Path::to_path_buf),
                execution_root_id: workspace.map(|entry| entry.execution_root_id.clone()),
                execution_root: workspace.map(|entry| entry.execution_root.clone()),
                include_user_skills: self.inner.resource_default_skill_visibility,
                host_write_generation,
                config_generation: self
                    .inner
                    .resource_config_generation
                    .load(Ordering::Acquire),
                work_revision: work
                    .filter(|work| work.context_plan_sources().next().is_some())
                    .map(|work| work.coverage().revision),
                plans: work
                    .into_iter()
                    .flat_map(|work| work.context_plan_sources())
                    .map(|item| ContextPlanSource {
                        work_item_id: item.id.clone(),
                        recorded: item.plan_artifact.is_some(),
                    })
                    .collect(),
            },
            bytes: source_bytes,
            records: source_records,
            _material: material,
        })
    }
    fn resource_source_matches(
        &self,
        source: &ContextResourceSource,
        state: &AgentState,
        work: Option<&super::resident_work::ResidentWorkDomain>,
    ) -> bool {
        let workspace = state.active_workspace_entry.as_ref();
        let config = self.inner.config_snapshot.load();
        self.with_context_resource_host_fence(source, || Ok(()))
            .is_ok()
            && source.scope.agent_id == state.id
            && source.scope.identity_incarnation == self.inner.identity_incarnation
            && source.agent_home == self.inner.storage.data_dir()
            && source.user_home == config.user_home_dir
            && source.workspace_id.as_deref() == workspace.map(|entry| entry.workspace_id.as_str())
            && source.workspace_anchor.as_deref() == self.workspace_anchor_for_state_ref(state)
            && source.execution_root_id.as_deref()
                == workspace.map(|entry| entry.execution_root_id.as_str())
            && source.execution_root.as_deref()
                == workspace.map(|entry| entry.execution_root.as_path())
            && source.include_user_skills == self.inner.resource_default_skill_visibility
            && source.work_revision
                == work
                    .filter(|work| work.context_plan_sources().next().is_some())
                    .map(|work| work.coverage().revision)
            && source
                .plans
                .iter()
                .map(|plan| (plan.work_item_id.as_str(), plan.recorded))
                .eq(work
                    .into_iter()
                    .flat_map(|work| work.context_plan_sources())
                    .map(|item| (item.id.as_str(), item.plan_artifact.is_some())))
            && source.config_generation
                == self
                    .inner
                    .resource_config_generation
                    .load(Ordering::Acquire)
    }
    fn current_resource_source(&self) -> Result<ResourceSourceRead> {
        let reservation = self.inner.mutation_owner.try_reserve()?;
        let root = reservation
            .capture_ready_snapshot()?
            .context("resource owner root is unpublished")?;
        anyhow::ensure!(
            root.state.id == self.instance_agent_id()
                && root.token.identity_incarnation == self.inner.identity_incarnation,
            "resource observation owner differs"
        );
        self.context_resource_source(&root.state, root.work.as_deref())
    }
    fn validate_resource_identity(
        &self,
        source: &ContextResourceSource,
        material: &ResourceReadBudget,
    ) -> Result<()> {
        use rusqlite::OptionalExtension;
        let mut connection = self.inner.runtime_db.connection()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
        let raw: Option<u64> = tx.query_row("SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+512 FROM agent_identities WHERE agent_id=?1", [&source.scope.agent_id], |row|row.get(0)).optional()?;
        if let Some(raw) = raw {
            material.charge_metadata(raw, 16)?;
            let nodes: usize = tx.query_row(
                "SELECT COUNT(*) FROM agent_identities,json_tree(payload_json) WHERE agent_id=?1",
                [&source.scope.agent_id],
                |row| row.get(0),
            )?;
            material.charge_metadata(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("resource identity nodes overflow")?,
                nodes,
            )?;
            // Missing incarnation keeps the existing legacy scope-one contract;
            // explicit null/string/zero never invents a resource namespace.
            let valid: bool = tx.query_row("SELECT agent_id=json_extract(payload_json,'$.agent_id') AND status=json_extract(payload_json,'$.status') AND status IN ('active','deleting') AND CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' AND json_extract(payload_json,'$.incarnation')>0 THEN json_extract(payload_json,'$.incarnation') ELSE 0 END=?2 FROM agent_identities WHERE agent_id=?1", rusqlite::params![&source.scope.agent_id,source.scope.identity_incarnation], |row|row.get(0))?;
            anyhow::ensure!(valid, ResourceObservationError::SourceChanged);
        }
        // Original standalone construction has always permitted an absent
        // identity with its already published scope; refresh creates no row.
        tx.commit()?;
        Ok(())
    }
    pub(super) fn initialize_context_resources(&self) {
        if let Err(error) = self.refresh_context_resources_sync() {
            tracing::warn!(agent_id=%self.instance_agent_id(),%error,"initial context resource observation failed; dependent capture remains fenced");
        }
    }
    /// Explicit refresh. No mutation reservation, RuntimeAgent mutex or SQL
    /// writer transaction is retained across filesystem reads.
    pub(crate) async fn refresh_context_resources(&self) -> Result<()> {
        self.refresh_context_resources_with_callbacks(|| Ok(()), || {})
            .await
    }
    async fn refresh_context_resources_with_callbacks(
        &self,
        after_read: impl FnOnce() -> Result<()> + Send + 'static,
        mut owner_busy: impl FnMut() + Send,
    ) -> Result<()> {
        let _refresh = self.inner.context_resource_observer.refresh.lock().await;
        let mut after_read = Some(after_read);
        // Contention is not an I/O failure. The named coordinator waits only
        // after its filesystem phase and releases the owner before retrying.
        for _ in 0..6 {
            let runtime = self.clone();
            let callback = after_read.take();
            let result = tokio::task::spawn_blocking(move || {
                runtime
                    .refresh_context_resources_sync_inner(|| callback.map_or(Ok(()), |read| read()))
            })
            .await?;
            match result {
                Err(error)
                    if matches!(
                        error.downcast_ref::<super::mutation_owner::MutationOwnerError>(),
                        Some(super::mutation_owner::MutationOwnerError::Busy)
                    ) =>
                {
                    owner_busy();
                    let reservation = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        self.inner.mutation_owner.reserve(),
                    )
                    .await
                    .context("resource observation is still waiting for its original owner")??;
                    drop(reservation);
                }
                result => return result,
            }
        }
        Err(super::mutation_owner::MutationOwnerError::Busy.into())
    }
    #[cfg(test)]
    pub(super) async fn quiesce_context_resource_observer_task_for_test(&self) -> Result<()> {
        // Test the foreground coordinator separately from its background tick.
        // Keep the registry and its actual immutable observation intact.
        self.inner.context_resource_observer.cancel.cancel();
        let task = self
            .inner
            .context_resource_observer
            .task
            .lock()
            .map_err(|_| anyhow!("resource test observer task poisoned"))?
            .take();
        if let Some(task) = task {
            task.await?;
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) async fn refresh_context_resources_with_contention_barrier_for_test(
        &self,
        reached: tokio::sync::oneshot::Sender<()>,
        resume: std::sync::mpsc::Receiver<()>,
        busy: tokio::sync::oneshot::Sender<()>,
    ) -> Result<()> {
        let mut busy = Some(busy);
        self.refresh_context_resources_with_callbacks(
            move || {
                reached
                    .send(())
                    .map_err(|_| anyhow!("resource test caller left"))?;
                resume
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .context("resource test refresh was not resumed")?;
                Ok(())
            },
            move || {
                if let Some(busy) = busy.take() {
                    let _ = busy.send(());
                }
            },
        )
        .await
    }
    pub(super) async fn refresh_context_resources_at_bootstrap(&self) -> Result<()> {
        match self.refresh_context_resources().await {
            Err(error)
                if matches!(
                    error.downcast_ref::<ResourceObservationError>(),
                    Some(ResourceObservationError::NotReady(
                        super::resource_observation::ResourceFreshness::Dirty(
                            ResourceInvalidation::KnownWrite
                        )
                    ))
                ) =>
            {
                tracing::debug!(agent_id=%self.instance_agent_id(), "resource refresh deferred until original physical writer exits");
                Ok(())
            }
            result => result,
        }
    }
    fn context_resource_freshness(&self) -> Result<super::resource_observation::ResourceFreshness> {
        let expected = match self
            .inner
            .host_bridge
            .as_ref()
            .map(RuntimeHostBridge::context_resource_generation)
            .transpose()
        {
            Ok(expected) => expected,
            Err(error) => {
                if let Some(ResourceObservationError::NotReady(freshness)) =
                    error.downcast_ref::<ResourceObservationError>()
                {
                    return Ok(freshness.clone());
                }
                return Err(error);
            }
        };
        self.inner
            .context_resources
            .freshness_for_host_generation(expected)
    }
    fn refresh_context_resources_sync(&self) -> Result<()> {
        self.refresh_context_resources_sync_inner(|| Ok(()))
    }
    fn refresh_context_resources_sync_inner(
        &self,
        after_read: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let _refresh_timing = crate::diagnostics::attribution::RESOURCE_REFRESH.start();
        let source_read = self.current_resource_source()?;
        let source = &source_read.source;
        let mut ticket = self
            .inner
            .context_resources
            .begin_refresh(source.clone(), source_read._material.clone())?;
        let (records, bytes) = self.resource_material_limits()?;
        let material = self.inner.input_evidence_budget.reserve_owned_completion(
            &source.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate::default(),
        )?;
        let io = ResourceReadBudget::enter(
            material,
            bytes
                .checked_sub(source_read.bytes)
                .context("resource remaining bytes underflow")?,
            records
                .checked_sub(source_read.records)
                .context("resource remaining records underflow")?,
        )?;
        let result = self.read_resource_source(source, &io);
        match result {
            Ok((body, plans)) => {
                let install_ready = (|| -> Result<_> {
                    after_read()?;
                    self.inner.context_resources.observe_refresh_change(
                        &mut ticket,
                        &super::resource_observation::observation_digest(&body, &plans)?,
                    )?;
                    let reservation = self.inner.mutation_owner.try_reserve()?;
                    let root = reservation
                        .capture_ready_snapshot()?
                        .context("resource install root is unpublished")?;
                    anyhow::ensure!(
                        self.resource_source_matches(source, &root.state, root.work.as_deref()),
                        ResourceObservationError::ObsoleteRefresh
                    );
                    self.validate_resource_identity(source, &io)?;
                    io.finish()?;
                    let (material_bytes, material_records) =
                        super::resource_observation::observation_material(&body, &plans)?;
                    let retained = io.seal_retained(material_bytes, material_records)?;
                    Ok((reservation, material_bytes, material_records, retained))
                })();
                let (_reservation, material_bytes, material_records, retained) = match install_ready
                {
                    Ok(ready) => ready,
                    Err(error) => {
                        if matches!(
                            error.downcast_ref::<super::mutation_owner::MutationOwnerError>(),
                            Some(super::mutation_owner::MutationOwnerError::Busy)
                        ) {
                            // Keep Ready for an unchanged observation, or the
                            // exact Dirty generation already observed above.
                            return Err(error);
                        }
                        self.inner
                            .context_resources
                            .fail(ticket, error.to_string())?;
                        return Err(error);
                    }
                };
                self.with_context_resource_host_fence(source, || {
                    self.inner.context_resources.install(
                        ticket,
                        source,
                        body,
                        plans,
                        material_bytes,
                        material_records,
                        retained,
                    )
                })?;
                self.inner.notify.notify_waiters();
                Ok(())
            }
            Err(error) => {
                if error
                    .chain()
                    .any(|cause| cause.to_string().contains("ResourceExhausted"))
                {
                    self.inner
                        .context_resources
                        .invalidate(ResourceInvalidation::WatchOverflow)?;
                }
                self.inner
                    .context_resources
                    .fail(ticket, error.to_string())?;
                Err(error)
            }
        }
    }
    fn read_resource_source(
        &self,
        source: &ContextResourceSource,
        io: &ResourceReadBudget,
    ) -> Result<(ContextResourceBody, BTreeMap<String, ObservedPlan>)> {
        let source_bytes = serialized_bytes(&(
            &source.scope,
            &source.agent_home,
            &source.user_home,
            &source.workspace_id,
            &source.workspace_anchor,
            &source.execution_root_id,
            &source.execution_root,
            source.work_revision,
        ))?;
        io.charge_metadata(
            source_bytes
                .checked_add(1024)
                .context("resource source bytes overflow")?,
            16,
        )?;
        self.validate_resource_identity(source, &io)?;
        io.charge_metadata(8192, 128)?;
        let guidance = crate::agents_md::load_agents_md(
            source.user_home.as_deref(),
            &source.agent_home,
            source.workspace_anchor.as_deref(),
        )?;
        let memory = crate::agent_memory::load_agent_memory(&source.agent_home)?;
        let candidate_count = crate::skills::COMPAT_SKILL_ROOT_SUFFIXES.len()
            + crate::skills::SKILL_ROOT_SUFFIXES.len() * 2;
        let root_path_bytes = source
            .user_home
            .as_ref()
            .map(|p| p.as_os_str().len())
            .unwrap_or(0)
            .checked_add(source.agent_home.as_os_str().len())
            .and_then(|n| {
                n.checked_add(
                    source
                        .execution_root
                        .as_ref()
                        .map(|p| p.as_os_str().len())
                        .unwrap_or(0),
                )
            })
            .context("skill root path lengths overflow")?;
        io.charge_metadata(
            u64::try_from(
                root_path_bytes
                    .checked_add(source.scope.agent_id.len())
                    .and_then(|n| n.checked_add(1024))
                    .and_then(|n| n.checked_mul(candidate_count))
                    .context("skill root descriptors overflow")?,
            )?,
            candidate_count
                .checked_mul(32)
                .context("skill root records overflow")?,
        )?;
        for (base, suffixes) in [
            (
                source.user_home.as_deref(),
                crate::skills::COMPAT_SKILL_ROOT_SUFFIXES.as_slice(),
            ),
            (
                Some(source.agent_home.as_path()),
                crate::skills::SKILL_ROOT_SUFFIXES.as_slice(),
            ),
            (
                source.execution_root.as_deref(),
                crate::skills::SKILL_ROOT_SUFFIXES.as_slice(),
            ),
        ] {
            if let Some(base) = base {
                for suffix in suffixes {
                    crate::diagnostics::resource_io::observe_metadata_source(&base.join(suffix))?;
                }
            }
        }
        crate::diagnostics::resource_io::observe_metadata_source(&source.agent_home.join("notes"))?;
        let roots = crate::skills::effective_skill_root_registrations(
            if source.include_user_skills {
                SkillVisibility::DefaultAgent
            } else {
                SkillVisibility::NonDefaultAgent
            },
            source.user_home.as_deref(),
            &source.scope.agent_id,
            &source.agent_home,
            source.execution_root.as_deref(),
        );
        let mut registry = crate::skills::SkillsRegistry::new();
        registry.replace_roots(roots.clone())?;
        let mut skills =
            crate::skills::skills_runtime_view_from_catalog(registry.catalog(), &roots, &[]);
        skills.agent_templates_catalog =
            discover_agent_templates_catalog(source.user_home.as_deref(), &source.agent_home);
        let (notes, notes_refresh_error) =
            crate::notes_catalog::observe_agent_home_notes_catalog(&source.agent_home);
        let mut plans = BTreeMap::new();
        for plan in &source.plans {
            io.charge_metadata(
                u64::try_from(plan.work_item_id.len())?
                    .checked_add(8192)
                    .context("plan metadata overflow")?,
                32,
            )?;
            let observed = crate::work_item_plan::observe_plan_artifact(
                &source.agent_home,
                &source.scope.agent_id,
                &plan.work_item_id,
                plan.recorded,
            );
            plans.insert(plan.work_item_id.clone(), observed);
        }
        // Optional legacy readers may omit an unreadable catalog; a quota
        // rejection is always fatal and cannot be mistaken for absence.
        io.finish()?;
        let body = ContextResourceBody {
            guidance,
            memory,
            skills,
            notes,
            notes_refresh_error,
            source_versions: io.observed_versions()?,
        };
        Ok((body, plans))
    }
    /// A transient execution has its own original source. It never replaces the
    /// active workspace registry and creates no workspace, Task or Runtime.
    pub(super) async fn prepare_subagent_resource_observation(
        &self,
        execution: &EffectiveExecution,
    ) -> Result<(Arc<CapturedResourceObservations>, SkillsRuntimeView)> {
        let reservation = self.inner.mutation_owner.reserve().await?;
        let root = reservation
            .capture_ready_snapshot()?
            .context("subagent resource root is unpublished")?;
        let original = self.context_resource_source(&root.state, root.work.as_deref())?;
        anyhow::ensure!(
            self.resource_source_matches(&original.source, &root.state, root.work.as_deref()),
            ResourceObservationError::SourceChanged
        );
        let (records, bytes) = self.resource_material_limits()?;
        let active_bytes = serialized_bytes(&root.state.active_skills)?
            .checked_add(
                u64::try_from(root.state.active_skills.len())?
                    .checked_mul(256)
                    .context("subagent active metadata overflow")?,
            )
            .and_then(|n| n.checked_mul(8))
            .context("subagent active material overflow")?;
        let active_records = root
            .state
            .active_skills
            .len()
            .checked_mul(24)
            .and_then(|n| n.checked_mul(8))
            .context("subagent active records overflow")?;
        let extra = serialized_bytes(&(
            execution.workspace.workspace_id(),
            execution.workspace.workspace_anchor(),
            execution.workspace.execution_root_id(),
            execution.workspace.execution_root(),
        ))?
        .checked_add(1024)
        .and_then(|n| n.checked_mul(8))
        .context("subagent execution source overflow")?;
        let source_bytes = original
            .bytes
            .checked_add(extra)
            .context("subagent source bytes overflow")?;
        let source_records = original
            .records
            .checked_add(64)
            .context("subagent source records overflow")?;
        let remaining_bytes = bytes
            .checked_sub(source_bytes)
            .and_then(|n| n.checked_sub(active_bytes))
            .context("subagent source allowance exhausted")?;
        let remaining_records = records
            .checked_sub(source_records)
            .and_then(|n| n.checked_sub(active_records))
            .context("subagent source records exhausted")?;
        let source_guard = self.inner.input_evidence_budget.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: source_bytes,
            },
        )?;
        let active_guard = self.inner.input_evidence_budget.register_owned_bytes(
            &original.source.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: active_bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let active = root.state.active_skills.clone();
        drop(root);
        drop(reservation);
        let generation = self.inner.context_resources.generation_stamp()?;
        let mut source = original.source.clone();
        source.workspace_id = execution.workspace.workspace_id().map(str::to_owned);
        source.workspace_anchor = execution
            .workspace
            .workspace_id()
            .map(|_| execution.workspace.workspace_anchor().to_path_buf());
        source.execution_root_id = execution.workspace.execution_root_id().map(str::to_owned);
        source.execution_root = Some(execution.workspace.execution_root().to_path_buf());
        source.work_revision = None;
        source.plans.clear();
        let runtime = self.clone();
        let source_for_io = source.clone();
        let (body, plans, retained, retained_bytes, retained_records) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let completion = runtime
                    .inner
                    .input_evidence_budget
                    .reserve_owned_completion(
                        &source_for_io.scope,
                        InputMemoryCategory::Candidate,
                        OwnedBytesEstimate::default(),
                    )?;
                let io = ResourceReadBudget::enter(completion, remaining_bytes, remaining_records)?;
                let (body, plans) = runtime.read_resource_source(&source_for_io, &io)?;
                let (retained_bytes, retained_records) =
                    super::resource_observation::observation_material(&body, &plans)?;
                let retained = io.seal_retained(retained_bytes, retained_records)?;
                Ok((body, plans, retained, retained_bytes, retained_records))
            })
            .await??;
        // Wait only after the filesystem phase has ended. The active owner
        // rechecks original source/identity/generation before installing locally.
        let reservation = self.inner.mutation_owner.reserve().await?;
        let current = reservation
            .capture_ready_snapshot()?
            .context("subagent resource install root is unpublished")?;
        anyhow::ensure!(
            self.resource_source_matches(&original.source, &current.state, current.work.as_deref())
                && self.inner.context_resources.generation_stamp()? == generation,
            ResourceObservationError::ObsoleteRefresh
        );
        let validation = self.inner.input_evidence_budget.reserve_owned_completion(
            &source.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate::default(),
        )?;
        let validation = ResourceReadBudget::enter(
            validation,
            remaining_bytes
                .checked_sub(retained_bytes)
                .context("subagent retained identity bytes exhausted")?,
            remaining_records
                .checked_sub(retained_records)
                .context("subagent retained identity records exhausted")?,
        )?;
        self.validate_resource_identity(&source, &validation)?;
        validation.finish()?;
        let registry = super::resource_observation::ContextResourceRegistry::for_transient(
            source.clone(),
            source_guard,
            generation,
        );
        let ticket = registry.begin_refresh(source.clone(), original._material.clone())?;
        let mut captured = self.with_context_resource_host_fence(&source, || {
            registry.install(
                ticket,
                &source,
                body,
                plans,
                retained_bytes,
                retained_records,
                retained,
            )?;
            registry.capture(
                &source,
                &self.inner.input_evidence_budget,
                remaining_records,
                remaining_bytes,
                None,
            )
        })?;
        captured.retain_active_material(
            active_guard,
            active_bytes + source_bytes,
            active_records + source_records,
        );
        let skills = captured.skills_with_visibility(&active, false);
        Ok((Arc::new(captured), skills))
    }
    pub(crate) fn capture_current_context_resources(
        &self,
    ) -> Result<Arc<CapturedResourceObservations>> {
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("resource capture root is unpublished")?;
        Ok(Arc::new(self.capture_context_resources_for_snapshot(
            &root.state,
            root.work.as_deref(),
            None,
        )?))
    }
    #[cfg(test)]
    pub(in crate::runtime) async fn original_continuation_resource_failure_for_test(
        &self,
    ) -> serde_json::Value {
        let bounded = |error: &dyn std::fmt::Display| {
            error.to_string().chars().take(1024).collect::<String>()
        };
        let freshness = || {
            self.context_resource_freshness()
                .map(|value| format!("{value:?}"))
                .unwrap_or_else(|error| bounded(&error))
        };
        let before = freshness();
        let observer = {
            match self.inner.context_resource_observer.task.lock() {
                Ok(task) => serde_json::json!({
                    "registered":task.is_some(),
                    "finished":task.as_ref().is_some_and(|task|task.is_finished()),
                    "closed":self.inner.context_resource_observer.closed.load(Ordering::Acquire),
                    "cancelled":self.inner.context_resource_observer.cancel.is_cancelled(),
                }),
                Err(error) => serde_json::json!({"lock_error":bounded(&error)}),
            }
        };
        let source = match self.current_resource_source() {
            Ok(read) => serde_json::json!({
                "incarnation":read.source.scope.identity_incarnation,
                "config_generation":read.source.config_generation,
                "host_generation":read.source.host_write_generation,
                "work_revision":read.source.work_revision,
                "plan_count":read.source.plans.len(),
                "plans":read.source.plans.iter().take(8).map(|plan|serde_json::json!({
                    "id":plan.work_item_id.chars().take(128).collect::<String>(),
                    "recorded":plan.recorded,
                })).collect::<Vec<_>>(),
            }),
            Err(error) => serde_json::json!({"error":bounded(&error)}),
        };
        // This is failure-only diagnosis after the original result is already
        // Err. Never retry the business call or change its original 5s timeout.
        let refresh = match tokio::time::timeout(
            std::time::Duration::from_secs(1),
            self.refresh_context_resources(),
        )
        .await
        {
            Ok(Ok(())) => "actual foreground refresh succeeded".to_string(),
            Ok(Err(error)) => bounded(&error),
            Err(_) => "actual foreground refresh diagnostic deadline (1s)".to_string(),
        };
        serde_json::json!({"before":before,"observer":observer,"source":source,
            "foreground_refresh":refresh,"after":freshness()})
    }

    #[cfg(test)]
    pub(super) async fn pause_resource_observer_for_test(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.context_resource_observer.refresh.lock().await
    }
    #[cfg(test)]
    pub(super) async fn refresh_resources_with_checkpoint_for_test(
        &self,
        reached: tokio::sync::oneshot::Sender<()>,
        resume: std::sync::mpsc::Receiver<()>,
    ) -> Result<()> {
        let _refresh = self.inner.context_resource_observer.refresh.lock().await;
        let runtime = self.clone();
        tokio::task::spawn_blocking(move || {
            runtime.refresh_context_resources_sync_inner(|| {
                reached
                    .send(())
                    .map_err(|_| anyhow!("resource test observer caller left"))?;
                resume
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .context("resource test refresh was not resumed")?;
                Ok(())
            })
        })
        .await?
    }
    /// Capture from one caller-held publication: State and resident Work must
    /// come from the same root/token. Neither is retained. Core-only summary
    /// omits selected Work; Context passes its same-publication selected Work.
    /// No latest-root recapture, SQL, file access or preparation occurs here.
    pub(crate) fn capture_context_resources_for_snapshot(
        &self,
        state: &AgentState,
        work: Option<&super::resident_work::ResidentWorkDomain>,
        selected_work: Option<&super::resident_work::CapturedWork>,
    ) -> Result<CapturedResourceObservations> {
        let (records, bytes) = self.resource_material_limits()?;
        self.capture_context_resources_for_snapshot_with_limits(
            state,
            work,
            selected_work,
            records,
            bytes,
        )
    }
    /// The descriptor itself uses the same request's remaining allowance.
    pub(crate) fn capture_context_resources_for_snapshot_with_limits(
        &self,
        state: &AgentState,
        work: Option<&super::resident_work::ResidentWorkDomain>,
        selected_work: Option<&super::resident_work::CapturedWork>,
        record_limit: usize,
        byte_limit: u64,
    ) -> Result<CapturedResourceObservations> {
        let (records, bytes) = self.resource_material_limits()?;
        let records = records.min(record_limit);
        let bytes = bytes.min(byte_limit);
        anyhow::ensure!(
            state.id == self.instance_agent_id(),
            ResourceObservationError::SourceChanged
        );
        let source = self.context_resource_source_with_limits(state, work, records, bytes)?;
        let active_bytes = serialized_bytes(&state.active_skills)?
            .checked_add(
                u64::try_from(state.active_skills.len())?
                    .checked_mul(256)
                    .context("active skill metadata overflow")?,
            )
            .and_then(|n| n.checked_mul(8))
            .context("active skill copies overflow")?;
        let active_records = state
            .active_skills
            .len()
            .checked_mul(24)
            .and_then(|n| n.checked_mul(8))
            .context("active skill record overflow")?;
        let remaining_bytes = bytes
            .checked_sub(source.bytes)
            .and_then(|n| n.checked_sub(active_bytes))
            .context("resource capture active skills bytes exhausted")?;
        let remaining_records = records
            .checked_sub(source.records)
            .and_then(|n| n.checked_sub(active_records))
            .context("resource capture active skill records exhausted")?;
        let active_guard = self.inner.input_evidence_budget.register_owned_bytes(
            &source.source.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: active_bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let captured = self
            .with_context_resource_host_fence(&source.source, || {
                self.inner.context_resources.capture(
                    &source.source,
                    &self.inner.input_evidence_budget,
                    remaining_records,
                    remaining_bytes,
                    selected_work,
                )
            })
            .map(|mut captured| {
                captured.retain_active_material(
                    active_guard,
                    active_bytes + source.bytes,
                    active_records + source.records,
                );
                captured
            });
        if captured.as_ref().err().is_some_and(|error| {
            matches!(
                error.downcast_ref::<ResourceObservationError>(),
                Some(ResourceObservationError::SourceChanged)
            )
        }) {
            self.invalidate_context_resources(ResourceInvalidation::WorkspaceChange)?;
        }
        captured
    }

    /// Only the original Work/file workflow can create a plan. This synchronous
    /// source phase ends before the caller acquires its business SQL owner.
    pub(super) fn create_work_plan_file(
        &self,
        item: &crate::types::WorkItemRecord,
        initial: Option<&str>,
    ) -> Result<(
        crate::types::WorkItemPlanArtifact,
        Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
    )> {
        let (artifact, material, _cost) =
            self.create_work_plan_file_with_material(item, initial)?;
        Ok((artifact, material))
    }
    pub(super) fn create_work_plan_file_with_material(
        &self,
        item: &crate::types::WorkItemRecord,
        initial: Option<&str>,
    ) -> Result<(
        crate::types::WorkItemPlanArtifact,
        Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
        (u64, usize),
    )> {
        let _timing = crate::diagnostics::attribution::RESOURCE_REFRESH.start();
        let _resource_write = self.begin_context_resource_write()?;
        let (records, bytes) = self.resource_material_limits()?;
        let scope = super::resident_input_evidence::InputEvidenceScope {
            agent_id: self.instance_agent_id().to_owned(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        anyhow::ensure!(
            item.agent_id == scope.agent_id,
            "plan file workflow belongs to another owner"
        );
        let completion = self.inner.input_evidence_budget.reserve_owned_completion(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate::default(),
        )?;
        let io = ResourceReadBudget::enter(completion, bytes, records)?;
        io.charge_metadata(
            u64::try_from(initial.map(str::len).unwrap_or(0))?
                .checked_add(serialized_bytes(&(
                    &item.id,
                    &item.agent_id,
                    self.inner.storage.data_dir(),
                ))?)
                .and_then(|n| n.checked_add(8192))
                .and_then(|n| n.checked_mul(8))
                .context("initial plan material overflow")?,
            32,
        )?;
        let artifact = crate::work_item_plan::ensure_plan_artifact(
            self.agent_home().as_path(),
            item,
            initial,
        )?;
        let cost = io.finish()?;
        let guard = io.completion_guard()?;
        Ok((artifact, guard, cost))
    }
    pub(super) async fn wait_for_context_resources(&self) -> Result<()> {
        let wait = async {
            loop {
                let notified = self.inner.notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                match self.context_resource_freshness()? {
                    super::resource_observation::ResourceFreshness::Ready => return Ok(()),
                    super::resource_observation::ResourceFreshness::Dirty(_)
                    | super::resource_observation::ResourceFreshness::Unobserved => {}
                    freshness => return Err(ResourceObservationError::NotReady(freshness).into()),
                }
                tokio::select! {_=self.inner.context_resource_observer.cancel.cancelled()=>return Err(ResourceObservationError::NotReady(super::resource_observation::ResourceFreshness::Closed).into()),_=notified=>{}}
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), wait)
            .await
            .context("context resource observation is still pending")?
    }
    pub(super) fn resources_changed_after_commit(
        &self,
        before: &AgentState,
        before_work: Option<&super::resident_work::ResidentWorkDomain>,
        after: &AgentState,
        after_work: Option<&super::resident_work::ResidentWorkDomain>,
    ) {
        let workspace_changed = before.active_workspace_entry != after.active_workspace_entry;
        let revision = |work: Option<&super::resident_work::ResidentWorkDomain>| {
            work.filter(|work| work.context_plan_sources().next().is_some())
                .map(|work| work.coverage().revision)
        };
        if !workspace_changed && revision(before_work) != revision(after_work) {
            // Only actual same-owner ACK publication reaches this hook. A
            // metadata-only Work revision cannot change an observed file body.
            let rebound = (|| -> Result<bool> {
                let source = self.context_resource_source(after, after_work)?;
                self.with_context_resource_host_fence(&source.source, || {
                    self.inner.context_resources.rebind_work_source(
                        revision(before_work),
                        &source.source,
                        source._material.clone(),
                        &self.inner.input_evidence_budget,
                    )
                })
            })();
            if matches!(rebound, Ok(true)) {
                return;
            }
        }
        if workspace_changed || revision(before_work) != revision(after_work) {
            let cause = if workspace_changed {
                ResourceInvalidation::WorkspaceChange
            } else {
                ResourceInvalidation::KnownWrite
            };
            if let Err(error) = self.invalidate_context_resources(cause) {
                tracing::warn!(%error,"resource invalidation failed; capture remains fenced");
            }
        }
    }
    pub(super) async fn refresh_dirty_context_resources_after_write(&self) {
        if matches!(
            self.inner.context_resources.freshness(),
            Ok(super::resource_observation::ResourceFreshness::Dirty(_))
        ) {
            // Some existing writers publish effects while their owner is still
            // held. Never wait on the observer mutex from that owner scope.
            let Ok(reservation) = self.inner.mutation_owner.try_reserve() else {
                return;
            };
            drop(reservation);
            if let Err(error) = self.refresh_context_resources().await {
                tracing::debug!(%error,"resource write refresh pending; dependent capture remains fenced");
            }
        }
    }
    /// Before an ApplyPatch input is cloned/retained by its original owned
    /// physical worker. This guard follows that same copy through handoff.
    pub(crate) fn retain_resource_writer_material(
        &self,
        input: &serde_json::Value,
        execution: &EffectiveExecution,
    ) -> Result<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>> {
        let nodes = super::child_task_terminal::completion_value_nodes(Some(input))?
            .checked_add(64)
            .and_then(|n| {
                execution
                    .attached_workspaces
                    .len()
                    .checked_mul(4)
                    .and_then(|v| n.checked_add(v))
            })
            .context("resource writer native nodes overflow")?;
        let records = nodes
            .checked_mul(8)
            .context("resource writer records overflow")?;
        let bytes = serialized_bytes(&(
            self.instance_agent_id(),
            input,
            &execution.profile,
            &execution.attached_workspaces,
            execution.workspace.workspace_id(),
            execution.workspace.workspace_anchor(),
            execution.workspace.execution_root(),
            execution.workspace.execution_root_id(),
        ))?
        .checked_add(
            u64::try_from(nodes)?
                .checked_mul(256)
                .context("resource writer metadata overflow")?,
        )
        .and_then(|n| n.checked_mul(8))
        .context("resource writer material overflow")?;
        let (record_limit, byte_limit) = self.resource_material_limits()?;
        anyhow::ensure!(
            records <= record_limit && bytes <= byte_limit,
            ResourceObservationError::ResourceExhausted("original writer input")
        );
        let scope = super::resident_input_evidence::InputEvidenceScope {
            agent_id: self.instance_agent_id().to_owned(),
            identity_incarnation: self.instance_incarnation(),
        };
        Ok(self.inner.input_evidence_budget.register_owned_bytes(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?)
    }
    pub(super) fn original_command_resource_changes(
        &self,
    ) -> Result<super::resource_observation::ResourceProcessChanges> {
        super::resource_observation::ResourceProcessChanges::start(
            &self.inner.context_resources,
            self.inner.context_resource_observer.notify.clone(),
            self.inner
                .host_bridge
                .as_ref()
                .map(RuntimeHostBridge::original_command_resource_changes)
                .transpose()?,
        )
    }
    pub(crate) fn begin_context_resource_write(
        &self,
    ) -> Result<Option<super::resource_observation::ResourceWriteLease>> {
        let host_write = self
            .inner
            .host_bridge
            .as_ref()
            .map(RuntimeHostBridge::begin_original_context_resource_write)
            .transpose()?;
        match (self.begin_local_context_resource_write()?, host_write) {
            (Some(mut local), host_write) => {
                local.host_write = host_write;
                Ok(Some(local))
            }
            (None, Some(host_write)) => Ok(Some(self.inner.context_resources.retain_host_kernel(
                self.inner.context_resource_observer.notify.clone(),
                host_write,
            ))),
            (None, None) => Ok(None),
        }
    }
    pub(crate) fn begin_local_context_resource_write(
        &self,
    ) -> Result<Option<super::resource_observation::ResourceWriteLease>> {
        self.inner
            .context_resources
            .begin_write(self.inner.context_resource_observer.notify.clone())
    }
    fn with_context_resource_host_fence<T>(
        &self,
        source: &ContextResourceSource,
        read: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        match (&self.inner.host_bridge, source.host_write_generation) {
            (Some(bridge), Some(generation)) => {
                bridge.with_context_resource_generation(generation, read)
            }
            (None, None) => read(),
            _ => Err(ResourceObservationError::SourceChanged.into()),
        }
    }
    pub(crate) fn invalidate_context_resources(&self, cause: ResourceInvalidation) -> Result<()> {
        self.inner.context_resources.invalidate(cause)?;
        self.inner.context_resource_observer.notify.notify_one();
        Ok(())
    }
    /// A bounded independent observer, not a dispatch-time freshness poll.
    pub(super) fn start_context_resource_observer(&self) -> Result<()> {
        let observer = &self.inner.context_resource_observer;
        let mut task = observer
            .task
            .lock()
            .map_err(|_| anyhow!("resource observer lock poisoned"))?;
        if observer.closed.load(Ordering::Acquire) || task.is_some() {
            return Ok(());
        }
        self.inner
            .context_resources
            .invalidate(ResourceInvalidation::WatchRestart)?;
        observer.notify.notify_one();
        let weak: Weak<RuntimeInner> = Arc::downgrade(&self.inner);
        let cancel = observer.cancel.clone();
        let notify = observer.notify.clone();
        let host_writes = self
            .inner
            .host_bridge
            .as_ref()
            .map(RuntimeHostBridge::context_resource_write_notify)
            .transpose()?;
        *task = Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval_at(
                tokio::time::Instant::now() + RECONCILE_INTERVAL,
                RECONCILE_INTERVAL,
            );
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = interval.tick() => {},
                    _ = notify.notified() => {},
                    _ = async {
                        if let Some(host_writes) = &host_writes {
                            host_writes.notified().await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    } => {},
                }
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                if inner
                    .context_resource_observer
                    .closed
                    .load(Ordering::Acquire)
                {
                    return;
                }
                let runtime = RuntimeHandle { inner };
                if let Err(error) = runtime.refresh_context_resources().await {
                    if error.downcast_ref::<ResourceObservationError>().is_none() {
                        tracing::debug!(agent_id=%runtime.instance_agent_id(),%error,"context resource observer refresh failed");
                    }
                }
            }
        }));
        Ok(())
    }
    pub(super) fn close_context_resource_observer(&self) {
        self.inner
            .context_resource_observer
            .closed
            .store(true, Ordering::Release);
        self.inner.context_resource_observer.cancel.cancel();
        if let Err(error) = self.inner.context_resources.close() {
            tracing::warn!(%error,"context resource close failed");
        }
    }
    pub(crate) async fn stop_context_resource_observer(&self) -> Result<()> {
        self.close_context_resource_observer();
        let task = self
            .inner
            .context_resource_observer
            .task
            .lock()
            .map_err(|_| anyhow!("resource observer lock poisoned"))?
            .take();
        if let Some(task) = task {
            task.await?;
        }
        Ok(())
    }
}
