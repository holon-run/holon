//! Frozen local effects require the original Stop ACK in a published Task view.
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use anyhow::{ensure, Context, Result};

use super::{
    command_task::ManagedTaskHandle,
    control_cancellation::{ControlJoinLease, SharedControlSignal},
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    mutation_owner::MutationOwner,
    task_stop_join::TaskStopMonitorJoin,
};
use crate::{
    runtime_db::{
        input_mutation_receipts::TaskStopReceiptBinding,
        input_mutation_receipts::{InputMutationFamily, PreparedInputMutation},
        task_stop::{PreparedTaskStop, TaskStopRecord, TaskStopSignal},
    },
    types::{TaskKind, TaskRecord},
};

enum OriginalSignal {
    Cancel(SharedControlSignal),
    Force(SharedControlSignal),
    Abort,
    Missing,
}

/// This owns exact local leases and frozen data, without a runtime or root.
pub(super) struct PreparedTaskStopEffects {
    prepared: PreparedInputMutation,
    intent: PreparedTaskStop,
    lease: Option<Arc<ControlJoinLease>>,
    signal: OriginalSignal,
    signalled: AtomicBool,
    published: Mutex<Option<TaskStopReceiptBinding>>,
    worker_active: AtomicBool,
    sql_finished: AtomicBool,
    pub(super) finished: Arc<AtomicBool>,
    _material: Arc<BudgetViewGuard>,
}

impl PreparedTaskStopEffects {
    /// Freeze while the original Task is reserved. No sender is taken or used.
    pub(super) fn freeze(
        prepared: &PreparedInputMutation,
        intent: &PreparedTaskStop,
        task: &TaskRecord,
        handle: Option<&ManagedTaskHandle>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Arc<Self>> {
        intent.validate_original(task)?;
        ensure!(
            prepared.family() == InputMutationFamily::Task
                && prepared.scopes().contains(&intent.task.scope)
                && intent
                    .child
                    .as_ref()
                    .is_none_or(|child| prepared.scopes().contains(&child.scope)),
            "TaskStop local plan differs from original command group"
        );
        let bytes = serialized_bytes(&(prepared, intent))?
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(1024))
            .context("TaskStop local material overflow")?;
        let limits = ledger.limits();
        let byte_limit = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("TaskStop local limit overflow")?
            .min(64 * 1024 * 1024);
        ensure!(
            bytes <= byte_limit && limits.record_limit >= 4,
            "TaskStop local ResourceExhausted before clone"
        );
        let material = ledger.register_owned_bytes(
            &intent.task.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let lease = handle.and_then(ManagedTaskHandle::control_lease);
        if let Some(lease) = &lease {
            ensure!(
                intent.lease_id.as_deref() == Some(lease.lease_id())
                    && lease.target_id() == task.id
                    && lease.matches_task_generation(task),
                "TaskStop local lease differs from original generation"
            );
        }
        ensure!(
            lease.is_some() == intent.lease_id.is_some(),
            "TaskStop local lease availability differs"
        );
        let signal = match (intent.signal, handle) {
            (TaskStopSignal::NoHandle, None) => OriginalSignal::Missing,
            (TaskStopSignal::Cancel, Some(ManagedTaskHandle::Command(command)))
                if task.kind == TaskKind::CommandTask =>
            {
                OriginalSignal::Cancel(
                    command
                        .cancel_tx
                        .as_ref()
                        .context("original cancel sender missing")?
                        .clone(),
                )
            }
            (TaskStopSignal::Force, Some(ManagedTaskHandle::Command(command)))
                if task.kind == TaskKind::CommandTask =>
            {
                OriginalSignal::Force(
                    command
                        .force_stop_tx
                        .as_ref()
                        .context("original force sender missing")?
                        .clone(),
                )
            }
            (TaskStopSignal::Abort, Some(ManagedTaskHandle::Async(_)))
                if task.kind != TaskKind::CommandTask =>
            {
                OriginalSignal::Abort
            }
            _ => anyhow::bail!("TaskStop local handle type or selected signal differs"),
        };
        Ok(Arc::new(Self {
            prepared: prepared.clone(),
            intent: intent.clone(),
            lease,
            signal,
            signalled: AtomicBool::new(false),
            published: Mutex::new(None),
            worker_active: AtomicBool::new(false),
            sql_finished: AtomicBool::new(false),
            finished: Arc::new(AtomicBool::new(false)),
            _material: material,
        }))
    }
    pub(super) fn mark_sql_finished(&self) {
        self.sql_finished.store(true, Ordering::Release);
    }
    pub(super) fn finished_attempt(self: &Arc<Self>) -> Result<FinishedOriginalTaskStopAttempt> {
        ensure!(
            self.sql_finished(),
            "original TaskStop SQL attempt has not finished"
        );
        Ok(FinishedOriginalTaskStopAttempt { plan: self.clone() })
    }
    pub(super) fn sql_finished(&self) -> bool {
        self.sql_finished.load(Ordering::Acquire)
    }
    pub(super) fn prepared(&self) -> &PreparedInputMutation {
        &self.prepared
    }
    pub(super) fn intent(&self) -> &PreparedTaskStop {
        &self.intent
    }
    pub(super) fn claim_worker(self: &Arc<Self>) -> Option<OriginalTaskStopWorker> {
        self.worker_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| OriginalTaskStopWorker { plan: self.clone() })
    }
    fn record_publication(&self, record: &TaskStopRecord) -> Result<()> {
        ensure!(
            self.matches(record),
            "TaskStop publication differs from original local plan"
        );
        let mut binding = self
            .published
            .lock()
            .map_err(|_| anyhow::anyhow!("TaskStop publication poisoned"))?;
        if let Some(original) = binding.as_ref() {
            ensure!(
                original == &record.receipt_binding(),
                "TaskStop original publication changed"
            );
        } else {
            *binding = Some(record.receipt_binding());
        }
        Ok(())
    }
    fn matches(&self, record: &TaskStopRecord) -> bool {
        self.prepared == record.prepared && self.intent == record.intent
    }
}

