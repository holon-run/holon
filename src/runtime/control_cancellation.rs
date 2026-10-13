//! Exact process-local cancellation leases retained across caller cancellation.
//!
//! Durable target identity comes from Control's domain snapshot. These leases
//! only identify original local handles; absence never proves an effect joined.

use std::{
    collections::BTreeMap,
    future::{Future, IntoFuture},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex,
    },
};

use super::control::PreparedControlCancellation;
use crate::{
    runtime_db::control_transition::ControlKey,
    types::{TaskRecord, TimerRecord},
};
use anyhow::{anyhow, ensure, Result};
use tokio::{
    sync::{oneshot, Mutex, Notify},
    task::{AbortHandle, JoinHandle},
};
use tokio_util::sync::CancellationToken;

/// Cloning the signal retains the same one-shot sender, never a new recipient.
#[derive(Clone)]
pub(super) struct SharedControlSignal(Arc<StdMutex<Option<oneshot::Sender<()>>>>);
impl SharedControlSignal {
    pub(super) fn new(sender: oneshot::Sender<()>) -> Self {
        Self(Arc::new(StdMutex::new(Some(sender))))
    }
    pub(super) fn send(self, (): ()) -> std::result::Result<(), ()> {
        self.0
            .lock()
            .expect("control signal poisoned")
            .take()
            .map_or(Ok(()), |sender| sender.send(()))
    }
}

enum CancellationSignal {
    Async(StdMutex<Option<AbortHandle>>),
    Command {
        cancel: SharedControlSignal,
        force: SharedControlSignal,
    },
    Timer(CancellationToken),
}

#[derive(Clone)]
enum JoinOutcome {
    Completed,
    Failed(String),
}
impl JoinOutcome {
    fn result(&self) -> Result<()> {
        match self {
            Self::Completed => Ok(()),
            Self::Failed(error) => Err(anyhow!(error.clone())),
        }
    }
}
#[derive(Default)]
struct JoinSlot {
    handle: Option<JoinHandle<()>>,
    outcome: Option<JoinOutcome>,
}

