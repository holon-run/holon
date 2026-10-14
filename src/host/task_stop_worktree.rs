//! Owned original Stop FS continuation. No transaction survives a Git await.
use super::*;
use crate::{
    runtime::{
        cleanup_frozen_worktree,
        input_evidence_budget::{
            serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
            OwnedBytesEstimate,
        },
        FrozenWorktreeCleanup, ObservedWorktreeCleanup, TaskOwnedWorktreeCleanupStatus,
    },
    runtime_db::{
        input_mutation_receipts::{PreparedInputMutation, TaskStopReceiptBinding},
        task_snapshot::TaskDomainLoadRequest,
        task_stop::{self, TaskStopRecord, TaskStopWorktreeState},
        transitions::PreparedInputTransition,
    },
    types::AuditEvent,
};

/// Derived result of the gated original FS operation. SQL/serde cannot mint it.
/// It retains only immutable addresses, original producer probes and charged
/// audit material; it has no Runtime, root, Host or storage reference.
pub(crate) struct TaskStopWorktreeCleanupObserved {
    original: PreparedInputMutation,
    binding: TaskStopReceiptBinding,
    status: TaskStopWorktreeState,
    at: chrono::DateTime<Utc>,
    audits: Vec<AuditEvent>,
    producer: Option<Arc<TaskStopWorktreeProducerExit>>,
    guards: Vec<Arc<BudgetViewGuard>>,
}
impl TaskStopWorktreeCleanupObserved {
    fn from_actual(
        original: &TaskStopRecord,
        at: chrono::DateTime<Utc>,
        observed: ObservedWorktreeCleanup,
        producer: Arc<TaskStopWorktreeProducerExit>,
        request: &TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        anyhow::ensure!(
            producer.matches(original),
            "original producer group proof changed"
        );
        let status = match observed.status {
            TaskOwnedWorktreeCleanupStatus::Cleaned => TaskStopWorktreeState::Removed,
            TaskOwnedWorktreeCleanupStatus::AlreadyRemoved => TaskStopWorktreeState::AlreadyRemoved,
            TaskOwnedWorktreeCleanupStatus::Retained => TaskStopWorktreeState::Retained,
            TaskOwnedWorktreeCleanupStatus::Failed => TaskStopWorktreeState::Failed,
        };
        Self::new(
            original,
            at,
            status,
            observed.audits,
            Some(producer),
            observed.guards,
            request,
            ledger,
        )
    }
    fn retained(
        original: &TaskStopRecord,
        at: chrono::DateTime<Utc>,
        reason: &'static str,
        request: &TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let event = AuditEvent::legacy(
            "task_worktree_cleanup_retained",
            json!({
            "task_id":original.intent.task.key.id,"reason":reason,"cleanup_trigger":"task_stop"}),
        );
        Self::new(
            original,
            at,
            TaskStopWorktreeState::Retained,
            vec![event],
            None,
            Vec::new(),
            request,
            ledger,
        )
    }
    fn new(
        original: &TaskStopRecord,
        at: chrono::DateTime<Utc>,
        status: TaskStopWorktreeState,
        audits: Vec<AuditEvent>,
        producer: Option<Arc<TaskStopWorktreeProducerExit>>,
        mut guards: Vec<Arc<BudgetViewGuard>>,
        request: &TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let bytes = serialized_bytes(&(&original.prepared, original.receipt_binding(), &audits))?
            .checked_mul(16)
            .context("worktree observation material overflow")?;
        anyhow::ensure!(
            bytes <= u64::try_from(request.byte_limit.min(64 * 1024 * 1024))?
                && audits
                    .len()
                    .checked_add(1)
                    .and_then(|n| n.checked_mul(16))
                    .is_some_and(|n| n <= request.record_limit.min(65536)),
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "worktree observation"
            )
        );
        guards.push(ledger.register_owned_bytes(
            &original.intent.task.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?);
        Ok(Self {
            original: original.prepared.clone(),
            binding: original.receipt_binding(),
            status,
            at,
            audits,
            producer,
            guards,
        })
    }
    pub(crate) fn matches(&self, record: &TaskStopRecord) -> bool {
        self.original == record.prepared
            && self.binding == record.receipt_binding()
            && (!matches!(
                self.status,
                TaskStopWorktreeState::Removed | TaskStopWorktreeState::AlreadyRemoved
            ) || self
                .producer
                .as_ref()
                .is_some_and(|exit| exit.matches(record)))
    }
    pub(crate) fn status(&self) -> TaskStopWorktreeState {
        self.status
    }
    pub(crate) fn at(&self) -> chrono::DateTime<Utc> {
        self.at
    }
    pub(crate) fn audit_events(&self) -> &[AuditEvent] {
        &self.audits
    }
    pub(crate) fn budget_views(&self) -> impl Iterator<Item = Arc<BudgetViewGuard>> + '_ {
        self.guards.iter().cloned()
    }
}
impl RuntimeHostBridge {
    pub(crate) fn finish_original_task_stop_worktree(
        &self,
        original: Arc<TaskStopRecord>,
        exit: Arc<TaskStopWorktreeProducerExit>,
        request: TaskDomainLoadRequest,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.host()?
                .finish_original_task_stop_worktree_owned(original, exit, request)
                .await
        })
    }
}
impl RuntimeHost {
    async fn finish_original_task_stop_worktree_owned(
        &self,
        original: Arc<TaskStopRecord>,
        exit: Arc<TaskStopWorktreeProducerExit>,
        request: TaskDomainLoadRequest,
    ) -> Result<()> {
        let ledger = &self.inner.input_evidence_budget;
        let probe = PreparedInputTransition::task_stop_worktree_cleanup_request(
            &original, &request, ledger,
        )?;
        let mut scopes = vec![(
            original.intent.task.scope.agent_id.clone(),
            original.intent.task.scope.identity_incarnation,
        )];
        if let Some(child) = &original.intent.child {
            scopes.push((
                child.scope.agent_id.clone(),
                child.scope.identity_incarnation,
            ));
        }
        scopes.sort();
        scopes.dedup();
        anyhow::ensure!(
            scopes.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "worktree original group aliases parent and child identity"
        );
        let mut bootstrap = Vec::with_capacity(scopes.len());
        for (id, _) in &scopes {
            bootstrap.push(self.agent_bootstrap_lock(id).lock_owned().await);
        }
        // reserve reconciles the original Unknown/ACK before any Git side effect.
        // A failed fresh resolution leaves the gate closed and returns here.
        let reservations = self.reserve_mutation_scopes(&scopes).await?;
        let replay = self.runtime_db().transaction(|tx| {
            crate::runtime_db::transitions::task_stop_worktree_receipt_exists_tx(
                tx, &probe, &request, ledger,
            )
        })?;
        if replay {
            drop(reservations);
            drop(bootstrap);
            self.bridge()
                .commit_task_stop_phase_metadata(probe, request)
                .await?;
            return Ok(());
        }
        let loaded = self.runtime_db().transaction(|tx| {
            task_stop::load_task_stop_by_sequence_tx(
                tx,
                &original.intent.task.scope,
                original.effect_seq,
                request.byte_limit,
                ledger,
            )
        })?;
        let current = loaded
            .records
            .first()
            .context("original worktree Stop missing")?;
        anyhow::ensure!(
            current.prepared == original.prepared
                && current.receipt_binding() == original.receipt_binding(),
            "worktree original Stop address differs"
        );
        if current.progress.worktree != TaskStopWorktreeState::Pending {
            return Ok(());
        }
        let at = std::cmp::max(Utc::now(), current.updated_at);
        let observation = if !exit.matches(current) {
            TaskStopWorktreeCleanupObserved::retained(
                current,
                at,
                "producer_exit_unavailable",
                &request,
                ledger,
            )?
        } else {
            let facts: Option<task_stop::OriginalTaskStopWorktreeRead> =
                self.runtime_db().transaction(|tx| {
                    task_stop::prepare_original_task_stop_worktree_tx(tx, current, &request, ledger)
                })?;
            if let Some(mut facts) = facts {
                let frozen = FrozenWorktreeCleanup::new(
                    facts.execution,
                    facts.repo_root,
                    current.intent.task.scope.clone(),
                    ledger.clone(),
                    &request,
                )?;
                let actual = cleanup_frozen_worktree(
                    frozen,
                    &current.intent.task.key.id,
                    &mut facts.metadata,
                    "task_stop",
                )
                .await?;
                // Owners and sorted bootstrap gates still cover the original
                // association through branch inspection, dirty check and FS.
                let mut cap = TaskStopWorktreeCleanupObserved::from_actual(
                    current,
                    std::cmp::max(Utc::now(), at),
                    actual,
                    exit,
                    &request,
                    ledger,
                )?;
                cap.guards.extend(facts.guards);
                cap
            } else {
                TaskStopWorktreeCleanupObserved::retained(
                    current,
                    at,
                    "original_association_unavailable",
                    &request,
                    ledger,
                )?
            }
        };
        let prepared = PreparedInputTransition::task_stop_worktree_cleanup_observed(
            current,
            Arc::new(observation),
            &request,
            ledger,
        )?;
        // The phase actor reserves its own original parent owner. Never nest it
        // beneath the FS reservation, or hold SQLite while crossing Git/FS.
        drop(reservations);
        drop(bootstrap);
        self.bridge()
            .commit_task_stop_phase_metadata(prepared, request)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "task_stop_worktree_tests.rs"]
mod tests;