/// Only the worker which observed its original SQL attempt end can mint this
/// local recovery address. It grants neither publication nor a physical signal.
pub(crate) struct FinishedOriginalTaskStopAttempt {
    plan: Arc<PreparedTaskStopEffects>,
}
impl FinishedOriginalTaskStopAttempt {
    pub(crate) fn prepared(&self) -> &PreparedInputMutation {
        &self.plan.prepared
    }
    pub(crate) fn intent(&self) -> &PreparedTaskStop {
        &self.plan.intent
    }
}

/// Coalesces one original descriptor, never independent Cancel/Force commands.
pub(super) struct OriginalTaskStopWorker {
    plan: Arc<PreparedTaskStopEffects>,
}
impl Drop for OriginalTaskStopWorker {
    fn drop(&mut self) {
        self.plan.worker_active.store(false, Ordering::Release);
    }
}

type OriginalKey = (String, u64, String);
fn key(prepared: &PreparedInputMutation, intent: &PreparedTaskStop) -> OriginalKey {
    (
        intent.task.scope.agent_id.clone(),
        intent.task.scope.identity_incarnation,
        prepared.command_nonce().to_owned(),
    )
}

#[derive(Default)]
pub(super) struct TaskStopEffectRegistry {
    prepared: Mutex<BTreeMap<OriginalKey, Arc<PreparedTaskStopEffects>>>,
}
impl TaskStopEffectRegistry {
    /// An unknown SQL result retains this exact plan without signalling it.
    pub(super) fn retain_before_sql(&self, plan: Arc<PreparedTaskStopEffects>) -> Result<()> {
        let mut prepared = self
            .prepared
            .lock()
            .map_err(|_| anyhow::anyhow!("TaskStop plans poisoned"))?;
        let address = key(&plan.prepared, &plan.intent);
        if let Some(existing) = prepared.get(&address) {
            ensure!(
                Arc::ptr_eq(existing, &plan),
                "TaskStop original plan was replaced"
            );
        } else {
            prepared.insert(address, plan);
        }
        Ok(())
    }

