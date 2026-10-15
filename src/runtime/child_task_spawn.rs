//! The accepted source owns one finite Host spawn even after its monitor aborts.
use super::*;
use crate::runtime::RuntimeHandle;
use crate::types::{AgentLifecycleDisposition, AgentModelResolution, AuthorityClass};

pub(crate) struct OriginalChildSpawn {
    task: TaskRecord,
    prompt: String,
    authority: AuthorityClass,
    worktree: bool,
    recovered: bool,
    lifecycle: AgentLifecycleDisposition,
    model: AgentModelResolution,
    at: DateTime<Utc>,
    registered: AtomicBool,
    registration_closed: AtomicBool,
    done: AtomicBool,
    outcome: Mutex<Option<std::result::Result<(), Arc<anyhow::Error>>>>,
    identity: Mutex<Option<(String, u64)>>,
    worktree_seed: Mutex<Option<Arc<crate::runtime::ManagedWorktreeSeed>>>,
    worktree_created: AtomicBool,
    changed: tokio::sync::Notify,
    continuation: tokio::sync::Mutex<()>,
    pub(crate) registration: tokio::sync::Mutex<()>,
    #[cfg(test)]
    identity_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    #[cfg(test)]
    registration_return_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    #[cfg(test)]
    enqueue_fault: Mutex<Option<TransitionFaultPoint>>,
    _material: Arc<BudgetViewGuard>,
}
impl OriginalChildTaskProducer {
    pub(in crate::runtime) fn retain_original_child_spawn(
        &self,
        task: TaskRecord,
        prompt: String,
        authority: AuthorityClass,
        worktree: bool,
        recovered: bool,
        lifecycle: AgentLifecycleDisposition,
        model: AgentModelResolution,
        at: DateTime<Utc>,
    ) -> Result<()> {
        let view = self.source_view()?;
        view.validate_generation(&task)?;
        ensure!(
            task.recovery == view.admitted_task().recovery,
            "original spawn changed recovery"
        );
        ensure!(
            accepted_child_spawn_command(
                task.recovery.as_ref(),
                &prompt,
                &authority,
                worktree,
                lifecycle
            ),
            "original spawn changed accepted command"
        );
        let material = self.completion_material()?;
        let bytes = serialized_bytes(&(
            &task, &prompt, &authority, worktree, recovered, lifecycle, &model, at,
        ))?;
        let model_nodes = model
            .resolved_parameters
            .as_ref()
            .map(|values| {
                values.values().try_fold(1usize, |n, value| {
                    n.checked_add(completion_value_nodes(Some(value))?)
                        .context("original model nodes overflow")
                })
            })
            .transpose()?
            .unwrap_or(0);
        let nodes = completion_value_nodes(task.detail.as_ref())?
            .checked_add(model_nodes)
            .context("original spawn node count overflow")?;
        material.observe_material(
            bytes.saturating_mul(8),
            (nodes as u64).saturating_add(64).saturating_mul(8 * 256),
        );
        let mut slot = self
            .spawn
            .lock()
            .map_err(|_| anyhow::anyhow!("original spawn plan poisoned"))?;
        ensure!(slot.is_none(), "original spawn plan already frozen");
        *slot = Some(Arc::new(OriginalChildSpawn {
            task,
            prompt,
            authority,
            worktree,
            recovered,
            lifecycle,
            model,
            at,
            registered: AtomicBool::new(false),
            registration_closed: AtomicBool::new(false),
            done: AtomicBool::new(false),
            outcome: Mutex::new(None),
            identity: Mutex::new(None),
            worktree_seed: Mutex::new(None),
            worktree_created: AtomicBool::new(false),
            changed: tokio::sync::Notify::new(),
            continuation: tokio::sync::Mutex::new(()),
            registration: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            identity_gate: Mutex::new(None),
            #[cfg(test)]
            registration_return_gate: Mutex::new(None),
            #[cfg(test)]
            enqueue_fault: Mutex::new(None),
            _material: material.guard(),
        }));
        Ok(())
    }
    pub(crate) fn original_child_spawn(&self) -> Option<Arc<OriginalChildSpawn>> {
        self.spawn
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    pub(crate) fn original_child_spawn_pending(&self) -> bool {
        self.spawn.lock().map(|slot| slot.is_some()).unwrap_or(true)
    }
    pub(crate) fn original_child_spawn_cancellation_error(&self, error: &anyhow::Error) -> bool {
        error
            .downcast_ref::<OriginalChildMonitorCancellationPending>()
            .is_some()
    }
    pub(crate) fn claim_original_child_spawn_registration(&self) -> Result<()> {
        let spawn = self
            .original_child_spawn()
            .context("original spawn plan absent")?;
        ensure!(
            !spawn.registration_closed.load(Ordering::Acquire),
            OriginalChildMonitorCancellationPending
        );
        ensure!(
            !self.original_lease()?.cancellation_requested(),
            OriginalChildMonitorCancellationPending
        );
        ensure!(
            !spawn.done.load(Ordering::Acquire),
            "original spawn already completed"
        );
        ensure!(
            !spawn.registered.swap(true, Ordering::AcqRel),
            "original spawn already registered"
        );
        Ok(())
    }
    pub(crate) fn validate_original_child_spawn_command(
        &self,
        task: &TaskRecord,
        prompt: &str,
        authority: &AuthorityClass,
        worktree: bool,
        model: &AgentModelResolution,
    ) -> Result<()> {
        let plan = self
            .original_child_spawn()
            .context("original spawn command has no plan")?;
        ensure!(
            plan.registered.load(Ordering::Acquire)
                && &plan.task == task
                && plan.prompt == prompt
                && &plan.authority == authority
                && plan.worktree == worktree
                && &plan.model == model,
            "Host child spawn differs from its exact frozen command"
        );
        self.source_view()?.validate_generation(task)
    }
    pub(crate) fn original_child_spawn_completed(&self) -> bool {
        self.original_child_spawn()
            .is_some_and(|spawn| spawn.done.load(Ordering::Acquire))
    }
    pub(crate) fn original_child_spawn_registered(&self) -> bool {
        self.original_child_spawn()
            .is_some_and(|spawn| spawn.registered.load(Ordering::Acquire))
    }
    pub(crate) fn record_original_child_spawn_completion(&self, result: Result<()>) -> Result<()> {
        let spawn = self
            .original_child_spawn()
            .context("original spawn completion has no plan")?;
        if let Err(error) = &result {
            struct Counter(usize);
            impl std::fmt::Write for Counter {
                fn write_str(&mut self, value: &str) -> std::fmt::Result {
                    self.0 = self.0.saturating_add(value.len());
                    Ok(())
                }
            }
            let mut count = Counter(0);
            std::fmt::write(&mut count, format_args!("{error:#}"))?;
            self.completion_material()?.observe_material(
                (count.0 as u64).saturating_mul(8),
                (error.chain().count() as u64)
                    .saturating_add(8)
                    .saturating_mul(8 * 256),
            );
        }
        let result = result.map_err(Arc::new);
        // Store the actual error before any Display/body conversion can fail.
        *spawn
            .outcome
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(result);
        spawn.done.store(true, Ordering::Release);
        spawn.changed.notify_waiters();
        Ok(())
    }
    pub(crate) fn original_spawn_identity_created(
        &self,
        identity: &crate::types::AgentIdentityRecord,
    ) -> Result<()> {
        ensure!(
            identity.incarnation > 0
                && identity.kind == crate::types::AgentKind::Child
                && identity.parent_agent_id.as_deref()
                    == Some(self.address.scope.agent_id.as_str())
                && identity.delegated_from_task_id.as_deref()
                    == Some(self.address.admitted.key.id.as_str()),
            "owned spawn identity crossed original association"
        );
        let spawn = self
            .original_child_spawn()
            .context("owned spawn identity lacks plan")?;
        self.completion_material()?.observe_material(
            (identity.agent_id.len() as u64)
                .saturating_add(256)
                .saturating_mul(8),
            1024,
        );
        let mut slot = spawn
            .identity
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        ensure!(slot.is_none(), "owned spawn identity already created");
        let mut original_scope = self
            .created_spawn_scope
            .lock()
            .map_err(|_| anyhow::anyhow!("original created child scope poisoned"))?;
        ensure!(
            original_scope.is_none(),
            "original created child scope already recorded"
        );
        *original_scope = Some(
            crate::runtime::resident_input_evidence::InputEvidenceScope {
                agent_id: identity.agent_id.clone(),
                identity_incarnation: identity.incarnation,
            },
        );
        *slot = Some((identity.agent_id.clone(), identity.incarnation));
        spawn.changed.notify_waiters();
        Ok(())
    }
    pub(crate) fn original_spawn_worktree_planned(
        &self,
        cwd: &std::path::Path,
        original_branch: &str,
        path: &std::path::Path,
        branch: &str,
    ) -> Result<()> {
        let spawn = self
            .original_child_spawn()
            .context("original worktree plan has no spawn")?;
        ensure!(
            spawn.registered.load(Ordering::Acquire) && spawn.worktree,
            "original spawn has no worktree command"
        );
        let material = self.completion_material()?;
        material.observe_material(
            serialized_bytes(&(cwd, original_branch, path, branch))
                .unwrap_or(u64::MAX)
                .saturating_mul(8),
            8 * 8 * 256,
        );
        let mut slot = spawn
            .worktree_seed
            .lock()
            .map_err(|_| anyhow::anyhow!("original worktree plan poisoned"))?;
        ensure!(slot.is_none(), "original worktree plan already issued");
        *slot = Some(Arc::new(crate::runtime::ManagedWorktreeSeed {
            original_cwd: cwd.to_path_buf(),
            original_branch: original_branch.to_owned(),
            worktree_path: path.to_path_buf(),
            worktree_branch: branch.to_owned(),
        }));
        Ok(())
    }
    /// The original registered Host worker alone continues this exact created
    /// child/physical seed. This validates a source; it grants no new FS right.
    pub(crate) fn validate_original_spawn_worktree_target(
        &self,
        child_id: &str,
        incarnation: u64,
        seed: &crate::runtime::ManagedWorktreeSeed,
    ) -> Result<()> {
        let spawn = self
            .original_child_spawn()
            .context("original worktree target has no spawn")?;
        ensure!(
            spawn.registered.load(Ordering::Acquire)
                && spawn.worktree
                && spawn.worktree_created.load(Ordering::Acquire)
                && !spawn.done.load(Ordering::Acquire),
            "original worktree target has no live actual creation"
        );
        let identity = spawn
            .identity
            .lock()
            .map_err(|_| anyhow::anyhow!("original child target poisoned"))?;
        ensure!(
            identity
                .as_ref()
                .is_some_and(|(id, inc)| id == child_id && *inc == incarnation),
            "original worktree target crossed child incarnation"
        );
        drop(identity);
        let planned = spawn
            .worktree_seed
            .lock()
            .map_err(|_| anyhow::anyhow!("original worktree seed poisoned"))?;
        ensure!(
            planned
                .as_ref()
                .is_some_and(|old| old.original_cwd == seed.original_cwd
                    && old.original_branch == seed.original_branch
                    && old.worktree_path == seed.worktree_path
                    && old.worktree_branch == seed.worktree_branch),
            "original worktree target changed its actual seed"
        );
        Ok(())
    }
    pub(crate) fn original_spawn_worktree_created(
        &self,
        path: &std::path::Path,
        branch: &str,
    ) -> Result<()> {
        let spawn = self
            .original_child_spawn()
            .context("actual worktree creation has no original spawn")?;
        {
            let slot = spawn
                .worktree_seed
                .lock()
                .map_err(|_| anyhow::anyhow!("original worktree plan poisoned"))?;
            let seed = slot
                .as_ref()
                .context("actual worktree creation has no frozen plan")?;
            ensure!(
                seed.worktree_path == path && seed.worktree_branch == branch,
                "actual worktree creation differs from original plan"
            );
        }
        spawn.worktree_created.store(true, Ordering::Release);
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn set_original_spawn_enqueue_fault_for_test(
        &self,
        fault: TransitionFaultPoint,
    ) -> Result<()> {
        let spawn = self
            .original_child_spawn()
            .context("owned spawn plan absent")?;
        *spawn.enqueue_fault.lock().unwrap() = Some(fault);
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn take_original_spawn_enqueue_fault_for_test(
        &self,
    ) -> Option<TransitionFaultPoint> {
        self.original_child_spawn()
            .and_then(|spawn| spawn.enqueue_fault.lock().unwrap().take())
    }
    #[cfg(test)]
    pub(crate) fn set_original_spawn_registration_return_gate_for_test(
        &self,
        gate: Arc<tokio::sync::Semaphore>,
    ) -> Result<()> {
        let spawn = self
            .original_child_spawn()
            .context("owned spawn plan absent")?;
        *spawn.registration_return_gate.lock().unwrap() = Some(gate);
        Ok(())
    }
    #[cfg(test)]
    pub(crate) async fn wait_original_spawn_registration_return_gate_for_test(&self) -> Result<()> {
        let Some(spawn) = self.original_child_spawn() else {
            return Ok(());
        };
        let gate = spawn.registration_return_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.acquire()
                .await
                .context("spawn registration return gate closed")?
                .forget();
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn set_original_spawn_identity_gate_for_test(
        &self,
        gate: Arc<tokio::sync::Semaphore>,
    ) -> Result<()> {
        let spawn = self
            .original_child_spawn()
            .context("spawn test plan absent")?;
        *spawn.identity_gate.lock().unwrap() = Some(gate);
        Ok(())
    }
    #[cfg(test)]
    pub(crate) async fn wait_original_spawn_identity_gate_for_test(&self) -> Result<()> {
        let spawn = self
            .original_child_spawn()
            .context("spawn test plan absent")?;
        let gate = spawn.identity_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            let _permit = gate.acquire_owned().await?;
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) async fn wait_original_spawn_identity_for_test(&self) -> Result<(String, u64)> {
        let spawn = self
            .original_child_spawn()
            .context("spawn test plan absent")?;
        loop {
            let notified = spawn.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(identity) = spawn.identity.lock().unwrap().clone() {
                return Ok(identity);
            }
            ensure!(
                !spawn.done.load(Ordering::Acquire),
                "spawn finished before identity creation"
            );
            notified.await;
        }
    }
}
impl RuntimeHandle {
    /// Only a registered Host worker calls this finite kernel. Its Task/source
    /// and model were frozen by the original admission; no new monitor is made.
    pub(crate) async fn perform_original_child_spawn_kernel(
        &self,
        source: Arc<OriginalChildTaskProducer>,
    ) -> Result<()> {
        let plan = source
            .original_child_spawn()
            .context("owned child spawn plan absent")?;
        ensure!(
            plan.registered.load(Ordering::Acquire),
            "finite child kernel has no registered Host owner"
        );
        let bridge = self
            .inner
            .host_bridge
            .as_ref()
            .context("owned child spawn requires Host")?;
        let existing = plan
            .task
            .detail
            .as_ref()
            .and_then(|detail| detail.get("child_agent_id"))
            .and_then(serde_json::Value::as_str);
        let (child, baseline, delivery, detail) = if let Some(id) = existing {
            if bridge.reusable_agent_exists(id).await? {
                let baseline = match plan
                    .task
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("child_turn_baseline"))
                    .and_then(serde_json::Value::as_u64)
                {
                    Some(value) => value,
                    None => bridge.child_turn_index(id).await?,
                };
                (
                    id.to_owned(),
                    baseline,
                    None,
                    plan.task
                        .detail
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({})),
                )
            } else {
                let spawned = bridge
                    .spawn_child_task_for_original_source(
                        self.clone(),
                        &source,
                        &plan.task,
                        plan.prompt.clone(),
                        plan.authority.clone(),
                        plan.worktree,
                        plan.model.clone(),
                    )
                    .await?;
                (
                    spawned.child_agent_id,
                    spawned.child_turn_baseline,
                    spawned.delivery_id,
                    spawned.task_detail,
                )
            }
        } else {
            let spawned = bridge
                .spawn_child_task_for_original_source(
                    self.clone(),
                    &source,
                    &plan.task,
                    plan.prompt.clone(),
                    plan.authority.clone(),
                    plan.worktree,
                    plan.model.clone(),
                )
                .await?;
            (
                spawned.child_agent_id,
                spawned.child_turn_baseline,
                spawned.delivery_id,
                spawned.task_detail,
            )
        };
        source.retain_monitor_stage(
            plan.task.clone(),
            plan.authority.clone(),
            plan.worktree,
            plan.recovered,
            plan.lifecycle,
            child,
            baseline,
            delivery,
            detail,
        )?;
        source
            .created_spawn_scope
            .lock()
            .map_err(|_| anyhow::anyhow!("original created child scope poisoned"))?
            .take();
        Ok(())
    }
    /// Called with no mutation reservation and no child-wait mutex held.
    pub(crate) fn continue_original_child_spawn(
        &self,
        source: Arc<OriginalChildTaskProducer>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let Some(spawn) = source.original_child_spawn() else {
                return Ok(());
            };
            let _worker = spawn.continuation.lock().await;
            if !spawn.registered.load(Ordering::Acquire) && !spawn.done.load(Ordering::Acquire) {
                let bridge = self
                    .inner
                    .host_bridge
                    .as_ref()
                    .context("owned child spawn requires Host")?;
                match bridge
                    .start_original_child_spawn(self.clone(), source.clone())
                    .await
                {
                    Ok(()) => {}
                    Err(error)
                        if error
                            .downcast_ref::<OriginalChildMonitorCancellationPending>()
                            .is_some() =>
                    {
                        return Err(error)
                    }
                    Err(error) => source.record_original_child_spawn_completion(Err(error))?,
                }
            }
            loop {
                let notified = spawn.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if spawn.done.load(Ordering::Acquire) {
                    break;
                }
                notified.await;
            }
            let outcome = spawn
                .outcome
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
                .context("owned spawn completed without outcome")?
                .clone();
            if let Err(error) = outcome {
                if source.observed_terminal().is_none() {
                    if let Some(failure) =
                        error.downcast_ref::<crate::host::ChildTaskSpawnFailure>()
                    {
                        source.retain_failed_spawn(failure)?;
                    }
                    // Display is counted without allocating an intermediate String.
                    struct Counter(usize);
                    impl std::fmt::Write for Counter {
                        fn write_str(&mut self, value: &str) -> std::fmt::Result {
                            self.0 = self.0.saturating_add(value.len());
                            Ok(())
                        }
                    }
                    let mut count = Counter(0);
                    std::fmt::write(&mut count, format_args!("{error:#}"))?;
                    source
                        .completion_material()?
                        .observe_material((count.0 as u64).saturating_mul(8), 1024);
                    let created_scope = source.original_created_spawn_scope()?;
                    let child = error
                        .downcast_ref::<crate::host::ChildTaskSpawnFailure>()
                        .map(|failure| failure.child_agent_id().to_owned());
                    if let Some(created) = created_scope.as_ref() {
                        ensure!(
                            child.as_deref() == Some(created.agent_id.as_str()),
                            "owned spawn failure lost its actual created child locator"
                        );
                    }
                    let mut detail = spawn
                        .task
                        .detail
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    if let Some(child) = child.as_ref() {
                        detail["child_agent_id"] = serde_json::json!(child);
                    }
                    if let Some(created) = created_scope.as_ref() {
                        detail["target_identity_incarnation"] =
                            serde_json::json!(created.identity_incarnation);
                    }
                    let artifact = spawn
                        .worktree_seed
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone();
                    if let Some(seed) = artifact.as_ref() {
                        detail["worktree"] = serde_json::json!({"worktree_path":seed.worktree_path,"worktree_branch":seed.worktree_branch,"cleanup_status":"retained","cleanup_reason":"original_child_exit_unproven","retained_for_review":true});
                        detail["worktree_setup_confirmed"] =
                            serde_json::json!(spawn.worktree_created.load(Ordering::Acquire));
                    }
                    let mut message = MessageEnvelope::new(
                        spawn.task.agent_id.clone(),
                        crate::types::MessageKind::TaskResult,
                        crate::types::MessageOrigin::Task {
                            task_id: spawn.task.id.clone(),
                        },
                        AuthorityClass::RuntimeInstruction,
                        crate::types::Priority::Next,
                        crate::types::MessageBody::Text {
                            text: format!("child agent failed: {error:#}"),
                        },
                    )
                    .with_admission(
                        crate::types::MessageDeliverySurface::TaskRejoin,
                        crate::types::AdmissionContext::RuntimeOwned,
                    );
                    message.id = format!("message:child-spawn-failure:{}", source.address.lease_id);
                    message.turn_id = Some(format!(
                        "turn:child-spawn-failure:{}",
                        source.address.lease_id
                    ));
                    message.created_at = spawn.at;
                    message.task_id = Some(spawn.task.id.clone());
                    message.work_item_id = spawn.task.work_item_id.clone();
                    message.metadata = Some(
                        serde_json::json!({"task_id":spawn.task.id,"task_kind":spawn.task.kind,"task_status":"failed","task_summary":spawn.task.summary,"task_detail":detail,"task_recovery":spawn.task.recovery,"work_item_id":spawn.task.work_item_id}),
                    );
                    let task = crate::runtime::tasks::task_with_result_message(
                        &spawn.task,
                        TaskStatus::Failed,
                        Some(detail),
                        &message,
                    );
                    let deletion=child.as_ref().filter(|_|spawn.lifecycle==AgentLifecycleDisposition::DeleteOnTerminal).map(|id|AgentDeletionRequest{agent_id:id.clone(),admission:crate::runtime_db::repositories::AgentDeletionAdmission::TerminalEphemeralChild{parent_agent_id:spawn.task.agent_id.clone(),task_id:spawn.task.id.clone()},requested_by:"parent_task_spawn_failed".into(),cascade_private_children:false});
                    // Enqueue can have committed its Queue ACK before returning
                    // a publication error. Keep the exact artifact in the Task
                    // and Result, but do not acquire cleanup authority without
                    // original child producer exit/join evidence.
                    source.observe_terminal_result(task, message, child, deletion, false)?;
                }
            }
            source
                .spawn
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "child_task_spawn_tests.rs"]
mod tests;

/// No body/Runtime/strong lease back-reference. Only the same registration lock
/// may seal this original plan against ever registering a Host kernel.
pub(crate) struct OriginalChildSpawnNoStart {
    source: std::sync::Weak<OriginalChildTaskProducer>,
    spawn: Arc<OriginalChildSpawn>,
}
impl OriginalChildSpawnNoStart {
    pub(crate) fn matches_original(&self, source: &OriginalChildTaskProducer) -> bool {
        self.source
            .upgrade()
            .is_some_and(|original| std::ptr::eq(original.as_ref(), source))
            && source
                .original_child_spawn()
                .is_some_and(|spawn| Arc::ptr_eq(&spawn, &self.spawn))
            && self.spawn.registration_closed.load(Ordering::Acquire)
            && !self.spawn.registered.load(Ordering::Acquire)
            && !self.spawn.done.load(Ordering::Acquire)
    }
    pub(crate) fn validate_durable_task(
        &self,
        source: &OriginalChildTaskProducer,
        task: &TaskRecord,
    ) -> Result<()> {
        ensure!(
            self.matches_original(source),
            "unstarted child proof crossed original spawn"
        );
        source.source_view()?.validate_generation(task)?;
        ensure!(
            task.recovery == self.spawn.task.recovery
                && task.work_item_id == self.spawn.task.work_item_id,
            "unstarted child durable recovery changed accepted command"
        );
        ensure!(
            accepted_child_spawn_command(
                task.recovery.as_ref(),
                &self.spawn.prompt,
                &self.spawn.authority,
                self.spawn.worktree,
                self.spawn.lifecycle
            ),
            "unstarted child has no complete original Task recovery"
        );
        for field in [
            "target_agent_id",
            "child_agent_id",
            "target_identity_incarnation",
            "child_turn_baseline",
            "delivery_id",
            "worktree",
            "model_resolution",
            "workspace_mode",
        ] {
            ensure!(
                task.detail.as_ref().and_then(|v| v.get(field))
                    == self.spawn.task.detail.as_ref().and_then(|v| v.get(field)),
                "unstarted child changed durable physical recovery metadata"
            );
        }
        Ok(())
    }
}
impl OriginalChildTaskProducer {
    pub(crate) async fn seal_original_child_spawn_without_start(
        self: &Arc<Self>,
    ) -> Result<Option<OriginalChildSpawnNoStart>> {
        let Some(spawn) = self.original_child_spawn() else {
            return Ok(None);
        };
        let _registration = spawn.registration.lock().await;
        let lease = self.original_lease()?;
        ensure!(
            lease
                .original_child()?
                .as_ref()
                .is_some_and(|installed| Arc::ptr_eq(installed, self)),
            "unstarted child lost installed original lease"
        );
        if !(lease.cancellation_requested() && lease.is_finished()) {
            return Ok(None);
        }
        if spawn.registered.load(Ordering::Acquire)
            || spawn.done.load(Ordering::Acquire)
            || spawn
                .identity
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
            || spawn
                .worktree_seed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
            || self.monitor_stage().is_some()
            || self.observed_terminal().is_some()
            || self.frozen_terminal()?.is_some()
        {
            return Ok(None);
        }
        // A concurrent Host claim owns this same mutex. If it won, registered
        // is already true; after sealing it can never pass the claim predicate.
        spawn.registration_closed.store(true, Ordering::Release);
        Ok(Some(OriginalChildSpawnNoStart {
            source: Arc::downgrade(self),
            spawn: spawn.clone(),
        }))
    }
    pub(crate) fn acknowledge_unstarted_child_handoff(
        &self,
        token: &OriginalChildSpawnNoStart,
        proof: &crate::runtime_db::child_task_terminal::OriginalChildSpawnNoStartHandoff,
    ) -> Result<()> {
        ensure!(
            token.matches_original(self) && proof.matches_original(self),
            "unstarted child handoff crossed original source"
        );
        ensure!(
            self.original_lease()?.cancellation_requested() && self.original_lease()?.is_finished(),
            "unstarted child lease has not completed cancellation"
        );
        self.spawn
            .lock()
            .map_err(|_| anyhow::anyhow!("original spawn plan poisoned"))?
            .take();
        self.source
            .lock()
            .map_err(|_| anyhow::anyhow!("original child source poisoned"))?
            .take();
        self.completion
            .lock()
            .map_err(|_| anyhow::anyhow!("original completion poisoned"))?
            .take();
        self.finished.store(true, Ordering::Release);
        Ok(())
    }
}
impl RuntimeHandle {
    pub(crate) async fn try_handoff_original_child_spawn_without_start(
        &self,
        source: Arc<OriginalChildTaskProducer>,
    ) -> Result<bool> {
        let Some(token) = source.seal_original_child_spawn_without_start().await? else {
            return Ok(false);
        };
        let limits = self.inner.input_evidence_budget.limits();
        let (records, bytes) =
            crate::runtime_db::task_snapshot::TaskDomainLoadRequest::bounded_material_limits(
                usize::try_from(limits.record_limit)?,
                usize::try_from(
                    limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("unstarted child limit overflow")?,
                )?,
            );
        let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
            query: Default::default(),
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            record_limit: records,
            byte_limit: bytes,
        };
        if let Some(bridge) = &self.inner.host_bridge {
            return bridge
                .handoff_original_child_spawn_without_start(&source, &token, &request)
                .await;
        }
        let held = self.inner.mutation_owner.reserve().await?;
        ensure!(
            held.agent_id() == source.address().scope.agent_id
                && held.identity_incarnation() == source.address().scope.identity_incarnation,
            "unstarted child local owner differs"
        );
        let proof=self.inner.runtime_db.transaction(|tx|crate::runtime_db::child_task_terminal::observe_original_child_spawn_no_start_handoff_tx(tx,&source,&token,&request,&self.inner.input_evidence_budget))?;
        let Some(proof) = proof else { return Ok(false) };
        source.acknowledge_unstarted_child_handoff(&token, &proof)?;
        drop(held);
        Ok(true)
    }
}

/// Borrow the accepted legacy contract; never normalize its durable recovery bytes.
fn accepted_child_spawn_command(
    recovery: Option<&crate::types::TaskRecoverySpec>,
    prompt: &str,
    authority: &AuthorityClass,
    worktree: bool,
    lifecycle: AgentLifecycleDisposition,
) -> bool {
    use crate::types::TaskRecoverySpec;
    let Some(recovery) = recovery else {
        return false;
    };
    let (expected_prompt, expected_authority, expected_worktree, expected_lifecycle) =
        match recovery {
            TaskRecoverySpec::ChildAgentTask {
                prompt,
                authority_class,
                workspace_mode,
                lifecycle_disposition,
                ..
            } => (
                prompt,
                authority_class,
                workspace_mode.is_worktree(),
                *lifecycle_disposition,
            ),
            TaskRecoverySpec::SubagentTask {
                prompt,
                authority_class,
                ..
            } => (
                prompt,
                authority_class,
                false,
                AgentLifecycleDisposition::DeleteOnTerminal,
            ),
            TaskRecoverySpec::WorktreeSubagentTask {
                prompt,
                authority_class,
                ..
            } => (
                prompt,
                authority_class,
                true,
                AgentLifecycleDisposition::DeleteOnTerminal,
            ),
            _ => return false,
        };
    expected_prompt == prompt
        && expected_authority == authority
        && expected_worktree == worktree
        && expected_lifecycle == lifecycle
}

#[cfg(test)]
mod accepted_legacy_command_tests {
    use super::*;
    use crate::types::{ChildAgentWorkspaceMode, TaskRecoverySpec};
    #[test]
    fn legacy_contract_is_borrowed_and_does_not_grant_invocation_spawn() {
        let invocation = TaskRecoverySpec::AgentInvocation {
            summary: "same".into(),
            prompt: "same".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            target_agent_id: None,
            created_new_subagent: true,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::DeleteOnTerminal,
        };
        assert!(!accepted_child_spawn_command(
            Some(&invocation),
            "same",
            &AuthorityClass::OperatorInstruction,
            false,
            AgentLifecycleDisposition::DeleteOnTerminal
        ));
        assert!(!accepted_child_spawn_command(
            None,
            "same",
            &AuthorityClass::OperatorInstruction,
            false,
            AgentLifecycleDisposition::DeleteOnTerminal
        ));
    }
}

#[cfg(test)]
#[path = "physical_worktree_source_tests.rs"]
mod physical_worktree_source_tests;
