//! Owned Control completion keeps its exact evidence until whole publication.
use super::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};
use super::mutation_owner::MutationReservation;
use super::mutation_witness::MutationWitness;
use super::*;
use crate::runtime_db::{
    control_transition::{ControlCancellationTarget, ControlKey},
    input_mutation_receipts::{InputMutationResult, OriginalInputMutationAttempt},
    task_snapshot::TaskDomainLoadRequest,
    task_status_metadata::{
        FrozenControlWorktreeCompletion, TaskStatusMetadataAck, TaskStatusMetadataSelection,
    },
    transitions::{InputMutationExecution, PreparedInputTransition},
};

pub(crate) struct OriginalControlMetadataCapsule {
    scope: super::resident_input_evidence::InputEvidenceScope,
    key: ControlKey,
    target: ControlCancellationTarget,
    at: chrono::DateTime<chrono::Utc>,
    pending_evidence: Option<serde_json::Value>,
    pending_audits: Vec<AuditEvent>,
    physical_failed: bool,
    completion: Arc<super::input_evidence_budget::OwnedCompletionBudget>,
    frozen: Option<Arc<FrozenControlWorktreeCompletion>>,
    nonce: String,
    prepared: Option<Arc<PreparedInputTransition>>,
}
impl OriginalControlMetadataCapsule {
    /// Move the actual result into the original saved plan before any fallible
    /// command encoding or SQL admission. Retrying never redoes physical work.
    pub(super) fn completed(
        scope: super::resident_input_evidence::InputEvidenceScope,
        key: &ControlKey,
        target: &ControlCancellationTarget,
        evidence: serde_json::Value,
        at: chrono::DateTime<chrono::Utc>,
        completion: Arc<super::input_evidence_budget::OwnedCompletionBudget>,
    ) -> Self {
        Self {
            scope,
            key: key.clone(),
            target: target.clone(),
            at,
            pending_evidence: Some(evidence),
            pending_audits: Vec::new(),
            physical_failed: false,
            completion,
            frozen: None,
            nonce: format!("control-worktree-metadata-{}", uuid::Uuid::new_v4()),
            prepared: None,
        }
    }
    pub(super) fn observed(
        scope: super::resident_input_evidence::InputEvidenceScope,
        key: &ControlKey,
        target: &ControlCancellationTarget,
        evidence: serde_json::Value,
        audits: Vec<AuditEvent>,
        physical_failed: bool,
        at: chrono::DateTime<chrono::Utc>,
        completion: Arc<super::input_evidence_budget::OwnedCompletionBudget>,
    ) -> Self {
        let mut value = Self::completed(scope, key, target, evidence, at, completion);
        value.pending_audits = audits;
        value.physical_failed = physical_failed;
        value
    }
    pub(super) fn physical_failed(&self) -> bool {
        self.physical_failed
    }
    pub(crate) fn scope(&self) -> &super::resident_input_evidence::InputEvidenceScope {
        &self.scope
    }
    pub(crate) fn task_id(&self) -> &str {
        &self.target.target_identity
    }
    pub(crate) fn prepare(
        &mut self,
        selected: Option<TaskStatusMetadataSelection>,
        fault: Option<crate::runtime_db::transitions::TransitionFaultPoint>,
        request: &TaskDomainLoadRequest,
    ) -> Result<&PreparedInputTransition> {
        if self.frozen.is_none() {
            let evidence = self
                .pending_evidence
                .as_ref()
                .context("Control completion evidence lost")?;
            let frozen = FrozenControlWorktreeCompletion::freeze_with_audits(
                &self.key,
                &self.target,
                evidence,
                &self.pending_audits,
                !self.physical_failed,
                self.at,
                request,
                self.completion.clone(),
            )?;
            self.frozen = Some(Arc::new(frozen));
            self.pending_evidence = None;
            self.pending_audits.clear();
        }
        // Only skipped materialization changes. The original nonce, body, clock
        // and digest remain exact; a new current head is observed after DNC.
        let prepared = PreparedInputTransition::control_worktree_metadata(
            self.frozen.as_ref().expect("frozen above").clone(),
            self.nonce.clone(),
            selected,
            fault,
        )?;
        if let Some(original) = &self.prepared {
            anyhow::ensure!(
                original.descriptor() == prepared.descriptor(),
                "Control metadata retry changed its original command"
            );
        }
        self.prepared = Some(Arc::new(prepared));
        Ok(self.prepared.as_deref().expect("prepared above"))
    }
}
pub(crate) struct TaskStatusMetadataRead {
    pub(crate) ack: Box<TaskStatusMetadataAck>,
    _material: Arc<BudgetViewGuard>,
}

