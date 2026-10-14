//! Original focused Stop phases, distinct from Task business and physical work.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_task::TaskKey,
};
use crate::runtime_db::{task_snapshot as tasks, task_stop as stops};
use std::sync::Arc;

#[derive(Debug)]
pub(super) struct PreparedTaskStopProgressCommand {
    address: stops::TaskStopPhaseAddress,
    action: stops::TaskStopProgressAction,
    at: chrono::DateTime<Utc>,
    authorization: stops::TaskStopProgressAuthorization,
    _metadata: Arc<BudgetViewGuard>,
    ready_runtime_only: bool,
}
impl serde::Serialize for PreparedTaskStopProgressCommand {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut fields = serializer.serialize_struct("PreparedTaskStopProgressCommand", 3)?;
        if matches!(
            self.action,
            stops::TaskStopProgressAction::WorktreeCleanupObserved
        ) {
            // Only this closed FS observation excludes local CAS and derived
            // status/audits. Every existing P1 command keeps its prior digest.
            let identity = self.address.immutable_request();
            fields.serialize_field("address", &identity)?;
            fields.serialize_field("action", &self.action)?;
            fields.serialize_field("at", &identity.0.prepared_at())?;
        } else {
            fields.serialize_field("address", &self.address)?;
            fields.serialize_field("action", &self.action)?;
            fields.serialize_field("at", &self.at)?;
        }
        fields.end()
    }
}
impl PreparedTaskStopProgressCommand {
    pub(super) fn scope(&self) -> &InputEvidenceScope {
        self.address.scope()
    }
}

impl PreparedInputTransition {
    /// Stable original FS address. Without an actual capability this can only
    /// replay an already committed receipt; it cannot perform a new phase write.
    pub(crate) fn task_stop_worktree_cleanup_request(
        original: &stops::TaskStopRecord,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        Self::prepare_worktree_cleanup(original, None, request, ledger)
    }
    pub(crate) fn task_stop_worktree_cleanup_observed(
        original: &stops::TaskStopRecord,
        observed: Arc<crate::host::TaskStopWorktreeCleanupObserved>,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        anyhow::ensure!(
            observed.matches(original),
            "worktree cleanup proof address differs"
        );
        Self::prepare_worktree_cleanup(original, Some(observed), request, ledger)
    }
    fn prepare_worktree_cleanup(
        original: &stops::TaskStopRecord,
        observation: Option<Arc<crate::host::TaskStopWorktreeCleanupObserved>>,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let nonce = format!(
            "stop-worktree-{}",
            canonical_command_digest(&(
                &original.prepared,
                original.receipt_binding(),
                "worktree_cleanup_observed"
            ))?
        );
        Self::prepare_stop_progress(
            original,
            nonce,
            original.prepared.prepared_at(),
            stops::TaskStopProgressAction::WorktreeCleanupObserved,
            stops::TaskStopProgressAuthorization::WorktreeCleanup(observation),
            request,
            ledger,
        )
    }
    /// Narrow local routing policy only, outside the original business digest.
    pub(crate) fn for_ready_runtime_task_stop_phase(mut self) -> Result<Self> {
        match self.command.as_mut() {
            InputTransitionCommand::TaskStopProgress(command) => command.ready_runtime_only = true,
            InputTransitionCommand::TaskStopContinuation(command) => command.restrict_runtime(),
            _ => anyhow::bail!("ready Stop routing requires a focused Stop phase"),
        }
        Ok(self)
    }
    pub(crate) fn task_stop_monitor_joined(
        expected: &stops::TaskStopRecord,
        nonce: String,
        at: chrono::DateTime<Utc>,
        joined: Arc<crate::runtime::task_stop_join::TaskStopMonitorJoin>,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        Self::prepare_stop_progress(
            expected,
            nonce,
            at,
            stops::TaskStopProgressAction::MonitorJoined,
            stops::TaskStopProgressAuthorization::MonitorJoined(joined),
            request,
            ledger,
        )
    }
    pub(crate) fn task_stop_child_retired(
        expected: &stops::TaskStopRecord,
        nonce: String,
        at: chrono::DateTime<Utc>,
        retired: Arc<crate::host::TaskStopChildRetirement>,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        Self::prepare_stop_progress(
            expected,
            nonce,
            at,
            stops::TaskStopProgressAction::ChildRetired,
            stops::TaskStopProgressAuthorization::ChildRetired(retired),
            request,
            ledger,
        )
    }
    /// These finite observations grant neither a physical exit nor deletion.
    pub(crate) fn task_stop_safe_progress(
        expected: &stops::TaskStopRecord,
        nonce: String,
        at: chrono::DateTime<Utc>,
        action: stops::TaskStopProgressAction,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        anyhow::ensure!(
            matches!(
                action,
                stops::TaskStopProgressAction::WorktreeRetained { .. }
                    | stops::TaskStopProgressAction::PhysicalAttention { .. }
                    | stops::TaskStopProgressAction::ChildAttention { .. }
            ),
            "TaskStop physical completion requires its original opaque capability"
        );
        Self::prepare_stop_progress(
            expected,
            nonce,
            at,
            action,
            stops::TaskStopProgressAuthorization::SafeObservation,
            request,
            ledger,
        )
    }
    fn prepare_stop_progress(
        expected: &stops::TaskStopRecord,
        nonce: String,
        at: chrono::DateTime<Utc>,
        action: stops::TaskStopProgressAction,
        authorization: stops::TaskStopProgressAuthorization,
        request: &tasks::TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        action.validate()?;
        anyhow::ensure!(
            nonce != expected.prepared.command_nonce(),
            "Stop phase nonce collides with initial ACK"
        );
        // Counting writer precedes descriptor/progress/address clones. The source
        // record itself remains under the caller's original selected-body guard.
        let bytes = usize::try_from(serialized_bytes(&(
            &expected.prepared,
            expected.effect_seq,
            &expected.intent_digest,
            &expected.generation_digest,
            &expected.ack_task_digest,
            &expected.progress,
            &action,
            &nonce,
            at,
        ))?)?;
        let metadata = charge(ledger, &expected.intent.task.scope, request, bytes, 8, 8)?;
        let command = PreparedTaskStopProgressCommand {
            address: stops::TaskStopPhaseAddress::from_record(expected)?,
            action,
            at,
            authorization,
            _metadata: metadata,
            ready_runtime_only: false,
        };
        Self::prepare(
            expected.intent.task.scope.clone(),
            nonce,
            at,
            InputTransitionCommand::TaskStopProgress(command),
        )
    }
}

