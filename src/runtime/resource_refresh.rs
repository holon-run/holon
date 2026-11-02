//! Event-only resource acquisition. Capture never calls a loader.
use super::resource_observation::{
    ContextPlanSource, ContextResourceBody, ContextResourceRegistry, ContextResourceSource,
    ResourceInvalidation, ResourceObservation, ResourceObservationError,
};
use super::*;
use crate::{
    diagnostics::resource_io::{self, ResourceReadMaterial},
    host::context_resources::ContextResources,
    runtime::resident_input_evidence::InputEvidenceScope,
    skills::{SkillVisibility, SkillsRegistry},
};
use sha2::{Digest, Sha256};
use std::sync::Weak;

pub(super) struct RuntimeResources {
    pub registry: ContextResourceRegistry,
    pub gate: Arc<ContextResources>,
    notify: Arc<Notify>,
    initial_done: AtomicBool,
    initial: Notify,
    closed: AtomicBool,
    scope: InputEvidenceScope,
    standalone_skills: Arc<RwLock<SkillsRegistry>>,
    refresh_lock: Arc<Mutex<()>>,
    observer: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    observer_drain: Mutex<Option<tokio::task::JoinHandle<()>>>,
    closing: Notify,
    retry_owner: AtomicBool,
    last_failure: StdMutex<Option<RefreshFailure>>,
    #[cfg(test)]
    checkpoint: StdMutex<Option<Arc<RefreshIoBarrier>>>,
    #[cfg(test)]
    attempts: AtomicUsize,
}
#[cfg(test)]
#[derive(Default)]
struct RefreshIoBarrier {
    reached: Notify,
    released: StdMutex<bool>,
    release: std::sync::Condvar,
    inventory_base: StdMutex<Option<SkillsRegistry>>,
}
#[cfg(test)]
pub(crate) struct PausedRefreshIo(Arc<RefreshIoBarrier>);
#[cfg(test)]
impl PausedRefreshIo {
    pub(crate) async fn wait(&self) {
        self.0.reached.notified().await;
    }
    pub(crate) fn release(&self) {
        *self.0.released.lock().unwrap() = true;
        self.0.release.notify_all();
    }
}
#[cfg(test)]
impl Drop for PausedRefreshIo {
    fn drop(&mut self) {
        self.release();
    }
}
#[derive(Clone, Copy)]
enum RefreshFailure {
    Exhausted,
    Io(std::io::ErrorKind),
    /// The published source moved while the refresh was reading it. The typed
    /// error survives, so a later capture can re-prepare instead of surfacing
    /// an opaque refresh failure.
    SourceChanged,
    Other,
}
/// Bounded re-preparations for a source that moves between the preparation
/// and the capture. The bound matches the writer-conflict re-entry that the
/// Context preparation path already allows.
pub(super) const RESOURCE_SOURCE_CHANGE_REPREPARES: usize = 2;
/// The typed source-change failure survives every context chain, so the
/// preparation paths can classify it and re-prepare.
pub(super) fn is_source_changed_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<ResourceObservationError>(),
            Some(ResourceObservationError::SourceChanged)
        )
    })
}
/// The typed resource failures that a preparation may re-prepare for. Every
/// other failure stays fail-closed.
fn is_repreparable_resource_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<ResourceObservationError>(),
            Some(ResourceObservationError::SourceChanged | ResourceObservationError::NotReady(_))
        )
    })
}
impl RefreshFailure {
    fn from_error(error: &anyhow::Error) -> Self {
        if crate::runtime_error::describe_runtime_error(error).code == "context_resource_exhausted"
        {
            Self::Exhausted
        } else if is_source_changed_error(error) {
            Self::SourceChanged
        } else if let Some(kind) = error.chain().find_map(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind)
        }) {
            Self::Io(kind)
        } else {
            Self::Other
        }
    }
    fn error(self) -> anyhow::Error {
        match self {
            Self::Exhausted => crate::runtime_error::RuntimeError::new(
                crate::runtime_error::RuntimeErrorDomain::Io,
                "context_resource_exhausted",
                "ResourceExhausted: context resource refresh material",
            )
            .with_retryable(true)
            .into(),
            Self::Io(kind) => {
                std::io::Error::new(kind, "context resource refresh reader failed").into()
            }
            Self::SourceChanged => ResourceObservationError::SourceChanged.into(),
            Self::Other => anyhow!("context resource refresh failed"),
        }
    }
}
impl RuntimeResources {
    pub(super) fn new(
        ledger: &input_evidence_budget::InputEvidenceBudgetLedger,
        scope: InputEvidenceScope,
        bridge: Option<&RuntimeHostBridge>,
    ) -> Result<Self> {
        let gate = match bridge {
            Some(bridge) => bridge.context_resources()?,
            None => Arc::new(ContextResources::default()),
        };
        let notify = Arc::new(Notify::new());
        gate.subscribe(&notify)?;
        Ok(Self {
            registry: ContextResourceRegistry::new(ledger.clone()),
            gate,
            notify,
            scope,
            initial_done: AtomicBool::new(false),
            initial: Notify::new(),
            closed: AtomicBool::new(false),
            standalone_skills: Arc::new(RwLock::new(SkillsRegistry::new())),
            refresh_lock: Arc::new(Mutex::new(())),
            observer: StdMutex::new(None),
            observer_drain: Mutex::new(None),
            closing: Notify::new(),
            retry_owner: AtomicBool::new(false),
            last_failure: StdMutex::new(None),
            #[cfg(test)]
            checkpoint: StdMutex::new(None),
            #[cfg(test)]
            attempts: AtomicUsize::new(0),
        })
    }
    pub(super) fn notify(&self) {
        self.notify.notify_one();
    }
    pub(super) fn close(&self) {
        let _observer = self.observer.lock().expect("resource observer poisoned");
        self.closed.store(true, Ordering::Release);
        let _ = self.registry.close();
        self.finish_initial();
        self.closing.notify_waiters();
        self.notify();
    }
    fn finish_initial(&self) {
        self.initial_done.store(true, Ordering::Release);
        self.initial.notify_waiters();
    }
    async fn closed(&self) {
        loop {
            let notified = self.closing.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.closed.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}
/// File ownership physically exits before this carrier is handed off.
/// Source terminal releases only the writer; immutable material follows clones.
pub(crate) struct WorkPlanPreparation {
    scope: InputEvidenceScope,
    writer: StdMutex<Option<crate::host::resource_writes::ResourceWrite>>,
    _material: Arc<input_evidence_budget::BudgetViewGuard>,
    notify: Arc<Notify>,
}
impl std::fmt::Debug for WorkPlanPreparation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkPlanPreparation")
            .finish_non_exhaustive()
    }
}
impl WorkPlanPreparation {
    #[cfg(test)]
    pub(super) fn material_for_test(&self) -> Arc<input_evidence_budget::BudgetViewGuard> {
        self._material.clone()
    }

