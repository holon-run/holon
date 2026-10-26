//! An accepted child monitor keeps its exact lease and one terminal responsibility.
use super::{
    control_cancellation::ControlJoinLease,
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate, OwnedCompletionBudget,
    },
    resident_task::{CanonicalTaskFact, QualifiedTaskFact},
    state_mutation::RuntimeMutation,
};
use crate::{
    runtime_db::{
        child_task_terminal::{ChildTaskTerminalAck, OriginalChildTaskAddress},
        input_mutation_receipts::canonical_command_digest,
        repositories::AgentDeletionRequest,
        transitions::{PreparedInputTransition, TransitionFaultPoint},
    },
    types::{MessageEnvelope, TaskRecord, TaskStatus},
};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, Weak,
};

#[path = "child_monitor_binding.rs"]
mod monitor_binding;

#[path = "child_task_spawn.rs"]
mod original_spawn;
pub(crate) use original_spawn::OriginalChildSpawnNoStart;

pub(crate) struct OriginalChildTaskProducer {
    address: OriginalChildTaskAddress,
    lease: Weak<ControlJoinLease>,
    source: Mutex<Option<ChildTaskSource>>,
    terminal: Mutex<Option<Arc<FrozenChildTaskTerminal>>>,
    observed: Mutex<Option<Arc<ObservedChildTerminal>>>,
    completed_observation: Mutex<Option<Arc<CompletedChildTerminalObservation>>>,
    delegation:
        Mutex<Option<Arc<crate::runtime_db::child_task_terminal::OriginalChildDelegationChoice>>>,
    pub(in crate::runtime) prepared: tokio::sync::Mutex<Option<Arc<PreparedInputTransition>>>,
    completion: Mutex<Option<Arc<OwnedCompletionBudget>>>,
    notifications: Mutex<Option<OriginalChildNotifications>>,
    monitor_stage: Mutex<Option<Arc<ChildTaskMonitorStage>>>,
    binding: monitor_binding::MonitorBindingState,
    pub(in crate::runtime) monitor_worker: tokio::sync::Mutex<()>,
    spawn: Mutex<Option<Arc<original_spawn::OriginalChildSpawn>>>,
    failed_spawn: Mutex<Option<Arc<crate::host::ChildTaskSpawnFailure>>>,
    created_spawn_scope: Mutex<Option<crate::runtime::resident_input_evidence::InputEvidenceScope>>,
    finished: AtomicBool,
    deletion_rejected: AtomicBool,
    #[cfg(test)]
    monitor_failure_for_test: Mutex<Option<String>>,
    #[cfg(test)]
    terminal_deletion_rejection_for_test: Mutex<Option<String>>,
    _address_material: Arc<BudgetViewGuard>,
}
struct ChildTaskSource {
    fact: Arc<QualifiedTaskFact>,
    _view: Arc<BudgetViewGuard>,
    _owned: Arc<BudgetViewGuard>,
    target: Option<Arc<crate::runtime_db::child_task_terminal::OriginalChildTaskTarget>>,
}
/// The original completion body survives definite failure, Unknown and owner replacement.
/// This carrier owns no Runtime, root, storage or strong lease back-reference.
pub(crate) struct FrozenChildTaskTerminal {
    pub(crate) nonce: String,
    pub(crate) at: DateTime<Utc>,
    pub(crate) task: TaskRecord,
    pub(crate) message: MessageEnvelope,
    pub(crate) child_id: Option<String>,
    pub(crate) deletion: Option<AgentDeletionRequest>,
    pub(crate) audits: Vec<crate::types::AuditEvent>,
    material_bytes: usize,
    material_records: usize,
    fault: Mutex<Option<TransitionFaultPoint>>,
    _material: Arc<BudgetViewGuard>,
}
impl OriginalChildTaskProducer {
    pub(in crate::runtime) fn admitted(
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
            anyhow::bail!("child monitor selected another Task family")
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
        .context("child producer source material overflow")?;
        let records = completion_value_nodes(original.detail.as_ref())?
            .checked_add(8)
            .and_then(|n| n.checked_mul(8))
            .context("child source nodes overflow")?;
        let limits = ledger.limits();
        let node_bytes = u64::try_from(records)?
            .checked_mul(256)
            .context("child source node bytes overflow")?;
        ensure!(
            usize::try_from(limits.record_limit)? >= records
                && bytes
                    .checked_add(node_bytes)
                    .context("child source combined bytes overflow")?
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("child source limit overflow")?
                        .min(64 * 1024 * 1024),
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "original child producer"
            )
        );
        let owned = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: node_bytes,
            },
        )?;
        qualified.validate(scope)?;
        ensure!(
            (task.kind.is_child_agent() || task.kind == crate::types::TaskKind::ActorInvocation)
                && same_generation(original, task)
                && !lease.is_command_producer()
                && lease.target_id() == task.id
                && matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
                )
                && lease.matches_task_generation(task),
            "child monitor crossed its admitted generation"
        );
        let address_material = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: serialized_bytes(&(
                    &qualified.locator,
                    owner_epoch,
                    lease.lease_id(),
                ))?
                .checked_add(256)
                .and_then(|n| n.checked_mul(4))
                .context("child address material overflow")?,
            },
        )?;
        let view = ledger.register_task_view(
            scope,
            InputMemoryCategory::Pins,
            std::iter::once(qualified),
        )?;
        // Registered before the monitor can produce a terminal body. Growing this
        // same observation cannot discard an already accepted completion.
        let completion = ledger.reserve_owned_completion(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: 256,
            },
        )?;
        let address = OriginalChildTaskAddress {
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
            lease: Arc::downgrade(lease),
            source: Mutex::new(Some(ChildTaskSource {
                fact: qualified.clone(),
                _view: view,
                _owned: owned,
                target: None,
            })),
            terminal: Mutex::new(None),
            observed: Mutex::new(None),
            completed_observation: Mutex::new(None),
            delegation: Mutex::new(None),
            prepared: tokio::sync::Mutex::new(None),
            completion: Mutex::new(Some(Arc::new(completion))),
            notifications: Mutex::new(None),
            monitor_stage: Mutex::new(None),
            binding: Default::default(),
            monitor_worker: tokio::sync::Mutex::new(()),
            spawn: Mutex::new(None),
            failed_spawn: Mutex::new(None),
            created_spawn_scope: Mutex::new(None),
            finished: AtomicBool::new(false),
            deletion_rejected: AtomicBool::new(false),
            #[cfg(test)]
            monitor_failure_for_test: Mutex::new(None),
            #[cfg(test)]
            terminal_deletion_rejection_for_test: Mutex::new(None),
            _address_material: address_material,
        }))
    }
    pub(in crate::runtime) fn retain_monitor_stage(
        &self,
        task: TaskRecord,
        authority: crate::types::AuthorityClass,
        worktree: bool,
        recovered: bool,
        lifecycle: crate::types::AgentLifecycleDisposition,
        child: String,
        baseline: u64,
        delivery: Option<String>,
        detail: serde_json::Value,
    ) -> Result<()> {
        self.source_view()?.validate_generation(&task)?;
        ensure!(!child.is_empty(), "original monitor target empty");
        let mut slot = self.monitor_stage.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(stage) = slot.as_ref() {
            ensure!(
                stage.task == task
                    && stage.authority == authority
                    && stage.worktree == worktree
                    && stage.recovered == recovered
                    && stage.lifecycle == lifecycle
                    && stage.child == child
                    && stage.baseline == baseline
                    && stage.delivery == delivery
                    && stage.detail == detail,
                "original monitor stage changed"
            );
        } else {
            let bytes = serialized_bytes(&(
                &task, &authority, worktree, recovered, lifecycle, &child, baseline, &delivery,
                &detail,
            ))
            .unwrap_or(u64::MAX);
            let nodes = completion_value_nodes(Some(&detail))
                .unwrap_or(usize::MAX)
                .saturating_add(completion_value_nodes(task.detail.as_ref()).unwrap_or(usize::MAX));
            let material = self.completion_material()?;
            material.observe_material(
                bytes.saturating_mul(8),
                (nodes as u64).saturating_add(64).saturating_mul(8 * 256),
            );
            *slot = Some(Arc::new(ChildTaskMonitorStage {
                task,
                authority,
                worktree,
                recovered,
                lifecycle,
                child,
                baseline,
                delivery,
                detail,
                running_attempted: AtomicBool::new(false),
                running_status: Mutex::new(None),
                _material: material.guard(),
            }));
        }
        Ok(())
    }
    pub(in crate::runtime) fn monitor_stage(&self) -> Option<Arc<ChildTaskMonitorStage>> {
        self.monitor_stage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(in crate::runtime) fn validate_monitor_target_incarnation(
        &self,
        actual: Option<&super::resident_input_evidence::InputEvidenceScope>,
    ) -> Result<()> {
        if let Some(stage) = self.monitor_stage() {
            if let Some(expected) = stage
                .detail
                .get("target_identity_incarnation")
                .and_then(serde_json::Value::as_u64)
            {
                ensure!(
                    expected > 0
                        && actual.is_some_and(|scope| scope.agent_id == stage.child
                            && scope.identity_incarnation == expected),
                    "original monitor target incarnation changed before binding"
                );
            }
        }
        Ok(())
    }
    pub(in crate::runtime) fn retain_failed_spawn(
        &self,
        failure: &crate::host::ChildTaskSpawnFailure,
    ) -> Result<()> {
        let completion = self.completion_material()?;
        completion.observe_material(
            (failure.child_agent_id().len() as u64)
                .saturating_add(256)
                .saturating_mul(8),
            1024,
        );
        let mut slot = self.failed_spawn.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(saved) = slot.as_ref() {
            ensure!(
                saved.child_agent_id() == failure.child_agent_id(),
                "original failed child changed"
            );
        } else {
            *slot = Some(Arc::new(failure.clone()));
        }
        Ok(())
    }
    /// Observation from the real Host createidentity callback, never from an
    /// error DTO or latest-by-ID lookup. Its completion guard still owns it.
    pub(crate) fn original_created_spawn_scope(
        &self,
    ) -> Result<Option<crate::runtime::resident_input_evidence::InputEvidenceScope>> {
        Ok(self
            .created_spawn_scope
            .lock()
            .map_err(|_| anyhow::anyhow!("original created child scope poisoned"))?
            .clone())
    }
    pub(in crate::runtime) fn failed_spawn(
        &self,
    ) -> Option<Arc<crate::host::ChildTaskSpawnFailure>> {
        self.failed_spawn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn original_deletion_requested(&self) -> bool {
        !self.deletion_rejected.load(Ordering::Acquire)
    }
    pub(in crate::runtime) fn retain_deletion_rejection(&self) -> Result<()> {
        ensure!(
            self.frozen_terminal()?
                .as_ref()
                .is_some_and(|frozen| frozen.deletion.is_some()),
            "child fallback has no original deletion request"
        );
        ensure!(
            !self.deletion_rejected.swap(true, Ordering::AcqRel),
            "child original deletion was already rejected"
        );
        Ok(())
    }
    pub(in crate::runtime) fn runner_material(&self) -> Result<Arc<BudgetViewGuard>> {
        let source = self
            .source
            .lock()
            .map_err(|_| anyhow::anyhow!("child source material poisoned"))?;
        Ok(source
            .as_ref()
            .context("child source already acknowledged")?
            ._owned
            .clone())
    }
    pub(crate) fn address(&self) -> &OriginalChildTaskAddress {
        &self.address
    }
    pub(crate) fn is_durable_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
    pub(in crate::runtime) fn original_lease(&self) -> Result<Arc<ControlJoinLease>> {
        let lease = self
            .lease
            .upgrade()
            .context("original child lease unavailable")?;
        ensure!(
            lease.lease_id() == self.address.lease_id
                && lease.target_id() == self.address.admitted.key.id,
            "child original lease changed"
        );
        Ok(lease)
    }
    pub(in crate::runtime) fn completion_material(&self) -> Result<Arc<OwnedCompletionBudget>> {
        self.completion
            .lock()
            .map_err(|_| anyhow::anyhow!("child completion budget poisoned"))?
            .as_ref()
            .cloned()
            .context("child completion already acknowledged")
    }
    pub(in crate::runtime) fn has_terminal_responsibility(&self) -> bool {
        !self.is_durable_finished()
            && (self.original_child_spawn_pending()
                || self
                    .monitor_stage
                    .lock()
                    .map(|stage| stage.is_some())
                    .unwrap_or(true)
                || self.observed.lock().map(|s| s.is_some()).unwrap_or(true)
                || self.terminal.lock().map(|s| s.is_some()).unwrap_or(true))
    }
    /// Move the real outcome before the first cleanup/persistence await. Even a
    /// later registration rejection leaves this capsule in the original lease.
    pub(in crate::runtime) fn observe_terminal_result(
        &self,
        mut task: TaskRecord,
        mut message: MessageEnvelope,
        child_id: Option<String>,
        deletion: Option<AgentDeletionRequest>,
        worktree: bool,
    ) -> Result<()> {
        {
            let view = self.source_view()?;
            let original = view.admitted_task();
            ensure!(
                original.id == task.id
                    && original.agent_id == task.agent_id
                    && original.kind == task.kind
                    && original.created_at == task.created_at
                    && original.work_item_id == task.work_item_id
                    && original.recovery == task.recovery
                    && message.agent_id == original.agent_id
                    && message.work_item_id == original.work_item_id
                    && message.kind == crate::types::MessageKind::TaskResult
                    && message.task_id.as_deref() == Some(original.id.as_str())
                    && matches!(&message.origin,crate::types::MessageOrigin::Task{task_id} if task_id==&original.id),
                "original child terminal provenance changed before partial rejoin normalization"
            );
            if let Some(child) = child_id.as_deref() {
                ensure!(
                    view.target()
                        .is_some_and(|target| target.target_id() == child)
                        || self
                            .failed_spawn()
                            .is_some_and(|failure| failure.child_agent_id() == child),
                    "original child terminal has no exact observed target or Host failure"
                );
            }
            // Only the helper's generated Result turn may be removed from an
            // originally partial contract. It remains on the Message itself.
            let metadata = original
                .detail
                .as_ref()
                .and_then(serde_json::Value::as_object);
            let values = (
                metadata.and_then(|detail| detail.get("rejoin_obligation_id")),
                metadata.and_then(|detail| detail.get("rejoin_generation")),
                metadata.and_then(|detail| detail.get("parent_turn_id")),
            );
            self.completion_material()?.observe_material(
                serialized_bytes(&values)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(8),
                8 * 256,
            );
            normalize_original_partial_result_contract(original, &mut task, &mut message)?;
            view.validate_generation(&task)?;
        }
        let completion = self.completion_material()?;
        let bytes = serialized_bytes(&(&task, &message, &child_id, &deletion)).unwrap_or(u64::MAX);
        let nodes = completion_value_nodes(task.detail.as_ref())
            .unwrap_or(usize::MAX)
            .saturating_add(
                completion_value_nodes(message.metadata.as_ref()).unwrap_or(usize::MAX),
            );
        completion.observe_material(
            bytes.saturating_mul(8),
            (nodes as u64)
                .saturating_mul(128)
                .saturating_mul(8)
                .saturating_add(1024),
        );
        let mut slot = self.observed.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(Arc::new(ObservedChildTerminal {
                task,
                message,
                child_id,
                deletion,
                worktree,
                _material: completion.guard(),
            }));
        }
        self.monitor_stage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        Ok(())
    }
    pub(in crate::runtime) fn observed_terminal(&self) -> Option<Arc<ObservedChildTerminal>> {
        self.observed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(in crate::runtime) fn completed_terminal_observation(
        &self,
    ) -> Option<Arc<CompletedChildTerminalObservation>> {
        self.completed_observation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// No fallible audit/encoding step may intervene between a successful FS
    /// return and moving its actual outcome here. The reservation predates FS.
    pub(in crate::runtime) fn save_completed_terminal_observation(
        &self,
        task: TaskRecord,
        message: MessageEnvelope,
        child_id: Option<String>,
        deletion: Option<AgentDeletionRequest>,
        audits: Vec<crate::types::AuditEvent>,
        completion: Arc<OwnedCompletionBudget>,
    ) -> Arc<CompletedChildTerminalObservation> {
        let nodes = completion_value_nodes(task.detail.as_ref())
            .unwrap_or(usize::MAX)
            .saturating_add(
                completion_value_nodes(message.metadata.as_ref()).unwrap_or(usize::MAX),
            );
        let nodes = audits.iter().fold(nodes, |n, event| {
            n.saturating_add(completion_value_nodes(Some(&event.data)).unwrap_or(usize::MAX))
        });
        completion.observe_material(
            serialized_bytes(&(&task, &message, &audits))
                .unwrap_or(u64::MAX)
                .saturating_mul(8),
            (nodes as u64)
                .saturating_mul(128)
                .saturating_mul(8)
                .saturating_add(1024),
        );
        let mut slot = self
            .completed_observation
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert_with(|| {
            Arc::new(CompletedChildTerminalObservation {
                task,
                message,
                child_id,
                deletion,
                audits,
                _material: completion.guard(),
            })
        })
        .clone()
    }
    pub(crate) fn validate_material_ledger(
        &self,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        let completion = self
            .completion
            .lock()
            .map_err(|_| anyhow::anyhow!("child completion budget poisoned"))?;
        ensure!(
            completion
                .as_ref()
                .is_some_and(|budget| budget.matches_scope(&self.address.scope)
                    && budget.matches_ledger(ledger)),
            "child terminal material crossed its original scope/actual ledger"
        );
        Ok(())
    }
    pub(crate) fn source_view(&self) -> Result<OriginalChildTaskSourceView<'_>> {
        let lease = self
            .lease
            .upgrade()
            .context("original child monitor lease unavailable")?;
        ensure!(
            lease.lease_id() == self.address.lease_id
                && lease.target_id() == self.address.admitted.key.id
                && !lease.is_command_producer(),
            "original child monitor lease changed"
        );
        let source = self
            .source
            .lock()
            .map_err(|_| anyhow::anyhow!("child producer source poisoned"))?;
        ensure!(
            source
                .as_ref()
                .is_some_and(|s| s.fact.locator == self.address.admitted),
            "original child producer admission source unavailable"
        );
        Ok(OriginalChildTaskSourceView {
            address: &self.address,
            source,
            _lease: lease,
        })
    }
    pub(in crate::runtime) fn install_target(
        &self,
        target: Arc<crate::runtime_db::child_task_terminal::OriginalChildTaskTarget>,
    ) -> Result<()> {
        ensure!(
            target.original() == &self.address,
            "child target crossed original monitor"
        );
        let mut source = self
            .source
            .lock()
            .map_err(|_| anyhow::anyhow!("child producer source poisoned"))?;
        let source = source
            .as_mut()
            .context("child producer already acknowledged")?;
        if let Some(original) = source.target.as_ref() {
            ensure!(
                original.same_target(&target),
                "original child target replaced"
            );
        } else {
            source.target = Some(target)
        }
        Ok(())
    }
    pub(in crate::runtime) fn freeze_terminal(
        &self,
        task: &TaskRecord,
        message: &MessageEnvelope,
        child_id: Option<&str>,
        deletion: Option<&AgentDeletionRequest>,
        audits: &[crate::types::AuditEvent],
        fault: Option<TransitionFaultPoint>,
    ) -> Result<Arc<FrozenChildTaskTerminal>> {
        let view = self.source_view()?;
        view.validate_generation(task)?;
        ensure!(
            super::task_state_reducer::is_terminal_task_status(&task.status),
            "child terminal cannot admit active work"
        );
        ensure!(
            message.agent_id == task.agent_id
                && message.task_id.as_deref() == Some(task.id.as_str())
                && matches!(&message.origin,crate::types::MessageOrigin::Task{task_id} if task_id==&task.id)
                && message.work_item_id == task.work_item_id,
            "child terminal result differs from original Task"
        );
        if let Some(child) = child_id {
            let target = view
                .target()
                .context("child terminal lacks its actual monitor target")?;
            ensure!(
                target.target_id() == child,
                "child terminal changed its actual monitor target"
            );
            if let Some(id) = task
                .detail
                .as_ref()
                .and_then(|v| v.get("child_agent_id").or_else(|| v.get("target_agent_id")))
                .and_then(serde_json::Value::as_str)
            {
                ensure!(
                    id == child,
                    "child terminal changed its stored target association"
                );
            }
        }
        if let Some(deletion) = deletion {
            ensure!(
                child_id == Some(deletion.agent_id.as_str())
                    && matches!(&deletion.admission,
                crate::runtime_db::repositories::AgentDeletionAdmission::TerminalEphemeralChild{parent_agent_id,task_id}
                    if parent_agent_id==&task.agent_id && task_id==&task.id),
                "child terminal deletion differs from its original monitor"
            );
        }
        let mut slot = self
            .terminal
            .lock()
            .map_err(|_| anyhow::anyhow!("child terminal capsule poisoned"))?;
        if let Some(frozen) = slot.as_ref() {
            return Ok(frozen.clone());
        }
        ensure!(
            matches!(message.body, crate::types::MessageBody::Text { .. }),
            "child monitor terminal requires its actual text result"
        );
        let nodes = completion_value_nodes(task.detail.as_ref())?
            .checked_add(completion_value_nodes(message.metadata.as_ref())?)
            .and_then(|n| n.checked_add(64))
            .context("child terminal owned nodes overflow")?;
        let nodes = audits
            .iter()
            .try_fold(nodes, |n, event| {
                n.checked_add(completion_value_nodes(Some(&event.data))?)
                    .context("child terminal audit nodes overflow")
            })?
            .checked_mul(8)
            .context("child terminal node copies overflow")?;
        let bytes = serialized_bytes(&(task, message, child_id, deletion, audits, &self.address))?
            .checked_add(1024)
            .and_then(|n| n.checked_mul(8))
            .context("child terminal material overflow")?;
        let completion = self
            .completion
            .lock()
            .map_err(|_| anyhow::anyhow!("child completion budget poisoned"))?;
        let completion = completion
            .as_ref()
            .context("child completion already acknowledged")?;
        let bytes = bytes
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(128)
                    .context("child terminal node bytes overflow")?,
            )
            .context("child terminal combined bytes overflow")?;
        completion.observe_material(bytes, 512);
        let frozen = Arc::new(FrozenChildTaskTerminal {
            nonce: format!("child-terminal:{}", self.address.lease_id),
            at: task.updated_at,
            task: task.clone(),
            message: message.clone(),
            child_id: child_id.map(str::to_owned),
            deletion: deletion.cloned(),
            audits: audits.to_vec(),
            material_bytes: usize::try_from(bytes)?,
            material_records: nodes,
            fault: Mutex::new(fault),
            _material: completion.guard(),
        });
        *slot = Some(frozen.clone());
        Ok(frozen)
    }
    pub(crate) fn validate_terminal_digest(&self, digest: &str) -> Result<()> {
        let slot = self
            .terminal
            .lock()
            .map_err(|_| anyhow::anyhow!("child terminal capsule poisoned"))?;
        let frozen = slot
            .as_ref()
            .context("original child terminal is not frozen")?;
        ensure!(
            frozen.digest()? == digest,
            "child terminal changed its original body"
        );
        Ok(())
    }
    pub(crate) fn validate_terminal_message(
        &self,
        task: &TaskRecord,
        message: &MessageEnvelope,
        digest: &str,
    ) -> Result<()> {
        self.validate_terminal_digest(digest)?;
        let frozen = self
            .frozen_terminal()?
            .context("child terminal body missing")?;
        ensure!(
            message.id == frozen.message.id
                && message.agent_id == self.address.scope.agent_id
                && message.task_id.as_deref() == Some(task.id.as_str())
                && message.work_item_id == task.work_item_id
                && message.body == frozen.message.body
                && message.turn_id == frozen.message.turn_id
                && message.created_at == frozen.message.created_at
                && message.origin == frozen.message.origin,
            "child terminal changed its original Result body/address"
        );
        Ok(())
    }
    pub(crate) fn frozen_terminal(&self) -> Result<Option<Arc<FrozenChildTaskTerminal>>> {
        Ok(self
            .terminal
            .lock()
            .map_err(|_| anyhow::anyhow!("child terminal capsule poisoned"))?
            .clone())
    }
    pub(crate) fn delegation_choice(
        &self,
    ) -> Result<Option<Arc<crate::runtime_db::child_task_terminal::OriginalChildDelegationChoice>>>
    {
        Ok(self
            .delegation
            .lock()
            .map_err(|_| anyhow::anyhow!("child delegation capsule poisoned"))?
            .clone())
    }
    pub(crate) fn install_delegation_choice(
        &self,
        choice: Arc<crate::runtime_db::child_task_terminal::OriginalChildDelegationChoice>,
    ) -> Result<Arc<crate::runtime_db::child_task_terminal::OriginalChildDelegationChoice>> {
        ensure!(
            choice.original() == &self.address,
            "child delegation crossed original monitor"
        );
        self.validate_terminal_digest(choice.seed_digest())?;
        let mut saved = self
            .delegation
            .lock()
            .map_err(|_| anyhow::anyhow!("child delegation capsule poisoned"))?;
        if let Some(original) = saved.as_ref() {
            ensure!(
                Arc::ptr_eq(original, &choice),
                "original child delegation choice replaced"
            );
            return Ok(original.clone());
        }
        *saved = Some(choice.clone());
        Ok(choice)
    }
    pub(in crate::runtime) fn retain_sparse_notifications(
        &self,
        proof: &crate::runtime_db::child_task_terminal::OriginalChildTerminalRecovery,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        self.validate_material_ledger(ledger)?;
        if proof.mode(&self.address.scope)?
            == crate::runtime_db::child_task_terminal::ChildTaskTerminalInstallMode::Current
        {
            return Ok(());
        }
        let mut slot = self
            .notifications
            .lock()
            .map_err(|_| anyhow::anyhow!("child notification capsule poisoned"))?;
        if let Some(original) = slot.as_ref() {
            ensure!(
                original.receipt == *proof.receipt(),
                "child original notification receipt changed"
            );
            return Ok(());
        }
        let bytes = serialized_bytes(proof.receipt())?
            .checked_mul(8)
            .context("child notification material overflow")?;
        let completion = self
            .completion
            .lock()
            .map_err(|_| anyhow::anyhow!("child completion budget poisoned"))?;
        let completion = completion
            .as_ref()
            .context("child completion already acknowledged")?;
        completion.observe_material(bytes, 512);
        *slot = Some(OriginalChildNotifications {
            receipt: proof.receipt().clone(),
            _material: completion.guard(),
        });
        Ok(())
    }
    pub(in crate::runtime) fn take_sparse_notifications(
        &self,
    ) -> Result<Option<OriginalChildNotifications>> {
        Ok(self
            .notifications
            .lock()
            .map_err(|_| anyhow::anyhow!("child notification capsule poisoned"))?
            .take())
    }
    pub(in crate::runtime) fn acknowledge(&self, ack: &ChildTaskTerminalAck) -> Result<()> {
        ensure!(
            ack.original == self.address,
            "child terminal ACK crossed original producer"
        );
        let mut source = self
            .source
            .lock()
            .map_err(|_| anyhow::anyhow!("child producer source poisoned"))?;
        self.terminal
            .lock()
            .map_err(|_| anyhow::anyhow!("child terminal capsule poisoned"))?
            .take();
        source.take();
        self.monitor_stage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.spawn
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        self.failed_spawn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.created_spawn_scope
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.observed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.completed_observation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.delegation
            .lock()
            .map_err(|_| anyhow::anyhow!("child delegation capsule poisoned"))?
            .take();
        self.completion
            .lock()
            .map_err(|_| anyhow::anyhow!("child completion budget poisoned"))?
            .take();
        self.finished.store(true, Ordering::Release);
        Ok(())
    }
}
pub(crate) struct OriginalChildTaskSourceView<'a> {
    address: &'a OriginalChildTaskAddress,
    source: std::sync::MutexGuard<'a, Option<ChildTaskSource>>,
    _lease: Arc<ControlJoinLease>,
}
impl OriginalChildTaskSourceView<'_> {
    pub(crate) fn address(&self) -> &OriginalChildTaskAddress {
        self.address
    }
    pub(crate) fn target(
        &self,
    ) -> Option<&crate::runtime_db::child_task_terminal::OriginalChildTaskTarget> {
        self.source
            .as_ref()
            .and_then(|source| source.target.as_deref())
    }
    pub(crate) fn admitted_task(&self) -> &TaskRecord {
        let CanonicalTaskFact::Task(task) =
            &self.source.as_ref().expect("sealed child source").fact.fact
        else {
            unreachable!()
        };
        task
    }
    pub(crate) fn validate_generation(&self, actual: &TaskRecord) -> Result<()> {
        ensure!(
            same_generation(self.admitted_task(), actual),
            "child terminal crossed original Task generation"
        );
        Ok(())
    }
    pub(crate) fn validate_frozen(&self, frozen: &FrozenChildTaskTerminal) -> Result<()> {
        self.validate_generation(&frozen.task)?;
        ensure!(
            frozen.nonce == format!("child-terminal:{}", self.address.lease_id)
                && frozen.at == frozen.task.updated_at,
            "child terminal frozen address differs"
        );
        Ok(())
    }
}
fn same_generation(before: &TaskRecord, actual: &TaskRecord) -> bool {
    before.id == actual.id
        && before.agent_id == actual.agent_id
        && before.kind == actual.kind
        && before.created_at == actual.created_at
        && before.work_item_id == actual.work_item_id
        && same_original_child_rejoin_contract(before, actual)
}
fn same_original_child_rejoin_contract(before: &TaskRecord, actual: &TaskRecord) -> bool {
    super::task_generation::same_task_rejoin_generation(before, actual)
}
impl FrozenChildTaskTerminal {
    pub(crate) fn material_estimate(
        &self,
    ) -> crate::runtime_db::task_snapshot::TaskMaintenanceMaterial {
        crate::runtime_db::task_snapshot::TaskMaintenanceMaterial {
            bytes: self.material_bytes,
            records: self.material_records,
        }
    }
    pub(crate) fn digest(&self) -> Result<String> {
        canonical_command_digest(&(
            &self.task,
            &self.message,
            &self.child_id,
            &self.deletion,
            &self.audits,
            self.at,
        ))
    }
    pub(crate) fn take_fault(&self) -> Result<Option<TransitionFaultPoint>> {
        Ok(self
            .fault
            .lock()
            .map_err(|_| anyhow::anyhow!("child terminal fault poisoned"))?
            .take())
    }
}
impl std::fmt::Debug for OriginalChildTaskProducer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginalChildTaskProducer")
            .field("address", &self.address)
            .field("finished", &self.is_durable_finished())
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for FrozenChildTaskTerminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrozenChildTaskTerminal")
            .field("nonce", &self.nonce)
            .field("at", &self.at)
            .finish_non_exhaustive()
    }
}