pub(super) struct ControlJoinLease {
    lease_id: String,
    target_id: String,
    domain_generation: serde_json::Value,
    signal: CancellationSignal,
    monitor: StdMutex<Option<AbortHandle>>,
    join: Mutex<JoinSlot>,
    cancel_requested: AtomicBool,
    registration: StdMutex<Option<Arc<super::control_reconcile::ControlRegistrationRecord>>>,
}
impl ControlJoinLease {
    fn new(
        target_id: String,
        domain_generation: serde_json::Value,
        signal: CancellationSignal,
    ) -> Arc<Self> {
        Arc::new(Self {
            lease_id: uuid::Uuid::new_v4().to_string(),
            target_id,
            domain_generation,
            signal,
            monitor: StdMutex::new(None),
            join: Mutex::new(JoinSlot::default()),
            cancel_requested: AtomicBool::new(false),
            registration: StdMutex::new(None),
        })
    }
    pub(super) fn lease_id(&self) -> &str {
        &self.lease_id
    }
    pub(super) fn domain_generation(&self) -> &serde_json::Value {
        &self.domain_generation
    }
    /// Must run synchronously before the handle is published in task_handles.
    pub(super) fn attach(&self, handle: JoinHandle<()>) {
        let abort = handle.abort_handle();
        if let CancellationSignal::Async(signal) = &self.signal {
            *signal.lock().expect("control abort signal poisoned") = Some(abort.clone());
        }
        *self.monitor.lock().expect("control monitor poisoned") = Some(abort);
        let mut slot = self
            .join
            .try_lock()
            .expect("unpublished control lease locked");
        assert!(
            slot.handle.is_none() && slot.outcome.is_none(),
            "control lease attached twice"
        );
        slot.handle = Some(handle);
        drop(slot);
        if self.cancel_requested.load(Ordering::Acquire) {
            self.cancel();
        }
    }
    fn record_joined(&self) {
        if let Some(record) = self
            .registration
            .lock()
            .expect("Control registration poisoned")
            .as_ref()
        {
            record.joined(self.lease_id());
        }
    }
    pub(super) fn is_finished(&self) -> bool {
        self.monitor
            .lock()
            .expect("control monitor poisoned")
            .as_ref()
            .is_some_and(AbortHandle::is_finished)
    }
    /// Only original handles are signalled; never look up a newer task by id.
    pub(super) fn cancel(&self) {
        self.cancel_requested.store(true, Ordering::Release);
        match &self.signal {
            CancellationSignal::Async(signal) => {
                if let Some(abort) = signal
                    .lock()
                    .expect("control abort signal poisoned")
                    .as_ref()
                {
                    abort.abort();
                }
            }
            CancellationSignal::Command { cancel, force } => {
                let _ = cancel.clone().send(());
                let _ = force.clone().send(());
            }
            CancellationSignal::Timer(token) => token.cancel(),
        }
    }
    /// Reap only proven successful completion; failures remain for a drainer.
    /// Polling a borrowed finished handle cannot detach a pending monitor.
    fn reap_finished_successfully(&self) -> bool {
        let Ok(mut slot) = self.join.try_lock() else {
            return false;
        };
        if let Some(outcome) = &slot.outcome {
            return matches!(outcome, JoinOutcome::Completed);
        }
        let Some(handle) = slot.handle.as_mut().filter(|handle| handle.is_finished()) else {
            return false;
        };
        let Some(result) = poll_finished(handle) else {
            return false;
        };
        slot.handle.take();
        let outcome = join_outcome(result);
        let succeeded = matches!(outcome, JoinOutcome::Completed);
        slot.outcome = Some(outcome);
        if succeeded {
            self.record_joined();
        }
        succeeded
    }
    /// The JoinHandle stays in the slot while await is pending. Dropping a
    /// drainer releases the guard, leaving the exact handle for the next one.
    pub(super) async fn join(&self) -> Result<()> {
        let mut slot = self.join.lock().await;
        if let Some(outcome) = slot.outcome.as_ref() {
            return outcome.result();
        }
        let handle = slot
            .handle
            .as_mut()
            .ok_or_else(|| anyhow!("control lease {} has no attached monitor", self.lease_id))?;
        let result = handle.await;
        slot.handle.take();
        let outcome = join_outcome(result);
        let result = outcome.result();
        slot.outcome = Some(outcome);
        if result.is_ok() {
            self.record_joined();
        }
        result
    }
}

fn poll_finished<T>(handle: &mut JoinHandle<T>) -> Option<Result<T, tokio::task::JoinError>> {
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match Pin::new(handle).poll(&mut context) {
        std::task::Poll::Ready(result) => Some(result),
        std::task::Poll::Pending => None,
    }
}

fn join_outcome(result: std::result::Result<(), tokio::task::JoinError>) -> JoinOutcome {
    match result {
        Ok(()) => JoinOutcome::Completed,
        Err(error) if error.is_cancelled() => JoinOutcome::Completed,
        Err(error) => JoinOutcome::Failed(error.to_string()),
    }
}

/// Preserves the existing Async handle surface while shared registry ownership
/// prevents removing a map entry from detaching its join responsibility.
#[derive(Clone)]
pub(super) struct SharedControlTaskHandle(pub(super) Arc<ControlJoinLease>);
impl SharedControlTaskHandle {
    pub(super) fn abort(&self) {
        self.0.cancel();
    }
    pub(super) fn lease(&self) -> Arc<ControlJoinLease> {
        self.0.clone()
    }
}
impl IntoFuture for SharedControlTaskHandle {
    type Output = Result<()>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move { self.0.join().await })
    }
}

