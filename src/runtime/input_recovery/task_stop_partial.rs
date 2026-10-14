//! Partial installation is sealed by the actual Stop phase's fresh SQL view.
use super::*;
use crate::runtime_db::task_stop::{
    TaskStopResultOutcome, TaskStopSparseReadView, TaskStopSparseRecoveryEvidence,
};

pub(in crate::runtime) struct PreparedTaskStopPartialKnown {
    original: crate::runtime_db::input_mutation_receipts::PreparedInputMutation,
    known: KnownCommit,
    _material: Option<Arc<super::super::input_evidence_budget::BudgetViewGuard>>,
}
#[derive(Debug)]
pub(in crate::runtime) struct TaskStopPartialKnown {
    evidence: TaskStopSparseRecoveryEvidence,
    known: KnownCommit,
    _material: Option<Arc<super::super::input_evidence_budget::BudgetViewGuard>>,
}
impl PreparedTaskStopPartialKnown {
    pub(super) fn finish(
        self,
        evidence: TaskStopSparseRecoveryEvidence,
    ) -> Result<TaskStopPartialKnown> {
        ensure!(
            self.original == evidence.receipt().prepared,
            "partial Stop original receipt changed"
        );
        Ok(TaskStopPartialKnown {
            evidence,
            known: self.known,
            _material: self._material,
        })
    }
}
impl TaskStopPartialKnown {
    pub(in crate::runtime) fn evidence(&self) -> &TaskStopSparseRecoveryEvidence {
        &self.evidence
    }
    pub(in crate::runtime) fn known(&self) -> KnownCommit {
        self.known.clone()
    }
}

