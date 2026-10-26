//! Host retains original Stop responsibility across startup and Runtime release.
use super::*;
use crate::{
    runtime::{
        input_evidence_budget::{BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate},
        resident_input_evidence::InputEvidenceScope,
        resident_task::TaskCaptureRequest,
    },
    runtime_db::{
        task_snapshot::TaskDomainLoadRequest,
        task_stop::{
            load_pending_task_stops_tx, pending_task_stop_host_scope_inventory_tx,
            PreparedTaskStops,
        },
    },
};

const RETRY_INTERVAL: Duration = Duration::from_secs(30);
const MAX_WORKERS: usize = 16;

#[derive(Default)]
pub(super) struct TaskStopSupervisor {
    token: CancellationToken,
    handle: Mutex<Option<JoinHandle<()>>>,
    sweep: AsyncMutex<()>,
}

pub(super) struct StopSweepWorkers {
    tasks: JoinSet<Result<()>>,
    failed: bool,
    _metadata: Arc<BudgetViewGuard>,
}
impl StopSweepWorkers {
    pub(super) fn new(host: &RuntimeHost) -> Result<Self> {
        // Bounded task slots, Arc selectors and fixed futures; original record
        // bodies have independent scope guards and are never copied here.
        let metadata = host.inner.input_evidence_budget.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(MAX_WORKERS * 4096)?,
            },
        )?;
        Ok(Self {
            tasks: JoinSet::new(),
            failed: false,
            _metadata: metadata,
        })
    }
    pub(super) async fn admit(
        &mut self,
        future: impl Future<Output = Result<()>> + Send + 'static,
    ) {
        while self.tasks.len() >= MAX_WORKERS {
            self.join_one().await;
        }
        self.tasks.spawn(future);
    }
    async fn join_one(&mut self) {
        let Some(result) = self.tasks.join_next().await else {
            return;
        };
        let result = result.map_err(anyhow::Error::from).and_then(|value| value);
        if let Err(error) = result {
            tracing::warn!(error = %error, "original TaskStop continuation remains durable");
            self.failed = true;
        }
    }
    fn record_error(&mut self, error: anyhow::Error) {
        tracing::warn!(error = %error, "original TaskStop discovery remains durable");
        self.failed = true;
    }
    pub(super) async fn finish(mut self) -> Result<()> {
        while !self.tasks.is_empty() {
            self.join_one().await;
        }
        anyhow::ensure!(
            !self.failed,
            "original TaskStop continuation failed; durable responsibility retained"
        );
        Ok(())
    }
}

