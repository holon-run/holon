//! Host-instance readiness fence. It neither admits business writes nor owns identity.
use super::*;
use crate::runtime::{
    input_evidence_budget::{BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate},
    resource_observation::{ResourceFreshness, ResourceInvalidation, ResourceObservationError},
};

#[derive(Default)]
pub(super) struct HostResourceWriteTracker {
    state: Mutex<HostResourceWriteState>,
    notify: Arc<Notify>,
}
#[derive(Default)]
struct HostResourceWriteState {
    active: usize,
    generation: u64,
    closing: bool,
    sealed: bool,
    exhausted: bool,
    waiters: BTreeMap<u64, ResourceWriteWaiter>,
}
struct ResourceWriteWaiter {
    physical_active: bool,
    worker: JoinHandle<()>,
    _material: Arc<BudgetViewGuard>,
}
impl HostResourceWriteState {
    fn reap_finished(&mut self) -> Result<()> {
        loop {
            let finished = self
                .waiters
                .iter()
                .find(|(_, entry)| !entry.physical_active && entry.worker.is_finished())
                .map(|(id, _)| *id);
            let Some(id) = finished else { return Ok(()) };
            let completion = {
                let entry = self
                    .waiters
                    .get_mut(&id)
                    .expect("observed finished resource waiter");
                let mut context = std::task::Context::from_waker(std::task::Waker::noop());
                std::future::Future::poll(std::pin::Pin::new(&mut entry.worker), &mut context)
            };
            let std::task::Poll::Ready(joined) = completion else {
                anyhow::bail!("finished resource waiter did not report completion");
            };
            // A Pending poll leaves the actual node, handle and guard retained.
            // Never discard a real JoinError while releasing a completed node.
            self.waiters
                .remove(&id)
                .expect("polled resource waiter disappeared");
            joined.context("original Host resource completion waiter failed")?;
        }
    }
}
#[derive(Default)]
struct PhysicalWriteCompletion {
    done: AtomicBool,
    notify: Notify,
}
impl PhysicalWriteCompletion {
    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.done.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}
