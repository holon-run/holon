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
    pub(super) async fn release_completed_task_handle(&self, task_id: &str) {
        let mut active = self.inner.task_handles.lock().await;
        let handle = active.remove(task_id);
        if let Some(super::command_task::ManagedTaskHandle::Async(handle)) = handle {
            let mut owned = self.inner.owned_task_handles.lock().unwrap();
            owned.retain(|task| !task.is_finished());
            owned.push(handle);
        }
    }
    pub(crate) async fn join_idle_owned_tasks(&self) -> Result<()> {
        let tasks = std::mem::take(&mut *self.inner.owned_task_handles.lock().unwrap());
        for task in tasks {
            task.await?;
        }
        Ok(())
    }

    pub(crate) fn instance_incarnation(&self) -> u64 {
        self.inner.identity_incarnation
    }
    pub(crate) fn close_execution_admission(&self) {
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