    pub(super) fn find_original(
        &self,
        record: &TaskStopRecord,
    ) -> Result<Option<Arc<PreparedTaskStopEffects>>> {
        let plans = self
            .prepared
            .lock()
            .map_err(|_| anyhow::anyhow!("TaskStop plans poisoned"))?;
        let plan = plans.get(&key(&record.prepared, &record.intent));
        if let Some(plan) = plan {
            ensure!(
                plan.matches(record),
                "TaskStop retained original plan differs"
            );
        }
        Ok(plan.cloned())
    }
    /// Only reuses the positive publication recorded by confirm_published. The
    /// caller owns a fresh, exact, whole-receipt-validated record body.
    pub(super) fn resume_published(
        &self,
        record: &Arc<TaskStopRecord>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Option<PublishedTaskStopEffects>> {
        let Some(plan) = self.find_original(record)? else {
            return Ok(None);
        };
        let published = plan
            .published
            .lock()
            .map_err(|_| anyhow::anyhow!("TaskStop publication poisoned"))?;
        let Some(binding) = published.as_ref() else {
            return Ok(None);
        };
        ensure!(
            binding == &record.receipt_binding(),
            "TaskStop retained publication differs"
        );
        drop(published);
        let view = ledger.register_task_stop_view(
            &record.intent.task.scope,
            InputMemoryCategory::Pins,
            std::iter::once(record),
        )?;
        Ok(Some(PublishedTaskStopEffects {
            plan,
            record: record.clone(),
            _view: view,
        }))
    }
    /// A current owner may positively publish the original ACK after epoch
    /// replacement. It grants only the already frozen, exact old local lease.
    pub(super) fn confirm_original_receipt_publication(
        &self,
        reservation: &super::mutation_owner::MutationReservation,
        expected: &crate::host::OriginalTaskStopMetadataRead,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<PublishedTaskStopEffects> {
        let record = expected.record();
        let root = reservation
            .capture_ready_snapshot()?
            .ok_or(super::resident_task::TaskReadError::IncompleteActive)?;
        ensure!(
            reservation.agent_id() == record.intent.task.scope.agent_id
                && reservation.identity_incarnation()
                    == record.intent.task.scope.identity_incarnation
                && root.token.owner_epoch == reservation.owner_epoch()
                && root.token.identity_incarnation == reservation.identity_incarnation(),
            "original Stop publication belongs to another current owner"
        );
        let tasks = root
            .tasks
            .as_ref()
            .ok_or(super::resident_task::TaskReadError::IncompleteActive)?;
        ensure!(
            tasks.scope() == &record.intent.task.scope,
            "original Stop current publication namespace differs"
        );
        let captured = tasks.capture_pending_stops()?;
        let actual = captured
            .pending_stops()?
            .find(|entry| entry.effect_seq == record.effect_seq)
            .context("current publication omits original pending Stop")?;
        ensure!(
            actual.as_ref() == record,
            "current publication differs from fresh original whole ACK"
        );
        let plan = self
            .find_original(record)?
            .context("published original Stop local plan unavailable")?;
        plan.record_publication(record)?;
        let view = ledger.register_task_stop_view(
            &record.intent.task.scope,
            InputMemoryCategory::Pins,
            std::iter::once(actual),
        )?;
        Ok(PublishedTaskStopEffects {
            plan,
            record: actual.clone(),
            _view: view,
        })
    }

    pub(super) fn retained_plans(
        &self,
        ledger: &InputEvidenceBudgetLedger,
        scope: &super::resident_input_evidence::InputEvidenceScope,
    ) -> Result<(Vec<Arc<PreparedTaskStopEffects>>, Arc<BudgetViewGuard>)> {
        let plans = self
            .prepared
            .lock()
            .map_err(|_| anyhow::anyhow!("TaskStop plans poisoned"))?;
        let count = plans.len();
        let bytes = u64::try_from(count)?
            .checked_mul(64)
            .context("TaskStop retained selectors overflow")?;
        let limits = ledger.limits();
        ensure!(
            u64::try_from(count)? <= limits.record_limit
                && bytes
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("TaskStop selector limits overflow")?,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "retained Stop selectors"
            )
        );
        let guard = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        Ok((plans.values().cloned().collect(), guard))
    }

    /// Capture itself rejects unknown/replaced owners. Only a published pending
    /// record can grant the capability below, never a SQL DTO or a boolean.
    pub(super) fn confirm_published(
        &self,
        owner: &MutationOwner,
        expected: &TaskStopRecord,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<PublishedTaskStopEffects> {
        let root = owner
            .capture()?
            .context("TaskStop owner has no published root")?;
        ensure!(
            root.token.identity_incarnation == expected.intent.task.scope.identity_incarnation
                && root.token.owner_epoch == expected.intent.owner_epoch
                && owner.owner_epoch() == expected.intent.owner_epoch,
            "TaskStop published owner differs from original epoch"
        );
        let tasks = root
            .tasks
            .as_ref()
            .context("TaskStop domain requires preparation")?;
        ensure!(
            tasks.scope() == &expected.intent.task.scope,
            "TaskStop published namespace differs"
        );
        let capture = tasks.capture_pending_stops()?;
        let current = capture
            .pending_stops()?
            .find(|record| record.effect_seq == expected.effect_seq)
            .context("TaskStop original effect is not published as pending")?;
        ensure!(
            current.prepared == expected.prepared
                && current.intent == expected.intent
                && current.intent_digest == expected.intent_digest
                && current.generation_digest == expected.generation_digest
                && current.ack_task_digest == expected.ack_task_digest,
            "TaskStop published original ACK differs"
        );
        let plan = self
            .prepared
            .lock()
            .map_err(|_| anyhow::anyhow!("TaskStop plans poisoned"))?
            .get(&key(&expected.prepared, &expected.intent))
            .cloned()
            .context("TaskStop original local plan unavailable")?;
        ensure!(
            plan.matches(current),
            "TaskStop published ACK differs from retained plan"
        );
        let view = ledger.register_task_stop_view(
            &expected.intent.task.scope,
            InputMemoryCategory::Pins,
            std::iter::once(current),
        )?;
        plan.record_publication(current)?;
        Ok(PublishedTaskStopEffects {
            plan,
            record: current.clone(),
            _view: view,
        })
    }

    /// The caller has resolved the finished original attempt as definitely absent.
    pub(super) fn release_original_absence(
        &self,
        plan: &Arc<PreparedTaskStopEffects>,
    ) -> Result<()> {
        self.release(plan)
    }
    pub(super) fn release_finished(&self, plan: &Arc<PreparedTaskStopEffects>) -> Result<()> {
        ensure!(
            plan.finished.load(Ordering::Acquire),
            "TaskStop original plan is unfinished"
        );
        self.release(plan)
    }
    fn release(&self, plan: &Arc<PreparedTaskStopEffects>) -> Result<()> {
        let mut prepared = self
            .prepared
            .lock()
            .map_err(|_| anyhow::anyhow!("TaskStop plans poisoned"))?;
        let address = key(&plan.prepared, &plan.intent);
        if let Some(existing) = prepared.get(&address) {
            ensure!(
                Arc::ptr_eq(existing, plan),
                "TaskStop release differs from original plan"
            );
            prepared.remove(&address);
        }
        Ok(())
    }
}

/// Private, nonserializable proof that the original ACK was installed in memory.
pub(super) struct PublishedTaskStopEffects {
    plan: Arc<PreparedTaskStopEffects>,
    record: Arc<TaskStopRecord>,
    _view: Arc<BudgetViewGuard>,
}
impl PublishedTaskStopEffects {
    #[cfg(test)]
    pub(super) fn record(&self) -> &Arc<TaskStopRecord> {
        &self.record
    }
    pub(super) fn signal_original(&self) {
        if self.plan.signalled.swap(true, Ordering::AcqRel) {
            return;
        }
        match &self.plan.signal {
            OriginalSignal::Cancel(signal) | OriginalSignal::Force(signal) => {
                let _ = signal.clone().send(());
            }
            OriginalSignal::Abort => {
                if let Some(lease) = &self.plan.lease {
                    lease.cancel();
                }
            }
            OriginalSignal::Missing => {}
        }
    }
    /// Joining a monitor proves neither external-process nor child exit.
    pub(super) async fn join_original(
        &self,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Option<TaskStopMonitorJoin>> {
        match &self.plan.lease {
            Some(lease) => TaskStopMonitorJoin::join_original(&self.record, lease.clone(), ledger)
                .await
                .map(Some),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
#[path = "task_stop_epoch_fixture.rs"]
mod host_epoch_fixture;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::StateMemoryLimits,
        runtime::{
            command_task::CommandTaskHandle,
            control_cancellation::ControlCancellationRegistry,
            resident_input_evidence::InputEvidenceScope,
            resident_task::{TaskFactLocator, TaskFamily, TaskKey},
        },
        runtime_db::{
            input_mutation_receipts::canonical_command_digest, task_stop::TaskStopProgress,
        },
        types::TaskStatus,
    };
    use tokio::sync::{mpsc, oneshot};

    fn task() -> TaskRecord {
        let at = "2026-10-01T00:00:00.123Z".parse().unwrap();
        TaskRecord {
            id: "same-task-id".into(),
            agent_id: "stop-owner".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: Some(serde_json::json!({"cancel_requested":false})),
            recovery: None,
        }
    }
    fn handle(
        task: &TaskRecord,
    ) -> (
        ManagedTaskHandle,
        oneshot::Receiver<()>,
        oneshot::Receiver<()>,
    ) {
        let (cancel, cancel_rx) = oneshot::channel();
        let (force, force_rx) = oneshot::channel();
        let cancel = SharedControlSignal::new(cancel);
        let force = SharedControlSignal::new(force);
        let lease =
            ControlCancellationRegistry::default().command(task, cancel.clone(), force.clone());
        let (input_tx, _) = mpsc::channel(1);
        (
            ManagedTaskHandle::Command(CommandTaskHandle {
                cancel_tx: Some(cancel),
                force_stop_tx: Some(force),
                input_tx,
                control_lease: Some(lease),
            }),
            cancel_rx,
            force_rx,
        )
    }
    fn intent(
        task: &TaskRecord,
        handle: &ManagedTaskHandle,
        signal: TaskStopSignal,
    ) -> Result<(PreparedInputMutation, PreparedTaskStop)> {
        let scope = InputEvidenceScope {
            agent_id: task.agent_id.clone(),
            identity_incarnation: 1,
        };
        let intent = PreparedTaskStop {
            task: TaskFactLocator {
                scope: scope.clone(),
                key: TaskKey::new(TaskFamily::Task, &task.id),
                revision: 1,
                content_digest: "a".repeat(64),
                projection_digest: "b".repeat(64),
            },
            kind: task.kind,
            created_at: task.created_at,
            rejoin: None,
            owner_epoch: 1,
            signal,
            lease_id: handle
                .control_lease()
                .map(|lease| lease.lease_id().to_owned()),
            child: None,
            child_unavailable: None,
            worktree: None,
            worktree_unavailable: None,
            result: None,
        };
        let prepared = PreparedInputMutation::new(
            InputMutationFamily::Task,
            "original-stop".into(),
            vec![scope],
            canonical_command_digest(&intent)?,
            task.updated_at,
        )?;
        Ok((prepared, intent))
    }
    // Routing tests use a local capability fixture. The separate unpublished
    // test exercises the production capability factory's refusal.
    fn routing_fixture(
        plan: Arc<PreparedTaskStopEffects>,
        task: &TaskRecord,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<PublishedTaskStopEffects> {
        let mut ack = task.clone();
        ack.status = TaskStatus::Cancelling;
        ack.detail.as_mut().unwrap()["cancel_requested"] = serde_json::json!(true);
        if plan.intent.signal == TaskStopSignal::Force {
            ack.detail.as_mut().unwrap()["force_stop_requested"] = serde_json::json!(true);
        }
        let record = Arc::new(TaskStopRecord {
            effect_seq: 1,
            prepared: plan.prepared.clone(),
            intent: plan.intent.clone(),
            intent_digest: canonical_command_digest(&plan.intent)?,
            generation_digest: plan.intent.generation_digest()?,
            ack_task_digest: canonical_command_digest(&ack)?,
            ack_task: ack,
            revision: 1,
            progress: TaskStopProgress::initial(&plan.intent),
            result_nonce: None,
            updated_at: task.updated_at,
        });
        let view = ledger.register_task_stop_view(
            &plan.intent.task.scope,
            InputMemoryCategory::Pins,
            std::iter::once(&record),
        )?;
        Ok(PublishedTaskStopEffects {
            plan,
            record,
            _view: view,
        })
    }
    #[tokio::test]
    async fn retaining_unknown_stop_does_not_signal_and_unpublished_dto_cannot_confirm(
    ) -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let task = task();
        let (handle, mut cancel, mut force) = handle(&task);
        let (prepared, intent) = intent(&task, &handle, TaskStopSignal::Cancel)?;
        let plan =
            PreparedTaskStopEffects::freeze(&prepared, &intent, &task, Some(&handle), &ledger)?;
        let registry = TaskStopEffectRegistry::default();
        registry.retain_before_sql(plan.clone())?;
        let fixture = routing_fixture(plan.clone(), &task, &ledger)?;
        let owner = MutationOwner::standalone(&task.agent_id, 1);
        assert!(registry
            .confirm_published(&owner, fixture.record(), &ledger)
            .is_err());
        assert!(matches!(
            cancel.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            force.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(ledger.report()?.observed_bytes > 0);
        registry.release_original_absence(&plan)?;
        drop(fixture);
        drop(plan);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[tokio::test]
    async fn cancel_retry_never_escalates_and_force_uses_only_its_original_sender() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        for selected in [TaskStopSignal::Cancel, TaskStopSignal::Force] {
            let task = task();
            let (original, mut cancel, mut force) = handle(&task);
            let (_successor, mut successor_cancel, mut successor_force) = handle(&task);
            let (prepared, intent) = intent(&task, &original, selected)?;
            let plan = PreparedTaskStopEffects::freeze(
                &prepared,
                &intent,
                &task,
                Some(&original),
                &ledger,
            )?;
            let published = routing_fixture(plan, &task, &ledger)?;
            published.signal_original();
            published.signal_original();
            match selected {
                TaskStopSignal::Cancel => {
                    assert_eq!(cancel.try_recv(), Ok(()));
                    assert!(matches!(
                        force.try_recv(),
                        Err(oneshot::error::TryRecvError::Empty)
                    ));
                }
                TaskStopSignal::Force => {
                    assert_eq!(force.try_recv(), Ok(()));
                    assert!(matches!(
                        cancel.try_recv(),
                        Err(oneshot::error::TryRecvError::Empty)
                    ));
                }
                _ => unreachable!(),
            }
            assert!(matches!(
                successor_cancel.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            assert!(matches!(
                successor_force.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[tokio::test]
    async fn same_id_successor_lease_cannot_replace_the_frozen_original_plan() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let task = task();
        let (original, _, _) = handle(&task);
        let (successor, _, _) = handle(&task);
        let (prepared, intent) = intent(&task, &original, TaskStopSignal::Cancel)?;
        assert!(PreparedTaskStopEffects::freeze(
            &prepared,
            &intent,
            &task,
            Some(&successor),
            &ledger
        )
        .is_err());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let first =
            PreparedTaskStopEffects::freeze(&prepared, &intent, &task, Some(&original), &ledger)?;
        let replacement =
            PreparedTaskStopEffects::freeze(&prepared, &intent, &task, Some(&original), &ledger)?;
        let registry = TaskStopEffectRegistry::default();
        registry.retain_before_sql(first.clone())?;
        assert!(registry.retain_before_sql(replacement).is_err());
        assert!(registry.release_finished(&first).is_err());
        registry.release_original_absence(&first)?;
        drop(first);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
}