pub(in crate::runtime) fn completion_value_nodes(
    value: Option<&serde_json::Value>,
) -> Result<usize> {
    fn visit(value: &serde_json::Value) -> Result<usize> {
        let children = match value {
            serde_json::Value::Array(values) => values.iter().try_fold(0usize, |n, v| {
                n.checked_add(visit(v)?)
                    .context("child terminal node overflow")
            })?,
            serde_json::Value::Object(values) => values.values().try_fold(0usize, |n, v| {
                n.checked_add(visit(v)?)
                    .context("child terminal node overflow")
            })?,
            _ => 0,
        };
        children
            .checked_add(1)
            .context("child terminal node overflow")
    }
    value.map(visit).transpose().map(|n| n.unwrap_or(0))
}

pub(in crate::runtime) struct OriginalChildNotifications {
    pub(in crate::runtime) receipt:
        crate::runtime_db::input_mutation_receipts::InputMutationReceipt,
    _material: Arc<BudgetViewGuard>,
}

pub(in crate::runtime) struct ObservedChildTerminal {
    pub(in crate::runtime) task: TaskRecord,
    pub(in crate::runtime) message: MessageEnvelope,
    pub(in crate::runtime) child_id: Option<String>,
    pub(in crate::runtime) deletion: Option<AgentDeletionRequest>,
    pub(in crate::runtime) worktree: bool,
    _material: Arc<BudgetViewGuard>,
}
pub(in crate::runtime) struct CompletedChildTerminalObservation {
    pub(in crate::runtime) task: TaskRecord,
    pub(in crate::runtime) message: MessageEnvelope,
    pub(in crate::runtime) child_id: Option<String>,
    pub(in crate::runtime) deletion: Option<AgentDeletionRequest>,
    pub(in crate::runtime) audits: Vec<crate::types::AuditEvent>,
    _material: Arc<BudgetViewGuard>,
}