/// A source outside the currently selected heads requires no Task hydration.
pub(crate) fn select_original_control_status_head(
    reservation: &MutationReservation,
    task_id: &str,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<TaskStatusMetadataSelection>> {
    use super::resident_task::{TaskCaptureRequest, TaskFamily, TaskKey};
    let Some(root) = reservation.capture_ready_snapshot()? else {
        return Ok(None);
    };
    let Some(tasks) = &root.tasks else {
        return Ok(None);
    };
    if !tasks.coverage().status.heads.contains_key(task_id) {
        return Ok(None);
    }
    let capture = tasks.capture(&TaskCaptureRequest {
        include_active: false,
        include_unsettled: false,
        terminal_limit: 0,
        required_keys: std::collections::BTreeSet::from([TaskKey::new(TaskFamily::Task, task_id)]),
    })?;
    let record = capture
        .records
        .iter()
        .find(|r| r.locator.key.family == TaskFamily::Task && r.locator.key.id == task_id)
        .context("observed Status Task body missing")?;
    Ok(Some(TaskStatusMetadataSelection::single(
        tasks.scope(),
        tasks.coverage().revision,
        record,
        request,
        ledger,
    )?))
}

/// Synchronous SQL and Known resolution cannot be detached from their witness.
pub(crate) fn execute_original_control_metadata(
    db: &crate::runtime_db::RuntimeDb,
    reservations: &mut [MutationReservation],
    prepared: &PreparedInputTransition,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    config: &crate::context::ContextConfig,
    models: &crate::config::RuntimeModelCatalog,
    original_runtime: Vec<Arc<super::state_mutation::RuntimeMutationFacts>>,
) -> Result<TaskStatusMetadataRead> {
    let scope = prepared
        .task_status_metadata_scope()
        .context("Control metadata requires its exact phase")?;
    anyhow::ensure!(
        reservations.len() == 1
            && reservations[0].agent_id() == scope.agent_id
            && reservations[0].identity_incarnation() == scope.identity_incarnation,
        "Control metadata reservation differs from its original scope"
    );
    let witness = MutationWitness::input_mutation(
        prepared.descriptor().command_nonce().to_owned(),
        OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
        vec![],
    )?
    .with_input_runtime(original_runtime)?
    .with_task_status_parent_observation(&reservations[0], prepared)?;
    MutationReservation::begin_all_with_witness(
        reservations,
        prepared.descriptor().command_nonce().to_owned(),
        witness.clone(),
    )?;
    if let Err(error) = witness.mark_pending_started() {
        witness.finish_original_attempt();
        MutationReservation::definitely_not_committed_all(reservations)?;
        return Err(error);
    }
    let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: super::resident_work::WorkCaptureRequest::current(),
        record_limit: request.record_limit,
        byte_limit: request.byte_limit,
    };
    let result = db
        .transitions_in_scope(scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            prepared, &work, ledger, request,
        );
    if matches!(&result, Ok(InputMutationExecution::Replayed { .. })) {
        witness.mark_input_storage_replayed()?;
    }
    witness.finish_original_attempt();
    let execution = match result {
        Ok(value) => value,
        Err(error) => {
            if crate::runtime_db::persistence_certainty(&error)
                == Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
            {
                witness.release_pending_not_committed(false)?;
                MutationReservation::definitely_not_committed_all(reservations)?;
            } else {
                witness.mark_pending_unknown()?;
            }
            return Err(error);
        }
    };
    let (receipt, _actual_material, fault) = match execution {
        InputMutationExecution::Committed { value, receipt } => {
            witness.mark_original_input_receipt_pending(&receipt, false)?;
            (
                receipt,
                value.effects.task_material_budget_views,
                value.effects.fault,
            )
        }
        InputMutationExecution::Replayed {
            receipt,
            _material_budget_views,
        } => {
            witness.mark_original_input_receipt_pending(&receipt, true)?;
            (receipt, _material_budget_views, None)
        }
    };
    let InputMutationResult::TaskStatusMetadata { ack } = &receipt.outcome.result else {
        anyhow::bail!("Control metadata whole ACK has another family")
    };
    let guard = ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: serialized_bytes(ack.as_ref())?,
        },
    )?;
    super::input_recovery::resolve_input_recovery(db, reservations, ledger, config, models)?;
    if fault == Some(crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate) {
        anyhow::bail!("original Control metadata ACK requires publication recovery")
    }
    Ok(TaskStatusMetadataRead {
        ack: ack.clone(),
        _material: guard,
    })
}
impl RuntimeHandle {
    pub(super) async fn commit_control_worktree_metadata(
        &self,
        capsule: &mut OriginalControlMetadataCapsule,
        request: &TaskDomainLoadRequest,
    ) -> Result<TaskStatusMetadataRead> {
        if let Some(bridge) = &self.inner.host_bridge {
            return bridge
                .commit_original_control_worktree_metadata(capsule, request)
                .await;
        }
        let mut reservations = self.reserve_control_obligation_scope(&capsule.key).await?;
        let mut facts = Vec::new();
        if let Some(root) = reservations[0].capture_ready_snapshot()? {
            facts.push(Arc::new(
                self.prepare_external_input_facts(
                    &reservations[0],
                    root.state.clone(),
                    root.episodes.clone(),
                    root.input_evidence.clone(),
                )
                .await?,
            ));
        }
        let selected = select_original_control_status_head(
            &reservations[0],
            capsule.task_id(),
            request,
            &self.inner.input_evidence_budget,
        )?;
        let prepared = capsule.prepare(selected, None, request)?;
        let version = self.inner.config_snapshot.load_full();
        let result = execute_original_control_metadata(
            &self.inner.runtime_db,
            &mut reservations,
            prepared,
            request,
            &self.inner.input_evidence_budget,
            &version.base_context_config,
            &version.model_catalog,
            facts,
        )?;
        self.reconcile_reserved_mutations(&mut reservations).await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completed_capsule_retains_exact_body_and_nonce_when_sql_allowance_rejects() -> Result<()> {
        let home = tempfile::tempdir()?;
        let db = crate::runtime_db::RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let scope = super::super::resident_input_evidence::InputEvidenceScope {
            agent_id: "original-capsule".into(),
            identity_incarnation: 1,
        };
        let budget = Arc::new(ledger.reserve_owned_completion(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 1024,
                metadata_estimate_bytes: 0,
            },
        )?);
        budget.observe_material(65 * 1024 * 8, 0);
        let evidence =
            serde_json::json!({"detail":{"worktree":{"cleanup_error":"x".repeat(65*1024)}}});
        let key = ControlKey {
            agent_id: scope.agent_id.clone(),
            identity_incarnation: 1,
            command_id: "accepted-original".into(),
        };
        let target = ControlCancellationTarget {
            obligation_id: "original-artifact".into(),
            target_kind: crate::runtime_db::control_transition::ControlCancellationKind::Worktree,
            target_identity: "original-task".into(),
            target_generation: serde_json::json!({"owner_epoch":1}),
            target: serde_json::json!({}),
        };
        let mut capsule = OriginalControlMetadataCapsule::completed(
            scope.clone(),
            &key,
            &target,
            evidence,
            chrono::Utc::now(),
            budget,
        );
        let request = TaskDomainLoadRequest {
            query: Default::default(),
            capture: super::super::resident_task::TaskCaptureRequest::current(),
            byte_limit: 4096,
            record_limit: 65536,
        };
        let prepared = capsule.prepare(None, None, &request)?;
        let descriptor = prepared.descriptor().clone();
        let before = ledger.report()?.observed_bytes;
        let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
            capture: super::super::resident_work::WorkCaptureRequest::current(),
            record_limit: 65536,
            byte_limit: 4096,
        };
        let error = db
            .transitions_in_scope(&scope)
            .commit_prepared_transition_with_work_request_and_memory_budget(
                prepared, &work, &ledger, &request,
            )
            .err()
            .context("SQL phase should reject completed material")?;
        assert!(matches!(
            error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
            Some(crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(_))
        ));
        assert_eq!(ledger.report()?.observed_bytes, before);
        assert_eq!(
            capsule.prepare(None, None, &request)?.descriptor(),
            &descriptor
        );
        assert!(capsule.pending_evidence.is_none() && capsule.frozen.is_some());
        assert!(
            serde_json::to_value(capsule.frozen.as_deref().unwrap())?["evidence"]["detail"]
                ["worktree"]["cleanup_error"]
                .as_str()
                .unwrap()
                .len()
                == 65 * 1024
        );
        drop(capsule);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
}
