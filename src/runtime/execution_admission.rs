//! Per-instance admission: cloned handles cannot start work after idle exit.
use crate::runtime_error::{RuntimeError, RuntimeErrorDomain};
use anyhow::{anyhow, Result};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

const CLOSED: usize = 1 << (usize::BITS - 1);
const FORCED: usize = 1 << (usize::BITS - 2);

#[derive(Default)]
pub(super) struct ExecutionAdmission(Arc<AtomicUsize>);
pub(crate) struct ExecutionLease(Arc<AtomicUsize>);
pub(super) struct ClosedAdmission {
    gate: Arc<AtomicUsize>,
    committed: bool,
}

pub(crate) fn closed_error() -> anyhow::Error {
    RuntimeError::new(
        RuntimeErrorDomain::Runtime,
        "runtime_instance_retiring",
        "runtime instance admission has closed",
    )
    .with_retryable(true)
    .with_recovery_hint("activate the agent again and retry against its current runtime instance")
    .into()
}

impl ExecutionAdmission {
    pub(super) fn is_closed_and_quiescent(&self) -> bool {
        let state = self.0.load(Ordering::Acquire);
        state & CLOSED != 0 && state & !(CLOSED | FORCED) == 0
    }
    pub(super) fn is_open(&self) -> bool {
        self.0.load(Ordering::Acquire) & CLOSED == 0
    }
    pub(super) fn lease(&self) -> Result<ExecutionLease> {
        let mut state = self.0.load(Ordering::Acquire);
        loop {
            if state >= FORCED - 1 {
                return Err(closed_error());
            }
            match self.0.compare_exchange_weak(
                state,
                state + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(ExecutionLease(self.0.clone())),
                Err(current) => state = current,
            }
        }
    }
    pub(super) fn force_close(&self) {
        self.0.fetch_or(CLOSED | FORCED, Ordering::AcqRel);
    }
    pub(super) fn try_close(&self) -> Result<ClosedAdmission> {
        self.0
            .compare_exchange(0, CLOSED, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| anyhow!("agent_cleanup_blocked: in_flight_admission"))?;
        Ok(ClosedAdmission {
            gate: self.0.clone(),
            committed: false,
        })
    }
}
impl Drop for ExecutionLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}
impl ClosedAdmission {
    pub(super) fn commit(mut self) {
        self.committed = true;
    }
}
impl Drop for ClosedAdmission {
    fn drop(&mut self) {
        if !self.committed {
            // A concurrent forced unload must survive failed cleanup admission.
            let _ = self
                .gate
                .compare_exchange(CLOSED, 0, Ordering::AcqRel, Ordering::Acquire);
        }
    }
}

impl super::RuntimeHandle {
    pub(super) fn track_owned_task(&self, task: tokio::task::JoinHandle<()>) {
        let mut tasks = self.inner.owned_task_handles.lock().unwrap();
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }
    pub(super) async fn admit_control_task_handle_registration(
        &self,
        task: &crate::types::TaskRecord,
    ) -> Result<super::state_mutation::RuntimeMutation> {
        let reservation = self.begin_mutation().await?;
        anyhow::ensure!(
            self.accepts_execution_admission(),
            "cannot register a task handle on a retiring runtime instance"
        );
        anyhow::ensure!(
            reservation.state.status != crate::types::AgentStatus::Stopped,
            "cannot register a task handle after administrative Stop"
        );
        let current = self
            .inner
            .runtime_db
            .tasks()
            .latest(&task.id)?
            .ok_or_else(|| anyhow!("task handle registration has no canonical Task"))?;
        anyhow::ensure!(
            matches!(
                current.status,
                crate::types::TaskStatus::Queued
                    | crate::types::TaskStatus::Running
                    | crate::types::TaskStatus::Cancelling
            ) && current.agent_id == task.agent_id
                && current.created_at == task.created_at
                && super::task_generation::same_task_rejoin_generation(&current, task),
            "task handle registration belongs to a closed or replaced Task"
        );
        Ok(reservation)
    }
    pub(super) async fn release_completed_task_handle_exact(&self, task_id: &str, lease_id: &str) {
        let mut active = self.inner.task_handles.lock().await;
        if active
            .get(task_id)
            .and_then(super::command_task::ManagedTaskHandle::control_lease)
            .is_some_and(|lease| lease.lease_id() == lease_id)
        {
            active.remove(task_id);
        }
        // The registry retains the JoinHandle regardless of map removal.
    }
    pub(crate) async fn join_idle_owned_tasks(&self) -> Result<()> {
        self.stop_context_resource_observer().await?;
        self.join_owned_mutations().await?;
        self.inner.control_cancellations.cancel_local_timer_loops();
        self.inner
            .control_cancellations
            .drain_for_original_runtime(self, false)
            .await?;
        let mut tasks = self.inner.owned_task_drain.lock().await;
        tasks.extend(std::mem::take(
            &mut *self.inner.owned_task_handles.lock().unwrap(),
        ));
        while let Some(task) = tasks.last_mut() {
            let result = task.await;
            tasks.pop();
            result?;
        }
        Ok(())
    }

    pub(crate) fn instance_incarnation(&self) -> u64 {
        self.inner.identity_incarnation
    }
    pub(crate) fn close_execution_admission(&self) {
        self.close_context_resource_observer();
        self.close_mutation_admission();
        self.inner.execution_admission.force_close();
        self.inner.shutdown_requested.store(true, Ordering::Release);
    }
    pub(crate) fn strong_handle_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }
    pub(crate) fn execution_admission_lease(&self) -> Result<ExecutionLease> {
        if self.inner.shutdown_requested.load(Ordering::Acquire) {
            return Err(closed_error());
        }
        self.inner.execution_admission.lease()
    }
    pub(crate) fn accepts_execution_admission(&self) -> bool {
        !self.inner.shutdown_requested.load(Ordering::Acquire)
            && self.inner.execution_admission.is_open()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_exit_cap_requires_closed_admission_with_no_original_lease() {
        let gate = ExecutionAdmission::default();
        assert!(!gate.is_closed_and_quiescent());
        let lease = gate.lease().unwrap();
        gate.force_close();
        assert!(!gate.is_closed_and_quiescent());
        drop(lease);
        assert!(gate.is_closed_and_quiescent());
    }
    #[test]
    fn forced_close_preserves_in_flight_leases_and_cannot_be_rolled_back() {
        let gate = ExecutionAdmission::default();
        let lease = gate.lease().unwrap();
        gate.force_close();
        assert!(gate.lease().is_err());
        drop(lease);
        assert!(!gate.is_open());
        assert!(gate.lease().is_err());

        let gate = ExecutionAdmission::default();
        let provisional_close = gate.try_close().unwrap();
        gate.force_close();
        drop(provisional_close);
        assert!(!gate.is_open());
        assert!(gate.lease().is_err());
    }

    #[test]
    fn work_and_closure_have_one_winner() {
        let gate = ExecutionAdmission::default();
        let lease = gate.lease().unwrap();
        assert!(gate.try_close().is_err());
        drop(lease);
        let closing = gate.try_close().unwrap();
        assert!(gate.lease().is_err());
        drop(closing); // failed admission reopens the instance.
        assert!(gate.lease().is_ok());
        gate.try_close().unwrap().commit();
        assert!(gate.lease().is_err());
    }
}