/// Exact actual monitor parameters, saved before its first fallible operation.
/// Responsibility only: the original DB binding still grants reads.
pub(in crate::runtime) struct ChildTaskMonitorStage {
    pub(in crate::runtime) task: TaskRecord,
    pub(in crate::runtime) authority: crate::types::AuthorityClass,
    pub(in crate::runtime) worktree: bool,
    pub(in crate::runtime) recovered: bool,
    pub(in crate::runtime) lifecycle: crate::types::AgentLifecycleDisposition,
    pub(in crate::runtime) child: String,
    pub(in crate::runtime) baseline: u64,
    pub(in crate::runtime) delivery: Option<String>,
    pub(in crate::runtime) detail: serde_json::Value,
    running_attempted: AtomicBool,
    running_status: Mutex<Option<(String, String)>>,
    _material: Arc<BudgetViewGuard>,
}
impl ChildTaskMonitorStage {
    pub(in crate::runtime) fn claim_running_attempt(&self) -> bool {
        !self.running_attempted.swap(true, Ordering::AcqRel)
    }
}

/// A local abort is not an actual child terminal result or a durable handoff.
#[derive(Debug, thiserror::Error)]
#[error("original child monitor cancelled before result; original responsibility remains pending")]
pub(in crate::runtime) struct OriginalChildMonitorCancellationPending;

