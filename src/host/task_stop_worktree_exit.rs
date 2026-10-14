//! Combines actual original child retirement with registered producer exit.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
    },
    RetiredProducerExit,
};
use crate::runtime_db::{
    input_mutation_receipts::TaskStopReceiptBinding,
    task_stop::{TaskStopChildTarget, TaskStopWorktreeTarget},
};

pub(crate) struct TaskStopWorktreeProducerExit {
    original: TaskStopReceiptBinding,
    original_nonce: String,
    child: TaskStopChildTarget,
    worktree: TaskStopWorktreeTarget,
    expected: BTreeMap<uuid::Uuid, u64>,
    instances: BTreeMap<uuid::Uuid, RetiredProducerExit>,
    _fleet_material: Arc<BudgetViewGuard>,
    _material: Arc<BudgetViewGuard>,
}
impl TaskStopWorktreeProducerExit {
    /// Only the sealed actual Host collector can supply the complete fleet.
    /// There is no constructor accepting a caller-selected Vec of exit caps.
    pub(super) fn from_complete_original_child_drain(
        retired: &TaskStopChildRetirement,
        complete: CompleteOriginalChildProducerExit,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<Option<Self>> {
        let record = &retired.original;
        let Some(child) = record.intent.child.as_ref() else {
            return Ok(None);
        };
        let Some(worktree) = record.intent.worktree.as_ref() else {
            return Ok(None);
        };
        if complete.scope != child.scope || !complete.is_exact_complete() {
            return Ok(None);
        }
        let bytes = serialized_bytes(&(
            &record.intent.task.scope,
            record.effect_seq,
            record.prepared.command_nonce(),
            &record.intent_digest,
            &record.generation_digest,
            &record.ack_task_digest,
            child,
            worktree,
        ))?
        .checked_add(
            u64::try_from(complete.instances.len())?
                .checked_mul(64)
                .context("worktree exit metadata overflow")?,
        )
        .and_then(|n| n.checked_mul(4))
        .context("worktree exit metadata overflow")?;
        let limits = ledger.limits();
        anyhow::ensure!(
            complete
                .instances
                .len()
                .checked_add(1)
                .and_then(|n| n.checked_mul(4))
                .is_some_and(|n| u64::try_from(n).is_ok_and(|n| n <= limits.record_limit))
                && bytes
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("worktree exit limit overflow")?
                        .min(64 * 1024 * 1024),
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "worktree producer exit metadata"
            )
        );
        let material = ledger.register_owned_bytes(
            &record.intent.task.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        Ok(Some(Self {
            original: record.receipt_binding(),
            original_nonce: record.prepared.command_nonce().to_owned(),
            child: child.clone(),
            worktree: worktree.clone(),
            expected: complete.expected,
            instances: complete.instances,
            _fleet_material: complete._material,
            _material: material,
        }))
    }
    pub(crate) fn matches(&self, record: &crate::runtime_db::task_stop::TaskStopRecord) -> bool {
        self.original.scope == record.intent.task.scope
            && self.original.effect_seq == record.effect_seq
            && self.original.intent_digest == record.intent_digest
            && self.original.generation_digest == record.generation_digest
            && self.original.task_ack_digest == record.ack_task_digest
            && self.original_nonce == record.prepared.command_nonce()
            && record.intent.child.as_ref() == Some(&self.child)
            && record.intent.worktree.as_ref() == Some(&self.worktree)
            && !self.expected.is_empty()
            && self.instances.len() == self.expected.len()
            && self.instances.iter().all(|(id, instance)| {
                *id == instance.registry_id()
                    && self.expected.get(id) == Some(&instance.registration_revision())
                    && instance.scope() == &self.child.scope
                    && instance.registered_groups_still_empty()
            })
    }
}

/// Minted only by the actual closed-fleet collection below. No Runtime/body
/// references escape the completed drain, and no raw subset factory exists.
pub(super) struct CompleteOriginalChildProducerExit {
    scope: crate::runtime::resident_input_evidence::InputEvidenceScope,
    expected: BTreeMap<uuid::Uuid, u64>,
    instances: BTreeMap<uuid::Uuid, RetiredProducerExit>,
    _material: Arc<BudgetViewGuard>,
}
impl CompleteOriginalChildProducerExit {
    fn is_exact_complete(&self) -> bool {
        !self.expected.is_empty()
            && self.instances.len() == self.expected.len()
            && self.instances.iter().all(|(id, exit)| {
                *id == exit.registry_id()
                    && exit.owner_epoch() > 0
                    && exit.scope() == &self.scope
                    && self.expected.get(id) == Some(&exit.registration_revision())
                    && exit.registered_groups_still_empty()
            })
    }
}
struct OriginalChildProcessInstance {
    runtime: RuntimeHandle,
    generation: Option<u64>,
    temporary: bool,
    monitor: Option<(tokio::task::AbortHandle, watch::Receiver<AgentRuntimePhase>)>,
    main_task: Option<JoinHandle<()>>,
}
impl HostRuntimeRegistry {
    pub(super) fn validate_retiring_process_instance_budget(
        &self,
        agent_id: &str,
        incarnation: u64,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<()> {
        let matching = |runtime: &RuntimeHandle| {
            runtime.instance_agent_id() == agent_id
                && runtime.instance_incarnation() == incarnation
                && !runtime.original_registered_groups_quiescent()
        };
        let count = self
            .unloading_mutations
            .values()
            .filter(|runtime| matching(runtime))
            .count()
            .checked_add(
                self.agents
                    .get(agent_id)
                    .filter(|entry| matching(&entry.runtime))
                    .map_or(0, |_| 1),
            )
            .and_then(|n| n.checked_add(1))
            .context("retiring instance count overflow")?;
        let count = u64::try_from(count)?;
        // Bound only retained instance metadata/address entries, not their native
        // allocations or RSS. Original Runtime retention remains a D7 audit item.
        let bytes = serialized_bytes(&(agent_id, incarnation))?
            .checked_add(1024)
            .and_then(|n| n.checked_mul(count))
            .and_then(|n| n.checked_mul(4))
            .context("retiring instance metadata overflow")?;
        let limits = ledger.limits();
        anyhow::ensure!(
            count <= limits.record_limit
                && bytes
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("retiring instance byte limit overflow")?
                        .min(64 * 1024 * 1024),
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "retiring original process instances"
            )
        );
        Ok(())
    }
    // Every release path must preserve an instance whose original registered
    // OS groups remain live/unknown. Reuse the existing unloading inventory.
    pub(super) fn retain_unproved_original_processes(&mut self, entry: &AgentEntry) {
        entry.runtime.close_execution_admission();
        if !entry.runtime.original_registered_groups_quiescent() {
            self.unloading_mutations
                .insert(entry.generation, entry.runtime.clone());
            self.unloading_exits.insert(
                entry.generation,
                (entry.task.abort_handle(), entry.phase.clone()),
            );
        }
    }
}
impl RuntimeHost {
    pub(super) fn drain_original_task_stop_child_producers<'a>(
        &'a self,
        scope: &'a crate::runtime::resident_input_evidence::InputEvidenceScope,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<CompleteOriginalChildProducerExit>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let matches = |runtime: &RuntimeHandle| {
                runtime.instance_agent_id() == scope.agent_id
                    && runtime.instance_incarnation() == scope.identity_incarnation
            };
            let (expected, instances, material) = {
                let mut registry = self.inner.runtimes.write().await;
                let count = registry
                    .agents
                    .get(&scope.agent_id)
                    .filter(|entry| matches(&entry.runtime))
                    .map_or(0, |_| 1usize)
                    .checked_add(
                        registry
                            .unloading_mutations
                            .values()
                            .filter(|runtime| matches(runtime))
                            .count(),
                    )
                    .and_then(|n| {
                        n.checked_add(
                            registry
                                .temporary_runtimes
                                .values()
                                .filter(|entry| matches(&entry.runtime))
                                .count(),
                        )
                    })
                    .context("TaskStop original fleet count overflow")?;
                if count == 0 {
                    return Ok(None);
                }
                // Only bounded scalar/borrowed metadata before the actual guard.
                let bytes = serialized_bytes(scope)?
                    .checked_add(512)
                    .and_then(|n| n.checked_mul(u64::try_from(count).ok()?.checked_add(1)?))
                    .and_then(|n| n.checked_mul(4))
                    .context("TaskStop fleet metadata overflow")?;
                let limits = self.inner.input_evidence_budget.limits();
                anyhow::ensure!(
                    u64::try_from(count)?
                        .checked_mul(4)
                        .is_some_and(|n| n <= limits.record_limit)
                        && bytes
                            <= limits
                                .history_per_agent_bytes
                                .checked_add(limits.pending_per_agent_bytes)
                                .context("TaskStop fleet budget overflow")?
                                .min(64 * 1024 * 1024),
                    crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                        "TaskStop original fleet"
                    )
                );
                let material = self.inner.input_evidence_budget.register_owned_bytes(
                    scope,
                    InputMemoryCategory::Candidate,
                    OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: bytes,
                    },
                )?;
                let current = if registry
                    .agents
                    .get(&scope.agent_id)
                    .is_some_and(|entry| matches(&entry.runtime))
                {
                    registry.agents.remove(&scope.agent_id)
                } else {
                    None
                };
                let mut main_task = None;
                if let Some(entry) = current {
                    entry.runtime.close_execution_admission();
                    registry
                        .unloading_mutations
                        .insert(entry.generation, entry.runtime.clone());
                    registry.unloading_exits.insert(
                        entry.generation,
                        (entry.task.abort_handle(), entry.phase.clone()),
                    );
                    main_task = Some((entry.runtime.original_process_registry_id(), entry.task));
                }
                let mut expected = BTreeMap::new();
                let mut instances = BTreeMap::new();
                for (generation, runtime) in registry
                    .unloading_mutations
                    .iter()
                    .filter(|(_, runtime)| matches(runtime))
                {
                    runtime.close_execution_admission();
                    let id = runtime.original_process_registry_id();
                    expected
                        .entry(id)
                        .or_insert_with(|| runtime.original_process_registration_revision());
                    instances
                        .entry(id)
                        .or_insert_with(|| OriginalChildProcessInstance {
                            runtime: runtime.clone(),
                            generation: Some(*generation),
                            temporary: false,
                            monitor: registry.unloading_exits.get(generation).cloned(),
                            main_task: None,
                        });
                }
                for entry in registry
                    .temporary_runtimes
                    .values()
                    .filter(|entry| matches(&entry.runtime))
                {
                    entry.runtime.close_execution_admission();
                    let id = entry.runtime.original_process_registry_id();
                    expected
                        .entry(id)
                        .or_insert_with(|| entry.runtime.original_process_registration_revision());
                    instances
                        .entry(id)
                        .or_insert_with(|| OriginalChildProcessInstance {
                            runtime: entry.runtime.clone(),
                            generation: None,
                            temporary: true,
                            monitor: Some((entry.abort.clone(), entry.phase.clone())),
                            main_task: None,
                        });
                }
                if let Some((id, task)) = main_task {
                    instances
                        .get_mut(&id)
                        .context("TaskStop original main instance disappeared")?
                        .main_task = Some(task);
                }
                (expected, instances, material)
            };
            let mut all_proved = true;
            let mut observations = BTreeMap::new();
            for (id, mut instance) in instances {
                instance
                    .runtime
                    .retire_instance_locally("runtime_unloaded")
                    .await;
                let monitor_known = instance.monitor.is_some();
                if let Some((abort, mut phase)) = instance.monitor.take() {
                    abort.abort();
                    if let Some(task) = instance.main_task.take() {
                        let _ = task.await;
                    } else {
                        while *phase.borrow_and_update() != AgentRuntimePhase::Terminated {
                            if phase.changed().await.is_err() {
                                break;
                            }
                        }
                    }
                }
                let proof = instance
                    .runtime
                    .join_retiring_resources_with_process_exit()
                    .await?;
                let valid = monitor_known
                    && proof.as_ref().is_some_and(|proof| {
                        proof.scope() == scope
                            && proof.registry_id() == id
                            && expected.get(&id) == Some(&proof.registration_revision())
                            && proof.registered_groups_still_empty()
                    });
                if valid {
                    observations.insert(id, proof.expect("validated original instance"));
                    let mut registry = self.inner.runtimes.write().await;
                    if let Some(generation) = instance.generation {
                        if registry
                            .unloading_mutations
                            .get(&generation)
                            .is_some_and(|runtime| runtime.original_process_registry_id() == id)
                        {
                            registry.unloading_mutations.remove(&generation);
                            registry.unloading_exits.remove(&generation);
                        }
                    }
                    if instance.temporary
                        && registry
                            .temporary_runtimes
                            .get(&scope.agent_id)
                            .is_some_and(|entry| entry.runtime.original_process_registry_id() == id)
                    {
                        registry.temporary_runtimes.remove(&scope.agent_id);
                    }
                } else {
                    all_proved = false;
                }
            }
            // Original child identity is already tombstoned. A concurrent preexisting
            // construction nevertheless cannot add an unobserved instance to this set.
            let registry = self.inner.runtimes.read().await;
            let unexpected = registry
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
                .filter(|runtime| matches(runtime))
                .any(|runtime| {
                    !expected.contains_key(&runtime.original_process_registry_id())
                        || expected.get(&runtime.original_process_registry_id())
                            != Some(&runtime.original_process_registration_revision())
                });
            drop(registry);
            self.notify_runtime_recovery(&scope.agent_id).await;
            if !all_proved || unexpected {
                return Ok(None);
            }
            let complete = CompleteOriginalChildProducerExit {
                scope: scope.clone(),
                expected,
                instances: observations,
                _material: material,
            };
            Ok(complete.is_exact_complete().then_some(complete))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::resident_input_evidence::InputEvidenceScope;
    fn host() -> (tempfile::TempDir, RuntimeHost) {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let mut config =
            crate::config::AppConfig::load_with_home(Some(home.path().into())).unwrap();
        config.user_home_dir = Some(home.path().into());
        let host = RuntimeHost::new_with_provider(
            config,
            Arc::new(crate::provider::StubProvider::new("done")),
        )
        .unwrap();
        host.ensure_default_agent_identity().unwrap();
        (home, host)
    }
    async fn loaded(host: &RuntimeHost, id: &str) -> Result<(RuntimeHandle, InputEvidenceScope)> {
        host.create_named_agent(id, None).await?;
        let runtime = host.get_public_agent(id).await?;
        runtime.wait_for_bootstrap().await?;
        let scope = InputEvidenceScope {
            agent_id: id.into(),
            identity_incarnation: runtime.instance_incarnation(),
        };
        Ok((runtime, scope))
    }
    #[tokio::test]
    async fn actual_host_complete_fleet_rejects_subset_and_changed_registration_revision(
    ) -> Result<()> {
        let (_home, host) = host();
        let (runtime, scope) = loaded(&host, "fleet-exact").await?;
        let id = runtime.original_process_registry_id();
        // Existing agents + unloading inventories may temporarily name one
        // instance twice. They must retain one original UUID, not two epochs.
        {
            let mut registry = host.inner.runtimes.write().await;
            let entry = registry.agents.get(&scope.agent_id).unwrap();
            let (generation, runtime, abort, phase) = (
                entry.generation,
                entry.runtime.clone(),
                entry.task.abort_handle(),
                entry.phase.clone(),
            );
            registry.unloading_mutations.insert(generation, runtime);
            registry.unloading_exits.insert(generation, (abort, phase));
        }
        let mut complete = host
            .drain_original_task_stop_child_producers(&scope)
            .await?
            .expect("actual closed fleet");
        assert_eq!(complete.expected.len(), 1);
        assert_eq!(complete.instances.len(), 1);
        assert!(complete.is_exact_complete());
        let original = complete.instances.remove(&id).unwrap();
        assert!(
            !complete.is_exact_complete(),
            "a supplied partial cap set cannot certify the frozen complete inventory"
        );
        complete.instances.insert(id, original);
        *complete.expected.get_mut(&id).unwrap() += 1;
        assert!(
            !complete.is_exact_complete(),
            "same UUID with a changed producer frontier is not the original set"
        );
        let material = Arc::downgrade(&complete._material);
        assert!(material.upgrade().is_some());
        drop(complete);
        assert!(
            material.upgrade().is_none(),
            "original collector allocation guard is released"
        );
        host.shutdown().await?;
        Ok(())
    }
    #[tokio::test]
    async fn original_inflight_instance_is_retained_and_only_exact_retry_can_complete() -> Result<()>
    {
        let (_home, host) = host();
        let (runtime, scope) = loaded(&host, "fleet-inflight").await?;
        let id = runtime.original_process_registry_id();
        let original_admission = runtime.execution_admission_lease()?;
        assert!(host
            .drain_original_task_stop_child_producers(&scope)
            .await?
            .is_none());
        assert!(host
            .inner
            .runtimes
            .read()
            .await
            .unloading_mutations
            .values()
            .any(|r| r.original_process_registry_id() == id));
        {
            let mut limits = crate::config::StateMemoryLimits::default();
            limits.record_limit = 1;
            let limited =
                crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(limits)?;
            let registry = host.inner.runtimes.read().await;
            let error = registry
                .validate_retiring_process_instance_budget(
                    &scope.agent_id,
                    scope.identity_incarnation,
                    &limited,
                )
                .unwrap_err();
            assert!(error
                .downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>()
                .is_some_and(|error| matches!(
                    error,
                    crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                        "retiring original process instances"
                    )
                )));
            assert!(
                registry
                    .unloading_mutations
                    .values()
                    .any(|r| r.original_process_registry_id() == id),
                "a structural gate cannot drop old responsibility"
            );
        }
        drop(original_admission);
        let complete = host
            .drain_original_task_stop_child_producers(&scope)
            .await?
            .expect("same original instance is now quiescent");
        assert!(complete.is_exact_complete());
        assert_eq!(complete.expected.len(), 1);
        assert!(complete.expected.contains_key(&id));
        assert!(!host
            .inner
            .runtimes
            .read()
            .await
            .unloading_mutations
            .values()
            .any(|r| r.original_process_registry_id() == id));
        drop(complete);
        host.shutdown().await?;
        Ok(())
    }
    #[tokio::test]
    async fn missing_or_foreign_incarnation_fleet_never_proves_an_empty_child() -> Result<()> {
        let (_home, host) = host();
        let (runtime, scope) = loaded(&host, "fleet-current").await?;
        let foreign = InputEvidenceScope {
            agent_id: scope.agent_id.clone(),
            identity_incarnation: scope.identity_incarnation + 1,
        };
        assert!(host
            .drain_original_task_stop_child_producers(&foreign)
            .await?
            .is_none());
        assert!(host
            .drain_original_task_stop_child_producers(&InputEvidenceScope {
                agent_id: "no-original-runtime".into(),
                identity_incarnation: 1
            })
            .await?
            .is_none());
        assert!(runtime.accepts_execution_admission());
        assert!(host
            .inner
            .runtimes
            .read()
            .await
            .agents
            .contains_key(&scope.agent_id));
        host.shutdown().await?;
        Ok(())
    }
    #[tokio::test]
    async fn ordinary_unload_preserves_unknown_process_instance_in_existing_inventory() -> Result<()>
    {
        let (_home, host) = host();
        let (runtime, scope) = loaded(&host, "fleet-release").await?;
        let id = runtime.original_process_registry_id();
        let admission = runtime.execution_admission_lease()?;
        host.drain_retired_runtime(&scope.agent_id, scope.identity_incarnation)
            .await?;
        assert!(host
            .inner
            .runtimes
            .read()
            .await
            .unloading_mutations
            .values()
            .any(|r| r.original_process_registry_id() == id));
        drop(admission);
        host.drain_retired_runtime(&scope.agent_id, scope.identity_incarnation)
            .await?;
        assert!(!host
            .inner
            .runtimes
            .read()
            .await
            .unloading_mutations
            .values()
            .any(|r| r.original_process_registry_id() == id));
        host.shutdown().await?;
        Ok(())
    }
}