pub(super) struct ControlRunLease {
    lease_id: String,
    run_id: String,
    activation_id: Option<String>,
    source_message_id: String,
    token: CancellationToken,
    reason: Arc<StdMutex<String>>,
    complete: AtomicBool,
    completed: Notify,
}
impl ControlRunLease {
    #[cfg(test)]
    pub(super) fn test_stopped_run(
        run_id: &str,
        lease_id: &str,
        activation_id: Option<String>,
        source_message_id: &str,
        completed: bool,
    ) -> Arc<Self> {
        let token = CancellationToken::new();
        token.cancel();
        Arc::new(Self {
            lease_id: lease_id.to_owned(),
            run_id: run_id.to_owned(),
            activation_id,
            source_message_id: source_message_id.to_owned(),
            token,
            reason: Arc::new(StdMutex::new("agent_stopped".into())),
            complete: AtomicBool::new(completed),
            completed: Notify::new(),
        })
    }
    pub(super) fn lease_id(&self) -> &str {
        &self.lease_id
    }
    pub(super) fn run_id(&self) -> &str {
        &self.run_id
    }
    pub(super) fn activation_id(&self) -> Option<&str> {
        self.activation_id.as_deref()
    }
    pub(super) fn source_message_id(&self) -> &str {
        &self.source_message_id
    }
    pub(super) fn was_stopped(&self) -> bool {
        self.token.is_cancelled()
            && self
                .reason
                .lock()
                .expect("run abort reason poisoned")
                .as_str()
                == "agent_stopped"
    }
    pub(super) fn is_complete(&self) -> bool {
        self.complete.load(Ordering::Acquire)
    }
    pub(super) fn cancel(&self) {
        *self.reason.lock().expect("run abort reason poisoned") = "agent_stopped".into();
        self.token.cancel();
    }
    fn cancel_locally(&self) {
        self.token.cancel();
    }
    pub(super) fn finish(&self) {
        self.complete.store(true, Ordering::Release);
        self.completed.notify_waiters();
    }
    pub(super) async fn join(&self) {
        loop {
            let notified = self.completed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_complete() {
                return;
            }
            notified.await;
        }
    }
}
/// One guard lives alongside the actual scheduled processing future. Clearing
/// administrative current_run fields alone must never mark physical completion.
pub(super) struct ControlRunCompletionGuard(Arc<ControlRunLease>);
impl ControlRunCompletionGuard {
    pub(super) fn lease(&self) -> Arc<ControlRunLease> {
        self.0.clone()
    }
}
impl Drop for ControlRunCompletionGuard {
    fn drop(&mut self) {
        self.0.finish();
    }
}

struct RetainedControlPreparation {
    digest: String,
    owner_epoch: u64,
    confirmed: bool,
    facts: Arc<PreparedControlCancellation>,
}
type PreparationKey = (String, u64, String);
fn preparation_key(key: &ControlKey) -> PreparationKey {
    (
        key.agent_id.clone(),
        key.identity_incarnation,
        key.command_id.clone(),
    )
}

struct CompletionWorker {
    handle: Option<JoinHandle<Result<()>>>,
    failure: Option<String>,
    original_plan_finished: Option<Arc<AtomicBool>>,
}
impl CompletionWorker {
    fn new(
        handle: JoinHandle<Result<()>>,
        original_plan_finished: Option<Arc<AtomicBool>>,
    ) -> Self {
        Self {
            handle: Some(handle),
            failure: None,
            original_plan_finished,
        }
    }
    fn resolved_by_original_retry(&self) -> bool {
        self.original_plan_finished
            .as_ref()
            .is_some_and(|finished| finished.load(Ordering::Acquire))
    }
    fn reap_successfully(&mut self) -> bool {
        if self.failure.is_some() && self.resolved_by_original_retry() {
            return true;
        }
        let Some(handle) = self.handle.as_mut().filter(|handle| handle.is_finished()) else {
            return false;
        };
        let Some(result) = poll_finished(handle) else {
            return false;
        };
        self.handle.take();
        match result {
            Ok(Ok(())) => true,
            Ok(Err(error)) => {
                self.failure = Some(format!("{error:#}"));
                false
            }
            Err(error) => {
                self.failure = Some(error.to_string());
                false
            }
        }
    }
    async fn join(&mut self) -> Result<()> {
        if let Some(error) = &self.failure {
            return if self.resolved_by_original_retry() {
                Ok(())
            } else {
                Err(anyhow!(error.clone()))
            };
        }
        let result = self
            .handle
            .as_mut()
            .ok_or_else(|| anyhow!("Control completion worker has no outcome"))?
            .await;
        self.handle.take();
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                let message = format!("{error:#}");
                self.failure = Some(message.clone());
                Err(anyhow!(message))
            }
            Err(error) => {
                let message = error.to_string();
                self.failure = Some(message.clone());
                Err(anyhow!(message))
            }
        }
    }
}