fn original_running_status_digest(message: &MessageEnvelope) -> Result<String> {
    use sha2::{Digest, Sha256};
    struct HashWriter(Sha256);
    impl std::io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(
        &mut writer,
        &(
            (
                &message.id,
                &message.turn_id,
                &message.agent_id,
                message.created_at,
                &message.kind,
                &message.origin,
                &message.authority_class,
                &message.priority,
                &message.trigger_kind,
                &message.work_item_id,
            ),
            (
                &message.task_id,
                &message.source_refs,
                &message.trace_context,
                &message.body,
                &message.delivery_surface,
                &message.admission_context,
                &message.metadata,
                &message.correlation_id,
                &message.causation_id,
                &message.routing_context,
            ),
        ),
    )?;
    Ok(format!("{:x}", writer.0.finalize()))
}
impl OriginalChildTaskProducer {
    pub(in crate::runtime) fn freeze_original_running_status(
        &self,
        message: &MessageEnvelope,
    ) -> Result<()> {
        let stage = self
            .monitor_stage()
            .context("original Running status needs its admitted monitor stage")?;
        ensure!(
            message.kind == crate::types::MessageKind::TaskStatus
                && message.agent_id == self.address.scope.agent_id
                && message.task_id.as_deref() == Some(stage.task.id.as_str())
                && matches!(&message.origin,crate::types::MessageOrigin::Task{task_id} if task_id==&stage.task.id)
                && message.authority_class == stage.authority
                && message.delivery_surface
                    == Some(crate::types::MessageDeliverySurface::TaskRejoin)
                && message.admission_context == Some(crate::types::AdmissionContext::RuntimeOwned)
                && message
                    .metadata
                    .as_ref()
                    .and_then(|v| v.get("task_created_at"))
                    == Some(&serde_json::to_value(stage.task.created_at)?),
            "original child Running status changed its accepted source"
        );
        let completion = self.completion_material()?;
        completion.observe_material(
            serialized_bytes(message)
                .unwrap_or(u64::MAX)
                .saturating_mul(8),
            u64::try_from(completion_value_nodes(message.metadata.as_ref())?)
                .unwrap_or(u64::MAX)
                .saturating_add(64)
                .saturating_mul(8 * 256),
        );
        let digest = original_running_status_digest(message)?;
        let mut slot = stage
            .running_status
            .lock()
            .map_err(|_| anyhow::anyhow!("original child Running status poisoned"))?;
        ensure!(
            slot.is_none(),
            "original child Running status already frozen"
        );
        *slot = Some((message.id.clone(), digest));
        Ok(())
    }
    pub(in crate::runtime) fn original_running_status_clock(
        &self,
        message: &MessageEnvelope,
    ) -> Result<Option<DateTime<Utc>>> {
        let Some(stage) = self.monitor_stage() else {
            return Ok(None);
        };
        let slot = stage
            .running_status
            .lock()
            .map_err(|_| anyhow::anyhow!("original child Running status poisoned"))?;
        let Some((id, digest)) = slot.as_ref().filter(|(id, _)| id == &message.id) else {
            return Ok(None);
        };
        ensure!(
            id == &message.id
                && message.agent_id == self.address.scope.agent_id
                && &original_running_status_digest(message)? == digest,
            "original child Running status differs from its frozen producer message"
        );
        Ok(Some(stage.task.created_at))
    }
}