pub(super) fn prepare_partial_stop_known_tx(
    tx: &rusqlite::Transaction<'_>,
    view: &TaskStopSparseReadView<'_, '_>,
    reservation: &MutationReservation,
    witness: &super::super::mutation_witness::MutationWitness,
    budget: &InputEvidenceBudgetLedger,
    config: &ContextConfig,
    models: &RuntimeModelCatalog,
) -> Result<Option<PreparedTaskStopPartialKnown>> {
    use crate::runtime_db::{
        input_mutation_receipts::InputMutationResult, task_stop::TaskStopPhaseInstallMode,
    };
    if view.mode() != TaskStopPhaseInstallMode::Incomplete {
        return Ok(None);
    }
    view.validate_read_view_tx(tx, view.scope())?;
    let scope = view.scope();
    ensure!(
        scope.agent_id == reservation.agent_id()
            && scope.identity_incarnation == reservation.identity_incarnation(),
        "partial Stop owner differs"
    );
    let InputMutationResult::TaskStopPhase { ack } = &view.receipt().outcome.result else {
        anyhow::bail!("partial Stop ACK missing");
    };
    let delivered = matches!(&ack.result,Some(result) if matches!(&result.outcome,TaskStopResultOutcome::Delivered{..}));
    if !delivered {
        // P1 / unavailable Result changes no State, Input or Work. Only retain
        // this original held runtime observation; never read a new State body.
        let outcome = &view.receipt().outcome.scopes[0];
        ensure!(
            outcome.install_keys == Default::default()
                && outcome.audit_events.is_empty()
                && !outcome.notify_memory_index
                && !outcome.notify_scheduler,
            "metadata-only Stop phase unexpectedly changed ordinary domains"
        );
        let known = if let Some(original) = witness.input_runtime(&scope.agent_id) {
            Arc::new(original.for_unchanged_task_stop_partial()?).partial_task_stop_known()
        } else if let Some(original) = witness.input_task_stop_parent() {
            original.clone()
        } else {
            return Ok(None);
        };
        return Ok(Some(PreparedTaskStopPartialKnown {
            original: view.receipt().prepared.clone(),
            known,
            _material: None,
        }));
    }
    // The phase really admitted its frozen source. Admit only bounded fresh
    // State/Queue/Episodes/Work/Input; never hydrate the incomplete Task domain.
    let bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",[&scope.agent_id],|r|r.get(0))?;
    let bytes = bytes
        .checked_mul(8)
        .context("partial Stop State material overflow")?;
    let limits = budget.limits();
    let cap = usize::try_from(
        limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("partial Stop limit overflow")?,
    )?;
    ensure!(
        bytes <= cap.min(64 * 1024 * 1024) && limits.record_limit >= 8,
        crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
            "Stop partial State material"
        )
    );
    let material = budget.register_owned_bytes(
        scope,
        super::super::input_evidence_budget::InputMemoryCategory::Candidate,
        super::super::input_evidence_budget::OwnedBytesEstimate {
            payload_bytes: u64::try_from(bytes)?,
            metadata_estimate_bytes: 0,
        },
    )?;
    let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
    let state = facts
        .state
        .as_ref()
        .context("Delivered Stop partial State missing")?;
    let context = models.resolved_context_config(config, state.model_override.as_ref());
    let mut roots = std::collections::BTreeSet::new();
    use super::super::resident_input_evidence::{InputEvidenceFamily as F, InputEvidenceKey as K};
    if let Some(id) = &state.current_turn_id {
        roots.insert(K::new(F::Turn, id));
    }
    if let Some(binding) = &state.current_execution_binding {
        roots.insert(K::new(F::Turn, &binding.turn_id));
        roots.insert(K::new(F::Message, &binding.source_message_id));
        if let Some(id) = &binding.activation_id {
            roots.insert(K::new(F::Attempt, id));
        }
    }
    if let Some(result) = &ack.result {
        if let TaskStopResultOutcome::Delivered { message_id, .. } = &result.outcome {
            roots.insert(K::new(F::Message, message_id));
        }
    }
    let request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        mandatory_roots: roots,
        windows: vec![],
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit: usize::try_from(limits.record_limit)?,
        byte_limit: cap,
    };
    let outcome = &view.receipt().outcome.scopes[0];
    let mut work = super::super::resident_work::WorkCaptureRequest::current();
    use super::super::resident_work::{WorkFamily, WorkKey};
    work.mandatory_roots.extend(
        outcome
            .install_keys
            .work_items
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Work, id)),
    );
    work.mandatory_roots.extend(
        outcome
            .install_keys
            .wait_conditions
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Wait, id)),
    );
    work.mandatory_roots.extend(
        outcome
            .install_keys
            .continuations
            .iter()
            .map(|id| WorkKey::new(WorkFamily::Frame, id)),
    );
    let current =
        crate::runtime_db::input_preparation::prepare_partial_task_stop_input_recovery_tx(
            tx,
            view,
            state,
            &request,
            context.recent_episode_candidates,
            budget,
            &work,
        )?;
    ensure!(
        current.tasks.is_none(),
        "partial Stop recovery prepared Task"
    );
    let work = current.work.context("partial Stop Work domain missing")?;
    // This Queue is newly built for Known installation; shared body Weak views
    // do not account for its independent containers / temporary charge IDs.
    let slots = current.pending.messages.len();
    let mut id_bytes = 0u64;
    for message in &current.pending.messages {
        id_bytes = id_bytes
            .checked_add(serialized_bytes(&message.id)?)
            .context("partial Queue IDs overflow")?;
    }
    let queue_bytes = u64::try_from(
        std::mem::size_of::<super::super::RuntimeQueue>()
            .checked_add(
                slots
                    .checked_mul(
                        std::mem::size_of::<Arc<crate::types::MessageEnvelope>>()
                            + std::mem::size_of::<PendingBodyCharge>(),
                    )
                    .context("partial Queue slots overflow")?,
            )
            .context("partial Queue metadata overflow")?,
    )?
    .checked_add(id_bytes)
    .and_then(|n| n.checked_mul(4))
    .context("partial Queue copies overflow")?;
    ensure!(
        slots <= usize::try_from(limits.record_limit)?
            && queue_bytes <= u64::try_from(cap.min(64 * 1024 * 1024))?,
        crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
            "Stop partial Queue metadata"
        )
    );
    let queue_metadata = budget.register_owned_bytes(
        scope,
        super::super::input_evidence_budget::InputMemoryCategory::Candidate,
        super::super::input_evidence_budget::OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: queue_bytes,
        },
    )?;
    let charges = current
        .pending
        .messages
        .iter()
        .map(|message| {
            Ok(PendingBodyCharge {
                message_id: message.id.clone(),
                body_bytes: serialized_bytes(&message.body)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    budget.install_actual_pending(scope, &charges)?;
    // This phase cannot write Timer/External bodies. Preserve original selectors
    // and clear only those legacy Task cache keys; never populate raw Task bodies.
    ensure!(
        outcome.install_keys.timers.is_empty() && outcome.install_keys.external_triggers.is_empty(),
        "Stop partial phase has unrelated install families"
    );
    let cache = crate::runtime_db::input_recovery_install::InputRecoveryInstallFacts {
        scope: scope.clone(),
        keys: outcome.install_keys.clone(),
        timers: vec![],
        external_triggers: vec![],
    };
    let mut runtime = RuntimeMutationFacts::for_input_recovery(
        reservation.owner_epoch(),
        Arc::new(state.clone()),
        Arc::new(current.episodes),
        Arc::new(current.inputs.with_budget(budget.clone())?),
        work,
        None,
        current.pending.messages,
        budget,
        witness.input_runtime(&scope.agent_id).map(Arc::as_ref),
        outcome,
        view.receipt().outcome.applied,
        facts.identity,
        cache,
    )?
    .with_task_stop_queue_metadata(queue_metadata);
    if witness.input_storage_replayed() {
        runtime.suppress_observed_input_replay_publication();
    }
    let runtime = Arc::new(runtime);
    Ok(Some(PreparedTaskStopPartialKnown {
        original: view.receipt().prepared.clone(),
        known: runtime.partial_task_stop_known(),
        _material: Some(material),
    }))
}