    pub(super) fn validate_scope(&self, scope: &InputEvidenceScope) -> Result<()> {
        anyhow::ensure!(&self.scope == scope, "plan preparation scope differs");
        Ok(())
    }

    pub(super) fn source_terminal(&self) {
        let writer = self
            .writer
            .lock()
            .expect("plan preparation writer poisoned")
            .take();
        if writer.is_some() {
            drop(writer);
            self.notify.notify_one();
        }
    }
}

pub(super) struct ResourceOwnerWake(pub Weak<RuntimeInner>);
impl Drop for ResourceOwnerWake {
    fn drop(&mut self) {
        if let Some(inner) = self.0.upgrade() {
            if inner.resources.retry_owner.load(Ordering::Acquire) {
                inner.resources.notify();
            }
        }
    }
}
impl RuntimeHandle {
    pub(super) async fn prepare_work_plan_file(
        &self,
        record: &mut WorkItemRecord,
        initial: Option<&str>,
    ) -> Result<(bool, Arc<WorkPlanPreparation>)> {
        let writer = self.begin_context_resource_write()?;
        let material = self.context_resource_material()?;
        let (owned_record, initial, home) = {
            let _io = material.enter();
            let admission = resource_io::charge_owned_metadata(
                input_evidence_budget::serialized_bytes(record)?
                    .saturating_add(initial.map_or(0, |text| text.len()) as u64)
                    .saturating_add(self.inner.storage.data_dir().as_os_str().len() as u64 + 4096),
                32,
            );
            material.finish()?;
            admission?;
            (
                record.clone(),
                initial.map(str::to_owned),
                self.agent_home(),
            )
        };
        let notify = self.inner.resources.notify.clone();
        let scope = self.inner.resources.scope.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let result = tokio::task::spawn_blocking(move || {
                let _io = material.enter();
                writer.enter_physical();
                let mut owned_record = owned_record;
                let artifact = if owned_record.plan_artifact.is_none() {
                    crate::work_item_plan::ensure_plan_artifact(
                        &home,
                        &owned_record,
                        initial.as_deref(),
                    )
                } else {
                    crate::work_item_plan::refresh_plan_artifact_metadata(&home, &mut owned_record)
                        .and_then(|_| {
                            owned_record
                                .plan_artifact
                                .take()
                                .context("prepared plan metadata missing")
                        })
                };
                // Finish the original phase even when its file kernel rejects.
                let material_guard = match material.guard() {
                    Ok(guard) => guard,
                    Err(error) => {
                        return Err(match artifact {
                            Err(file_error) => error.context(file_error),
                            Ok(_) => error,
                        });
                    }
                };
                let artifact = artifact?;
                let preparation = Arc::new(WorkPlanPreparation {
                    scope,
                    writer: StdMutex::new(Some(writer)),
                    _material: material_guard,
                    notify,
                });
                Ok::<_, anyhow::Error>((artifact, preparation))
            })
            .await
            .context("plan file physical worker failed")
            .and_then(|result| result);
            let _ = send.send(result);
        })?;
        let (artifact, preparation) = receive.await.context("plan file owner disappeared")??;
        let changed = record.plan_artifact.as_ref() != Some(&artifact);
        record.plan_artifact = Some(artifact);
        Ok((changed, preparation))
    }

    #[cfg(test)]
    pub(crate) fn pause_next_resource_refresh(&self) -> PausedRefreshIo {
        let checkpoint = Arc::new(RefreshIoBarrier::default());
        *self.inner.resources.checkpoint.lock().unwrap() = Some(checkpoint.clone());
        PausedRefreshIo(checkpoint)
    }
    pub(super) fn start_resource_worker(&self) -> Result<()> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Ok(());
        }
        let mut observer = self
            .inner
            .resources
            .observer
            .lock()
            .expect("resource observer poisoned");
        if observer.is_some() || self.inner.resources.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let weak = Arc::downgrade(&self.inner);
        let notify = self.inner.resources.notify.clone();
        *observer = Some(tokio::spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                if inner.resources.closed.load(Ordering::Acquire) {
                    return;
                }
                let runtime = RuntimeHandle { inner };
                let result = runtime.ensure_context_resources().await;
                let owner_busy = matches!(
                    result.as_ref().err().and_then(
                        |error| error.downcast_ref::<mutation_owner::MutationOwnerError>()
                    ),
                    Some(mutation_owner::MutationOwnerError::Busy)
                );
                if let Err(error) = &result {
                    tracing::debug!(agent_id = %runtime.instance_agent_id(), error = %error,
                        "context resource refresh deferred or failed; awaiting a new event");
                }
                // The registry/captures, not an idle worker, own the observation.
                drop(result);
                // Busy has not completed an initial observation. Its existing
                // owner-release wake must run before initial capture proceeds.
                if !owner_busy {
                    runtime.inner.resources.finish_initial();
                }
                let closed = runtime.inner.resources.closed.load(Ordering::Acquire);
                drop(runtime);
                if closed {
                    return;
                }
                notify.notified().await;
            }
        }));
        Ok(())
    }
    pub(super) fn quiesce_resource_refresh(&self) -> Result<tokio::sync::MutexGuard<'_, ()>> {
        self.inner
            .resources
            .refresh_lock
            .try_lock()
            .map_err(|_| anyhow!("agent_cleanup_blocked: resource_refresh_exit_pending"))
    }
    #[cfg(test)]
    pub(crate) async fn hold_resource_refresh_for_test(
        &self,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        self.lock_context_resource_refresh().await
    }
    pub(crate) fn resource_refresh_pending(&self) -> bool {
        let resources = &self.inner.resources;
        (!resources.initial_done.load(Ordering::Acquire)
            && resources
                .observer
                .lock()
                .expect("resource observer poisoned")
                .is_some())
            || resources.refresh_lock.try_lock().is_err()
    }
    pub(super) async fn join_resource_worker(&self) -> Result<()> {
        self.inner.resources.close();
        let mut drain = self.inner.resources.observer_drain.lock().await;
        if drain.is_none() {
            *drain = self
                .inner
                .resources
                .observer
                .lock()
                .expect("resource observer poisoned")
                .take();
        }
        if let Some(observer) = drain.as_mut() {
            let result = observer.await;
            *drain = None;
            result?;
        }
        Ok(())
    }
    pub(super) async fn await_initial_resources(&self) {
        self.start_resource_worker()
            .expect("resource observer startup");
        loop {
            let notified = self.inner.resources.initial.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.inner.resources.initial_done.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
    #[cfg(test)]
    pub(crate) async fn await_initial_resources_for_test(&self) {
        self.await_initial_resources().await;
    }
    /// Explicit refresh/event worker only; no mutation owner survives the I/O.
    #[cfg(test)]
    pub(crate) async fn refresh_context_resources(&self) -> Result<Arc<ResourceObservation>> {
        let refresh = self.lock_context_resource_refresh().await?;
        // Bind this explicit event to its own result, not a racing observer.
        self.inner
            .resources
            .registry
            .invalidate(ResourceInvalidation::ExplicitRefresh)?;
        self.run_context_resource_refresh(refresh).await
    }
    pub(super) async fn prepare_summary_instruction_resources(&self) -> Result<()> {
        let captured = {
            let _owner = self.reserve_resource_source().await?;
            let root = self
                .inner
                .mutation_owner
                .capture()?
                .context("summary resource source has no published root")?;
            let config = self.inner.config_snapshot.load_full();
            let material = self.context_resource_material()?;
            let captured = self.capture_instruction_resources(&root.state, &config, &material);
            material.finish()?;
            captured
        };
        match captured {
            Ok(_) => Ok(()),
            Err(error) if is_repreparable_resource_error(&error) => {
                // Explicit preparation, before the summary reserves its owner.
                drop(self.ensure_context_resources().await?);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub(super) async fn ensure_context_resources(&self) -> Result<Arc<ResourceObservation>> {
        let mut source_changes = 0usize;
        loop {
            {
                let _owner = self.reserve_resource_source().await?;
                let root = self
                    .inner
                    .mutation_owner
                    .capture()?
                    .context("resource source has no published root")?;
                let config = self.inner.config_snapshot.load_full();
                let material = self.context_resource_material()?;
                if let Ok(observation) =
                    self.capture_context_resources(&root.state, &config, &material)
                {
                    material.finish()?;
                    return Ok(observation);
                }
            }
            let refresh = self.lock_context_resource_refresh().await?;
            let result = self.run_context_resource_refresh(refresh).await;
            if result.as_ref().err().is_some_and(|error| {
                error
                    .downcast_ref::<crate::host::context_resources::ContextResourceWritersActive>()
                    .is_some()
            }) {
                // The physical attempt has exited. A plan writer may need SQL,
                // so neither its refresh lock nor an agent owner can be held.
                tokio::select! {
                    result = self.inner.resources.gate.wait_for_writers() => result?,
                    _ = self.inner.resources.closed() => {
                        return Err(ResourceObservationError::NotReady(
                            super::resource_observation::ResourceFreshness::Closed
                        ).into());
                    }
                }
                continue;
            }
            if source_changes < RESOURCE_SOURCE_CHANGE_REPREPARES
                && result.as_ref().err().is_some_and(is_source_changed_error)
            {
                // The published source moved while the refresh was reading it.
                // Re-prepare with the current source on this bounded path
                // instead of returning the failed attempt or a stale view.
                // The refresh keeps its own owner discipline: the owner is
                // taken after the physical read and released with it.
                source_changes += 1;
                continue;
            }
            if !matches!(
                result
                    .as_ref()
                    .err()
                    .and_then(|error| error.downcast_ref::<mutation_owner::MutationOwnerError>()),
                Some(mutation_owner::MutationOwnerError::Busy)
            ) {
                return result;
            }
            // The physical attempt has exited. Wait without its refresh lock,
            // and release the owner before acquiring resources on the next attempt.
            drop(self.reserve_resource_source().await?);
        }
    }
    async fn reserve_resource_source(&self) -> Result<mutation_owner::MutationReservation> {
        tokio::select! {
            owner = self.inner.mutation_owner.reserve() => Ok(owner?),
            _ = self.inner.resources.closed() => {
                Err(ResourceObservationError::NotReady(
                    super::resource_observation::ResourceFreshness::Closed
                ).into())
            }
        }
    }

    async fn lock_context_resource_refresh(&self) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        let refresh = tokio::select! {
            guard = self.inner.resources.refresh_lock.clone().lock_owned() => guard,
            _ = self.inner.resources.closed() => {
                return Err(ResourceObservationError::NotReady(
                    super::resource_observation::ResourceFreshness::Closed
                ).into());
            }
        };
        anyhow::ensure!(
            !self.inner.resources.closed.load(Ordering::Acquire),
            ResourceObservationError::NotReady(
                super::resource_observation::ResourceFreshness::Closed
            )
        );
        Ok(refresh)
    }
    async fn run_context_resource_refresh(
        &self,
        refresh: tokio::sync::OwnedMutexGuard<()>,
    ) -> Result<Arc<ResourceObservation>> {
        let runtime = self.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        // A cancelled observer/caller cannot detach physical I/O or its budget.
        self.spawn_owned_mutation(async move {
            let _refresh = refresh;
            let result = runtime.refresh_context_resources_owned().await;
            let _ = send.send(result);
        })?;
        receive
            .await
            .context("resource refresh supervisor failed")?
    }
    async fn refresh_context_resources_owned(&self) -> Result<Arc<ResourceObservation>> {
        let mut reordered = false;
        let result = loop {
            #[cfg(test)]
            self.inner.resources.attempts.fetch_add(1, Ordering::AcqRel);
            let runtime = self.clone();
            let result =
                tokio::task::spawn_blocking(move || runtime.refresh_context_resources_sync())
                    .await
                    .context("resource refresh physical worker failed")?;
            match result
                .as_ref()
                .err()
                .and_then(|error| error.downcast_ref::<ResourceObservationError>())
            {
                Some(ResourceObservationError::InventoryContended) => {
                    let inventory = match self.inner.host_bridge.as_ref() {
                        Some(bridge) => bridge.skills_registry()?,
                        None => self.inner.resources.standalone_skills.clone(),
                    };
                    // The physical worker has released owner, publication fence
                    // and inventory guards before this actual availability wait.
                    tokio::select! {
                        available = inventory.write() => drop(available),
                        _ = self.inner.resources.closed() => {
                            break Err(ResourceObservationError::NotReady(
                                super::resource_observation::ResourceFreshness::Closed
                            ).into());
                        }
                    }
                }
                Some(ResourceObservationError::InventorySnapshotChanged) if !reordered => {
                    reordered = true;
                }
                _ => break result,
            }
        };
        let mut failure = self
            .inner
            .resources
            .last_failure
            .lock()
            .map_err(|_| anyhow!("resource refresh failure observation poisoned"))?;
        if result.is_ok() {
            *failure = None;
        } else if !matches!(
            result
                .as_ref()
                .err()
                .and_then(|error| error.downcast_ref::<mutation_owner::MutationOwnerError>()),
            Some(mutation_owner::MutationOwnerError::Busy)
        ) {
            *failure = result.as_ref().err().map(RefreshFailure::from_error);
        }
        result
    }
    pub(super) fn context_resource_source(
        &self,
        state: &AgentState,
        config: &ConfigSnapshot,
    ) -> Result<ContextResourceSource> {
        let workspace = state.active_workspace_entry.as_ref();
        let path_bytes = self.inner.storage.data_dir().as_os_str().len()
            + config
                .user_home_dir
                .as_ref()
                .map_or(0, |path| path.as_os_str().len())
            + workspace.map_or(0, |entry| {
                entry.execution_root.as_os_str().len()
                    + entry.workspace_anchor.as_os_str().len()
                    + entry.workspace_id.len()
                    + entry.execution_root_id.len()
            });
        resource_io::charge_owned_metadata(
            (path_bytes * 8 + state.id.len() * 8 + 8192) as u64
                + input_evidence_budget::serialized_bytes(&state.active_skills)?,
            128,
        )?;
        let selector = Sha256::digest(serde_json::to_vec(&state.active_skills)?);
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("plan source has no root")?;
        let work = root
            .work
            .as_ref()
            .context("plan source has no prepared Work")?;
        anyhow::ensure!(
            work.scope() == &self.inner.resources.scope,
            "plan source Work scope differs"
        );
        let captured =
            work.capture(&super::context_capture::context_work_capture_request(state))?;
        let mut plans = Vec::new();
        for value in &captured.records {
            if let super::resident_work::CanonicalWorkRecord::Work(item) = &value.record {
                resource_io::charge_owned_metadata(
                    (item.id.len() + std::mem::size_of::<ContextPlanSource>() + 512) as u64,
                    4,
                )?;
                plans.push(ContextPlanSource {
                    work_item_id: item.id.clone(),
                    recorded: item.plan_artifact.is_some(),
                });
            }
        }
        plans.sort_by(|left, right| left.work_item_id.cmp(&right.work_item_id));
        Ok(ContextResourceSource {
            scope: self.inner.resources.scope.clone(),
            agent_home: self.agent_home(),
            user_home: config.user_home_dir.clone(),
            workspace_id: workspace.map(|entry| entry.workspace_id.clone()),
            workspace_anchor: self
                .workspace_anchor_for_state_ref(state)
                .map(Path::to_path_buf),
            execution_root_id: workspace.map(|entry| entry.execution_root_id.clone()),
            execution_root: workspace.map(|entry| entry.execution_root.clone()),
            include_user_skills: state.id == self.inner.default_agent_id,
            config_generation: config.resource_generation,
            host_generation: self.inner.resources.gate.generation()?,
            active_skills_selector: selector.into(),
            work_revision: captured.revision,
            plans,
        })
    }
    pub(super) fn capture_context_resources(
        &self,
        state: &AgentState,
        config: &ConfigSnapshot,
        material: &ResourceReadMaterial,
    ) -> Result<Arc<ResourceObservation>> {
        let _io = material.enter();
        let source = self.context_resource_source(state, config)?;
        let _fence = self.inner.resources.gate.ready(source.host_generation)?;
        self.inner
            .resources
            .registry
            .capture(&source)
            .map_err(|error| self.context_resource_capture_error(error))
    }
    pub(super) fn capture_instruction_resources(
        &self,
        state: &AgentState,
        config: &ConfigSnapshot,
        material: &ResourceReadMaterial,
    ) -> Result<super::resource_observation::InstructionResourceRead> {
        let _io = material.enter();
        let source = self.context_resource_source(state, config)?;
        let _fence = self.inner.resources.gate.ready(source.host_generation)?;
        self.inner
            .resources
            .registry
            .capture_instructions(&source)
            .map_err(|error| self.context_resource_capture_error(error))
    }
    fn context_resource_capture_error(&self, error: anyhow::Error) -> anyhow::Error {
        if matches!(
            self.inner.resources.registry.freshness(),
            Ok(
                super::resource_observation::ResourceFreshness::RefreshFailed
                    | super::resource_observation::ResourceFreshness::Unobserved
            )
        ) {
            if let Ok(failure) = self.inner.resources.last_failure.lock() {
                if let Some(failure) = *failure {
                    return failure.error();
                }
            }
        }
        error
    }
    fn refresh_context_resources_sync(&self) -> Result<Arc<ResourceObservation>> {
        let resources = &self.inner.resources;
        anyhow::ensure!(
            !resources.closed.load(Ordering::Acquire),
            ResourceObservationError::NotReady(
                super::resource_observation::ResourceFreshness::Closed
            )
        );
        resources.retry_owner.store(true, Ordering::Release);
        let owner = self.inner.mutation_owner.try_reserve()?;
        resources.retry_owner.store(false, Ordering::Release);
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("resource source has no published root")?;
        let state = root.state.clone();
        drop(root);
        let config = self.inner.config_snapshot.load_full();
        let source_material =
            ResourceReadMaterial::new(&self.inner.input_evidence_budget, &resources.scope)?;
        let source = {
            let _io = source_material.enter();
            self.context_resource_source(&state, &config)?
        };
        // Only the synchronous source capture owns this reservation.
        drop(owner);
        if let Ok(value) = resources.registry.capture(&source) {
            return Ok(value);
        }
        let inventory = match self.inner.host_bridge.as_ref() {
            Some(bridge) => bridge.skills_registry()?,
            None => resources.standalone_skills.clone(),
        };
        let base = inventory
            .try_read()
            .map_err(|_| ResourceObservationError::InventoryContended)?
            .clone();
        let candidate = std::cell::RefCell::new(None);
        resources.registry.refresh_fenced_after(
            &source,
            source_material.finish()?,
            || {
                #[cfg(test)]
                let checkpoint = resources.checkpoint.lock().unwrap().take();
                #[cfg(test)]
                if let Some(checkpoint) = checkpoint {
                    *checkpoint.inventory_base.lock().unwrap() = Some(base.clone());
                    checkpoint.reached.notify_one();
                    let mut released = checkpoint.released.lock().unwrap();
                    while !*released {
                        released = checkpoint.release.wait(released).unwrap();
                    }
                }
                resource_io::charge_owned_metadata(
                    input_evidence_budget::serialized_bytes(&state.active_skills)? + 16384,
                    256,
                )?;
                // Parser scratch exits before the independent Host skill inventory is retained.
                let agent_templates_catalog = discover_agent_templates_catalog(
                    source.user_home.as_deref(),
                    &source.agent_home,
                );
                resource_io::check_active()?;
                let host_material = ResourceReadMaterial::host(&self.inner.input_evidence_budget)?;
                let (mut catalogue, roots) = {
                    let _io = host_material.enter();
                    base.admit_candidate()?;
                    let mut catalogue = base.clone();
                    resource_io::charge_owned_metadata(16384, 256)?;
                    let roots = effective_skill_root_registrations(
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
                    catalogue.sync_effective_roots(roots.clone())?;
                    for root in catalogue.roots() {
                        if let crate::types::SkillRootScanStatus::Failed { error } =
                            &root.scan_status
                        {
                            bail!("context skill catalogue read failed: {error}");
                        }
                    }
                    host_material.finish()?;
                    (catalogue, roots)
                };
                catalogue.retain_material(host_material.guard()?);
                let catalogue = Arc::new(catalogue);
                let mut skills = skills_runtime_view_from_catalog(
                    catalogue.catalog_for_roots(&roots, None),
                    &roots,
                    &state.active_skills,
                )?;
                skills.agent_templates_catalog = agent_templates_catalog;
                resource_io::check_active()?;
                let mut plans = std::collections::BTreeMap::new();
                for plan in &source.plans {
                    resource_io::charge_owned_metadata((plan.work_item_id.len() + 512) as u64, 16)?;
                    let observed = crate::work_item_plan::observe_plan_artifact(
                        &source.agent_home,
                        &source.scope.agent_id,
                        &plan.work_item_id,
                        plan.recorded,
                    )?;
                    plans.insert(plan.work_item_id.clone(), observed);
                }
                let body = ContextResourceBody {
                    guidance: load_agents_md(
                        source.user_home.as_deref(),
                        &source.agent_home,
                        source.workspace_anchor.as_deref(),
                    )?,
                    memory: load_agent_memory(&source.agent_home)?,
                    notes: crate::notes_catalog::render_agent_home_notes_catalog_checked(
                        &source.agent_home,
                    )?,
                    skills,
                    plans,
                    _host_catalogue: Some(catalogue.clone()),
                };
                *candidate.borrow_mut() = Some(catalogue);
                Ok(body)
            },
            || {
                resources.retry_owner.store(true, Ordering::Release);
                let owner = self.inner.mutation_owner.try_reserve()?;
                resources.retry_owner.store(false, Ordering::Release);
                Ok(owner)
            },
            |owner, expected| {
                anyhow::ensure!(
                    !resources.closed.load(Ordering::Acquire),
                    ResourceObservationError::SourceChanged
                );
                let root = owner
                    .capture_ready_snapshot()?
                    .context("resource validation has no published root")?;
                let current_config = self.inner.config_snapshot.load_full();
                let actual = self.context_resource_source(&root.state, &current_config)?;
                anyhow::ensure!(actual == *expected, ResourceObservationError::SourceChanged);
                let fence = resources.gate.ready(expected.host_generation)?;
                let mut inventory = inventory
                    .try_write()
                    .map_err(|_| ResourceObservationError::InventoryContended)?;
                anyhow::ensure!(
                    inventory.same_snapshot(&base),
                    ResourceObservationError::InventorySnapshotChanged
                );
                let catalogue = candidate
                    .borrow_mut()
                    .take()
                    .context("resource catalogue candidate missing")?;
                *inventory = catalogue.as_ref().clone();
                drop(inventory);
                Ok(fence)
            },
        )
    }
    pub(crate) fn begin_context_resource_write(
        &self,
    ) -> Result<crate::host::resource_writes::ResourceWrite> {
        let writer = self.inner.resources.gate.begin_write()?;
        self.inner
            .resources
            .registry
            .invalidate(ResourceInvalidation::KnownWrite)?;
        Ok(writer)
    }
    pub(crate) fn context_resource_material(&self) -> Result<ResourceReadMaterial> {
        ResourceReadMaterial::new(
            &self.inner.input_evidence_budget,
            &self.inner.resources.scope,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tests::support::{context_config, tempdir, StubProvider};

    fn synchronous_runtime(id: &str, home: &Path) -> Result<RuntimeHandle> {
        let runtime = RuntimeHandle::new(
            id,
            home.to_path_buf(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            id.into(),
            context_config(),
        )?;
        let mut config = (*runtime.inner.config_snapshot.load_full()).clone();
        config.user_home_dir = None;
        runtime.inner.config_snapshot.store(Arc::new(config));
        Ok(runtime)
    }

    async fn shared_inventory_runtimes() -> Result<(
        tempfile::TempDir,
        crate::host::RuntimeHost,
        RuntimeHandle,
        RuntimeHandle,
    )> {
        let home = tempdir()?;
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let mut config = crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        config.user_home_dir = None;
        let host = crate::host::RuntimeHost::new_with_provider(
            config,
            Arc::new(StubProvider::new("unused")),
        )?;
        let first = host.default_runtime().await?;
        first.await_initial_resources().await;
        host.create_named_agent("resource-inventory-peer", None)
            .await?;
        let second = host.get_or_create_agent("resource-inventory-peer").await?;
        second.await_initial_resources().await;
        Ok((home, host, first, second))
    }

    async fn wait_ready(runtime: &RuntimeHandle) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(2), async {
            while runtime.inner.resources.registry.freshness()?
                != super::super::resource_observation::ResourceFreshness::Ready
            {
                tokio::task::yield_now().await;
            }
            Result::<()>::Ok(())
        })
        .await?
    }

    #[tokio::test]
    async fn resource_revision_ready_notification_does_not_start_finite_refresh() -> Result<()> {
        let home = tempdir()?;
        let runtime = synchronous_runtime("ready-resource-notification", home.path())?;
        runtime.await_initial_resources().await;
        let refresh = runtime.inner.resources.refresh_lock.lock().await;
        let attempts = runtime.inner.resources.attempts.load(Ordering::Acquire);
        tokio::time::timeout(Duration::from_secs(2), runtime.ensure_context_resources()).await??;
        assert_eq!(
            runtime.inner.resources.attempts.load(Ordering::Acquire),
            attempts
        );
        drop(refresh);
        runtime.close_execution_admission();
        runtime.join_retiring_resources().await?;
        Ok(())
    }

    fn change_inventory_skill(runtime: &RuntimeHandle, revision: &str) -> Result<()> {
        let skill = runtime
            .inner
            .storage
            .data_dir()
            .join("skills/resource-revision");
        std::fs::create_dir_all(&skill)?;
        std::fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: resource-revision\ndescription: {revision}\n---\n{revision}\n"),
        )?;
        Ok(())
    }

    #[tokio::test]
    async fn resource_revision_shared_inventory_conflict_reorders_without_business_wake(
    ) -> Result<()> {
        let (_home, host, first, second) = shared_inventory_runtimes().await?;
        let inventory_before = host.skills_registry().read().await.clone();
        let first_pause = first.pause_next_resource_refresh();
        let second_pause = second.pause_next_resource_refresh();
        let second_before = second.inner.resources.attempts.load(Ordering::Acquire);
        let first_refresh = {
            let runtime = first.clone();
            tokio::spawn(async move { runtime.refresh_context_resources().await })
        };
        let second_refresh = {
            let runtime = second.clone();
            tokio::spawn(async move { runtime.refresh_context_resources().await })
        };
        tokio::time::timeout(Duration::from_secs(5), first_pause.wait()).await?;
        tokio::time::timeout(Duration::from_secs(5), second_pause.wait()).await?;
        for pause in [&first_pause, &second_pause] {
            assert!(pause
                .0
                .inventory_base
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .same_snapshot(&inventory_before));
        }
        change_inventory_skill(&first, "first publication changes the shared snapshot")?;
        first_pause.release();
        tokio::time::timeout(Duration::from_secs(5), first_refresh).await???;
        assert!(
            !host
                .skills_registry()
                .read()
                .await
                .same_snapshot(&inventory_before),
            "first publication must actually replace the shared inventory"
        );
        second_pause.release();
        tokio::time::timeout(Duration::from_secs(5), second_refresh).await???;
        assert_eq!(
            second.inner.resources.attempts.load(Ordering::Acquire),
            second_before + 2
        );
        second
            .preview_prompt(
                "after inventory conflict".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await?;
        host.shutdown().await
    }

    #[tokio::test]
    async fn resource_revision_inventory_write_lock_release_wakes_observation() -> Result<()> {
        let (_home, host, first, _second) = shared_inventory_runtimes().await?;
        let inventory = host.skills_registry();
        let held = inventory.write().await;
        let before = first.inner.resources.attempts.load(Ordering::Acquire);
        first
            .inner
            .resources
            .registry
            .invalidate(ResourceInvalidation::ExplicitRefresh)?;
        first.inner.resources.notify();
        tokio::time::timeout(Duration::from_secs(2), async {
            while first.inner.resources.attempts.load(Ordering::Acquire) == before {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_ne!(
            first.inner.resources.registry.freshness()?,
            super::super::resource_observation::ResourceFreshness::Ready
        );
        drop(held);
        wait_ready(&first).await?;
        host.shutdown().await
    }

    #[tokio::test]
    async fn resource_revision_continuous_snapshot_conflict_fails_after_one_reorder() -> Result<()>
    {
        let (_home, host, first, second) = shared_inventory_runtimes().await?;
        let before = second.inner.resources.attempts.load(Ordering::Acquire);
        let pause = second.pause_next_resource_refresh();
        let refresh = {
            let runtime = second.clone();
            tokio::spawn(async move { runtime.refresh_context_resources().await })
        };
        tokio::time::timeout(Duration::from_secs(5), pause.wait()).await?;
        let retry_pause = second.pause_next_resource_refresh();
        let inventory_before = host.skills_registry().read().await.clone();
        assert!(pause
            .0
            .inventory_base
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .same_snapshot(&inventory_before));
        change_inventory_skill(&first, "first conflict")?;
        first.refresh_context_resources().await?;
        assert!(
            !host
                .skills_registry()
                .read()
                .await
                .same_snapshot(&inventory_before),
            "first publication must actually replace the shared inventory"
        );
        pause.release();
        tokio::time::timeout(Duration::from_secs(5), retry_pause.wait()).await?;
        let inventory_before = host.skills_registry().read().await.clone();
        assert!(retry_pause
            .0
            .inventory_base
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .same_snapshot(&inventory_before));
        change_inventory_skill(&first, "second conflict has different bytes")?;
        first.refresh_context_resources().await?;
        assert!(!host
            .skills_registry()
            .read()
            .await
            .same_snapshot(&inventory_before));
        retry_pause.release();
        let error = tokio::time::timeout(Duration::from_secs(5), refresh)
            .await??
            .expect_err("the second snapshot conflict must not retry again");
        assert!(
            matches!(
                error.downcast_ref::<ResourceObservationError>(),
                Some(ResourceObservationError::InventorySnapshotChanged)
            ),
            "{error:#}"
        );
        assert_eq!(
            second.inner.resources.attempts.load(Ordering::Acquire),
            before + 2
        );
        assert_eq!(
            second.inner.resources.registry.freshness()?,
            super::super::resource_observation::ResourceFreshness::RefreshFailed
        );
        assert!(second.inner.resources.initial_done.load(Ordering::Acquire));
        let message = MessageEnvelope::new(
            second.agent_id().await?,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "conflict must not fake Ready".into(),
            },
        );
        // Loaded capture observes the failed attempt; debug preparation may
        // explicitly start a new one and must not hide this rejection.
        assert!(tokio::time::timeout(
            Duration::from_secs(5),
            second.build_loaded_context_prompt(&message, None, None, false),
        )
        .await?
        .is_err());
        assert_eq!(
            second.inner.resources.attempts.load(Ordering::Acquire),
            before + 2
        );
        assert_eq!(
            second.inner.resources.registry.freshness()?,
            super::super::resource_observation::ResourceFreshness::RefreshFailed
        );
        tokio::time::timeout(
            Duration::from_secs(5),
            second.preview_prompt(
                "explicit preparation after conflict".into(),
                AuthorityClass::OperatorInstruction,
            ),
        )
        .await??;
        assert_eq!(
            second.inner.resources.registry.freshness()?,
            super::super::resource_observation::ResourceFreshness::Ready
        );
        host.shutdown().await
    }

    #[test]
    fn resource_revision_close_unblocks_inventory_wait_without_unlock_or_business_event(
    ) -> Result<()> {
        let home = tempdir()?;
        let runtime = synchronous_runtime("resource-close-inventory", home.path())?;
        tokio::runtime::Runtime::new()?.block_on(async {
            let inventory = runtime.inner.resources.standalone_skills.clone();
            let held = inventory.write().await;
            runtime.start_resource_worker()?;
            tokio::time::timeout(Duration::from_secs(2), async {
                while runtime.inner.resources.attempts.load(Ordering::Acquire) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await?;
            tokio::time::sleep(Duration::from_millis(20)).await;
            runtime.inner.resources.close();
            tokio::time::timeout(Duration::from_secs(2), runtime.await_initial_resources()).await?;
            tokio::time::timeout(Duration::from_secs(2), runtime.join_owned_mutations()).await??;
            assert_eq!(
                runtime.inner.resources.registry.freshness()?,
                super::super::resource_observation::ResourceFreshness::Closed
            );
            drop(held);
            Result::<()>::Ok(())
        })
    }

    #[tokio::test]
    async fn resource_revision_cancelled_refresh_keeps_physical_worker_owned_until_exit(
    ) -> Result<()> {
        let (_home, host, runtime, _second) = shared_inventory_runtimes().await?;
        let pause = runtime.pause_next_resource_refresh();
        let request = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.refresh_context_resources().await }
        });
        tokio::time::timeout(Duration::from_secs(5), pause.wait()).await?;
        request.abort();
        let _ = request.await;
        let draining = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.join_owned_mutations().await }
        });
        tokio::task::yield_now().await;
        assert!(!draining.is_finished());
        pause.release();
        tokio::time::timeout(Duration::from_secs(5), draining).await???;
        host.shutdown().await
    }

    #[test]
    fn resource_revision_sync_constructor_starts_on_first_async_context() -> Result<()> {
        let home = tempdir()?;
        let runtime = synchronous_runtime("resource-sync-constructor", home.path())?;
        tokio::runtime::Runtime::new()?.block_on(async {
            let prompt = tokio::time::timeout(
                Duration::from_secs(2),
                runtime.preview_prompt("first context".into(), AuthorityClass::OperatorInstruction),
            )
            .await??;
            assert!(prompt._resource_read_material.is_some());
            runtime.join_owned_mutations().await
        })
    }

    #[test]
    fn resource_revision_close_releases_every_initial_waiter() -> Result<()> {
        let home = tempdir()?;
        let runtime = synchronous_runtime("resource-close-initial", home.path())?;
        tokio::runtime::Runtime::new()?.block_on(async {
            let mut waits = Vec::new();
            for _ in 0..8 {
                let runtime = runtime.clone();
                waits.push(tokio::spawn(async move {
                    runtime.await_initial_resources().await
                }));
            }
            tokio::task::yield_now().await;
            runtime.inner.resources.close();
            tokio::time::timeout(Duration::from_secs(2), async {
                for wait in waits {
                    let _ = wait.await?;
                }
                Result::<()>::Ok(())
            })
            .await??;
            runtime.join_owned_mutations().await
        })
    }

    #[test]
    fn resource_revision_concurrent_first_context_and_close_settle() -> Result<()> {
        let home = tempdir()?;
        let runtime = synchronous_runtime("resource-start-close-race", home.path())?;
        tokio::runtime::Runtime::new()?.block_on(async {
            let mut waits = Vec::new();
            for _ in 0..16 {
                let runtime = runtime.clone();
                waits.push(tokio::spawn(async move {
                    runtime
                        .preview_prompt(
                            "first context race".into(),
                            AuthorityClass::OperatorInstruction,
                        )
                        .await
                }));
            }
            tokio::task::yield_now().await;
            runtime.inner.resources.close();
            tokio::time::timeout(Duration::from_secs(2), async {
                for wait in waits {
                    let _ = wait.await?;
                }
                Result::<()>::Ok(())
            })
            .await??;
            runtime.join_owned_mutations().await
        })
    }

    #[tokio::test]
    async fn initial_owner_busy_waits_for_release_before_publishing_readiness() -> Result<()> {
        let home = tempdir()?;
        let runtime = RuntimeHandle::new(
            "initial-resource-busy",
            home.path().to_path_buf(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "initial-resource-busy".into(),
            context_config(),
        )?;
        let mut config = (*runtime.inner.config_snapshot.load_full()).clone();
        config.user_home_dir = None;
        runtime.inner.config_snapshot.store(Arc::new(config));
        let checkpoint = runtime.pause_next_resource_refresh();
        let wake = ResourceOwnerWake(Arc::downgrade(&runtime.inner));
        tokio::time::timeout(Duration::from_secs(5), checkpoint.wait()).await?;
        let owner = runtime.inner.mutation_owner.try_reserve()?;
        checkpoint.release();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !runtime.inner.resources.retry_owner.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
            let _refresh = runtime.inner.resources.refresh_lock.lock().await;
        })
        .await?;
        assert!(!runtime.inner.resources.initial_done.load(Ordering::Acquire));
        assert_eq!(
            runtime.inner.resources.registry.freshness()?,
            super::super::resource_observation::ResourceFreshness::Unobserved
        );
        drop(owner);
        drop(wake);
        tokio::time::timeout(Duration::from_secs(5), runtime.await_initial_resources()).await?;
        assert_eq!(
            runtime.inner.resources.registry.freshness()?,
            super::super::resource_observation::ResourceFreshness::Ready
        );
        runtime.join_owned_mutations().await?;
        Ok(())
    }

    #[test]
    fn explicit_preparation_owner_busy_waits_or_closes_without_refresh_lock() -> Result<()> {
        for close in [false, true] {
            let home = tempdir()?;
            let runtime = synchronous_runtime("explicit-resource-busy", home.path())?;
            tokio::runtime::Runtime::new()?.block_on(async {
                let checkpoint = runtime.pause_next_resource_refresh();
                let request = tokio::spawn({
                    let runtime = runtime.clone();
                    async move { runtime.ensure_context_resources().await }
                });
                tokio::time::timeout(Duration::from_secs(5), checkpoint.wait()).await?;
                let mut owner = Some(runtime.inner.mutation_owner.try_reserve()?);
                checkpoint.release();
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !runtime.inner.resources.retry_owner.load(Ordering::Acquire) {
                        tokio::task::yield_now().await;
                    }
                    let _refresh = runtime.inner.resources.refresh_lock.lock().await;
                })
                .await?;
                assert!(
                    !request.is_finished(),
                    "owner contention must not abort preparation"
                );
                assert_eq!(runtime.inner.resources.attempts.load(Ordering::Acquire), 1);
                assert_eq!(
                    runtime.inner.resources.registry.freshness()?,
                    super::super::resource_observation::ResourceFreshness::Unobserved
                );
                if close {
                    runtime.inner.resources.close();
                } else {
                    drop(owner.take());
                }
                let result = tokio::time::timeout(Duration::from_secs(5), request).await??;
                if close {
                    assert!(matches!(
                        result.err().and_then(|error| {
                            error.downcast::<ResourceObservationError>().ok()
                        }),
                        Some(ResourceObservationError::NotReady(
                            super::super::resource_observation::ResourceFreshness::Closed
                        ))
                    ));
                } else {
                    result?;
                    assert_eq!(runtime.inner.resources.attempts.load(Ordering::Acquire), 2);
                    assert_eq!(
                        runtime.inner.resources.registry.freshness()?,
                        super::super::resource_observation::ResourceFreshness::Ready
                    );
                }
                drop(owner);
                runtime.join_owned_mutations().await
            })?;
        }
        Ok(())
    }

    #[test]
    fn typed_source_change_survives_the_refresh_failure_projection() {
        let failure: anyhow::Error = ResourceObservationError::SourceChanged.into();
        assert!(matches!(
            RefreshFailure::from_error(&failure),
            RefreshFailure::SourceChanged
        ));
        let projected = RefreshFailure::SourceChanged.error();
        assert!(matches!(
            projected.downcast_ref::<ResourceObservationError>(),
            Some(ResourceObservationError::SourceChanged)
        ));
        // A wrapped failure keeps its type for the preparation paths.
        let wrapped = failure.context("summary capture");
        assert!(is_source_changed_error(&wrapped));
        assert!(matches!(
            RefreshFailure::from_error(&wrapped),
            RefreshFailure::SourceChanged
        ));
        // Every other failure stays opaque and fail-closed.
        assert!(matches!(
            RefreshFailure::from_error(&anyhow!("context resource refresh failed")),
            RefreshFailure::Other
        ));
        assert!(!is_source_changed_error(&anyhow!(
            "context resource refresh failed"
        )));
    }
}