/// Pure normalization is usable only after the private source checked the exact
/// admitted lease, Task/clock and actual target/failure above.
fn normalize_original_partial_result_contract(
    original: &TaskRecord,
    task: &mut TaskRecord,
    message: &mut MessageEnvelope,
) -> Result<bool> {
    let Some(before) = original
        .detail
        .as_ref()
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(false);
    };
    if before.contains_key("parent_turn_id")
        || before
            .get("rejoin_obligation_id")
            .and_then(serde_json::Value::as_str)
            != Some(original.id.as_str())
        || before
            .get("rejoin_generation")
            .and_then(serde_json::Value::as_u64)
            .is_none_or(|generation| generation == 0)
    {
        return Ok(false);
    }
    let detail = task
        .detail
        .as_mut()
        .and_then(serde_json::Value::as_object_mut)
        .context("original partial child Task detail missing")?;
    ensure!(
        ["rejoin_obligation_id", "rejoin_generation"]
            .iter()
            .all(|key| detail.get(*key) == before.get(*key)),
        "original partial child rejoin obligation/generation changed"
    );
    let parent = detail.get("parent_turn_id");
    ensure!(
        parent.is_none()
            || parent
                .and_then(serde_json::Value::as_str)
                .is_some_and(|parent| message.turn_id.as_deref() == Some(parent)),
        "original partial child acquired an unrelated parent turn"
    );
    if let Some(metadata) = message
        .metadata
        .as_mut()
        .and_then(serde_json::Value::as_object_mut)
    {
        let detail = metadata
            .entry("task_detail")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .context("original partial child Result detail is not an object")?;
        for key in ["rejoin_obligation_id", "rejoin_generation"] {
            ensure!(
                detail.get(key).is_none() || detail.get(key) == before.get(key),
                "original partial child Result generation changed"
            );
        }
        let parent = detail.get("parent_turn_id");
        ensure!(
            parent.is_none()
                || parent
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|parent| message.turn_id.as_deref() == Some(parent)),
            "original partial child Result acquired an unrelated parent turn"
        );
        for key in ["rejoin_obligation_id", "rejoin_generation"] {
            if !detail.contains_key(key) {
                detail.insert(key.into(), before[key].clone());
            }
        }
        detail.remove("parent_turn_id");
    }
    detail.remove("parent_turn_id");
    Ok(true)
}

