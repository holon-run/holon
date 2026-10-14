//! Only a held, qualified Command registration can mint this producer capsule.
use super::super::{
    control_cancellation::ControlJoinLease,
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_task::{CanonicalTaskFact, QualifiedTaskFact},
    state_mutation::RuntimeMutation,
};
use crate::{
    runtime_db::{
        command_task_terminal::{CommandTaskTerminalAck, OriginalCommandTaskAddress},
        input_mutation_receipts::canonical_command_digest,
    },
    types::{MessageEnvelope, TaskKind, TaskRecord, TaskStatus},
};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use std::sync::{Arc, Mutex, Weak};

pub(crate) struct OriginalCommandTaskProducer {
    address: OriginalCommandTaskAddress,
    _address_material: Arc<BudgetViewGuard>,
    lease: Weak<ControlJoinLease>,
    source: Mutex<Option<OriginalCommandSource>>,
    pub(super) prepared:
        tokio::sync::Mutex<Option<Arc<crate::runtime_db::transitions::PreparedInputTransition>>>,
    notifications: Mutex<Option<OriginalCommandNotifications>>,
    terminal: Mutex<Option<Arc<FrozenCommandTaskTerminal>>>,
    finished: std::sync::atomic::AtomicBool,
}
struct OriginalCommandSource {
    fact: Arc<QualifiedTaskFact>,
    _view: Arc<BudgetViewGuard>,
    _owned: Arc<BudgetViewGuard>,
}
pub(super) struct OriginalCommandNotifications {
    pub(super) receipt: crate::runtime_db::input_mutation_receipts::InputMutationReceipt,
    _material: Arc<BudgetViewGuard>,
}
pub(crate) struct FrozenCommandTaskTerminal {
    pub(crate) nonce: String,
    pub(crate) at: DateTime<Utc>,
    pub(crate) task: TaskRecord,
    pub(crate) message: Option<MessageEnvelope>,
    fault: Mutex<Option<crate::runtime_db::transitions::TransitionFaultPoint>>,
    _material: Arc<BudgetViewGuard>,
}
impl OriginalCommandTaskProducer {
    /// Called by the original registration before its handle/monitor is published.
    pub(super) fn admitted(
        registration: &RuntimeMutation,
        qualified: &Arc<QualifiedTaskFact>,
        original: &TaskRecord,
        lease: &Arc<ControlJoinLease>,
        owner_epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Arc<Self>> {
        let scope = &qualified.locator.scope;
        registration.validate_original_command_source_scope(scope, owner_epoch)?;
        let CanonicalTaskFact::Task(task) = &qualified.fact else {
            anyhow::bail!("Command registration selected another family")
        };
        let bytes = serialized_bytes(&(
            qualified.as_ref(),
            original,
            lease.lease_id(),
            lease.domain_generation(),
            owner_epoch,
        ))?
        .checked_add(1024)
        .and_then(|n| n.checked_mul(8))
        .context("Command producer source material overflow")?;
        let limits = ledger.limits();
        ensure!(
            limits.record_limit >= 8
                && bytes
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("Command producer limit overflow")?
                        .min(64 * 1024 * 1024),
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "original Command producer"
            )
        );
        let owned = ledger.register_owned_bytes(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        qualified.validate(scope)?;
        ensure!(
            task.kind == TaskKind::CommandTask
                && original.id == task.id
                && original.agent_id == task.agent_id
                && original.kind == task.kind
                && original.created_at == task.created_at
                && original.work_item_id == task.work_item_id
                && super::super::task_generation::same_task_rejoin_generation(original, task)
                && matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
                )
                && lease.is_command_producer()
                && lease.target_id() == task.id
                && lease.matches_task_generation(task),
            "original Command producer crossed its admitted generation"
        );
        let address_bytes =
            serialized_bytes(&(scope, &qualified.locator, owner_epoch, lease.lease_id()))?
                .checked_add(256)
                .and_then(|n| n.checked_mul(4))
                .context("Command address material overflow")?;
        ensure!(
            address_bytes <= bytes,
            "Command address estimate exceeds its source bound"
        );
        let address_material = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: address_bytes,
            },
        )?;
        let view = ledger.register_task_view(
            &scope,
            InputMemoryCategory::Pins,
            std::iter::once(qualified),
        )?;
        let address = OriginalCommandTaskAddress {
            scope: scope.clone(),
            admitted: qualified.locator.clone(),
            owner_epoch,
            lease_id: lease.lease_id().into(),
            generation_digest: canonical_command_digest(&(
                task.id.as_str(),
                task.agent_id.as_str(),
                task.kind,
                task.created_at,
                task.work_item_id.as_ref(),
                task.rejoin_fence().ok(),
            ))?,
        };
        address.validate()?;
        Ok(Arc::new(Self {
            address,
            _address_material: address_material,
            lease: Arc::downgrade(lease),
            source: Mutex::new(Some(OriginalCommandSource {
                fact: qualified.clone(),
                _view: view,
                _owned: owned,
            })),
            prepared: tokio::sync::Mutex::new(None),
            notifications: Mutex::new(None),
            terminal: Mutex::new(None),
            finished: std::sync::atomic::AtomicBool::new(false),
        }))
    }
    pub(crate) fn address(&self) -> &OriginalCommandTaskAddress {
        &self.address
    }
    pub(in crate::runtime) fn is_durable_finished(&self) -> bool {
        self.finished.load(std::sync::atomic::Ordering::Acquire)
    }
    pub(in crate::runtime) fn original_lease(&self) -> Result<Arc<ControlJoinLease>> {
        let lease = self
            .lease
            .upgrade()
            .context("original Command lease unavailable")?;
        ensure!(
            lease.lease_id() == self.address.lease_id,
            "original Command lease replaced"
        );
        Ok(lease)
    }
    pub(super) fn freeze_terminal(
        &self,
        task: &TaskRecord,
        message: Option<&MessageEnvelope>,
        ledger: &InputEvidenceBudgetLedger,
        fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
    ) -> Result<Arc<FrozenCommandTaskTerminal>> {
        ensure!(
            matches!(
                task.status,
                TaskStatus::Completed
                    | TaskStatus::Failed
                    | TaskStatus::Cancelled
                    | TaskStatus::Interrupted
            ),
            "Command terminal capsule cannot admit active work"
        );
        let source_guard = self
            .source
            .lock()
            .map_err(|_| anyhow::anyhow!("Command producer source poisoned"))?;
        let mut slot = self
            .terminal
            .lock()
            .map_err(|_| anyhow::anyhow!("Command terminal capsule poisoned"))?;
        if let Some(original) = slot.as_ref() {
            // A failed/retried invocation never replaces an already-frozen body.
            return Ok(original.clone());
        }
        let source = source_guard
            .as_ref()
            .context("Command terminal already acknowledged")?;
        let CanonicalTaskFact::Task(before) = &source.fact.fact else {
            unreachable!()
        };
        let bytes = serialized_bytes(&(task, message, &self.address))?
            .checked_add(1024)
            .and_then(|n| n.checked_mul(8))
            .context("Command terminal frozen material overflow")?;
        let limits = ledger.limits();
        ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Command terminal limits overflow")?
                    .min(64 * 1024 * 1024)
                && limits.record_limit >= 16,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "original Command terminal"
            )
        );
        let material = ledger.register_owned_bytes(
            &self.address.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        ensure!(
            task.id == before.id
                && task.agent_id == before.agent_id
                && task.kind == before.kind
                && task.created_at == before.created_at
                && task.work_item_id == before.work_item_id
                && super::super::task_generation::same_task_rejoin_generation(task, before),
            "Command terminal changed original generation"
        );
        let frozen = Arc::new(FrozenCommandTaskTerminal {
            nonce: format!("command-terminal:{}", self.address.lease_id),
            at: task.updated_at,
            task: task.clone(),
            message: message.cloned(),
            fault: Mutex::new(fault),
            _material: material,
        });
        *slot = Some(frozen.clone());
        Ok(frozen)
    }
    pub(in crate::runtime) fn acknowledge(&self, ack: &CommandTaskTerminalAck) -> Result<()> {
        ensure!(
            ack.original == self.address,
            "Command terminal ACK crossed original producer"
        );
        let mut source = self
            .source
            .lock()
            .map_err(|_| anyhow::anyhow!("Command producer source poisoned"))?;
        self.terminal
            .lock()
            .map_err(|_| anyhow::anyhow!("Command terminal capsule poisoned"))?
            .take();
        source.take();
        self.finished
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}

