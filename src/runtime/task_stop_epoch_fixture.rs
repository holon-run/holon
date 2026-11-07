//! Actual typed Actor admission/initial Stop loss for the Host epoch test.
use super::*;
use crate::{
    runtime::{
        command_task::ManagedTaskHandle, control_cancellation::SharedControlTaskHandle,
        RuntimeHandle,
    },
    runtime_db::transitions::TransitionFaultPoint,
    types::{TaskKind, TaskStatus},
};

pub(crate) struct OriginalStopEpochFixture {
    plan: Arc<PreparedTaskStopEffects>,
    monitor: TaskStopActorMonitorFixture,
}
impl OriginalStopEpochFixture {
    pub(crate) fn finished_attempt(&self) -> Result<FinishedOriginalTaskStopAttempt> {
        self.plan.finished_attempt()
    }
    pub(crate) fn published(&self) -> Result<bool> {
        Ok(self
            .plan
            .published
            .lock()
            .map_err(|_| anyhow::anyhow!("fixture publication poisoned"))?
            .is_some())
    }
    pub(crate) fn signalled(&self) -> bool {
        self.plan.signalled.load(Ordering::Acquire)
    }
    pub(crate) fn released_complete(&self) -> bool {
        self.plan.finished.load(Ordering::Acquire)
    }
    pub(crate) fn monitor_finished(&self) -> bool {
        self.monitor.is_finished()
    }
}

pub(crate) struct TaskStopActorMonitorFixture {
    lease: Arc<ControlJoinLease>,
}
impl TaskStopActorMonitorFixture {
    pub(crate) fn is_finished(&self) -> bool {
        self.lease.is_finished()
    }
    pub(crate) fn lease_id(&self) -> &str {
        self.lease.lease_id()
    }
}

impl RuntimeHandle {
    pub(crate) async fn register_task_stop_actor_host_fixture(
        &self,
        task_id: &str,
    ) -> Result<TaskStopActorMonitorFixture> {
        let at = chrono::DateTime::from_timestamp_millis(self.now().timestamp_millis())
            .context("fixture timestamp")?;
        let task = TaskRecord {
            id: task_id.into(),
            agent_id: self.instance_agent_id().into(),
            kind: TaskKind::ActorInvocation,
            status: TaskStatus::Running,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: Some(serde_json::json!({"cancel_requested":false})),
            recovery: None,
        };
        self.persist_task_transition(&task, "stop_epoch_actual_actor_admission")
            .await?;
        let admission = self.admit_control_task_handle_registration(&task).await?;
        let lease = self.inner.control_cancellations.task(&task);
        let (started, observed) = tokio::sync::oneshot::channel();
        lease.attach(tokio::spawn(async move {
            let _ = started.send(());
            std::future::pending::<()>().await;
        }));
        self.inner.task_handles.lock().await.insert(
            task.id.clone(),
            ManagedTaskHandle::Async(SharedControlTaskHandle(lease.clone())),
        );
        drop(admission);
        observed.await?;
        Ok(TaskStopActorMonitorFixture { lease })
    }

    pub(crate) async fn original_task_stop_lost_ack_epoch_host_fixture(
        &self,
        task_id: &str,
    ) -> Result<OriginalStopEpochFixture> {
        let monitor = self.register_task_stop_actor_host_fixture(task_id).await?;
        let response = self
            .start_original_task_stop_owned_with_initial_fault(
                task_id,
                Some(TransitionFaultPoint::BeforeCacheUpdate),
            )?
            .await?;
        ensure!(response.is_err(), "fixture must lose original initial ACK");
        self.join_owned_mutations().await?;
        let scope = super::super::resident_input_evidence::InputEvidenceScope {
            agent_id: self.instance_agent_id().into(),
            identity_incarnation: self.instance_incarnation(),
        };
        let (plans, _selectors) = self
            .inner
            .task_stop_effects
            .retained_plans(&self.inner.input_evidence_budget, &scope)?;
        ensure!(
            plans.len() == 1
                && plans[0].intent().task.key.id == task_id
                && plans[0].intent().lease_id.as_deref() == Some(monitor.lease_id()),
            "fixture must retain the actual original plan and lease"
        );
        let fixture = OriginalStopEpochFixture {
            plan: plans[0].clone(),
            monitor,
        };
        ensure!(
            !fixture.published()? && !fixture.signalled() && !fixture.monitor_finished(),
            "lost initial ACK must not authorize any original signal"
        );
        Ok(fixture)
    }
}