#[cfg(test)]
mod partial_result_contract_tests {
    use super::*;
    #[test]
    fn only_the_exact_original_partial_contract_and_result_turn_are_normalized() -> Result<()> {
        let at = Utc::now();
        let original = TaskRecord {
            id: "original-partial".into(),
            agent_id: "owner".into(),
            kind: crate::types::TaskKind::ActorInvocation,
            status: TaskStatus::Queued,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            recovery: None,
            detail: Some(
                serde_json::json!({"rejoin_obligation_id":"original-partial","rejoin_generation":1}),
            ),
        };
        let message = MessageEnvelope {
            turn_id: Some("result-provenance".into()),
            metadata: Some(serde_json::json!({"task_detail":{}})),
            ..MessageEnvelope::new(
                "owner",
                crate::types::MessageKind::TaskResult,
                crate::types::MessageOrigin::Task {
                    task_id: original.id.clone(),
                },
                crate::types::AuthorityClass::RuntimeInstruction,
                crate::types::Priority::Next,
                crate::types::MessageBody::Text {
                    text: "actual result".into(),
                },
            )
        };
        let mut terminal = original.clone();
        terminal.status = TaskStatus::Completed;
        terminal.detail.as_mut().unwrap()["parent_turn_id"] =
            serde_json::json!("result-provenance");
        let mut accepted = terminal.clone();
        let mut wire = message.clone();
        assert!(normalize_original_partial_result_contract(
            &original,
            &mut accepted,
            &mut wire
        )?);
        assert!(same_generation(&original, &accepted));
        assert_eq!(wire.turn_id, message.turn_id);
        let mut changed_partial = original.clone();
        changed_partial.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
        assert!(
            !same_generation(&original, &changed_partial),
            "different valid partial generations are not None==None"
        );
        for (key, value) in [
            ("rejoin_obligation_id", serde_json::Value::Null),
            ("rejoin_generation", serde_json::json!(0)),
            ("parent_turn_id", serde_json::Value::Null),
        ] {
            let mut invalid = original.clone();
            invalid.detail.as_mut().unwrap()[key] = value;
            assert!(
                !same_generation(&invalid, &invalid),
                "invalid raw contract must not fold to None"
            );
        }

        assert!(accepted
            .detail
            .as_ref()
            .unwrap()
            .get("parent_turn_id")
            .is_none());
        for (key, value) in [
            ("rejoin_obligation_id", serde_json::json!("foreign")),
            ("rejoin_generation", serde_json::json!(2)),
            ("rejoin_generation", serde_json::Value::Null),
            ("parent_turn_id", serde_json::json!("foreign-parent")),
            ("parent_turn_id", serde_json::Value::Null),
        ] {
            let mut wrong = terminal.clone();
            wrong.detail.as_mut().unwrap()[key] = value;
            assert!(
                normalize_original_partial_result_contract(
                    &original,
                    &mut wrong,
                    &mut message.clone()
                )
                .is_err(),
                "{key}"
            );
        }
        let mut complete = original.clone();
        complete.detail.as_mut().unwrap()["parent_turn_id"] = serde_json::json!("original-parent");
        let mut wrong = terminal.clone();
        assert!(!normalize_original_partial_result_contract(
            &complete,
            &mut wrong,
            &mut message.clone()
        )?);
        assert!(
            !same_generation(&complete, &wrong),
            "an effective original fence must remain strict"
        );
        for detail in [
            serde_json::json!({"rejoin_obligation_id":"foreign","rejoin_generation":1}),
            serde_json::json!({"rejoin_obligation_id":"original-partial","rejoin_generation":0}),
            serde_json::json!({"rejoin_obligation_id":"original-partial"}),
            serde_json::json!({"rejoin_generation":1}),
            serde_json::json!({"rejoin_obligation_id":"original-partial","rejoin_generation":1,"parent_turn_id":null}),
        ] {
            let mut invalid = original.clone();
            invalid.detail = Some(detail);
            let mut wrong = terminal.clone();
            assert!(!normalize_original_partial_result_contract(
                &invalid,
                &mut wrong,
                &mut message.clone()
            )?);
        }
        Ok(())
    }
}