#[derive(Default)]
pub(super) struct ControlCancellationRegistry {
    joins: StdMutex<BTreeMap<String, Arc<ControlJoinLease>>>,
    runs: StdMutex<BTreeMap<String, Arc<ControlRunLease>>>,
    prepared: StdMutex<BTreeMap<PreparationKey, RetainedControlPreparation>>,
    completion_workers: Mutex<Vec<CompletionWorker>>,
    pending_workers: StdMutex<Vec<CompletionWorker>>,
    registrations: super::control_reconcile::ControlRegistrationLedger,
    retiring: AtomicBool,
}
impl ControlCancellationRegistry {
    /// Must run before SQL. Indeterminate/lost ACK retains the exact plan without
    /// signalling it; a receipt resolver later confirms or discards this entry.
    pub(super) fn retain_prepared(
        &self,
        key: &ControlKey,
        digest: &str,
        owner_epoch: u64,
        facts: Arc<PreparedControlCancellation>,
    ) -> Result<()> {
        let mut prepared = self.prepared.lock().expect("Control preparations poisoned");
        if let Some(existing) = prepared.get(&preparation_key(key)) {
            ensure!(
                existing.digest == digest
                    && existing.owner_epoch == owner_epoch
                    && Arc::ptr_eq(&existing.facts, &facts),
                "Control preparation identity was reused"
            );
            return Ok(());
        }
        prepared.insert(
            preparation_key(key),
            RetainedControlPreparation {
                digest: digest.into(),
                owner_epoch,
                confirmed: false,
                facts,
            },
        );
        Ok(())
    }
    /// Original receipt lookup only. This does not confirm a commit or inspect
    /// current handles; callers must reconcile/publish the owner first.
    pub(super) fn prepared_for_receipt(
        &self,
        key: &ControlKey,
        digest: &str,
    ) -> Result<Option<(u64, Arc<PreparedControlCancellation>)>> {
        let prepared = self.prepared.lock().expect("Control preparations poisoned");
        let Some(original) = prepared.get(&preparation_key(key)) else {
            return Ok(None);
        };
        ensure!(
            original.digest == digest,
            "Control receipt differs from retained preparation"
        );
        Ok(Some((original.owner_epoch, original.facts.clone())))
    }