impl RuntimeHost {
    fn task_stop_supervisor_request(&self) -> Result<TaskDomainLoadRequest> {
        let limits = self.inner.input_evidence_budget.limits();
        Ok(TaskDomainLoadRequest {
            query: Default::default(),
            capture: TaskCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit)?.min(65_536),
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Stop supervisor limits overflow")?,
            )?
            .min(64 * 1024 * 1024),
        })
    }

    /// Called by the already registered startup Host actor, before Queue's
    /// original activation inventory. It never supplies its own activation IDs.
    pub(super) async fn sweep_pending_task_stops_at_startup(&self) -> Result<()> {
        self.sweep_pending_task_stops().await
    }

    pub(super) fn start_task_stop_supervisor(&self) {
        let mut handle = self
            .inner
            .task_stop_supervisor
            .handle
            .lock()
            .expect("TaskStop supervisor poisoned");
        if handle.is_some() || self.inner.task_stop_supervisor.token.is_cancelled() {
            return;
        }
        let host = self.clone();
        *handle = Some(tokio::spawn(async move {
            // Startup already completed one bounded pass. Every later pass is
            // owned independently of a disconnected API caller.
            loop {
                tokio::select! {
                    _ = host.inner.task_stop_supervisor.token.cancelled() => break,
                    _ = tokio::time::sleep(RETRY_INTERVAL) => {},
                }
                if host.inner.runtimes.read().await.phase != HostRuntimePhase::Open {
                    break;
                }
                if let Err(error) = host.sweep_pending_task_stops().await {
                    tracing::warn!(error = %error, "TaskStop supervisor will retry original pending responsibilities");
                }
            }
        }));
    }

    pub(super) fn stop_task_stop_supervisor_discovery(&self) {
        self.inner.task_stop_supervisor.token.cancel();
    }

    pub(super) async fn shutdown_task_stop_supervisor(&self) {
        self.stop_task_stop_supervisor_discovery();
        let handle = self
            .inner
            .task_stop_supervisor
            .handle
            .lock()
            .expect("TaskStop supervisor poisoned")
            .take();
        if let Some(handle) = handle {
            // Never abort an accepted continuation after its original SQL ACK.
            if let Err(error) = handle.await {
                tracing::warn!(error = %error, "TaskStop supervisor exited before shutdown drain");
            }
        }
    }

    pub(super) async fn sweep_pending_task_stops_after_runtime_drain(&self) -> Result<()> {
        anyhow::ensure!(
            self.inner.runtimes.read().await.phase == HostRuntimePhase::Closing,
            "TaskStop closing sweep requires the original shutdown actor"
        );
        self.sweep_pending_task_stops().await
    }

    pub(super) async fn load_task_stop_supervisor_scope(
        &self,
        scope: &InputEvidenceScope,
        request: &TaskDomainLoadRequest,
    ) -> Result<(
        PreparedTaskStops,
        Arc<BudgetViewGuard>,
        Option<RuntimeHandle>,
    )> {
        // Exact old incarnation is selected by durable Stop metadata; the
        // current identity and Task terminal flag do not replace this address.
        let reservations = self
            .reserve_mutation_scopes(&[(scope.agent_id.clone(), scope.identity_incarnation)])
            .await?;
        let reservation = reservations
            .first()
            .context("Stop supervisor owner missing")?;
        anyhow::ensure!(
            reservations.len() == 1,
            "Stop supervisor owner group changed"
        );
        let loaded = self.bridge().runtime_for_mutation(reservation).await?;
        let rows = self.runtime_db().transaction(|tx| {
            load_pending_task_stops_tx(
                tx,
                scope,
                request.record_limit,
                request.byte_limit,
                &self.inner.input_evidence_budget,
            )
        })?;
        let source_material = self.inner.input_evidence_budget.register_task_stop_view(
            scope,
            InputMemoryCategory::Candidate,
            &rows.records,
        )?;
        drop(reservations);
        Ok((rows, source_material, loaded))
    }

    async fn original_task_stop_runtime(
        &self,
        original: &crate::runtime_db::task_stop::TaskStopRecord,
        current: Option<&RuntimeHandle>,
    ) -> Result<Option<RuntimeHandle>> {
        let registry = self.inner.runtimes.read().await;
        let mut exact: Option<&RuntimeHandle> = None;
        for runtime in registry
            .agents
            .values()
            .map(|entry| &entry.runtime)
            .chain(registry.unloading_mutations.values())
            .chain(
                registry
                    .temporary_runtimes
                    .values()
                    .map(|entry| &entry.runtime),
            )
        {
            if runtime.instance_agent_id() != original.intent.task.scope.agent_id
                || runtime.instance_incarnation() != original.intent.task.scope.identity_incarnation
                || runtime.instance_owner_epoch() != original.intent.owner_epoch
                || !runtime.has_original_task_stop_plan(original)?
            {
                continue;
            }
            if let Some(previous) = exact {
                anyhow::ensure!(
                    previous.original_process_registry_id()
                        == runtime.original_process_registry_id(),
                    "original TaskStop plan is ambiguous across resource instances"
                );
            } else {
                exact = Some(runtime);
            }
        }
        // Only after the complete registered fleet has no exact old plan may
        // current runtime's helper select its honest restart/cold path.
        Ok(exact.or(current).cloned())
    }

    pub(super) async fn retained_task_stop_runtime_inventory(
        &self,
        request: &TaskDomainLoadRequest,
    ) -> Result<(Vec<RuntimeHandle>, Arc<BudgetViewGuard>)> {
        let registry = self.inner.runtimes.read().await;
        let mut count = 0usize;
        let mut bytes = 0usize;
        for runtime in registry
            .agents
            .values()
            .map(|entry| &entry.runtime)
            .chain(registry.unloading_mutations.values())
            .chain(
                registry
                    .temporary_runtimes
                    .values()
                    .map(|entry| &entry.runtime),
            )
        {
            count = count
                .checked_add(1)
                .context("retained Stop fleet count overflow")?;
            bytes = bytes
                .checked_add(runtime.instance_agent_id().len())
                .and_then(|n| n.checked_add(128))
                .context("retained Stop fleet bytes overflow")?;
        }
        let bytes = bytes
            .checked_mul(4)
            .context("retained Stop fleet bytes overflow")?;
        let count = count
            .checked_mul(4)
            .context("retained Stop fleet count overflow")?;
        anyhow::ensure!(
            count <= request.record_limit.min(65_536)
                && bytes <= request.byte_limit.min(64 * 1024 * 1024),
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "retained Stop fleet metadata"
            )
        );
        let metadata = self.inner.input_evidence_budget.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(bytes)?,
            },
        )?;
        let mut instances = BTreeMap::new();
        for runtime in registry
            .agents
            .values()
            .map(|entry| &entry.runtime)
            .chain(registry.unloading_mutations.values())
            .chain(
                registry
                    .temporary_runtimes
                    .values()
                    .map(|entry| &entry.runtime),
            )
        {
            instances
                .entry(runtime.original_process_registry_id())
                .or_insert_with(|| runtime.clone());
        }
        Ok((instances.into_values().collect(), metadata))
    }

    async fn sweep_pending_task_stops(&self) -> Result<()> {
        let _serial = self.inner.task_stop_supervisor.sweep.lock().await;
        let request = self.task_stop_supervisor_request()?;
        let inventory = self.runtime_db().transaction(|tx| {
            pending_task_stop_host_scope_inventory_tx(
                tx,
                &request,
                &self.inner.input_evidence_budget,
            )
        })?;
        let mut workers = StopSweepWorkers::new(self)?;
        // Initial unknown/all-facets-Done plans do not appear in SQL's Pending
        // inventory. Their exact original witness must still be reconciled.
        let (retained, _fleet_metadata) =
            self.retained_task_stop_runtime_inventory(&request).await?;
        for runtime in retained {
            workers
                .admit(async move { runtime.continue_retained_task_stop_plans_owned().await })
                .await;
        }
        for (scope, _) in inventory.scopes() {
            let (rows, source_material, loaded) =
                match self.load_task_stop_supervisor_scope(scope, &request).await {
                    Ok(rows) => rows,
                    Err(error) => {
                        workers.record_error(error);
                        continue;
                    }
                };
            // The finite complete scope body inventory remains charged while
            // selectors are moved into independently owned worker futures.
            for original in &rows.records {
                let original = original.clone();
                let source_material = source_material.clone();
                let bridge = self.bridge();
                let loaded = match self
                    .original_task_stop_runtime(&original, loaded.as_ref())
                    .await
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        workers.record_error(error);
                        continue;
                    }
                };
                let request = request.clone();
                workers
                    .admit(async move {
                        if let Some(runtime) = loaded {
                            runtime
                                .continue_original_task_stop_loaded_owned(
                                    original,
                                    source_material,
                                    request,
                                )
                                .await
                        } else {
                            crate::runtime::continue_original_task_stop_metadata(
                                &bridge,
                                original,
                                source_material,
                                request,
                            )
                            .await
                        }
                    })
                    .await;
            }
        }
        workers.finish().await
    }
}