// Failure evidence only; these reads never reconcile, retry or consume a producer.
#[cfg(test)]
impl super::RuntimeHandle {
    pub(crate) fn recovered_child_monitor_diagnostics_for_test(
        &self,
        task_id: &str,
    ) -> serde_json::Value {
        let bootstrap = self
            .inner
            .bootstrap_result
            .try_lock()
            .map(|result| format!("{result:?}"))
            .unwrap_or_else(|error| format!("unavailable: {error}"));
        let last_deletion_rejection = self
            .inner
            .last_child_deletion_rejections_for_test
            .try_lock()
            .map(|reasons| serde_json::json!(reasons.get(task_id)))
            .unwrap_or_else(|error| serde_json::json!(format!("unavailable: {error}")));
        let lease = match self.inner.task_handles.try_lock() {
            Ok(handles) => handles
                .get(task_id)
                .and_then(|handle| handle.control_lease()),
            Err(error) => {
                return serde_json::json!({
                    "bootstrap": bootstrap,
                    "task_handles": format!("unavailable: {error}"),
                });
            }
        };
        let Some(lease) = lease else {
            return serde_json::json!({"bootstrap": bootstrap, "task_handle": "absent",
                "last_deletion_rejection": last_deletion_rejection});
        };
        let source = match lease.original_child() {
            Ok(Some(source)) => source,
            Ok(None) => {
                return serde_json::json!({
                    "bootstrap": bootstrap,
                    "lease_id": lease.lease_id(),
                    "original_source": "absent",
                });
            }
            Err(error) => {
                return serde_json::json!({
                    "bootstrap": bootstrap,
                    "lease_id": lease.lease_id(),
                    "original_source_error": error.to_string(),
                });
            }
        };
        let monitor_stage_present = source
            .monitor_stage
            .try_lock()
            .map(|stage| serde_json::json!(stage.is_some()))
            .unwrap_or_else(|error| serde_json::json!(format!("unavailable: {error}")));
        let observed_terminal = source
            .observed
            .try_lock()
            .map(|observed| serde_json::json!(observed.is_some()))
            .unwrap_or_else(|error| serde_json::json!(format!("unavailable: {error}")));
        let prepared_descriptor = source
            .prepared
            .try_lock()
            .map(|prepared| serde_json::json!(prepared.as_ref().map(|value| value.descriptor())))
            .unwrap_or_else(|error| serde_json::json!(format!("unavailable: {error}")));
        let binding_descriptor = source
            .binding
            .prepared
            .try_lock()
            .map(|prepared| serde_json::json!(prepared.as_ref().map(|value| value.descriptor())))
            .unwrap_or_else(|error| serde_json::json!(format!("unavailable: {error}")));
        let frozen_terminal = source
            .terminal
            .try_lock()
            .map(|terminal| {
                serde_json::json!(terminal.as_ref().map(|value| (&value.nonce, value.at)))
            })
            .unwrap_or_else(|error| serde_json::json!(format!("unavailable: {error}")));
        serde_json::json!({
            "bootstrap": bootstrap,
            "lease_id": lease.lease_id(),
            "lease_generation": lease.domain_generation(),
            "last_deletion_rejection": last_deletion_rejection,
            "original_address": source.address(),
            "monitor_stage_present": monitor_stage_present,
            "monitor_binding_confirmed": source.monitor_binding_confirmed(),
            "observed_terminal": observed_terminal,
            "durable_finished": source.is_durable_finished(),
            "original_deletion_requested": source.original_deletion_requested(),
            "terminal_deletion_rejection": source.terminal_deletion_rejection_for_test
                .try_lock()
                .map(|reason| serde_json::json!(*reason))
                .unwrap_or_else(|error| serde_json::json!(format!("unavailable: {error}"))),
            "binding_descriptor": binding_descriptor,
            "prepared_descriptor": prepared_descriptor,
            "frozen_terminal": frozen_terminal,
        })
    }
}

#[cfg(test)]
impl OriginalChildTaskProducer {
    pub(crate) fn record_terminal_deletion_rejection_for_test(&self, error: &anyhow::Error) {
        *self.terminal_deletion_rejection_for_test.lock().unwrap() = Some(format!("{error:#}"));
    }
    pub(crate) fn record_clock_monitor_failure_for_test(&self, error: &anyhow::Error) {
        *self.monitor_failure_for_test.lock().unwrap() = Some(format!("{error:#}"));
    }
    pub(crate) fn clock_monitor_failure_for_test(&self) -> Option<String> {
        self.monitor_failure_for_test.lock().unwrap().clone()
    }
}