/// The actual non-cancellable kernel owns this value (or shares its Arc).
/// The Host completion waiter keeps the original maintenance lease alive.
pub(crate) struct HostResourceWriteLease {
    tracker: Arc<HostResourceWriteTracker>,
    registration: u64,
    completion: Arc<PhysicalWriteCompletion>,
    _material: Arc<BudgetViewGuard>,
}
impl HostResourceWriteLease {
    pub(super) fn material_estimate(&self) -> (u64, usize) {
        (1024, 16)
    }
}
impl Drop for HostResourceWriteLease {
    fn drop(&mut self) {
        let mut state = self.tracker.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active = state
            .active
            .checked_sub(1)
            .expect("original Host resource writer");
        state
            .waiters
            .get_mut(&self.registration)
            .expect("original retained resource waiter")
            .physical_active = false;
        if let Some(next) = state.generation.checked_add(1) {
            state.generation = next;
        } else {
            state.exhausted = true;
        }
        drop(state);
        self.completion.done.store(true, Ordering::Release);
        self.completion.notify.notify_waiters();
        self.tracker.notify.notify_waiters();
    }
}
/// Start/actual-status boundary only; never an active writer lease.
pub(crate) struct HostResourceProcessChanges {
    tracker: Weak<HostResourceWriteTracker>,
}
impl HostResourceProcessChanges {
    pub(crate) fn physical_exit(self) {
        if let Some(tracker) = self.tracker.upgrade() {
            if let Err(error) = tracker.invalidate_process_boundary() {
                tracing::warn!(%error, "original Command Host resource exit invalidation failed");
            }
        }
    }
}
impl HostResourceWriteTracker {
    fn invalidate_process_boundary(&self) -> Result<()> {
        let mut state = self.lock()?;
        state.generation = match state.generation.checked_add(1) {
            Some(next) if !state.exhausted => next,
            _ => {
                state.exhausted = true;
                return Err(ResourceObservationError::GenerationExhausted.into());
            }
        };
        drop(state);
        self.notify.notify_waiters();
        Ok(())
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HostResourceWriteState>> {
        self.state
            .lock()
            .map_err(|_| anyhow!("Host resource writer lock poisoned"))
    }
    fn ready_generation(state: &HostResourceWriteState) -> Result<u64> {
        anyhow::ensure!(
            !state.exhausted,
            ResourceObservationError::GenerationExhausted
        );
        anyhow::ensure!(
            !state.closing && !state.sealed,
            ResourceObservationError::NotReady(ResourceFreshness::Closed)
        );
        anyhow::ensure!(
            state.active == 0,
            ResourceObservationError::NotReady(ResourceFreshness::Dirty(
                ResourceInvalidation::KnownWrite
            ))
        );
        Ok(state.generation)
    }
    pub(super) fn close_new_writers(&self) -> Result<()> {
        self.lock()?.closing = true;
        self.notify.notify_waiters();
        Ok(())
    }
    /// Runtime producers have joined. Actual nodes/handles keep their guards
    /// through the final physical exit and their original Join result.
    /// An inner error is reported only after physical idle and every real join.
    pub(super) async fn seal_and_drain(&self) -> Result<Option<anyhow::Error>> {
        self.lock()?.sealed = true;
        let waiters = loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let ready = {
                let mut state = self.lock()?;
                (state.active == 0).then(|| std::mem::take(&mut state.waiters))
            };
            if let Some(waiters) = ready {
                break waiters;
            }
            notified.await;
        };
        let mut first_error = None;
        for (_, entry) in waiters {
            let ResourceWriteWaiter {
                worker, _material, ..
            } = entry;
            if let Err(error) = worker.await {
                first_error.get_or_insert_with(|| anyhow::Error::new(error));
            }
            drop(_material);
        }
        Ok(first_error
            .map(|error| error.context("draining original Host resource completion waiters")))
    }
}
impl RuntimeHost {
    pub(crate) fn context_resource_generation(&self) -> Result<u64> {
        let state = self.inner.resource_writes.lock()?;
        HostResourceWriteTracker::ready_generation(&state)
    }
    /// For final bounded cold-read ownership handoff, never around file I/O.
    pub(crate) fn with_context_resource_generation<T>(
        &self,
        expected: u64,
        read: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let state = self.inner.resource_writes.lock()?;
        anyhow::ensure!(
            HostResourceWriteTracker::ready_generation(&state)? == expected,
            ResourceObservationError::ObsoleteRefresh
        );
        read()
    }
    pub(super) fn begin_open_context_resource_write(&self) -> Result<HostResourceWriteLease> {
        let tracker = &self.inner.resource_writes;
        let state = tracker.lock()?;
        anyhow::ensure!(!state.closing, "runtime admission closed");
        self.retain_context_resource_kernel(tracker, state)
    }
    fn begin_original_context_resource_kernel(&self) -> Result<HostResourceWriteLease> {
        let tracker = &self.inner.resource_writes;
        // Readiness is already Closed during shutdown. Existing authorized
        // Runtime kernels may finish until all original producers have joined.
        let state = tracker.lock()?;
        self.retain_context_resource_kernel(tracker, state)
    }
    fn retain_context_resource_kernel(
        &self,
        tracker: &Arc<HostResourceWriteTracker>,
        mut state: std::sync::MutexGuard<'_, HostResourceWriteState>,
    ) -> Result<HostResourceWriteLease> {
        anyhow::ensure!(!state.sealed, "original resource kernels already drained");
        anyhow::ensure!(
            !state.exhausted,
            ResourceObservationError::GenerationExhausted
        );
        state.reap_finished()?;
        let retained = state
            .waiters
            .len()
            .checked_add(1)
            .context("Host resource retained waiters overflow")?;
        let active = state
            .active
            .checked_add(1)
            .context("Host resource writers overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        let record_limit = usize::try_from(limits.record_limit.min(65_536))?;
        let byte_limit = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("Host resource writer limits overflow")?
            .min(64 * 1024 * 1024);
        const METADATA_BYTES: u64 = 1024;
        const METADATA_RECORDS: usize = 16;
        anyhow::ensure!(
            retained
                .checked_mul(METADATA_RECORDS)
                .context("Host writer records overflow")?
                <= record_limit
                && u64::try_from(retained)?
                    .checked_mul(METADATA_BYTES)
                    .context("Host writer bytes overflow")?
                    <= byte_limit,
            ResourceObservationError::ResourceExhausted("Host physical writers")
        );
        let material = self.inner.input_evidence_budget.register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: METADATA_BYTES,
            },
        )?;
        let next = match state.generation.checked_add(1) {
            Some(next) => next,
            None => {
                state.exhausted = true;
                return Err(ResourceObservationError::GenerationExhausted.into());
            }
        };
        let completion = Arc::new(PhysicalWriteCompletion::default());
        let original_host = self.clone();
        let original_completion = completion.clone();
        let original_material = material.clone();
        // This focused Host-owned inventory has no generic task Vec capacity.
        // Each actual node, handle, lease and future shares the same guard until
        // physical completion plus actual finished-handle reap or final drain.
        let worker = tokio::spawn(async move {
            let _original_host = original_host;
            let _original_material = original_material;
            original_completion.wait().await;
        });
        state.waiters.insert(
            next,
            ResourceWriteWaiter {
                physical_active: true,
                worker,
                _material: material.clone(),
            },
        );
        state.generation = next;
        state.active = active;
        drop(state);
        tracker.notify.notify_waiters();
        Ok(HostResourceWriteLease {
            tracker: tracker.clone(),
            registration: next,
            completion,
            _material: material,
        })
    }
}
impl RuntimeHostBridge {
    pub(crate) fn context_resource_generation(&self) -> Result<u64> {
        self.host()?.context_resource_generation()
    }
    pub(crate) fn with_context_resource_generation<T>(
        &self,
        expected: u64,
        read: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.host()?
            .with_context_resource_generation(expected, read)
    }
    pub(crate) fn original_command_resource_changes(&self) -> Result<HostResourceProcessChanges> {
        let host = self.host()?;
        host.inner.resource_writes.invalidate_process_boundary()?;
        Ok(HostResourceProcessChanges {
            tracker: Arc::downgrade(&host.inner.resource_writes),
        })
    }
    pub(crate) fn context_resource_write_notify(&self) -> Result<Arc<Notify>> {
        Ok(self.host()?.inner.resource_writes.notify.clone())
    }
    pub(crate) fn begin_original_context_resource_write(&self) -> Result<HostResourceWriteLease> {
        self.host()?.begin_original_context_resource_kernel()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::StubProvider;

    fn host() -> Result<(tempfile::TempDir, RuntimeHost)> {
        host_with_record_limit(None)
    }
    fn host_with_record_limit(
        record_limit: Option<u64>,
    ) -> Result<(tempfile::TempDir, RuntimeHost)> {
        let home = tempfile::tempdir()?;
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        config.user_home_dir = Some(home.path().to_path_buf());
        config.stored_config.runtime.state_memory.record_limit = record_limit;
        let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
        host.ensure_default_agent_identity()?;
        Ok((home, host))
    }

    #[tokio::test]
    async fn global_writer_fences_before_fleet_and_survives_caller_cancel_and_host_shutdown(
    ) -> Result<()> {
        let (_home, host) = host()?;
        let config = host.config().as_ref().clone();
        host.adopt_runtime_db_maintenance_lease(crate::runtime_db::RuntimeDbLock::try_lock(
            config.runtime_db_maintenance_lock_path(),
        )?)
        .await?;
        let agent_id = config.default_agent_id.clone();
        let path = host.agent_data_dir(&agent_id).join("AGENTS.md");
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, "# original global guidance")?;
        let fleet = host.inner.runtimes.write().await;
        assert!(fleet.agents.is_empty());
        let (reached_tx, reached_rx) = oneshot::channel();
        let (finished_tx, finished_rx) = oneshot::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let writer_host = host.clone();
        let caller = tokio::spawn(async move {
            let writes = writer_host.begin_loaded_context_resource_write().await?;
            tokio::task::spawn_blocking(move || -> Result<()> {
                let _writes = writes;
                std::fs::write(&path, "# partially written global guidance")?;
                let _ = reached_tx.send(());
                resume_rx.recv()?;
                std::fs::write(&path, "# completed global guidance")?;
                let _ = finished_tx.send(());
                Ok(())
            })
            .await?
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if host.inner.resource_writes.lock()?.active == 1 {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        // The fleet lock still prevents the old implementation's first fence.
        assert!(host.context_resource_generation().is_err());
        assert!(!caller.is_finished());
        drop(fleet);
        tokio::time::timeout(Duration::from_secs(5), reached_rx).await??;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert_eq!(host.inner.resource_writes.lock()?.active, 1);
        assert!(host.context_resource_generation().is_err());
        // This Runtime was absent from the fleet at write admission. Its core
        // bootstrap remains usable, while mandatory resources remain fenced.
        let runtime = host.default_runtime().await?;
        tokio::time::timeout(Duration::from_secs(5), runtime.wait_for_bootstrap()).await??;
        assert_eq!(runtime.agent_state().await?.id, agent_id);
        let unavailable = runtime
            .capture_current_context_resources()
            .err()
            .context("new activation must not capture a partial resource")?;
        assert!(matches!(
            unavailable.downcast_ref::<ResourceObservationError>(),
            Some(ResourceObservationError::NotReady(
                ResourceFreshness::Dirty(ResourceInvalidation::KnownWrite)
            ))
        ));
        let shutdown_host = host.clone();
        let shutdown = tokio::spawn(async move { shutdown_host.shutdown().await });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if host.inner.runtimes.read().await.phase == HostRuntimePhase::Closing
                    && host.inner.resource_writes.lock().unwrap().sealed
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert!(
            !shutdown.is_finished(),
            "the actual physical kernel is still paused"
        );
        assert!(host.begin_open_context_resource_write().is_err());
        assert!(host.context_resource_generation().is_err());
        assert!(crate::runtime_db::RuntimeDbLock::try_lock(
            config.runtime_db_maintenance_lock_path()
        )
        .is_err());
        shutdown.abort();
        assert!(shutdown.await.unwrap_err().is_cancelled());
        assert!(crate::runtime_db::RuntimeDbLock::try_lock(
            config.runtime_db_maintenance_lock_path()
        )
        .is_err());
        resume_tx.send(())?;
        tokio::time::timeout(Duration::from_secs(5), finished_rx).await??;
        tokio::time::timeout(Duration::from_secs(10), host.shutdown()).await??;
        assert_eq!(host.inner.resource_writes.lock()?.active, 0);
        assert!(host.inner.resource_writes.lock()?.waiters.is_empty());
        assert!(host
            .inner
            .external_mutation_tasks
            .lock()
            .unwrap()
            .is_empty());
        // A new Host can observe only after the original physical worker and
        // its original Host-owned maintenance responsibility have completed.
        let released =
            crate::runtime_db::RuntimeDbLock::try_lock(config.runtime_db_maintenance_lock_path())?;
        drop(released);
        let next = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
        let next_runtime = next.default_runtime().await?;
        tokio::time::timeout(Duration::from_secs(5), next_runtime.wait_for_bootstrap()).await??;
        let resources = next_runtime.capture_current_context_resources()?;
        assert_eq!(
            resources.guidance().agent_source.as_ref().unwrap().content,
            "# completed global guidance"
        );
        next.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn host_writer_metadata_and_actual_waiter_release_together_without_scoped_pending(
    ) -> Result<()> {
        let (_home, host) = host()?;
        let before = host.inner.input_evidence_budget.report()?;
        let original_generation = host.context_resource_generation()?;
        let write = host.begin_open_context_resource_write()?;
        let held = host.inner.input_evidence_budget.report()?;
        assert_eq!(
            held.host_owned.metadata_estimate_bytes,
            before.host_owned.metadata_estimate_bytes + 1024
        );
        assert_eq!(held.pending, before.pending);
        assert!(host.bridge().context_resource_generation().is_err());
        assert!(host
            .with_context_resource_generation(original_generation, || Ok(()))
            .is_err());
        drop(write);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if host
                    .inner
                    .resource_writes
                    .lock()?
                    .waiters
                    .values()
                    .all(|entry| entry.worker.is_finished())
                {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        assert_eq!(
            host.inner
                .input_evidence_budget
                .report()?
                .host_owned
                .metadata_estimate_bytes,
            before.host_owned.metadata_estimate_bytes + 1024
        );
        host.inner.resource_writes.lock()?.reap_finished()?;
        let after = host.inner.input_evidence_budget.report()?;
        assert_eq!(after.host_owned, before.host_owned);
        assert_eq!(after.pending, before.pending);
        assert!(host.context_resource_generation()? > original_generation);
        assert!(host
            .with_context_resource_generation(original_generation, || Ok(()))
            .is_err());
        host.shutdown().await?;
        Ok(())
    }
    #[tokio::test]
    async fn exhausted_host_write_generation_never_reopens_an_old_read_fence() -> Result<()> {
        let (_home, host) = host()?;
        host.inner.resource_writes.lock()?.generation = u64::MAX;
        let original = host.context_resource_generation()?;
        let before = host.inner.input_evidence_budget.report()?;
        assert!(host.begin_open_context_resource_write().is_err());
        assert_eq!(host.inner.resource_writes.lock()?.active, 0);
        assert!(host
            .inner
            .external_mutation_tasks
            .lock()
            .unwrap()
            .is_empty());
        assert!(matches!(
            host.context_resource_generation()
                .unwrap_err()
                .downcast_ref::<ResourceObservationError>(),
            Some(ResourceObservationError::GenerationExhausted)
        ));
        assert!(host
            .with_context_resource_generation(original, || Ok(()))
            .is_err());
        let after = host.inner.input_evidence_budget.report()?;
        assert_eq!(after.host_owned, before.host_owned);
        assert_eq!(after.pending, before.pending);
        host.shutdown().await?;
        Ok(())
    }
    #[tokio::test]
    async fn no_yield_finished_physical_writes_keep_bounded_nodes_and_guards_until_actual_reap(
    ) -> Result<()> {
        let (_home, host) = host_with_record_limit(Some(32))?;
        let before = host.inner.input_evidence_budget.report()?;
        // No await/yield: neither completion waiter can have been polled on
        // this current-thread runtime, although both physical leases ended.
        drop(host.begin_open_context_resource_write()?);
        drop(host.begin_open_context_resource_write()?);
        assert_eq!(host.inner.resource_writes.lock()?.active, 0);
        assert_eq!(host.inner.resource_writes.lock()?.waiters.len(), 2);
        let rejected = host
            .begin_open_context_resource_write()
            .err()
            .context("retained nodes must reach the original record bound")?;
        assert!(matches!(
            rejected.downcast_ref::<ResourceObservationError>(),
            Some(ResourceObservationError::ResourceExhausted(
                "Host physical writers"
            ))
        ));
        assert_eq!(host.inner.resource_writes.lock()?.waiters.len(), 2);
        assert_eq!(
            host.inner
                .input_evidence_budget
                .report()?
                .host_owned
                .metadata_estimate_bytes,
            before.host_owned.metadata_estimate_bytes + 2048
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if host
                    .inner
                    .resource_writes
                    .lock()?
                    .waiters
                    .values()
                    .all(|entry| entry.worker.is_finished())
                {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        // Actual finished handles and BTree nodes are still owned, so their
        // guard cannot disappear merely because the future completed.
        assert_eq!(
            host.inner
                .input_evidence_budget
                .report()?
                .host_owned
                .metadata_estimate_bytes,
            before.host_owned.metadata_estimate_bytes + 2048
        );
        let retry = host.begin_open_context_resource_write()?;
        assert_eq!(host.inner.resource_writes.lock()?.waiters.len(), 1);
        assert_eq!(
            host.inner
                .input_evidence_budget
                .report()?
                .host_owned
                .metadata_estimate_bytes,
            before.host_owned.metadata_estimate_bytes + 1024
        );
        drop(retry);
        host.shutdown().await?;
        assert!(host.inner.resource_writes.lock()?.waiters.is_empty());
        let after = host.inner.input_evidence_budget.report()?;
        assert_eq!(after.host_owned, before.host_owned);
        assert_eq!(after.pending, before.pending);
        Ok(())
    }
    #[tokio::test]
    async fn failed_completion_waiter_keeps_physical_lease_and_shutdown_reports_actual_join_error(
    ) -> Result<()> {
        let (_home, host) = host()?;
        let lock_path = host.config().runtime_db_maintenance_lock_path();
        host.adopt_runtime_db_maintenance_lease(crate::runtime_db::RuntimeDbLock::try_lock(
            &lock_path,
        )?)
        .await?;
        let before = host.inner.input_evidence_budget.report()?;
        let write = host.begin_open_context_resource_write()?;
        host.inner
            .resource_writes
            .lock()?
            .waiters
            .values()
            .next()
            .unwrap()
            .worker
            .abort();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if host
                    .inner
                    .resource_writes
                    .lock()?
                    .waiters
                    .values()
                    .all(|entry| entry.worker.is_finished())
                {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        // A cancelled waiter cannot stand for the still-live physical kernel.
        assert_eq!(host.inner.resource_writes.lock()?.active, 1);
        assert_eq!(host.inner.resource_writes.lock()?.waiters.len(), 1);
        assert!(host.context_resource_generation().is_err());
        assert!(crate::runtime_db::RuntimeDbLock::try_lock(&lock_path).is_err());
        drop(write);
        let error = host.shutdown().await.unwrap_err();
        assert!(format!("{error:#}").contains("draining original Host resource completion waiters"));
        let _next_host_maintenance = crate::runtime_db::RuntimeDbLock::try_lock(&lock_path)?;
        assert_eq!(host.inner.resource_writes.lock()?.active, 0);
        assert!(host.inner.resource_writes.lock()?.waiters.is_empty());
        let after = host.inner.input_evidence_budget.report()?;
        assert_eq!(after.host_owned, before.host_owned);
        assert_eq!(after.pending, before.pending);
        Ok(())
    }
}
