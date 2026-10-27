//! One original accepted monitor's recovery binding; no new monitor or target is minted.
use super::*;
use crate::runtime::RuntimeHandle;
use crate::runtime_db::task_snapshot::TaskDomainLoadRequest;

#[derive(Default)]
pub(super) struct MonitorBindingState {
    pub(super) worker: tokio::sync::Mutex<()>,
    pub(super) prepared: tokio::sync::Mutex<Option<Arc<PreparedInputTransition>>>,
    clock: Mutex<Option<DateTime<Utc>>>,
    confirmed: AtomicBool,
    #[cfg(test)]
    lost_ack: AtomicBool,
}
impl OriginalChildTaskProducer {
    #[cfg(test)]
    pub(crate) fn lose_binding_ack_for_test(&self) {
        self.binding.lost_ack.store(true, Ordering::Release);
    }
    #[cfg(test)]
    pub(crate) fn take_binding_lost_ack_for_test(&self) -> bool {
        self.binding.lost_ack.swap(false, Ordering::AcqRel)
    }
    pub(crate) fn monitor_binding_confirmed(&self) -> bool {
        self.binding.confirmed.load(Ordering::Acquire)
    }
    pub(crate) fn monitor_stage_present(&self) -> bool {
        self.monitor_stage().is_some()
    }
    pub(crate) fn original_monitor_binding_clock(
        &self,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>> {
        let mut clock = self
            .binding
            .clock
            .lock()
            .map_err(|_| anyhow::anyhow!("monitor binding clock poisoned"))?;
        Ok(*clock.get_or_insert(now))
    }
    pub(crate) fn original_monitor_binding_target(
        &self,
    ) -> Result<Arc<crate::runtime_db::child_task_terminal::OriginalChildTaskTarget>> {
        self.source
            .lock()
            .map_err(|_| anyhow::anyhow!("original binding source poisoned"))?
            .as_ref()
            .and_then(|source| source.target.clone())
            .context("original binding target absent")
    }
    pub(crate) fn validate_monitor_binding_current(&self, current: &TaskRecord) -> Result<bool> {
        let stage = self
            .monitor_stage()
            .context("monitor binding has no original stage")?;
        let source = self.source_view()?;
        source.validate_generation(current)?;
        ensure!(
            current.recovery == source.admitted_task().recovery,
            "monitor binding changed original recovery contract"
        );
        let target = source
            .target()
            .context("monitor binding has no actual target observation")?;
        ensure!(
            target.original() == self.address() && target.target_id() == stage.child,
            "monitor binding changed its actual original target"
        );
        self.validate_monitor_target_incarnation(target.target_scope())?;
        Ok(target.target_scope().is_some()
            && matches!(
                current.status,
                TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
            ))
    }
    pub(crate) fn monitor_binding_task_material(&self) -> Result<(usize, usize)> {
        let stage = self
            .monitor_stage()
            .context("monitor binding stage absent")?;
        let bytes = usize::try_from(serialized_bytes(&(
            &stage.task,
            &stage.detail,
            &stage.child,
            stage.baseline,
            &stage.delivery,
        ))?)?
        .checked_add(1024)
        .and_then(|n| n.checked_mul(8))
        .context("monitor binding body estimate overflow")?;
        let records = completion_value_nodes(stage.task.detail.as_ref())?
            .checked_add(completion_value_nodes(Some(&stage.detail))?)
            .and_then(|n| n.checked_add(32))
            .and_then(|n| n.checked_mul(8))
            .context("monitor binding nodes overflow")?;
        Ok((
            bytes
                .checked_add(
                    records
                        .checked_mul(256)
                        .context("monitor binding nodes bytes overflow")?,
                )
                .context("monitor binding total overflow")?,
            records,
        ))
    }
    pub(crate) fn project_original_monitor_binding(
        &self,
        current: &TaskRecord,
        at: DateTime<Utc>,
    ) -> Result<TaskRecord> {
        ensure!(
            self.validate_monitor_binding_current(current)?,
            "monitor binding target or Task is unavailable"
        );
        let stage = self
            .monitor_stage()
            .context("monitor binding stage absent")?;
        let (bytes, records) = self.monitor_binding_task_material()?;
        self.completion_material()?.observe_material(
            u64::try_from(bytes)?,
            u64::try_from(records)?.saturating_mul(256),
        );
        let mut task = current.clone();
        let mut detail = current
            .detail
            .clone()
            .unwrap_or_else(|| serde_json::json!({}));
        let object = detail
            .as_object_mut()
            .context("monitor binding current detail is not an object")?;
        let actual = stage
            .detail
            .as_object()
            .context("monitor binding actual detail is not an object")?;
        for (key, value) in actual {
            if !matches!(
                key.as_str(),
                "rejoin_obligation_id"
                    | "rejoin_generation"
                    | "parent_turn_id"
                    | "work_item_id"
                    | "cancel_requested"
                    | "force_stop_requested"
            ) {
                object.insert(key.clone(), value.clone());
            }
        }
        object.insert(
            "child_turn_baseline".into(),
            serde_json::json!(stage.baseline),
        );
        if let Some(delivery) = stage.delivery.as_ref() {
            object.insert("delivery_id".into(), serde_json::json!(delivery));
        }
        let view = self.source_view()?;
        let target = view
            .target()
            .context("monitor binding actual target absent")?;
        let scope = target
            .target_scope()
            .context("monitor binding actual identity absent")?;
        object.insert(
            "target_identity_incarnation".into(),
            serde_json::json!(scope.identity_incarnation),
        );
        // The real source, not a status Turn, owns complete/partial rejoin shape.
        let original = view
            .admitted_task()
            .detail
            .as_ref()
            .and_then(serde_json::Value::as_object);
        for name in [
            "rejoin_obligation_id",
            "rejoin_generation",
            "parent_turn_id",
        ] {
            match original.and_then(|detail| detail.get(name)) {
                Some(value) => {
                    object.insert(name.into(), value.clone());
                }
                None => {
                    object.remove(name);
                }
            }
        }
        task.detail = Some(detail);
        ensure!(
            task.effective_work_item_id() == current.effective_work_item_id(),
            "binding changed original effective Work ownership"
        );
        if task.detail != current.detail {
            task.updated_at = at.max(current.updated_at);
        }
        Ok(task)
    }
}
impl RuntimeHandle {
    /// Root initial Stop calls this only after releasing its first parent owner.
    /// This worker never acquires the child-wait mutex or looks up a new lease.
    pub(crate) fn continue_original_child_monitor_binding(
        &self,
        source: Arc<OriginalChildTaskProducer>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + '_>> {
        Box::pin(async move {
            let _worker = source.binding.worker.lock().await;
            if source.monitor_binding_confirmed() {
                return Ok(true);
            }
            ensure!(
                !source.is_durable_finished(),
                "finished monitor has no binding responsibility"
            );
            let bridge = self
                .inner
                .host_bridge
                .as_ref()
                .context("original monitor binding requires its Host")?;
            let limits = self.inner.input_evidence_budget.limits();
            let (record_limit, byte_limit) = TaskDomainLoadRequest::bounded_material_limits(
                usize::try_from(limits.record_limit)?,
                usize::try_from(
                    limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("monitor binding limits overflow")?,
                )?,
            );
            let request = TaskDomainLoadRequest {
                recovery_at: None,
                capture: super::super::resident_task::TaskCaptureRequest::current(),
                query: Default::default(),
                record_limit,
                byte_limit,
            };
            if source.source_view()?.target().is_none() {
                let stage = source
                    .monitor_stage()
                    .context("original binding stage absent")?;
                self.bind_original_child_monitor_target(
                    &source,
                    &stage.child,
                    stage.delivery.as_deref(),
                    stage.baseline,
                )?;
            }
            let mut pending = source.binding.prepared.lock().await;
            let result = bridge
                .commit_original_child_monitor_binding(&source, &mut pending, &request)
                .await?;
            if result {
                source.binding.confirmed.store(true, Ordering::Release);
                pending.take();
            }
            Ok(result)
        })
    }
}

#[cfg(test)]
impl OriginalChildTaskProducer {
    pub(crate) async fn binding_descriptor_for_test(
        &self,
    ) -> Option<crate::runtime_db::input_mutation_receipts::PreparedInputMutation> {
        self.binding
            .prepared
            .lock()
            .await
            .as_ref()
            .map(|prepared| prepared.descriptor().clone())
    }
}
#[cfg(test)]
#[path = "child_monitor_binding_tests.rs"]
mod tests;