impl OriginalChildTaskProducer {
    pub(crate) fn validate_original_monitor_cancellation(&self) -> Result<()> {
        ensure!(
            !self.is_durable_finished()
                && self.original_lease()?.cancellation_requested()
                && self.monitor_stage().is_some()
                && self.observed_terminal().is_none()
                && self.frozen_terminal()?.is_none(),
            "original child monitor has no unobserved cancellation stage"
        );
        Ok(())
    }
    /// This compares only an independently validated whole Stop ACK with the
    /// immutable actual stage. It neither reads a new child nor grants deletion.
    pub(crate) fn original_stop_covers_actual_monitor_facets(
        &self,
        intent: &crate::runtime_db::task_stop::PreparedTaskStop,
        acknowledged: &TaskRecord,
        progress: &crate::runtime_db::task_stop::TaskStopProgress,
    ) -> Result<bool> {
        use crate::runtime_db::task_stop::{TaskStopChildState, TaskStopWorktreeState};
        let stage = self
            .monitor_stage()
            .context("original Stop monitor stage absent")?;
        let view = self.source_view()?;
        view.validate_generation(acknowledged)?;
        let Some(target) = view.target() else {
            return Ok(false);
        };
        ensure!(
            target.original() == self.address() && target.target_id() == stage.child,
            "original Stop actual target differs from its source"
        );
        self.validate_monitor_target_incarnation(target.target_scope())?;
        let ack_detail = acknowledged.detail.as_ref();
        let ack_str = |key: &str| {
            ack_detail
                .and_then(|detail| detail.get(key))
                .and_then(serde_json::Value::as_str)
        };
        // Actual created-child duty is determined by its original qualified
        // producer and recorded target, not by absent pre-spawn Task fields.
        let created = target.has_created_child_duty();
        if created {
            let Some(actual_scope) = target.target_scope() else {
                return Ok(false);
            };
            let child_covered = intent.child.as_ref().is_some_and(|child| {
                &child.scope == actual_scope
                    && child.parent_agent_id == self.address.scope.agent_id
                    && child.creation_task_id == self.address.admitted.key.id
            });
            let retained = intent.child_unavailable.is_some()
                && progress.child == TaskStopChildState::NeedsAttention
                && ack_str("child_agent_id") == Some(stage.child.as_str())
                && ack_detail
                    .and_then(|detail| detail.get("target_identity_incarnation"))
                    .and_then(serde_json::Value::as_u64)
                    == Some(actual_scope.identity_incarnation);
            if !child_covered && !retained {
                return Ok(false);
            }
        }
        if stage.worktree {
            let artifact = stage.detail.get("worktree");
            let path = artifact
                .and_then(|value| value.get("worktree_path"))
                .and_then(serde_json::Value::as_str);
            let branch = artifact
                .and_then(|value| value.get("worktree_branch"))
                .and_then(serde_json::Value::as_str);
            let (Some(path), Some(branch)) = (path, branch) else {
                return Ok(false);
            };
            let covered = intent
                .worktree
                .as_ref()
                .is_some_and(|worktree| worktree.path == path && worktree.branch == branch);
            let ack_artifact = ack_detail.and_then(|detail| detail.get("worktree"));
            let retained = intent.worktree_unavailable.is_some()
                && progress.worktree == TaskStopWorktreeState::Retained
                && ack_artifact
                    .and_then(|value| value.get("worktree_path"))
                    .and_then(serde_json::Value::as_str)
                    == Some(path)
                && ack_artifact
                    .and_then(|value| value.get("worktree_branch"))
                    .and_then(serde_json::Value::as_str)
                    == Some(branch);
            if !covered && !retained {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub(in crate::runtime) fn acknowledge_monitor_stop_handoff(
        &self,
        proof: &crate::runtime_db::child_task_terminal::OriginalChildMonitorStopHandoff,
    ) -> Result<()> {
        self.validate_original_monitor_cancellation()?;
        ensure!(
            proof.matches_original(self),
            "original child Stop transfer crossed its admitted source"
        );
        self.monitor_stage
            .lock()
            .map_err(|_| anyhow::anyhow!("original monitor stage poisoned"))?
            .take();
        self.source
            .lock()
            .map_err(|_| anyhow::anyhow!("original monitor source poisoned"))?
            .take();
        self.completion
            .lock()
            .map_err(|_| anyhow::anyhow!("original monitor completion poisoned"))?
            .take();
        self.finished.store(true, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
#[path = "child_stop_actual_facets_tests.rs"]
mod stop_actual_facets_tests;

impl OriginalChildTaskProducer {
    pub(crate) fn monitor_recovery_target(
        &self,
    ) -> Result<Option<Arc<crate::runtime_db::child_task_terminal::OriginalChildTaskTarget>>> {
        self.validate_original_monitor_cancellation()?;
        if !self.original_lease()?.is_finished() {
            return Ok(None);
        }
        let source = self
            .source
            .lock()
            .map_err(|_| anyhow::anyhow!("original monitor source poisoned"))?;
        Ok(source.as_ref().and_then(|source| source.target.clone()))
    }
    pub(crate) fn validate_durable_monitor_recovery(
        &self,
        task: &TaskRecord,
        target: &crate::runtime_db::child_task_terminal::OriginalChildTaskTarget,
    ) -> Result<bool> {
        use crate::types::{AgentLifecycleDisposition, TaskRecoverySpec};
        self.validate_original_monitor_cancellation()?;
        ensure!(
            self.original_lease()?.is_finished(),
            "original monitor has not physically finished"
        );
        let stage = self
            .monitor_stage()
            .context("original monitor recovery stage absent")?;
        let source = self.source_view()?;
        source.validate_generation(task)?;
        ensure!(
            task.recovery == source.admitted_task().recovery
                && matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
                ),
            "durable monitor recovery changed the original active Task"
        );
        let Some(stored) = task.detail.as_ref().and_then(serde_json::Value::as_object) else {
            return Ok(false);
        };
        let retained = stage
            .detail
            .as_object()
            .context("original monitor detail is not an object")?;
        if !(retained
            .iter()
            .all(|(key, value)| stored.get(key) == Some(value))
            && stored
                .get("child_turn_baseline")
                .and_then(serde_json::Value::as_u64)
                == Some(stage.baseline)
            && stored
                .get("target_identity_incarnation")
                .and_then(serde_json::Value::as_u64)
                == target
                    .target_scope()
                    .map(|scope| scope.identity_incarnation)
            && stored
                .get("delivery_id")
                .and_then(serde_json::Value::as_str)
                == stage.delivery.as_deref()
            && target.original() == &self.address
            && target.target_id() == stage.child
            && target.recovery_delivery_id() == stage.delivery.as_deref()
            && target.recovery_turn_baseline() == stage.baseline)
        {
            return Ok(false);
        };
        let (authority, worktree, lifecycle) = match task.recovery.as_ref() {
            Some(TaskRecoverySpec::AgentInvocation {
                authority_class,
                workspace_mode,
                lifecycle_disposition,
                target_agent_id,
                ..
            }) => {
                let id = target_agent_id
                    .as_deref()
                    .or_else(|| {
                        stored
                            .get("target_agent_id")
                            .and_then(serde_json::Value::as_str)
                    })
                    .or_else(|| {
                        stored
                            .get("child_agent_id")
                            .and_then(serde_json::Value::as_str)
                    });
                ensure!(
                    id == Some(stage.child.as_str()),
                    "durable invocation target changed"
                );
                (
                    authority_class,
                    workspace_mode.is_worktree(),
                    *lifecycle_disposition,
                )
            }
            Some(TaskRecoverySpec::ChildAgentTask {
                authority_class,
                workspace_mode,
                lifecycle_disposition,
                ..
            }) => {
                ensure!(
                    stored
                        .get("child_agent_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(stage.child.as_str())
                        && stage.delivery.is_none(),
                    "durable child target changed"
                );
                (
                    authority_class,
                    workspace_mode.is_worktree(),
                    *lifecycle_disposition,
                )
            }
            Some(TaskRecoverySpec::SubagentTask {
                authority_class, ..
            }) => {
                ensure!(
                    stored
                        .get("child_agent_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(stage.child.as_str())
                        && stage.delivery.is_none(),
                    "durable subagent target changed"
                );
                (
                    authority_class,
                    false,
                    AgentLifecycleDisposition::DeleteOnTerminal,
                )
            }
            Some(TaskRecoverySpec::WorktreeSubagentTask {
                authority_class, ..
            }) => {
                ensure!(
                    stored
                        .get("child_agent_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(stage.child.as_str())
                        && stage.delivery.is_none(),
                    "durable worktree target changed"
                );
                (
                    authority_class,
                    true,
                    AgentLifecycleDisposition::DeleteOnTerminal,
                )
            }
            _ => anyhow::bail!("original monitor has no exact durable recovery path"),
        };
        ensure!(
            authority == &stage.authority
                && worktree == stage.worktree
                && lifecycle == stage.lifecycle,
            "durable monitor recovery changed original authority or lifecycle"
        );
        Ok(true)
    }
    pub(crate) fn acknowledge_monitor_recovery_handoff(
        &self,
        proof: &crate::runtime_db::child_task_terminal::OriginalChildMonitorRecoveryHandoff,
    ) -> Result<()> {
        self.validate_original_monitor_cancellation()?;
        ensure!(
            self.original_lease()?.is_finished() && proof.matches_original(self),
            "monitor recovery handoff crossed original lease"
        );
        self.monitor_stage
            .lock()
            .map_err(|_| anyhow::anyhow!("original monitor stage poisoned"))?
            .take();
        self.source
            .lock()
            .map_err(|_| anyhow::anyhow!("original monitor source poisoned"))?
            .take();
        self.completion
            .lock()
            .map_err(|_| anyhow::anyhow!("original monitor completion poisoned"))?
            .take();
        self.finished.store(true, Ordering::Release);
        Ok(())
    }
}