/// Borrowed provenance only; the database must still prove its current existing
/// Task generation and qualification in the actual committing transaction.
pub(crate) struct OriginalCommandTaskProducerView<'a> {
    address: &'a OriginalCommandTaskAddress,
    source: std::sync::MutexGuard<'a, Option<OriginalCommandSource>>,
    _lease: Arc<ControlJoinLease>,
}
impl OriginalCommandTaskProducer {
    pub(crate) fn source_view(&self) -> Result<OriginalCommandTaskProducerView<'_>> {
        let lease = self
            .lease
            .upgrade()
            .context("original Command producer lease unavailable")?;
        ensure!(
            lease.lease_id() == self.address.lease_id && lease.is_command_producer(),
            "original Command producer lease changed"
        );
        let source = self
            .source
            .lock()
            .map_err(|_| anyhow::anyhow!("Command producer source poisoned"))?;
        ensure!(
            source
                .as_ref()
                .is_some_and(|row| row.fact.locator == self.address.admitted),
            "original Command producer admission source unavailable"
        );
        Ok(OriginalCommandTaskProducerView {
            address: &self.address,
            source,
            _lease: lease,
        })
    }
}
impl OriginalCommandTaskProducerView<'_> {
    pub(crate) fn address(&self) -> &OriginalCommandTaskAddress {
        self.address
    }
    pub(crate) fn admitted_task(&self) -> &TaskRecord {
        let CanonicalTaskFact::Task(task) = &self
            .source
            .as_ref()
            .expect("sealed producer source")
            .fact
            .fact
        else {
            unreachable!()
        };
        task
    }
    pub(crate) fn validate_original_generation(&self, actual: &TaskRecord) -> Result<()> {
        let before = self.admitted_task();
        ensure!(
            actual.id == before.id
                && actual.agent_id == before.agent_id
                && actual.kind == before.kind
                && actual.created_at == before.created_at
                && actual.work_item_id == before.work_item_id
                && super::super::task_generation::same_task_rejoin_generation(actual, before),
            "Command maintenance target crossed original admitted Task generation"
        );
        Ok(())
    }
}