    pub(super) fn confirm_prepared(
        &self,
        key: &ControlKey,
        digest: &str,
        owner_epoch: u64,
    ) -> Result<Option<Arc<PreparedControlCancellation>>> {
        let mut prepared = self.prepared.lock().expect("Control preparations poisoned");
        let Some(existing) = prepared.get_mut(&preparation_key(key)) else {
            return Ok(None);
        };
        ensure!(
            existing.digest == digest && existing.owner_epoch == owner_epoch,
            "Control prepared witness scope differs"
        );
        existing.confirmed = true;
        Ok(Some(existing.facts.clone()))
    }
    /// Only typed original absence/definite rollback, or completion of every
    /// exact obligation, authorizes dropping its process-local responsibility.
    pub(super) fn release_prepared(
        &self,
        key: &ControlKey,
        digest: &str,
        owner_epoch: u64,
    ) -> Result<()> {
        let mut prepared = self.prepared.lock().expect("Control preparations poisoned");
        if let Some(existing) = prepared.get(&preparation_key(key)) {
            ensure!(
                existing.digest == digest && existing.owner_epoch == owner_epoch,
                "Control prepared witness scope differs"
            );
        }
        prepared.remove(&preparation_key(key));
        Ok(())
    }
    pub(super) fn track_completion(&self, worker: JoinHandle<Result<()>>) {
        let mut workers = self
            .pending_workers
            .lock()
            .expect("Control completion workers poisoned");
        workers.retain_mut(|worker| !worker.reap_successfully());
        workers.push(CompletionWorker::new(worker, None));
    }
    pub(super) fn track_control_completion(
        &self,
        worker: JoinHandle<Result<()>>,
        original_plan_finished: Arc<AtomicBool>,
    ) {
        let mut workers = self
            .pending_workers
            .lock()
            .expect("Control completion workers poisoned");
        workers.retain_mut(|worker| !worker.reap_successfully());
        workers.push(CompletionWorker::new(worker, Some(original_plan_finished)));
    }
    fn retain_join(&self, lease: Arc<ControlJoinLease>) -> Arc<ControlJoinLease> {
        let mut leases = self.joins.lock().expect("control leases poisoned");
        leases.retain(|_, existing| !existing.reap_finished_successfully());
        leases.insert(lease.lease_id.clone(), lease.clone());
        if self.retiring.load(Ordering::Acquire) {
            lease.cancel();
        }
        lease
    }
    pub(super) fn task(&self, task: &TaskRecord) -> Arc<ControlJoinLease> {
        let lease = self.retain_join(ControlJoinLease::new(
            task.id.clone(),
            serde_json::json!({"created_at": task.created_at, "rejoin": task.rejoin_fence().ok()}),
            CancellationSignal::Async(StdMutex::new(None)),
        ));
        if let Some(record) = self.registrations.for_task(task) {
            *lease
                .registration
                .lock()
                .expect("Control registration poisoned") = Some(record.clone());
            record.attach(&lease);
        }
        lease
    }
    /// Call synchronously before the active Task transaction. Its drop cannot
    /// prove anything for an already spawned command/independent child.
    pub(super) fn before_active_task_sql(
        &self,
        task: &TaskRecord,
        incarnation: u64,
        owner_epoch: u64,
    ) -> super::control_reconcile::ControlRegistrationTicket {
        self.registrations
            .before_active_task_sql(task, incarnation, owner_epoch)
    }
    pub(super) fn registration_for_task(
        &self,
        task: &TaskRecord,
    ) -> Option<Arc<super::control_reconcile::ControlRegistrationRecord>> {
        self.registrations.for_task(task)
    }
    pub(super) fn command(
        &self,
        task: &TaskRecord,
        cancel: SharedControlSignal,
        force: SharedControlSignal,
    ) -> Arc<ControlJoinLease> {
        self.retain_join(ControlJoinLease::new(
            task.id.clone(),
            serde_json::json!({"created_at": task.created_at, "rejoin": task.rejoin_fence().ok()}),
            CancellationSignal::Command { cancel, force },
        ))
    }
    pub(super) fn timer(
        &self,
        timer: &TimerRecord,
        token: CancellationToken,
    ) -> Arc<ControlJoinLease> {
        self.retain_join(ControlJoinLease::new(
            timer.id.clone(),
            serde_json::json!({"created_at": timer.created_at}),
            CancellationSignal::Timer(token),
        ))
    }
    pub(super) fn run(
        &self,
        run_id: String,
        token: CancellationToken,
        reason: Arc<StdMutex<String>>,
        activation_id: Option<String>,
        source_message_id: String,
    ) -> Result<Arc<ControlRunLease>> {
        let mut runs = self.runs.lock().expect("control run leases poisoned");
        runs.retain(|_, run| !run.is_complete());
        ensure!(
            !runs.contains_key(&run_id),
            "run cancellation lease already registered"
        );
        let lease = Arc::new(ControlRunLease {
            lease_id: uuid::Uuid::new_v4().to_string(),
            run_id: run_id.clone(),
            activation_id,
            source_message_id,
            token,
            reason,
            complete: AtomicBool::new(false),
            completed: Notify::new(),
        });
        runs.insert(run_id, lease.clone());
        if self.retiring.load(Ordering::Acquire) {
            lease.cancel_locally();
        }
        Ok(lease)
    }
    pub(super) fn run_for(&self, run_id: &str) -> Option<Arc<ControlRunLease>> {
        self.runs
            .lock()
            .expect("control run leases poisoned")
            .get(run_id)
            .cloned()
    }
    pub(super) fn run_completion(&self, run_id: &str) -> Option<ControlRunCompletionGuard> {
        self.run_for(run_id).map(ControlRunCompletionGuard)
    }
    pub(super) fn timer_for(&self, timer_id: &str) -> Vec<Arc<ControlJoinLease>> {
        self.joins
            .lock()
            .expect("control leases poisoned")
            .values()
            .filter(|lease| {
                lease.target_id == timer_id
                    && !lease.is_finished()
                    && matches!(&lease.signal, CancellationSignal::Timer(_))
            })
            .cloned()
            .collect()
    }
    /// Physical instance retirement only; no canonical Control/Task mutation.
    pub(super) fn cancel_local_producers(&self) {
        self.retiring.store(true, Ordering::Release);
        let leases = self
            .joins
            .lock()
            .expect("control leases poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let runs = self
            .runs
            .lock()
            .expect("control run leases poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for lease in leases {
            lease.cancel();
        }
        for run in runs {
            run.cancel_locally();
        }
    }
    pub(super) fn cancel_local_timer_loops(&self) {
        let leases = self
            .joins
            .lock()
            .expect("control leases poisoned")
            .values()
            .filter(|lease| matches!(&lease.signal, CancellationSignal::Timer(_)))
            .cloned()
            .collect::<Vec<_>>();
        for lease in leases {
            lease.cancel();
        }
    }
    pub(super) async fn drain_retiring(&self) -> Result<()> {
        self.cancel_local_producers();
        self.drain().await
    }
    pub(super) async fn drain(&self) -> Result<()> {
        {
            let prepared = self.prepared.lock().expect("Control preparations poisoned");
            ensure!(
                prepared.values().all(|preparation| preparation.confirmed),
                "Control commit outcome remains unknown; original cancellation plan retained"
            );
        }
        let mut workers = self.completion_workers.lock().await;
        loop {
            // A registration worker may hand a rejected OS process to another
            // cleanup worker before completing. Drain that new responsibility
            // too, without dropping any pending JoinHandle on cancellation.
            workers.extend(std::mem::take(
                &mut *self
                    .pending_workers
                    .lock()
                    .expect("Control completion workers poisoned"),
            ));
            let Some(worker) = workers.last_mut() else {
                break;
            };
            worker.join().await?;
            workers.pop();
        }
        drop(workers);
        if self.retiring.load(Ordering::Acquire) {
            self.cancel_local_producers();
        }
        ensure!(
            self.prepared
                .lock()
                .expect("Control preparations poisoned")
                .is_empty(),
            "Control cancellation obligations remain unresolved"
        );
        let leases = self
            .joins
            .lock()
            .expect("control leases poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let runs = self
            .runs
            .lock()
            .expect("control run leases poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for lease in leases {
            lease.join().await?;
            self.joins
                .lock()
                .expect("control leases poisoned")
                .remove(lease.lease_id());
        }
        for run in runs {
            run.join().await;
            self.runs
                .lock()
                .expect("control run leases poisoned")
                .remove(run.run_id());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelling_a_drainer_retains_original_monitor_until_next_join() {
        let registry = Arc::new(ControlCancellationRegistry::default());
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let lease = registry.retain_join(ControlJoinLease::new(
            "task-one".into(),
            serde_json::Value::Null,
            CancellationSignal::Async(StdMutex::new(None)),
        ));
        let task_entered = entered.clone();
        let task_release = release.clone();
        lease.attach(tokio::spawn(async move {
            task_entered.notify_one();
            task_release.notified().await;
        }));
        entered.notified().await;
        let draining = tokio::spawn({
            let registry = registry.clone();
            async move { registry.drain().await }
        });
        tokio::task::yield_now().await;
        draining.abort();
        let _ = draining.await;
        assert!(!lease.is_finished());
        assert!(registry
            .joins
            .lock()
            .unwrap()
            .contains_key(lease.lease_id()));
        release.notify_one();
        registry.drain().await.unwrap();
        assert!(lease.is_finished());
    }
    #[tokio::test]
    async fn old_task_lease_does_not_cancel_same_id_successor() {
        let registry = ControlCancellationRegistry::default();
        let old = registry.retain_join(ControlJoinLease::new(
            "same-task".into(),
            serde_json::Value::Null,
            CancellationSignal::Async(StdMutex::new(None)),
        ));
        old.attach(tokio::spawn(std::future::pending()));
        let new = registry.retain_join(ControlJoinLease::new(
            "same-task".into(),
            serde_json::Value::Null,
            CancellationSignal::Async(StdMutex::new(None)),
        ));
        new.attach(tokio::spawn(std::future::pending()));
        assert_ne!(old.lease_id(), new.lease_id());
        old.cancel();
        old.join().await.unwrap();
        assert!(old.is_finished());
        assert!(!new.is_finished());
        new.cancel();
        registry.drain().await.unwrap();
    }
    #[tokio::test]
    async fn command_cancel_sends_original_signals_and_retains_monitor_join() {
        let registry = ControlCancellationRegistry::default();
        let (cancel, cancelled) = oneshot::channel();
        let (force, forced) = oneshot::channel();
        let lease = registry.retain_join(ControlJoinLease::new(
            "command-one".into(),
            serde_json::Value::Null,
            CancellationSignal::Command {
                cancel: SharedControlSignal::new(cancel),
                force: SharedControlSignal::new(force),
            },
        ));
        lease.attach(tokio::spawn(async move {
            cancelled.await.unwrap();
            forced.await.unwrap();
        }));
        lease.cancel();
        lease.cancel();
        registry.drain().await.unwrap();
        assert!(lease.is_finished());
    }
    #[tokio::test]
    async fn cancelled_run_is_not_joined_until_actual_processing_future_drops() {
        let registry = ControlCancellationRegistry::default();
        let token = CancellationToken::new();
        let run = registry
            .run(
                "run-one".into(),
                token.clone(),
                Arc::new(StdMutex::new(String::new())),
                Some("attempt-one".into()),
                "message-one".into(),
            )
            .unwrap();
        let completion = registry.run_completion("run-one").unwrap();
        run.cancel();
        assert!(token.is_cancelled());
        assert!(!run.is_complete());
        drop(completion);
        run.join().await;
        assert!(run.is_complete());
    }
    #[tokio::test]
    async fn registering_more_work_reaps_finished_monitors_without_pinning_payloads() {
        let registry = ControlCancellationRegistry::default();
        let old = registry.retain_join(ControlJoinLease::new(
            "finished-task".into(),
            serde_json::Value::Null,
            CancellationSignal::Async(StdMutex::new(None)),
        ));
        let weak = Arc::downgrade(&old);
        old.attach(tokio::spawn(async {}));
        while !old.is_finished() {
            tokio::task::yield_now().await;
        }
        drop(old);
        let next = registry.retain_join(ControlJoinLease::new(
            "next-task".into(),
            serde_json::Value::Null,
            CancellationSignal::Async(StdMutex::new(None)),
        ));
        next.attach(tokio::spawn(std::future::pending()));
        assert!(weak.upgrade().is_none());
        next.cancel();
        registry.drain().await.unwrap();
    }

    #[tokio::test]
    async fn background_monitor_failure_is_retained_for_the_drainer() {
        let registry = ControlCancellationRegistry::default();
        let failed = registry.retain_join(ControlJoinLease::new(
            "failed-task".into(),
            serde_json::Value::Null,
            CancellationSignal::Async(StdMutex::new(None)),
        ));
        failed.attach(tokio::spawn(async { panic!("original monitor failed") }));
        while !failed.is_finished() {
            tokio::task::yield_now().await;
        }
        let next = registry.retain_join(ControlJoinLease::new(
            "next-task".into(),
            serde_json::Value::Null,
            CancellationSignal::Async(StdMutex::new(None)),
        ));
        next.attach(tokio::spawn(async {}));
        assert!(registry
            .joins
            .lock()
            .unwrap()
            .contains_key(failed.lease_id()));
        for _ in 0..2 {
            assert!(registry
                .drain()
                .await
                .unwrap_err()
                .to_string()
                .contains("original monitor failed"));
            assert!(registry
                .joins
                .lock()
                .unwrap()
                .contains_key(failed.lease_id()));
        }
    }
    #[tokio::test]
    async fn failed_generic_completion_remains_visible_on_every_drain() {
        let registry = ControlCancellationRegistry::default();
        registry.track_completion(tokio::spawn(async { Err(anyhow!("cleanup not joined")) }));
        for _ in 0..2 {
            assert!(registry
                .drain()
                .await
                .unwrap_err()
                .to_string()
                .contains("cleanup not joined"));
        }
    }
    #[tokio::test]
    async fn only_the_same_original_plan_success_resolves_its_failed_worker() {
        let registry = ControlCancellationRegistry::default();
        let original = Arc::new(AtomicBool::new(false));
        let unrelated = Arc::new(AtomicBool::new(true));
        registry.track_control_completion(
            tokio::spawn(async { Err(anyhow!("original cleanup failed")) }),
            original.clone(),
        );
        assert!(registry.drain().await.is_err());
        registry.track_control_completion(tokio::spawn(async { Ok(()) }), unrelated);
        assert!(registry.drain().await.is_err());
        // The exact retained plan sets this only after all original obligations
        // have completed or have an explicit durable handoff disposition.
        original.store(true, Ordering::Release);
        registry.drain().await.unwrap();
    }
    #[tokio::test]
    async fn retiring_before_attach_cancels_the_late_original_monitor() {
        let registry = ControlCancellationRegistry::default();
        registry.cancel_local_producers();
        let lease = registry.retain_join(ControlJoinLease::new(
            "late".into(),
            serde_json::Value::Null,
            CancellationSignal::Async(StdMutex::new(None)),
        ));
        lease.attach(tokio::spawn(std::future::pending()));
        tokio::time::timeout(std::time::Duration::from_secs(1), registry.drain_retiring())
            .await
            .unwrap()
            .unwrap();
        assert!(lease.is_finished());
    }
    #[tokio::test]
    async fn physical_retirement_preserves_the_existing_run_abort_reason() {
        let registry = ControlCancellationRegistry::default();
        let token = CancellationToken::new();
        let reason = Arc::new(StdMutex::new("runtime_unloaded".into()));
        let lease = registry
            .run(
                "run".into(),
                token.clone(),
                reason.clone(),
                None,
                "source".into(),
            )
            .unwrap();
        registry.cancel_local_producers();
        assert!(token.is_cancelled());
        assert_eq!(*reason.lock().unwrap(), "runtime_unloaded");
        lease.finish();
        registry.drain_retiring().await.unwrap();
    }
}