pub(super) fn charge(
    ledger: &InputEvidenceBudgetLedger,
    scope: &InputEvidenceScope,
    request: &tasks::TaskDomainLoadRequest,
    bytes: usize,
    records: usize,
    copies: usize,
) -> Result<Arc<BudgetViewGuard>> {
    let bytes = bytes
        .checked_mul(copies)
        .context("Stop phase material overflow")?;
    let records = records
        .checked_mul(copies)
        .context("Stop phase records overflow")?;
    anyhow::ensure!(
        bytes <= request.byte_limit.min(64 * 1024 * 1024)
            && records <= request.record_limit.min(65536),
        tasks::TaskSnapshotError::ResourceExhausted("Stop phase material")
    );
    Ok(ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(bytes)?,
            metadata_estimate_bytes: 0,
        },
    )?)
}

pub(super) fn commit_progress_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    command: &PreparedTaskStopProgressCommand,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<InputMutationExecution<TransitionCommit>> {
    let scope = command.scope();
    anyhow::ensure!(
        &prepared.primary_scope == scope
            && prepared.descriptor.scopes() == std::slice::from_ref(scope)
            && prepared.memory.is_none()
            && prepared.ended_memory.is_none()
            && prepared.original_dispatch_claim.is_none()
            && prepared.auto_base_descriptor.is_none(),
        "Stop phase can only write its original parent scope"
    );
    // Reuse the full original receipt length/checksum protocol. It is legal to
    // replay after retirement or later progress without repeating a capability.
    let receipt_guard = super::super::prepared_task_result::receipt_budget_tx(
        tx,
        &prepared.descriptor,
        scope,
        request,
        ledger,
    )?;
    if let Some(receipt) = lookup_input_mutation_receipt_tx(tx, &prepared.descriptor)? {
        let InputMutationResult::TaskStopPhase { ack } = &receipt.outcome.result else {
            anyhow::bail!("Stop phase receipt has another result family");
        };
        let mut guards = vec![receipt_guard];
        if command.ready_runtime_only {
            guards.push(charge(
                ledger,
                scope,
                request,
                stops::task_stop_phase_identity_material_tx(tx, scope)?,
                1,
                8,
            )?);
            anyhow::ensure!(
                ack.install_mode == stops::TaskStopPhaseInstallMode::Current
                    && stops::observe_task_stop_phase_mode_tx(tx, scope)?
                        == stops::TaskStopPhaseInstallMode::Current,
                stops::TaskStopPhaseError::RequiresMetadataActor
            );
        }
        return Ok(InputMutationExecution::Replayed {
            receipt,
            _material_budget_views: guards,
        });
    }
    let identity_guard = charge(
        ledger,
        scope,
        request,
        stops::task_stop_phase_identity_material_tx(tx, scope)?,
        1,
        8,
    )?;
    let mode = stops::observe_task_stop_phase_mode_tx(tx, scope)?;
    anyhow::ensure!(
        !command.ready_runtime_only || mode == stops::TaskStopPhaseInstallMode::Current,
        stops::TaskStopPhaseError::RequiresMetadataActor
    );
    let selected = stops::prepare_task_stop_phase_tx(tx, &command.address, |bytes, records| {
        charge(ledger, scope, request, bytes, records, 8)
    })?;
    let mut guards = vec![receipt_guard, identity_guard];
    let ready = mode == stops::TaskStopPhaseInstallMode::Current;
    // Only this original current Ready incarnation may read State/Task bodies.
    let proof = if ready {
        let material = tasks::task_maintenance_material_tx(tx, scope, &BTreeSet::new())?;
        guards.push(charge(
            ledger,
            scope,
            request,
            material.bytes,
            material.records,
            1,
        )?);
        let state_bytes: usize = tx.query_row(
            "SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1",
            [&scope.agent_id],
            |r| r.get(0),
        )?;
        guards.push(charge(ledger, scope, request, state_bytes, 1, 8)?);
        let proof = tasks::prepare_original_task_cleanup_tx(tx, scope, &BTreeSet::new())?;
        tasks::preflight_task_write_snapshot_with_budget_tx(tx, &proof, request, ledger)?
            .require_current()?;
        Some(proof)
    } else {
        None
    };
    let at = command
        .authorization
        .worktree_observation()
        .map_or(command.at, |proof| proof.at());
    let actual = stops::apply_task_stop_progress_tx(
        selected,
        &command.address,
        &command.action,
        &command.authorization,
        at,
    )?;
    guards.push(actual.budget_view());
    let mut effects = PostCommitEffects::default();
    let invalidation = if let Some(proof) = proof {
        let state =
            agent_state_tx(tx, &scope.agent_id)?.context("Stop phase current State disappeared")?;
        anyhow::ensure!(state.id == scope.agent_id, "Stop phase State owner differs");
        let facts = tasks::register_task_writes_with_budget_tx(
            tx,
            &proof,
            &BTreeSet::<TaskKey>::new(),
            &state,
            request,
            ledger,
        )?;
        guards.push(ledger.register_task_view(
            scope,
            InputMemoryCategory::Candidate,
            facts.delta.records.iter(),
        )?);
        guards.push(ledger.register_task_stop_view(
            scope,
            InputMemoryCategory::Candidate,
            facts.delta.stop_records.iter(),
        )?);
        guards.push(ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: serialized_bytes(&facts.delta.coverage)?,
            },
        )?);
        effects.task_delta = Some(facts.delta);
        effects.task_delta_requires_prepared_domain = true;
        None
    } else {
        tasks::invalidate_task_scope_for_stop_progress_tx(tx, &actual)?
    };
    if actual.changed() {
        if let Some(observation) = command.authorization.worktree_observation() {
            for event in observation.audit_events() {
                let (event, _) = append_audit_event_tx(tx, Some(&scope.agent_id), event)?;
                effects.audit_events.push(event);
            }
            effects
                .task_material_budget_views
                .extend(observation.budget_views());
        }
    }
    effects.notify_scheduler = !effects.audit_events.is_empty();
    effects.task_material_budget_views.extend(guards);
    let ack = actual.ack(invalidation, mode);
    let outcome = InputMutationOutcome {
        result: InputMutationResult::TaskStopPhase { ack: Box::new(ack) },
        applied: actual.changed(),
        delivery_receipt: None,
        scopes: vec![InputMutationScopeOutcome {
            scope: scope.clone(),
            install_keys: Default::default(),
            audit_events: effects.audit_events.clone(),
            notify_memory_index: false,
            notify_scheduler: !effects.audit_events.is_empty(),
        }],
    };
    let receipt = insert_input_mutation_receipt_tx(tx, &prepared.descriptor, &outcome, at)?;
    Ok(InputMutationExecution::Committed {
        value: TransitionCommit {
            applied: actual.changed(),
            effects: Box::new(effects),
            ..Default::default()
        },
        receipt,
    })
}

/// Read only, after the original owner group has reconciled Unknown. The same
/// full receipt protocol precedes FS; malformed/colliding receipts fail closed.
pub(crate) fn worktree_receipt_exists_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<bool> {
    let InputTransitionCommand::TaskStopProgress(command) = prepared.command.as_ref() else {
        anyhow::bail!("worktree receipt observation requires the closed phase");
    };
    anyhow::ensure!(
        matches!(
            command.action,
            stops::TaskStopProgressAction::WorktreeCleanupObserved
        ),
        "worktree receipt observation requires the closed phase"
    );
    let _guard = super::super::prepared_task_result::receipt_budget_tx(
        tx,
        prepared.descriptor(),
        command.scope(),
        request,
        ledger,
    )?;
    let receipt = lookup_input_mutation_receipt_tx(tx, prepared.descriptor())?;
    if let Some(receipt) = receipt {
        anyhow::ensure!(
            matches!(
                receipt.outcome.result,
                InputMutationResult::TaskStopPhase { .. }
            ),
            "worktree receipt result family differs"
        );
        Ok(true)
    } else {
        Ok(false)
    }
}