impl std::fmt::Debug for OriginalCommandTaskProducer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginalCommandTaskProducer")
            .field("address", &self.address)
            .field("finished", &self.is_durable_finished())
            .finish_non_exhaustive()
    }
}

impl OriginalCommandTaskProducer {
    pub(crate) fn validate_terminal_digest(&self, digest: &str) -> Result<()> {
        let slot = self
            .terminal
            .lock()
            .map_err(|_| anyhow::anyhow!("Command terminal capsule poisoned"))?;
        let frozen = slot
            .as_ref()
            .context("original Command terminal not frozen")?;
        ensure!(
            canonical_command_digest(&(&frozen.task, &frozen.message, frozen.at))? == digest,
            "Command terminal changed its frozen original body"
        );
        Ok(())
    }
}
impl OriginalCommandTaskProducerView<'_> {
    pub(crate) fn validate_frozen(&self, frozen: &FrozenCommandTaskTerminal) -> Result<()> {
        self.validate_original_generation(&frozen.task)?;
        ensure!(
            frozen.nonce == format!("command-terminal:{}", self.address.lease_id)
                && frozen.at == frozen.task.updated_at,
            "Command terminal frozen address differs"
        );
        Ok(())
    }
}

impl std::fmt::Debug for FrozenCommandTaskTerminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrozenCommandTaskTerminal")
            .field("nonce", &self.nonce)
            .finish_non_exhaustive()
    }
}
impl OriginalCommandTaskProducer {
    pub(super) fn frozen_terminal(&self) -> Result<Option<Arc<FrozenCommandTaskTerminal>>> {
        Ok(self
            .terminal
            .lock()
            .map_err(|_| anyhow::anyhow!("Command terminal capsule poisoned"))?
            .clone())
    }
    pub(crate) fn retain_sparse_notifications(
        &self,
        proof: &crate::runtime_db::command_task_terminal::OriginalCommandTaskSparseRecovery,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        use crate::runtime_db::input_mutation_receipts::InputMutationResult;
        let InputMutationResult::CommandTaskTerminal { ack } = &proof.receipt().outcome.result
        else {
            anyhow::bail!("Command notification proof family differs")
        };
        ensure!(
            ack.original == self.address && !self.is_durable_finished(),
            "Command notification proof address differs"
        );
        let mut slot = self
            .notifications
            .lock()
            .map_err(|_| anyhow::anyhow!("Command notifications poisoned"))?;
        if let Some(original) = slot.as_ref() {
            ensure!(
                original.receipt == *proof.receipt(),
                "Command notification original receipt changed"
            );
            return Ok(());
        }
        let bytes = serialized_bytes(proof.receipt())?
            .checked_mul(8)
            .context("Command notification material overflow")?;
        let limits = ledger.limits();
        ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Command notification cap overflow")?
                    .min(64 * 1024 * 1024),
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "Command notifications"
            )
        );
        let guard = ledger.register_owned_bytes(
            &self.address.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        *slot = Some(OriginalCommandNotifications {
            receipt: proof.receipt().clone(),
            _material: guard,
        });
        Ok(())
    }
    pub(super) fn take_sparse_notifications(&self) -> Result<Option<OriginalCommandNotifications>> {
        Ok(self
            .notifications
            .lock()
            .map_err(|_| anyhow::anyhow!("Command notifications poisoned"))?
            .take())
    }
}

impl OriginalCommandTaskProducer {
    pub(crate) fn validate_terminal_message(
        &self,
        task: &TaskRecord,
        message: &MessageEnvelope,
        digest: &str,
    ) -> Result<()> {
        let slot = self
            .terminal
            .lock()
            .map_err(|_| anyhow::anyhow!("Command terminal capsule poisoned"))?;
        let frozen = slot
            .as_ref()
            .context("Command original terminal is not frozen")?;
        ensure!(
            canonical_command_digest(&(&frozen.task, &frozen.message, frozen.at))? == digest,
            "Command frozen terminal differs"
        );
        let mut expected = frozen
            .message
            .as_ref()
            .context("Command source has no frozen Message")?
            .clone();
        expected.normalize_admission_fields();
        if let Some(metadata) = expected.metadata.as_mut().and_then(|v| v.as_object_mut()) {
            metadata.insert("task_detail".into(), serde_json::to_value(&task.detail)?);
        }
        ensure!(
            &expected == message,
            "Command derived Message differs from original frozen fields"
        );
        Ok(())
    }
}

impl FrozenCommandTaskTerminal {
    pub(crate) fn take_fault(
        &self,
    ) -> Result<Option<crate::runtime_db::transitions::TransitionFaultPoint>> {
        Ok(self
            .fault
            .lock()
            .map_err(|_| anyhow::anyhow!("Command fault slot poisoned"))?
            .take())
    }
}
impl OriginalCommandTaskProducer {
    pub(super) fn runner_material(&self) -> Result<Arc<BudgetViewGuard>> {
        let source = self
            .source
            .lock()
            .map_err(|_| anyhow::anyhow!("Command original source poisoned"))?;
        Ok(source
            .as_ref()
            .context("Command original source released")?
            ._owned
            .clone())
    }
}
