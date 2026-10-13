//! Synchronous preactivation phases keep each original SQL responsibility.
use super::*;
use std::{collections::BTreeSet, ops::Deref};

use crate::{
    memory::episode_transition::FrozenEpisodeMemoryValues,
    runtime::{
        ended_dispatch::EndedDispatchAuthorization,
        input_evidence_budget::{
            BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
        },
        mutation_witness::MutationWitness,
        resident_input_evidence::InputEvidenceScope,
        resident_work::WorkCaptureRequest,
    },
    runtime_db::{
        context_inputs::ContextDurableRead,
        control_transition::{
            find_committed_control_stop_for_run_tx, stopped_run_proof_material_tx,
            ControlStoppedRun,
        },
        dispatch_memory::{
            load_dispatch_memory_binding_by_turn_tx, DispatchMemoryBinding, DispatchMemoryIntent,
        },
        input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery,
        input_mutation_receipts::{
            with_fresh_input_mutation_resolution, InputMutationResolution,
            InputMutationScopeOutcome, OriginalInputMutationAttempt,
        },
        startup_ended_memory::{StartupEndedDispatchAuthorization, StartupEndedMemorySession},
        transitions::{
            AgentPostureTransitionCommand, AgentStateMutation, InputMutationExecution,
            PostCommitEffects, PreparedInputTransition, StartupRuntimeRecoveryReport,
            TurnTerminalTransitionCommand,
        },
        work_snapshot::WorkDomainLoadRequest,
    },
    types::{TurnRecord, TurnTerminalKind},
};
use rusqlite::{params, OptionalExtension};

const MAX_RECORDS: usize = 65_536;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const HEADER_ALLOWANCE: usize = 128 * 1024;

/// Only metadata survives a later-phase error. It cannot retain a resident root,
/// physical lease, startup capability or body. Each owned clone gets its own view.
#[derive(Default)]
pub(super) struct RetainedStartupRecoveryReport {
    report: StartupRuntimeRecoveryReport,
    guards: Vec<Arc<BudgetViewGuard>>,
    phases: BTreeMap<String, StartupPhaseAttempt>,
}
struct StartupPhaseAttempt {
    metadata_bytes: usize,
    witness: MutationWitness,
    _guard: Arc<BudgetViewGuard>,
    notifications: Option<StartupPhaseNotifications>,
    notification_bytes: usize,
    notification_guard: Option<Arc<BudgetViewGuard>>,
}
#[derive(serde::Serialize, PartialEq)]
struct StartupPhaseNotifications {
    scope: InputEvidenceScope,
    audit_events: Vec<crate::types::AuditEvent>,
    notify_memory_index: bool,
    notify_scheduler: bool,
}
impl StartupPhaseNotifications {
    fn from_outcome(outcome: InputMutationScopeOutcome) -> Self {
        Self {
            scope: outcome.scope,
            audit_events: outcome.audit_events,
            notify_memory_index: outcome.notify_memory_index,
            notify_scheduler: outcome.notify_scheduler,
        }
    }
    fn from_effects(scope: InputEvidenceScope, effects: &mut PostCommitEffects) -> Self {
        Self {
            scope,
            audit_events: std::mem::take(&mut effects.audit_events),
            notify_memory_index: effects.notify_memory_index,
            notify_scheduler: effects.notify_scheduler,
        }
    }
}
pub(super) struct ObservedStartupRecoveryReport {
    report: StartupRuntimeRecoveryReport,
    _guards: Vec<Arc<BudgetViewGuard>>,
}
impl Deref for ObservedStartupRecoveryReport {
    type Target = StartupRuntimeRecoveryReport;
    fn deref(&self) -> &Self::Target {
        &self.report
    }
}
impl RetainedStartupRecoveryReport {
    fn merge(
        &mut self,
        incoming: StartupRuntimeRecoveryReport,
        scopes: &[InputEvidenceScope],
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        // The caller invokes this in the ACK span before any later phase. Keep
        // targets even if reporting/observation fails after canonical commit.
        self.report
            .recovered_queue_agent_ids
            .extend(incoming.recovered_queue_agent_ids);
        self.report
            .recovered_execution_agent_ids
            .extend(incoming.recovered_execution_agent_ids);
        self.report.recovered_queue_agent_ids.sort();
        self.report.recovered_queue_agent_ids.dedup();
        self.report.recovered_execution_agent_ids.sort();
        self.report.recovered_execution_agent_ids.dedup();
        macro_rules! add {
            ($field:ident) => {
                self.report.$field = self
                    .report
                    .$field
                    .checked_add(incoming.$field)
                    .context("startup recovery report counter overflow")?;
            };
        }
        add!(queue_entries_changed);
        add!(interrupted_turns);
        add!(superseded_turns);
        add!(orphaned_claim_turns);
        add!(daemon_restart_turns);
        add!(interrupted_execution_attempts);
        self.guards = report_guards(&self.report, scopes, ledger, InputMemoryCategory::Current)?;
        Ok(())
    }
    fn snapshot(
        &self,
        scopes: &[InputEvidenceScope],
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<ObservedStartupRecoveryReport> {
        let guards = report_guards(&self.report, scopes, ledger, InputMemoryCategory::Pins)?;
        Ok(ObservedStartupRecoveryReport {
            report: self.report.clone(),
            _guards: guards,
        })
    }
}
fn report_guards(
    report: &StartupRuntimeRecoveryReport,
    scopes: &[InputEvidenceScope],
    ledger: &InputEvidenceBudgetLedger,
    category: InputMemoryCategory,
) -> Result<Vec<Arc<BudgetViewGuard>>> {
    let ids = report
        .recovered_queue_agent_ids
        .iter()
        .chain(&report.recovered_execution_agent_ids)
        .collect::<BTreeSet<_>>();
    let bytes = ids.iter().try_fold(
        std::mem::size_of::<StartupRuntimeRecoveryReport>(),
        |n, id| {
            n.checked_add(
                id.len()
                    .checked_mul(2)
                    .context("startup report length overflow")?
                    + 128,
            )
            .context("startup report length overflow")
        },
    )?;
    anyhow::ensure!(
        ids.len() <= MAX_RECORDS && bytes <= MAX_BYTES,
        "startup recovery report exceeds metadata budget"
    );
    let mut guards = ids
        .into_iter()
        .map(|id| {
            let scope = scopes
                .iter()
                .find(|scope| scope.agent_id == *id)
                .context("startup report target was not reserved")?;
            observe(
                ledger,
                scope,
                id.len()
                    .checked_mul(2)
                    .context("startup report length overflow")?
                    + 256,
                category,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    if guards.is_empty()
        && (report.queue_entries_changed != 0
            || report.interrupted_turns != 0
            || report.superseded_turns != 0
            || report.orphaned_claim_turns != 0
            || report.daemon_restart_turns != 0
            || report.interrupted_execution_attempts != 0)
    {
        let scope = scopes
            .first()
            .context("startup report has no original owner")?;
        guards.push(observe(
            ledger,
            scope,
            std::mem::size_of::<StartupRuntimeRecoveryReport>() + 128,
            category,
        )?);
    }
    Ok(guards)
}

#[derive(Debug)]
struct CheckpointAddress {
    scope: InputEvidenceScope,
    turn_id: String,
    source_message_id: String,
    turn_index: u64,
}
struct StartupIdentityFacts {
    facts: Vec<IdentityMutationFacts>,
    _guards: Vec<Arc<BudgetViewGuard>>,
}
struct SelectedCheckpoint {
    binding: DispatchMemoryBinding,
    identity: Arc<AgentIdentityRecord>,
    state: AgentState,
    turn: TurnRecord,
    authorization: StartupEndedDispatchAuthorization,
    _guard: Arc<BudgetViewGuard>,
}

impl RuntimeHost {
    pub(super) async fn reserve_startup_phase_scopes(
        &self,
        scopes: &[(String, u64)],
        admission: Option<&AcceptedStartupRecovery>,
    ) -> Result<Vec<MutationReservation>> {
        loop {
            let owners = scopes
                .iter()
                .map(|(agent, inc)| self.inner.mutation_owners.current_owner(agent, *inc))
                .collect::<Result<Vec<_>>>()?;
            let mut reservations = match MutationOwner::reserve_all(owners).await {
                Ok(reservations) => reservations,
                Err(MutationOwnerError::Replaced) => continue,
                Err(error) => return Err(error.into()),
            };
            {
                let handoff = self.inner.bootstrap_handoff_lock.lock().await;
                let registry = self.inner.runtimes.read().await;
                anyhow::ensure!(
                    !registry.ever_activated
                        && (registry.phase == HostRuntimePhase::Open
                            || (registry.phase == HostRuntimePhase::Closing
                                && admission.is_some_and(|a| a.permits_closing(self))))
                        && registry.agents.is_empty()
                        && registry.temporary_runtimes.is_empty()
                        && registry.unloading_mutations.is_empty(),
                    "startup phase recovery requires a never-activated Host"
                );
                drop(registry);
                let mut lease = self
                    .inner
                    .runtime_db_maintenance_lock
                    .lock()
                    .expect("maintenance lease poisoned");
                if lease.is_none() {
                    *lease = Some(crate::runtime_db::RuntimeDbLock::try_lock(
                        self.config().runtime_db_maintenance_lock_path(),
                    )?);
                }
                let capability = ExclusiveStartupInputRecovery::for_pre_activation_host(
                    self.runtime_db(),
                    lease.as_ref().expect("startup lease installed"),
                    &self.config().runtime_db_maintenance_lock_path(),
                    &handoff,
                )?;
                self.reconcile_startup_phase_unknowns(&mut reservations, &capability)?;
            }
            // The physical capability is gone before generic reconciliation can
            // suspend. Other unknown families retain their existing resolver.
            self.reconcile_external_mutations(&mut reservations).await?;
            return Ok(reservations);
        }
    }

    fn reconcile_startup_phase_unknowns(
        &self,
        reservations: &mut [MutationReservation],
        capability: &ExclusiveStartupInputRecovery<'_>,
    ) -> Result<()> {
        for index in 0..reservations.len() {
            let Some(recovery) = reservations[index].recovery()? else {
                continue;
            };
            let Some(saved) = recovery.witness.as_ref() else {
                continue;
            };
            let entry = {
                let retained = self
                    .inner
                    .startup_recovery_report
                    .lock()
                    .expect("startup recovery report poisoned");
                retained
                    .phases
                    .get(&recovery.key.operation_id)
                    .map(|phase| phase.witness.clone())
            };
            let Some(witness) = entry else { continue };
            anyhow::ensure!(
                saved.same_attempt(&witness),
                "startup phase registry differs from original attempt"
            );
            let attempt = witness
                .input_attempt()
                .context("startup phase lacks original input attempt")?;
            let descriptor = attempt.prepared();
            anyhow::ensure!(
                descriptor.scopes() == [reservation_scope(&reservations[index])],
                "startup phase descriptor requires another owner group"
            );
            let metadata_bytes = serde_json::to_vec(descriptor)?
                .len()
                .checked_mul(6)
                .context("startup phase metadata overflow")?;
            let _phase_guard = observe(
                &self.inner.input_evidence_budget,
                &reservation_scope(&reservations[index]),
                metadata_bytes,
                InputMemoryCategory::Candidate,
            )?;
            let single = &mut reservations[index..index + 1];
            if recovery.known.is_none() {
                MutationReservation::validate_input_unknown(single, &recovery.key, &witness)?;
                let mut connection = self.runtime_db().connection()?;
                let sizes:(usize,usize)=connection.query_row("SELECT COALESCE((SELECT length(CAST(prepared_json AS BLOB))+length(CAST(scopes_json AS BLOB))+length(CAST(outcome_json AS BLOB))+512 FROM input_mutation_receipts WHERE command_nonce=?1),0),COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?2),0)",params![descriptor.command_nonce(),single[0].agent_id()],|r|Ok((r.get(0)?,r.get(1)?)))?;
                let material = sizes
                    .0
                    .checked_add(sizes.1)
                    .context("startup phase resolution material overflow")?;
                anyhow::ensure!(
                    material <= MAX_BYTES,
                    "startup phase resolution exceeds metadata budget"
                );
                let _resolution_guard = observe(
                    &self.inner.input_evidence_budget,
                    &reservation_scope(&single[0]),
                    material
                        .checked_mul(6)
                        .context("startup phase resolution material overflow")?,
                    InputMemoryCategory::Candidate,
                )?;
                let current = with_fresh_input_mutation_resolution(
                    &mut connection,
                    attempt,
                    |tx, resolution| {
                        let InputMutationResolution::Present(receipt) = resolution else {
                            return Ok(None);
                        };
                        anyhow::ensure!(
                            receipt.prepared == *descriptor,
                            "startup phase receipt differs from original descriptor"
                        );
                        let (agent,status,payload):(String,String,String)=tx.query_row("SELECT agent_id,status,payload_json FROM agent_identities WHERE agent_id=?1",[single[0].agent_id()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
                        let identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
                        anyhow::ensure!(
                            agent == single[0].agent_id()
                                && identity.agent_id == agent
                                && identity.incarnation == single[0].identity_incarnation()
                                && matches!(
                                    identity.status,
                                    AgentRegistryStatus::Active | AgentRegistryStatus::Deleting
                                )
                                && serde_json::to_value(&identity.status)?.as_str()
                                    == Some(status.as_str()),
                            "startup phase identity changed"
                        );
                        witness.mark_pending_result(receipt.outcome.applied, true)?;
                        if !witness.input_storage_replayed() {
                            anyhow::ensure!(
                                receipt.outcome.scopes.len() == 1,
                                "startup phase receipt has another owner group"
                            );
                            self.record_startup_notifications(
                                &witness,
                                StartupPhaseNotifications::from_outcome(
                                    receipt
                                        .outcome
                                        .scopes
                                        .into_iter()
                                        .next()
                                        .expect("validated single scope"),
                                ),
                            )?;
                        }
                        Ok(Some(vec![KnownCommit::Metadata(Arc::new(
                            IdentityMutationFacts {
                                identity: Some(Arc::new(identity)),
                                state: None,
                            },
                        ))]))
                    },
                )?;
                MutationReservation::validate_input_unknown(single, &recovery.key, &witness)?;
                MutationReservation::resolve_startup_input_unknown(
                    single,
                    &recovery.key,
                    &witness,
                    current,
                    capability,
                    self.runtime_db(),
                )?;
            }
            let known = MutationReservation::validate_group_known(single)?;
            if known.iter().any(Option::is_some) {
                anyhow::ensure!(known.iter().flatten().all(|facts|matches!(facts,KnownCommit::Metadata(metadata) if metadata.state.is_none())),"startup phase contains a prepared resident publication");
                MutationReservation::reconcile_all(single, known)?;
            }
            self.publish_startup_phase_notifications(&witness)?;
            self.remove_startup_phase(&witness)?;
        }
        Ok(())
    }

    fn register_startup_phase(
        &self,
        scope: &InputEvidenceScope,
        witness: &MutationWitness,
    ) -> Result<()> {
        let descriptor = witness
            .input_attempt()
            .context("startup phase lacks original input attempt")?
            .prepared();
        let bytes = serde_json::to_vec(descriptor)?
            .len()
            .checked_mul(6)
            .context("startup phase descriptor length overflow")?;
        anyhow::ensure!(
            bytes <= MAX_BYTES && descriptor.scopes() == std::slice::from_ref(scope),
            "startup phase metadata exceeds its original scope budget"
        );
        let guard = observe(
            &self.inner.input_evidence_budget,
            scope,
            bytes,
            InputMemoryCategory::Current,
        )?;
        let mut retained = self
            .inner
            .startup_recovery_report
            .lock()
            .expect("startup recovery report poisoned");
        anyhow::ensure!(
            !retained.phases.contains_key(descriptor.command_nonce()),
            "startup phase nonce already registered"
        );
        let total = retained.phases.values().try_fold(bytes, |n, phase| {
            n.checked_add(phase.metadata_bytes)
                .and_then(|n| n.checked_add(phase.notification_bytes))
                .context("startup phase registry overflow")
        })?;
        anyhow::ensure!(
            retained.phases.len() < MAX_RECORDS && total <= MAX_BYTES,
            "startup phase registry exceeds aggregate metadata budget"
        );
        retained.phases.insert(
            descriptor.command_nonce().into(),
            StartupPhaseAttempt {
                metadata_bytes: bytes,
                witness: witness.clone(),
                _guard: guard,
                notifications: None,
                notification_bytes: 0,
                notification_guard: None,
            },
        );
        Ok(())
    }
    fn record_startup_notifications(
        &self,
        witness: &MutationWitness,
        notifications: StartupPhaseNotifications,
    ) -> Result<()> {
        let descriptor = witness
            .input_attempt()
            .context("startup phase lacks attempt")?
            .prepared();
        anyhow::ensure!(
            !witness.input_storage_replayed()
                && descriptor.scopes() == std::slice::from_ref(&notifications.scope),
            "startup notifications differ from original scope or observed replay"
        );
        let bytes = serde_json::to_vec(&notifications)?.len();
        anyhow::ensure!(
            bytes <= MAX_BYTES,
            "startup original notifications exceed preparation budget"
        );
        let guard = observe(
            &self.inner.input_evidence_budget,
            &notifications.scope,
            bytes,
            InputMemoryCategory::Current,
        )?;
        let mut retained = self
            .inner
            .startup_recovery_report
            .lock()
            .expect("startup recovery report poisoned");
        let phase = retained
            .phases
            .get(descriptor.command_nonce())
            .context("startup notification phase missing")?;
        anyhow::ensure!(
            phase.witness.same_attempt(witness),
            "startup notification attempt changed"
        );
        if let Some(original) = &phase.notifications {
            anyhow::ensure!(
                *original == notifications,
                "startup original notifications changed"
            );
            return Ok(());
        }
        let total = retained.phases.values().try_fold(bytes, |n, phase| {
            n.checked_add(phase.metadata_bytes)
                .and_then(|n| n.checked_add(phase.notification_bytes))
                .context("startup notification registry overflow")
        })?;
        anyhow::ensure!(
            total <= MAX_BYTES,
            "startup original notification registry exceeds preparation budget"
        );
        let phase = retained
            .phases
            .get_mut(descriptor.command_nonce())
            .expect("phase validated");
        phase.notifications = Some(notifications);
        phase.notification_bytes = bytes;
        phase.notification_guard = Some(guard);
        Ok(())
    }
    fn publish_startup_phase_notifications(&self, witness: &MutationWitness) -> Result<()> {
        let descriptor = witness
            .input_attempt()
            .context("startup phase lacks attempt")?
            .prepared();
        let (notifications, _guard) = {
            let mut retained = self
                .inner
                .startup_recovery_report
                .lock()
                .expect("startup recovery report poisoned");
            let phase = retained
                .phases
                .get_mut(descriptor.command_nonce())
                .context("startup notification phase missing")?;
            anyhow::ensure!(
                phase.witness.same_attempt(witness),
                "startup notification attempt changed"
            );
            anyhow::ensure!(
                !witness.input_storage_replayed() || phase.notifications.is_none(),
                "observed replay cannot own original startup notifications"
            );
            phase.notification_bytes = 0;
            (phase.notifications.take(), phase.notification_guard.take())
        };
        let Some(notifications) = notifications else {
            return Ok(());
        };
        // Publication for the complete original cold group has already finished.
        // Cold schedulers see durable work only when the original activation
        // policy admits them; notification replay never starts a runtime.
        let agent_id = notifications.scope.agent_id;
        let effects = PostCommitEffects {
            audit_events: notifications.audit_events,
            notify_memory_index: notifications.notify_memory_index,
            notify_scheduler: notifications.notify_scheduler,
            ..Default::default()
        };
        match self.agent_storage(&agent_id) {
            Ok(storage) => {
                for warning in storage
                    .publish_transition_events(&effects)
                    .into_iter()
                    .chain(storage.notify_transition_memory_index(&effects))
                {
                    tracing::warn!(agent_id,effect=warning.effect,error=%warning.message,"startup phase committed with post-commit warning");
                }
            }
            Err(error) => {
                tracing::warn!(agent_id,error=%error,"startup phase notification storage unavailable")
            }
        }
        Ok(())
    }
    fn remove_startup_phase(&self, witness: &MutationWitness) -> Result<()> {
        let nonce = witness
            .input_attempt()
            .context("startup phase lacks attempt")?
            .prepared()
            .command_nonce();
        let mut retained = self
            .inner
            .startup_recovery_report
            .lock()
            .expect("startup recovery report poisoned");
        anyhow::ensure!(
            retained
                .phases
                .get(nonce)
                .is_some_and(|phase| phase.witness.same_attempt(witness)),
            "startup phase registry changed"
        );
        retained.phases.remove(nonce);
        Ok(())
    }
    /// Exercise the production preactivation phases without automatically
    /// activating recovered sources, so fixtures can inspect their original ACK.
    #[cfg(test)]
    pub(crate) async fn recover_startup_dispatch_memory_for_fixture(
        &self,
    ) -> Result<StartupRuntimeRecoveryReport> {
        let host = self.clone();
        self.run_owned_external_mutation(async move {
            let admission = AcceptedStartupRecovery {
                host: Arc::downgrade(&host.inner),
            };
            let report = host
                .recover_startup_dispatch_memory_inner(&admission)
                .await?;
            let result = (*report).clone();
            host.finish_startup_recovery_report();
            Ok(result)
        })
        .await
    }

    pub(super) async fn recover_startup_dispatch_memory_inner(
        &self,
        admission: &AcceptedStartupRecovery,
    ) -> Result<ObservedStartupRecoveryReport> {
        let (mut reservations, handoff) = self
            .reserve_startup_mutations_inner(Some(admission))
            .await?;
        for reservation in &reservations {
            if let Some(root) = reservation.capture_ready_snapshot()? {
                anyhow::ensure!(
                    root.episodes.is_none() && root.input_evidence.is_none() && root.work.is_none(),
                    "startup recovery cannot replace a prepared resident root"
                );
            }
        }
        let scopes = reservations
            .iter()
            .map(reservation_scope)
            .collect::<Vec<_>>();
        let _scope_guard = inventory_guard(&scopes, &self.inner.input_evidence_budget)?;
        let input_scopes = self.runtime_db().startup_input_recovery_scopes(
            &scopes
                .iter()
                .map(|s| s.agent_id.clone())
                .collect::<Vec<_>>(),
        )?;
        let final_guards;
        {
            // No suspension is allowed inside the borrowed physical capability.
            let mut lease = self
                .inner
                .runtime_db_maintenance_lock
                .lock()
                .expect("maintenance lease poisoned");
            if lease.is_none() {
                *lease = Some(crate::runtime_db::RuntimeDbLock::try_lock(
                    self.config().runtime_db_maintenance_lock_path(),
                )?);
            }
            let capability = ExclusiveStartupInputRecovery::for_pre_activation_host(
                self.runtime_db(),
                lease.as_ref().expect("startup lease installed"),
                &self.config().runtime_db_maintenance_lock_path(),
                &handoff,
            )?;
            let session = StartupEndedMemorySession::new(self.runtime_db(), &capability)?;
            let cold_facts = self.startup_identity_facts(&reservations, false)?;
            let _cold_guards = cold_facts._guards;
            MutationReservation::begin_all(&mut reservations, ids::audit_event_id())?;
            let global = self
                .runtime_db()
                .recover_interrupted_runtime_state_at_startup_in_scopes(&input_scopes, &capability);
            let global = match global {
                Ok(global) => global,
                Err(error) => {
                    clear_definite_failure(&mut reservations, &error)?;
                    return Err(error);
                }
            };
            // The SQL ACK and exact cold publication are recorded before any
            // further proof preparation can fail. These old deltas are not roots.
            let report_result = self
                .inner
                .startup_recovery_report
                .lock()
                .expect("startup recovery report poisoned")
                .merge(global.report, &scopes, &self.inner.input_evidence_budget);
            MutationReservation::committed_all(
                &mut reservations,
                metadata_commits(cold_facts.facts),
            )?;
            drop(global.input_deltas);
            self.publish_startup_cold_phase(&mut reservations)?;
            report_result?;

            let (addresses, _address_guard) = self.startup_memory_addresses(&scopes)?;
            for address in addresses {
                let index = reservations
                    .iter()
                    .position(|r| {
                        r.agent_id() == address.scope.agent_id
                            && r.identity_incarnation() == address.scope.identity_incarnation
                    })
                    .context("unfinished memory scope was not reserved")?;
                let selected = self.select_startup_checkpoint(&address, &session)?;
                let Some(selected) = selected else { continue };
                self.finish_startup_checkpoint(&mut reservations[index..index + 1], selected)?;
            }
            // Capture final identity and State in one fresh view. Domains stay
            // cold until ordinary bootstrap, never carried through old metadata.
            MutationReservation::begin_all(&mut reservations, ids::audit_event_id())?;
            match self.startup_identity_facts(&reservations, true) {
                Ok(facts) => {
                    final_guards = facts._guards;
                    MutationReservation::committed_all(
                        &mut reservations,
                        metadata_commits(facts.facts),
                    )?;
                }
                Err(error) => {
                    // Read-only preparation has no canonical SQL write.
                    MutationReservation::definitely_not_committed_all(&mut reservations)?;
                    return Err(error);
                }
            }
        }
        // Session/maintenance borrow end before registry installation can await.
        drop(handoff);
        self.reconcile_external_mutations(&mut reservations).await?;
        drop(final_guards);
        self.inner
            .startup_recovery_report
            .lock()
            .expect("startup recovery report poisoned")
            .snapshot(&scopes, &self.inner.input_evidence_budget)
    }

    fn publish_startup_cold_phase(&self, reservations: &mut [MutationReservation]) -> Result<()> {
        #[cfg(test)]
        if self
            .inner
            .external_publication_fault
            .swap(false, Ordering::SeqCst)
        {
            // The injected first install fails; the original Known ACK remains
            // and is reconciled synchronously without decoding latest state.
            MutationReservation::validate_group_known(reservations)?;
        }
        MutationReservation::publish_all_known(reservations)?;
        Ok(())
    }

    pub(super) fn finish_startup_recovery_report(&self) {
        let mut retained = self
            .inner
            .startup_recovery_report
            .lock()
            .expect("startup recovery report poisoned");
        debug_assert!(retained.phases.is_empty());
        retained.report = Default::default();
        retained.guards.clear();
    }

    fn startup_identity_facts(
        &self,
        reservations: &[MutationReservation],
        include_state: bool,
    ) -> Result<StartupIdentityFacts> {
        let mut connection = self.runtime_db().connection()?;
        let tx = connection.transaction()?;
        let mut facts = Vec::with_capacity(reservations.len());
        let mut guards = Vec::new();
        let mut total_bytes = 0usize;
        for reservation in reservations {
            let scope = reservation_scope(reservation);
            let bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1),0)",[&scope.agent_id],|r|r.get(0))?;
            anyhow::ensure!(
                bytes <= MAX_BYTES,
                "startup identity metadata exceeds preparation budget"
            );
            total_bytes = total_bytes
                .checked_add(bytes)
                .context("startup identity metadata overflow")?;
            anyhow::ensure!(
                total_bytes <= MAX_BYTES,
                "startup identity facts exceed aggregate preparation budget"
            );
            guards.push(observe(
                &self.inner.input_evidence_budget,
                &scope,
                bytes
                    .checked_mul(3)
                    .context("startup identity size overflow")?,
                InputMemoryCategory::Candidate,
            )?);
            let read = if include_state {
                ContextDurableRead::new(&tx, &scope.agent_id).scope_facts()?
            } else {
                let payload: Option<String> = tx
                    .query_row(
                        "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
                        [&scope.agent_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                crate::runtime_db::context_inputs::ContextScopeFacts {
                    identity: payload
                        .map(|p| serde_json::from_str::<AgentIdentityRecord>(&p))
                        .transpose()?,
                    state: None,
                    control_revision: None,
                }
            };
            anyhow::ensure!(
                read.identity
                    .as_ref()
                    .is_none_or(|id| id.agent_id == scope.agent_id
                        && id.incarnation == scope.identity_incarnation
                        && id.incarnation > 0),
                "startup metadata identity projection changed"
            );
            if let Some(identity) = &read.identity {
                let (agent, status): (String, String) = tx.query_row(
                    "SELECT agent_id,status FROM agent_identities WHERE agent_id=?1",
                    [&scope.agent_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                anyhow::ensure!(
                    agent == identity.agent_id
                        && serde_json::to_value(&identity.status)?.as_str()
                            == Some(status.as_str()),
                    "startup metadata normalized identity differs from its payload"
                );
            }
            let same_identity = read
                .identity
                .as_ref()
                .is_some_and(|id| id.incarnation == scope.identity_incarnation);
            let state = if include_state
                && (same_identity || (read.identity.is_none() && scope.identity_incarnation == 1))
            {
                read.state.map(Arc::new)
            } else {
                None
            };
            facts.push(IdentityMutationFacts {
                identity: read.identity.map(Arc::new),
                state,
            });
        }
        Ok(StartupIdentityFacts {
            facts,
            _guards: guards,
        })
    }

    fn startup_memory_addresses(
        &self,
        scopes: &[InputEvidenceScope],
    ) -> Result<(Vec<CheckpointAddress>, Vec<Arc<BudgetViewGuard>>)> {
        let mut connection = self.runtime_db().connection()?;
        let tx = connection.transaction()?;
        let mut current_scopes = Vec::new();
        let mut count = 0usize;
        let mut bytes = 0usize;
        for scope in scopes {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND state!='done')",
                params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
            if !exists {
                continue;
            }
            let current: Option<(String, String, u64, String)> = tx.query_row(
                "SELECT status,json_extract(payload_json,'$.agent_id'),json_extract(payload_json,'$.incarnation'),json_extract(payload_json,'$.status') FROM agent_identities WHERE agent_id=?1",
                [&scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            let Some((status, agent, inc, payload_status)) = current else {
                anyhow::bail!("actual unfinished dispatch has no durable identity")
            };
            anyhow::ensure!(
                agent == scope.agent_id
                    && inc == scope.identity_incarnation
                    && inc > 0
                    && payload_status == status,
                "startup unfinished identity projection changed"
            );
            // A retired disposition has its own bounded address validator. It
            // is never unfinished work or an activation request for this actor.
            let mut handoffs = tx.prepare("SELECT length(CAST(run_id AS BLOB))+length(CAST(runtime_lease_id AS BLOB))+length(CAST(source_message_id AS BLOB)),run_id,runtime_lease_id,source_message_id,json_extract(payload_json,'$.physical_stop.owner_epoch') FROM dispatch_memory_retired_handoffs WHERE agent_id=?1 AND identity_incarnation=?2 ORDER BY turn_id")?;
            let mut rows = handoffs.query(params![scope.agent_id, scope.identity_incarnation])?;
            while let Some(row) = rows.next()? {
                let size: usize = row.get(0)?;
                anyhow::ensure!(
                    size <= 64 * 1024,
                    "retired startup address exceeds bounded proof budget"
                );
                let run: String = row.get(1)?;
                let lease: String = row.get(2)?;
                let source: String = row.get(3)?;
                let epoch: u64 = row.get(4)?;
                let _guard = observe(
                    &self.inner.input_evidence_budget,
                    scope,
                    HEADER_ALLOWANCE * 12,
                    InputMemoryCategory::Candidate,
                )?;
                anyhow::ensure!(
                    crate::runtime_db::dispatch_memory::retired_physical_handoff_exists_tx(
                        &tx, scope, &run, &lease, epoch, &source
                    )?,
                    "retired startup disposition disappeared"
                );
            }
            if status == "deleted" {
                continue;
            }
            anyhow::ensure!(
                matches!(status.as_str(), "active" | "deleting"),
                "startup memory has an unsupported identity status"
            );
            let (scope_count,scope_bytes):(usize,usize)=tx.query_row(
                "SELECT count(*),COALESCE(sum(length(CAST(c.agent_id AS BLOB))+length(CAST(c.turn_id AS BLOB))+length(CAST(c.source_message_id AS BLOB))+96),0) FROM dispatch_memory_checkpoints c WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.state!='done' AND NOT EXISTS(SELECT 1 FROM dispatch_memory_retired_handoffs h WHERE h.agent_id=c.agent_id AND h.identity_incarnation=c.identity_incarnation AND h.turn_id=c.turn_id)",
                params![scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)))?;
            count = count
                .checked_add(scope_count)
                .context("unfinished startup count overflow")?;
            bytes = bytes
                .checked_add(scope_bytes)
                .context("unfinished startup metadata overflow")?;
            anyhow::ensure!(
                count <= MAX_RECORDS && bytes <= MAX_BYTES,
                "unfinished dispatch inventory exceeds metadata budget"
            );
            current_scopes.push(scope);
        }
        let mut addresses = Vec::with_capacity(count);
        let mut guards = Vec::new();
        for scope in current_scopes {
            let mut query=tx.prepare("SELECT turn_id,source_message_id,turn_index FROM dispatch_memory_checkpoints c WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.state!='done' AND NOT EXISTS(SELECT 1 FROM dispatch_memory_retired_handoffs h WHERE h.agent_id=c.agent_id AND h.identity_incarnation=c.identity_incarnation AND h.turn_id=c.turn_id) ORDER BY c.turn_index,c.turn_id")?;
            let mut rows = query.query(params![scope.agent_id, scope.identity_incarnation])?;
            while let Some(row) = rows.next()? {
                let address = CheckpointAddress {
                    scope: scope.clone(),
                    turn_id: row.get(0)?,
                    source_message_id: row.get(1)?,
                    turn_index: row.get(2)?,
                };
                anyhow::ensure!(
                    address.turn_index > 0
                        && !address.turn_id.is_empty()
                        && !address.source_message_id.is_empty(),
                    "invalid unfinished memory address"
                );
                guards.push(observe(
                    &self.inner.input_evidence_budget,
                    &address.scope,
                    address.scope.agent_id.len()
                        + address.turn_id.len()
                        + address.source_message_id.len()
                        + 256,
                    InputMemoryCategory::Candidate,
                )?);
                addresses.push(address);
            }
        }
        Ok((addresses, guards))
    }

    fn select_startup_checkpoint(
        &self,
        address: &CheckpointAddress,
        session: &StartupEndedMemorySession<'_, '_>,
    ) -> Result<Option<SelectedCheckpoint>> {
        let mut connection = self.runtime_db().connection()?;
        let tx = connection.transaction()?;
        // Header/descriptor are each <=64KiB. All ordinary owned material is
        // counted before ContextDurableRead or the canonical proof decodes it.
        let bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM messages WHERE evidence_id=?2),0)+COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM turn_records WHERE turn_id=?3),0)",params![address.scope.agent_id,address.source_message_id,address.turn_id],|r|r.get(0))?;
        anyhow::ensure!(
            bytes <= MAX_BYTES,
            "startup dispatch source material exceeds preparation budget"
        );
        let mut charged = bytes
            .checked_add(HEADER_ALLOWANCE)
            .and_then(|n| n.checked_mul(12))
            .context("startup dispatch material overflow")?;
        let source_guard = observe(
            &self.inner.input_evidence_budget,
            &address.scope,
            charged,
            InputMemoryCategory::Candidate,
        )?;
        let binding =
            load_dispatch_memory_binding_by_turn_tx(&tx, &address.scope, &address.turn_id)?;
        let Some(binding) = binding else {
            return Ok(None);
        };
        anyhow::ensure!(
            binding.invocation().message_id == address.source_message_id
                && binding.invocation().turn_index == address.turn_index,
            "unfinished memory inventory differs from its immutable Start"
        );
        let read = ContextDurableRead::new(&tx, &address.scope.agent_id);
        let facts = read.scope_facts()?;
        let identity = Arc::new(facts.identity.context("startup memory identity missing")?);
        anyhow::ensure!(
            identity.incarnation == address.scope.identity_incarnation
                && matches!(
                    identity.status,
                    AgentRegistryStatus::Active | AgentRegistryStatus::Deleting
                ),
            "startup memory belongs to another incarnation"
        );
        let state = facts
            .state
            .context("unfinished memory has no original State")?;
        let turn = read
            .turn_by_id(&address.turn_id)?
            .context("unfinished memory actual Turn missing")?;
        let run = turn
            .run_id
            .as_deref()
            .context("unfinished memory actual Turn has no run")?;
        anyhow::ensure!(
            state.turn_index == address.turn_index
                && state
                    .current_run_id
                    .as_deref()
                    .is_none_or(|current| current == run),
            "startup memory cannot clear an advanced dispatch or new run"
        );
        anyhow::ensure!(
            state
                .current_turn_id
                .as_deref()
                .is_none_or(|id| id == address.turn_id)
                && state
                    .current_execution_binding
                    .as_ref()
                    .is_none_or(|binding| binding.turn_id == address.turn_id
                        && binding.source_message_id == address.source_message_id),
            "startup memory cannot clear another current dispatch binding"
        );
        let terminal = turn
            .terminal
            .as_ref()
            .context("global startup recovery did not end original Turn")?;
        anyhow::ensure!(
            terminal.kind == TurnTerminalKind::Interrupted,
            "startup memory original Turn has no canonical interruption"
        );
        let (authorization, guard) = if terminal.reason.as_deref() == Some("agent_stopped") {
            let (proof_bytes, proof_records) =
                stopped_run_proof_material_tx(&tx, &address.scope, run, None)?;
            anyhow::ensure!(
                proof_records <= MAX_RECORDS,
                "startup Stop proof record budget exceeded"
            );
            charged = charged
                .checked_add(
                    proof_bytes
                        .checked_mul(12)
                        .context("startup Stop size overflow")?,
                )
                .context("startup Stop size overflow")?;
            let guard = observe(
                &self.inner.input_evidence_budget,
                &address.scope,
                charged,
                InputMemoryCategory::Candidate,
            )?;
            let source = read
                .message_by_id(&address.source_message_id)?
                .context("startup Stop source missing")?;
            // Only original persisted generations are candidates; no UUID or
            // owner epoch is synthesized for an absent original local lease.
            let mut query=tx.prepare("SELECT target_generation_json FROM control_cancellation_obligations WHERE agent_id=?1 AND identity_incarnation=?2 AND target_kind='run' AND target_identity=?3 ORDER BY command_id")?;
            let mut rows = query.query(params![
                address.scope.agent_id,
                address.scope.identity_incarnation,
                run
            ])?;
            let mut selected = None;
            while let Some(row) = rows.next()? {
                let generation: Value = serde_json::from_str(&row.get::<_, String>(0)?)?;
                let lease=generation.get("runtime_lease_id").and_then(Value::as_str).context("startup Stop has no original lease address; unfinished memory requires explicit repair")?;
                let epoch = generation
                    .get("owner_epoch")
                    .and_then(Value::as_u64)
                    .context("startup Stop owner epoch missing")?;
                let activation = generation.get("activation_id").and_then(Value::as_str);
                let original = ControlStoppedRun {
                    agent_id: &address.scope.agent_id,
                    identity_incarnation: address.scope.identity_incarnation,
                    owner_epoch: epoch,
                    run_id: run,
                    runtime_lease_id: lease,
                    activation_id: activation,
                    source: &source,
                };
                if let Some(proof) = find_committed_control_stop_for_run_tx(&tx, &original)? {
                    proof.matches_memory_dispatch(binding.key(), binding.invocation())?;
                    let key = proof.receipt().key.clone();
                    selected = Some(session.authorize_stopped_tx(&tx, &binding, &key, &original)?);
                    break;
                }
            }
            (
                selected.context("startup original Stop proof missing")?,
                guard,
            )
        } else {
            let (proof_bytes, proof_records) =
                session.interrupted_proof_material_tx(&tx, &binding)?;
            anyhow::ensure!(
                proof_records <= MAX_RECORDS,
                "startup interruption proof record budget exceeded"
            );
            charged = charged
                .checked_add(
                    proof_bytes
                        .checked_mul(12)
                        .context("startup proof size overflow")?,
                )
                .context("startup proof size overflow")?;
            let guard = observe(
                &self.inner.input_evidence_budget,
                &address.scope,
                charged,
                InputMemoryCategory::Candidate,
            )?;
            (session.authorize_interrupted_tx(&tx, &binding)?, guard)
        };
        drop(source_guard);
        Ok(Some(SelectedCheckpoint {
            binding,
            identity,
            state,
            turn,
            authorization,
            _guard: guard,
        }))
    }

    fn finish_startup_checkpoint(
        &self,
        reservations: &mut [MutationReservation],
        selected: SelectedCheckpoint,
    ) -> Result<()> {
        let scope = selected.binding.scope();
        let ledger = &self.inner.input_evidence_budget;
        let limits = ledger.limits();
        let mut capture = WorkCaptureRequest::current();
        capture
            .required_turn_ids
            .insert(selected.turn.turn_id.clone());
        capture
            .required_trigger_message_ids
            .insert(selected.binding.invocation().message_id.clone());
        let work_request = WorkDomainLoadRequest {
            capture,
            record_limit: usize::try_from(limits.record_limit)?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("startup memory acquisition limit overflow")?,
            )?,
        };
        let mut state = selected.state;
        if state.current_run_id.is_some()
            || state.current_turn_id.is_some()
            || state.current_execution_binding.is_some()
            || state.status == AgentStatus::AwakeRunning
        {
            let mut cleared = state.clone();
            cleared.current_run_id = None;
            cleared.current_turn_id = None;
            cleared.current_execution_binding = None;
            if cleared.status == AgentStatus::AwakeRunning {
                cleared.status = AgentStatus::AwakeIdle;
            }
            let posture = PreparedInputTransition::posture(
                scope.clone(),
                ids::audit_event_id(),
                Utc::now(),
                AgentPostureTransitionCommand {
                    agent_id: scope.agent_id.clone(),
                    expected_incarnation: Some(scope.identity_incarnation),
                    agent_state: AgentStateMutation {
                        expected: Some(Box::new(state)),
                        record: Box::new(cleared.clone()),
                    },
                    audit_events: vec![],
                    fault: None,
                },
                None,
            )?;
            state = self
                .commit_startup_phase(reservations, &posture, &selected.identity, &work_request)?
                .context("startup posture ACK omitted its exact final State")?;
        }
        let finish = PreparedInputTransition::terminal(
            scope.clone(),
            ids::audit_event_id(),
            Utc::now(),
            TurnTerminalTransitionCommand {
                agent_id: scope.agent_id.clone(),
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(state.clone())),
                    record: Box::new(state),
                },
                turn_record: selected.turn,
                terminal_tool_executions: vec![],
                audit_events: vec![],
                fault: None,
            },
        )?
        .with_ended_dispatch_memory(
            DispatchMemoryIntent::Finish {
                binding: selected.binding,
                frozen: frozen_memory(),
            },
            EndedDispatchAuthorization::from_startup(selected.authorization),
        )?;
        self.commit_startup_phase(reservations, &finish, &selected.identity, &work_request)
            .map(|_| ())
    }

    fn commit_startup_phase(
        &self,
        reservations: &mut [MutationReservation],
        prepared: &PreparedInputTransition,
        identity: &Arc<AgentIdentityRecord>,
        request: &WorkDomainLoadRequest,
    ) -> Result<Option<AgentState>> {
        let scope = reservation_scope(&reservations[0]);
        let _command_guard = observe(
            &self.inner.input_evidence_budget,
            &scope,
            usize::try_from(prepared.owned_payload_bytes()?)?
                .checked_mul(3)
                .context("startup prepared size overflow")?,
            InputMemoryCategory::Candidate,
        )?;
        let operation = prepared.descriptor().command_nonce().to_owned();
        let witness = MutationWitness::input_mutation(
            operation.clone(),
            OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
            vec![],
        )?;
        self.register_startup_phase(&scope, &witness)?;
        if let Err(error) =
            MutationReservation::begin_all_with_witness(reservations, operation, witness.clone())
        {
            self.remove_startup_phase(&witness)?;
            return Err(error.into());
        }
        let committed = self
            .runtime_db()
            .transitions_in_scope(&scope)
            .commit_prepared_transition_with_work_request_and_memory_budget(
                prepared,
                request,
                &self.inner.input_evidence_budget,
            );
        let replay_observation =
            if matches!(&committed, Ok(InputMutationExecution::Replayed { .. })) {
                witness.mark_input_storage_replayed()
            } else {
                Ok(())
            };
        witness.finish_original_attempt();
        replay_observation?;
        match committed {
            Ok(execution) => {
                let actual = match execution {
                    InputMutationExecution::Committed { mut value, .. } => {
                        witness.mark_pending_result(value.applied, false)?;
                        self.record_startup_notifications(
                            &witness,
                            StartupPhaseNotifications::from_effects(
                                scope.clone(),
                                &mut value.effects,
                            ),
                        )?;
                        value.effects.agent_state.map(|mutation| *mutation.record)
                    }
                    InputMutationExecution::Replayed { .. } => None,
                };
                MutationReservation::committed_all(
                    reservations,
                    vec![KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                        identity: Some(identity.clone()),
                        state: None,
                    }))],
                )?;
                self.publish_startup_cold_phase(reservations)?;
                self.publish_startup_phase_notifications(&witness)?;
                self.remove_startup_phase(&witness)?;
                let actual = match actual {
                    Some(actual) => Some(actual),
                    None => {
                        let facts = self.startup_identity_facts(reservations, true)?;
                        facts
                            .facts
                            .into_iter()
                            .next()
                            .and_then(|fact| fact.state)
                            .map(|state| (*state).clone())
                    }
                };
                Ok(actual)
            }
            Err(error) => {
                clear_definite_failure(reservations, &error)?;
                if crate::runtime_db::persistence_certainty(&error)
                    == Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
                {
                    self.remove_startup_phase(&witness)?;
                }
                Err(error)
            }
        }
    }
}
fn metadata_commits(facts: Vec<IdentityMutationFacts>) -> Vec<KnownCommit> {
    facts
        .into_iter()
        .map(|facts| KnownCommit::Metadata(Arc::new(facts)))
        .collect()
}
fn reservation_scope(reservation: &MutationReservation) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: reservation.agent_id().into(),
        identity_incarnation: reservation.identity_incarnation(),
    }
}
fn clear_definite_failure(
    reservations: &mut [MutationReservation],
    error: &anyhow::Error,
) -> Result<()> {
    if crate::runtime_db::persistence_certainty(error)
        == Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
    {
        MutationReservation::definitely_not_committed_all(reservations)?;
    }
    Ok(())
}
fn frozen_memory() -> FrozenEpisodeMemoryValues {
    FrozenEpisodeMemoryValues {
        timestamp: Utc::now(),
        initial_builder_id: ids::episode_id(),
        fallback_builder_id: ids::episode_id(),
        next_builder_id: ids::episode_id(),
        audit_event_id: ids::audit_event_id(),
    }
}
fn observe(
    ledger: &InputEvidenceBudgetLedger,
    scope: &InputEvidenceScope,
    bytes: usize,
    category: InputMemoryCategory,
) -> Result<Arc<BudgetViewGuard>> {
    Ok(ledger.register_owned_bytes(
        scope,
        category,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(bytes)?,
            metadata_estimate_bytes: 128,
        },
    )?)
}
fn inventory_guard(
    scopes: &[InputEvidenceScope],
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Vec<Arc<BudgetViewGuard>>> {
    let bytes = scopes.iter().try_fold(0usize, |n, s| {
        n.checked_add(s.agent_id.len() + 64)
            .context("startup scope inventory overflow")
    })?;
    anyhow::ensure!(
        scopes.len() <= MAX_RECORDS && bytes <= MAX_BYTES,
        "startup scope inventory exceeds metadata budget"
    );
    scopes
        .iter()
        .map(|scope| {
            observe(
                ledger,
                scope,
                scope.agent_id.len() + 128,
                InputMemoryCategory::Candidate,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        provider::StubProvider,
        runtime_db::{
            dispatch_memory::{DispatchMemoryStart, DISPATCH_MEMORY_SCHEMA},
            episode_memory::{EpisodeMemoryInvocation, EpisodeMemoryKey},
            evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
            input_evidence_snapshot::{
                register_committed_input_evidence_tx, CanonicalInputEvidence,
            },
            repositories::{
                committed_turn_record_tx, upsert_queue_entry_tx, upsert_turn_record_tx,
            },
        },
        types::{
            ClosureDecision, ClosureOutcome, QueueEntryRecord, RuntimePosture, TurnTriggerSummary,
            WorkItemExecutionBinding,
        },
    };
    use chrono::DateTime;

    fn host() -> Result<(tempfile::TempDir, RuntimeHost)> {
        let home = tempfile::tempdir()?;
        fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )?;
        let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
        config.user_home_dir = Some(home.path().to_path_buf());
        let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
        host.ensure_default_agent_identity()?;
        Ok((home, host))
    }
    fn scope(host: &RuntimeHost) -> Result<InputEvidenceScope> {
        let identity = host.ensure_default_agent_identity()?;
        Ok(InputEvidenceScope {
            agent_id: identity.agent_id,
            identity_incarnation: identity.incarnation,
        })
    }
    fn posture(
        host: &RuntimeHost,
        scope: &InputEvidenceScope,
        nonce: &str,
    ) -> Result<PreparedInputTransition> {
        let before = host
            .runtime_db()
            .agent_states()
            .latest(&scope.agent_id)?
            .context("fixture State missing")?;
        let mut next = before.clone();
        next.status = AgentStatus::Stopped;
        PreparedInputTransition::posture(
            scope.clone(),
            nonce.into(),
            Utc::now(),
            AgentPostureTransitionCommand {
                agent_id: scope.agent_id.clone(),
                expected_incarnation: Some(scope.identity_incarnation),
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(before)),
                    record: Box::new(next),
                },
                audit_events: vec![crate::types::AuditEvent::legacy(
                    "startup_phase_fixture",
                    serde_json::json!({"nonce":nonce}),
                )],
                fault: None,
            },
            None,
        )
    }
    fn phase_witness(prepared: &PreparedInputTransition) -> Result<MutationWitness> {
        MutationWitness::input_mutation(
            prepared.descriptor().command_nonce().into(),
            OriginalInputMutationAttempt::new(prepared.descriptor().clone()),
            vec![],
        )
    }
    async fn state_only_root(
        host: &RuntimeHost,
        scope: &InputEvidenceScope,
    ) -> Result<MutationOwner> {
        let identity = host
            .runtime_db()
            .agent_identities()
            .latest(&scope.agent_id)?
            .unwrap();
        let state = host
            .runtime_db()
            .agent_states()
            .latest(&scope.agent_id)?
            .unwrap();
        let owner = host
            .inner
            .mutation_owners
            .current_owner(&scope.agent_id, scope.identity_incarnation)?;
        let mut reservations = MutationOwner::reserve_all(vec![owner.clone()]).await?;
        MutationReservation::begin_all(&mut reservations, ids::audit_event_id())?;
        MutationReservation::committed_all(
            &mut reservations,
            vec![KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                identity: Some(Arc::new(identity)),
                state: Some(Arc::new(state)),
            }))],
        )?;
        MutationReservation::publish_all_known(&mut reservations)?;
        assert!(reservations[0].capture_ready_snapshot()?.is_some());
        Ok(owner)
    }

    #[tokio::test]
    async fn startup_phase_lost_ack_uses_original_receipt_and_keeps_state_only_root_cold(
    ) -> Result<()> {
        let (_home, host) = host()?;
        let scope = scope(&host)?;
        host.runtime_db()
            .agent_states()
            .upsert_for_fixture(&AgentState::new(&scope.agent_id))?;
        let owner = state_only_root(&host, &scope).await?;
        let prepared = posture(&host, &scope, "startup-phase-lost-ack")?;
        let witness = phase_witness(&prepared)?;
        {
            let mut reservations = MutationOwner::reserve_all(vec![owner.clone()]).await?;
            host.register_startup_phase(&scope, &witness)?;
            MutationReservation::begin_all_with_witness(
                &mut reservations,
                prepared.descriptor().command_nonce().into(),
                witness.clone(),
            )?;
            host.runtime_db()
                .transitions_in_scope(&scope)
                .commit_prepared_transition(&prepared)?;
            witness.finish_original_attempt();
            assert!(reservations[0].recovery()?.unwrap().known.is_none());
        }
        let reservations = host
            .reserve_startup_phase_scopes(
                &[(scope.agent_id.clone(), scope.identity_incarnation)],
                None,
            )
            .await?;
        assert!(reservations[0].recovery()?.is_none());
        assert!(
            reservations[0].capture_ready_snapshot()?.is_none(),
            "phase recovery must not install a prepared or obsolete State"
        );
        assert_eq!(
            host.runtime_db()
                .agent_states()
                .latest(&scope.agent_id)?
                .unwrap()
                .status,
            AgentStatus::Stopped
        );
        assert!(host
            .inner
            .startup_recovery_report
            .lock()
            .unwrap()
            .phases
            .is_empty());
        let receipts: usize = host.runtime_db().connection()?.query_row(
            "SELECT count(*) FROM input_mutation_receipts WHERE command_nonce=?1",
            [prepared.descriptor().command_nonce()],
            |r| r.get(0),
        )?;
        assert_eq!(receipts, 1);
        drop(reservations);
        Ok(())
    }

    #[tokio::test]
    async fn startup_phase_absence_clears_only_finished_registered_attempt() -> Result<()> {
        let (_home, host) = host()?;
        let scope = scope(&host)?;
        host.runtime_db()
            .agent_states()
            .upsert_for_fixture(&AgentState::new(&scope.agent_id))?;
        let owner = state_only_root(&host, &scope).await?;
        let prepared = posture(&host, &scope, "startup-phase-no-sql")?;
        let witness = phase_witness(&prepared)?;
        {
            let mut reservations = MutationOwner::reserve_all(vec![owner.clone()]).await?;
            host.register_startup_phase(&scope, &witness)?;
            MutationReservation::begin_all_with_witness(
                &mut reservations,
                prepared.descriptor().command_nonce().into(),
                witness.clone(),
            )?;
            // Original future disappeared before entering SQL; unfinished attempts
            // cannot acquire negative evidence even though no receipt exists.
        }
        assert!(host
            .reserve_startup_phase_scopes(
                &[(scope.agent_id.clone(), scope.identity_incarnation)],
                None
            )
            .await
            .is_err());
        assert!(owner.capture().is_err());
        witness.finish_original_attempt();
        let reservations = host
            .reserve_startup_phase_scopes(
                &[(scope.agent_id.clone(), scope.identity_incarnation)],
                None,
            )
            .await?;
        assert!(reservations[0].recovery()?.is_none());
        assert!(
            reservations[0].capture_ready_snapshot()?.is_some(),
            "absence preserves the original State-only observation"
        );
        assert!(host
            .inner
            .startup_recovery_report
            .lock()
            .unwrap()
            .phases
            .is_empty());
        drop(reservations);
        // A different family/no-witness attempt is never cleared by the private
        // phase registry or by a later phase's definitely-not-committed outcome.
        let mut unrelated = MutationOwner::reserve_all(vec![owner]).await?;
        MutationReservation::begin_all(&mut unrelated, "unrelated-global-unknown".into())?;
        drop(unrelated);
        assert!(host
            .reserve_startup_phase_scopes(
                &[(scope.agent_id.clone(), scope.identity_incarnation)],
                None
            )
            .await
            .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn startup_phase_same_inc_deleting_receipt_is_cold_but_successor_is_rejected(
    ) -> Result<()> {
        let (_home, host) = host()?;
        let scope = scope(&host)?;
        let mut identity = host
            .runtime_db()
            .agent_identities()
            .latest(&scope.agent_id)?
            .unwrap();
        identity.status = AgentRegistryStatus::Deleting;
        host.runtime_db().transaction(|tx| {
            upsert_agent_identity_tx(tx, &identity)?;
            upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id))?;
            Ok(())
        })?;
        let owner = state_only_root(&host, &scope).await?;
        let prepared = posture(&host, &scope, "startup-deleting-phase")?;
        let witness = phase_witness(&prepared)?;
        {
            let mut reservations = MutationOwner::reserve_all(vec![owner.clone()]).await?;
            host.register_startup_phase(&scope, &witness)?;
            MutationReservation::begin_all_with_witness(
                &mut reservations,
                prepared.descriptor().command_nonce().into(),
                witness.clone(),
            )?;
            host.runtime_db()
                .transitions_in_scope(&scope)
                .commit_prepared_transition(&prepared)?;
            witness.finish_original_attempt();
        }
        let reservations = host
            .reserve_startup_phase_scopes(
                &[(scope.agent_id.clone(), scope.identity_incarnation)],
                None,
            )
            .await?;
        assert!(reservations[0].capture_ready_snapshot()?.is_none());
        assert!(reservations[0].recovery()?.is_none());
        drop(reservations);
        // The resolver's original-incarnation fence remains strict even when
        // the SQL receipt is valid. Deliberate corruption models a successor
        // arriving without the required original-owner retirement boundary.
        let other = posture(&host, &scope, "startup-successor-phase")?;
        let other_witness = phase_witness(&other)?;
        {
            let mut reservations = MutationOwner::reserve_all(vec![owner.clone()]).await?;
            host.register_startup_phase(&scope, &other_witness)?;
            MutationReservation::begin_all_with_witness(
                &mut reservations,
                other.descriptor().command_nonce().into(),
                other_witness.clone(),
            )?;
            host.runtime_db()
                .transitions_in_scope(&scope)
                .commit_prepared_transition(&other)?;
            other_witness.finish_original_attempt();
            identity.incarnation += 1;
            host.runtime_db().connection()?.execute(
                "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
                params![serde_json::to_string(&identity)?, scope.agent_id],
            )?;
        }
        assert!(host
            .reserve_startup_phase_scopes(
                &[(scope.agent_id.clone(), scope.identity_incarnation)],
                None
            )
            .await
            .is_err());
        assert!(owner.capture().is_err());
        assert_eq!(
            host.inner
                .startup_recovery_report
                .lock()
                .unwrap()
                .phases
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn startup_inventory_does_not_charge_unreserved_retired_history() -> Result<()> {
        let (_home, host) = host()?;
        let original = seed_started_dispatch(&host)?;
        host.runtime_db().connection()?.execute_batch(
            "WITH RECURSIVE old(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM old WHERE n<65537)
             INSERT INTO dispatch_memory_checkpoints(agent_id,identity_incarnation,turn_id,source_message_id,turn_index,state,seed_digest,record_digest,payload_json)
             SELECT 'retired-agent-'||n,1,'retired-turn-'||n,'retired-source-'||n,1,'started',printf('%064d',n),printf('%064d',n),'{}' FROM old;"
        )?;
        let total: usize = host.runtime_db().connection()?.query_row(
            "SELECT count(*) FROM dispatch_memory_checkpoints WHERE state!='done'",
            [],
            |r| r.get(0),
        )?;
        assert!(total > MAX_RECORDS);
        // Historical addresses outside the original owner group cannot consume
        // this preparation budget, become activation targets, or be hydrated.
        let (addresses, _guards) = host.startup_memory_addresses(&[original.scope()])?;
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].turn_id, original.key().actual_dispatch_turn_id);
        assert_eq!(addresses[0].scope, original.scope());
        Ok(())
    }

    fn seed_started_dispatch(host: &RuntimeHost) -> Result<DispatchMemoryBinding> {
        use crate::domain::execution_protocol::*;
        use crate::runtime_db::transitions::execution_protocol_repository as protocol;
        let scope = scope(host)?;
        let at: DateTime<Utc> = "2026-10-01T01:00:00Z".parse()?;
        let (start, source, turn) = host.runtime_db().transaction(|tx| {
            tx.execute_batch(DISPATCH_MEMORY_SCHEMA)?;
            let mut source = MessageEnvelope::new(
                &scope.agent_id,
                MessageKind::Control,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "start".into(),
                },
            );
            source.id = "host-startup-original-source".into();
            source.created_at = at;
            let (source, inserted) = append_message_tx(tx, &source)?;
            assert!(inserted);
            let mut turn = TurnRecord::new(&scope.agent_id, "host-startup-original-turn", 1);
            turn.run_id = Some("host-startup-original-run".into());
            turn.created_at = at;
            turn.input_message_ids = vec![source.id.clone()];
            turn.trigger = Some(TurnTriggerSummary::from_message(&source));
            upsert_turn_record_tx(tx, &turn)?;
            let turn = committed_turn_record_tx(tx, &scope.agent_id, &turn.turn_id)?.record;
            let mut state = AgentState::new(&scope.agent_id);
            state.status = AgentStatus::AwakeRunning;
            state.turn_index = 1;
            state.total_message_count = 1;
            state.current_turn_id = Some(turn.turn_id.clone());
            state.current_run_id = turn.run_id.clone();
            let baseline = crate::types::WorkingMemorySnapshot {
                objective: Some("original startup dispatch memory".into()),
                ..Default::default()
            };
            let mut builder = crate::types::ActiveEpisodeBuilder::new_with_start(
                &scope.agent_id,
                &baseline,
                1,
                1,
            );
            builder.id = "host-startup-original-episode".into();
            builder.started_at = at;
            builder.source_turn_ids = vec![turn.turn_id.clone()];
            state.working_memory.current_working_memory = baseline;
            state.working_memory.active_episode_id = Some(builder.id.clone());
            state.working_memory.active_episode_builder = Some(builder);
            state.current_execution_binding = Some(WorkItemExecutionBinding {
                activation_id: None,
                admission_provenance: None,
                source_message_id: source.id.clone(),
                turn_id: turn.turn_id.clone(),
                owner: None,
                work_item_id: None,
                claimed_work_revision: None,
            });
            upsert_agent_state_tx(tx, &state)?;
            upsert_queue_entry_tx(
                tx,
                &QueueEntryRecord {
                    message_id: source.id.clone(),
                    agent_id: scope.agent_id.clone(),
                    priority: source.priority.clone(),
                    status: QueueEntryStatus::Dequeued,
                    created_at: at,
                    updated_at: at,
                },
            )?;
            register_committed_input_evidence_tx(
                tx,
                &scope,
                &[
                    CanonicalInputEvidence::Message(source.clone()),
                    CanonicalInputEvidence::Turn(turn.clone()),
                ],
            )?;
            let start = DispatchMemoryStart::new(
                EpisodeMemoryKey {
                    agent_id: scope.agent_id.clone(),
                    identity_incarnation: scope.identity_incarnation,
                    actual_dispatch_turn_id: turn.turn_id.clone(),
                },
                EpisodeMemoryInvocation {
                    message_id: source.id.clone(),
                    turn_index: 1,
                },
                &source,
                ClosureDecision {
                    outcome: ClosureOutcome::Completed,
                    waiting_reason: None,
                    work_signal: None,
                    runtime_posture: RuntimePosture::Awake,
                    evidence: vec![],
                },
                state.working_memory,
            )?;
            Ok((start, source, turn))
        })?;
        let state = host
            .runtime_db()
            .agent_states()
            .latest(&scope.agent_id)?
            .unwrap();
        let prepared = PreparedInputTransition::posture(
            scope.clone(),
            "host-startup-original-start".into(),
            at,
            AgentPostureTransitionCommand {
                agent_id: scope.agent_id.clone(),
                expected_incarnation: Some(scope.identity_incarnation),
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(state.clone())),
                    record: Box::new(state),
                },
                audit_events: vec![],
                fault: None,
            },
            Some(turn),
        )?
        .with_dispatch_memory(DispatchMemoryIntent::Started {
            start: Box::new(start.clone()),
        })?;
        host.runtime_db()
            .transitions_in_scope(&scope)
            .commit_prepared_transition(&prepared)?;
        host.runtime_db().transaction(|tx| {
            let authority = protocol::authority_fences_tx(tx, &scope.agent_id)?;
            let sequence = source.message_seq.unwrap();
            let attempt = ExecutionAttempt {
                attempt_id: format!("activation:message:{}", source.id),
                agent_id: scope.agent_id.clone(),
                source_message_id: Some(source.id.clone()),
                source: ExecutionSource {
                    identity: ExecutionSourceIdentity::QueueMessage {
                        message_id: source.id.clone(),
                    },
                    generation: sequence,
                },
                binding: ExecutionBinding::AgentLifecycle {
                    agent_id: scope.agent_id.clone(),
                },
                provenance: ExecutionProvenance {
                    origin: ExecutionOrigin::Operator,
                    trust: ExecutionTrust::OperatorInstruction,
                    priority: ExecutionPriority::Normal,
                    correlation_id: None,
                    causation_id: None,
                },
                admitted_fences: AdmittedFences {
                    source_revision: sequence,
                    work_item_source_revision: None,
                    work_item_generation: None,
                    rejoin: None,
                    agent_control_revision: authority.agent_control_revision,
                    host_registry_revision: authority.host_registry_revision,
                },
                state: ExecutionAttemptState::Open,
                run_id: Some("host-startup-original-run".into()),
                turn_id: Some(start.key().actual_dispatch_turn_id.clone()),
                recovery_of_attempt_id: None,
                terminal_outcome_id: None,
                admitted_at: at.to_rfc3339(),
                terminal_at: None,
            };
            let commands = [ExecutionProtocolCommand::Admit(Box::new(AdmitExecution {
                attempt,
            }))];
            let prepared = protocol::validate_execution_commands_tx(
                tx,
                &scope.agent_id,
                Some(&ExecutionProtocolState::empty(&scope.agent_id)),
                &commands,
                &[],
                &[],
                &[],
            )?;
            let (_, actual) =
                protocol::persist_execution_commands_with_touched_facts_tx(tx, prepared)?;
            register_committed_input_evidence_tx(tx, &scope, &actual)?;
            Ok(())
        })?;
        Ok(DispatchMemoryBinding::from_start(&start))
    }

    #[tokio::test]
    async fn startup_interrupt_posture_finish_survive_restart_without_volatile_activation_report(
    ) -> Result<()> {
        let (_home, host) = host()?;
        let binding = seed_started_dispatch(&host)?;
        let admission = AcceptedStartupRecovery {
            host: Arc::downgrade(&host.inner),
        };
        let report = host
            .recover_startup_dispatch_memory_inner(&admission)
            .await?;
        assert_eq!(report.interrupted_execution_attempts, 1);
        assert!(report
            .recovered_queue_agent_ids
            .contains(&binding.scope().agent_id));
        let state = host
            .runtime_db()
            .agent_states()
            .latest(&binding.scope().agent_id)?
            .unwrap();
        assert_eq!(state.status, AgentStatus::AwakeIdle);
        assert!(state.current_run_id.is_none());
        assert!(state.current_execution_binding.is_none());
        let checkpoint: String = host.runtime_db().connection()?.query_row(
            "SELECT state FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND turn_id=?2",
            params![
                binding.scope().agent_id,
                binding.key().actual_dispatch_turn_id
            ],
            |r| r.get(0),
        )?;
        assert_eq!(checkpoint, "done");
        host.finish_startup_recovery_report();
        let config = host.config().as_ref().clone();
        drop(report);
        drop(host);
        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
        assert!(restarted
            .runtime_db()
            .queue_entries()
            .recovery_candidate_agent_ids()?
            .contains(&binding.scope().agent_id));
        let admission = AcceptedStartupRecovery {
            host: Arc::downgrade(&restarted.inner),
        };
        let report = restarted
            .recover_startup_dispatch_memory_inner(&admission)
            .await?;
        assert_eq!(report.interrupted_execution_attempts, 0);
        let receipts: usize = restarted.runtime_db().connection()?.query_row(
            "SELECT count(*) FROM episode_memory_results WHERE agent_id=?1",
            [&binding.scope().agent_id],
            |r| r.get(0),
        )?;
        assert_eq!(receipts, 1, "original dispatch finishes memory once");
        Ok(())
    }

    #[tokio::test]
    async fn startup_finish_lost_ack_recovers_after_session_drop_without_erasing_global_report(
    ) -> Result<()> {
        let (_home, host) = host()?;
        let binding = seed_started_dispatch(&host)?;
        let mut published = host.subscribe_events();
        let scope = binding.scope();
        let owner = host
            .inner
            .mutation_owners
            .current_owner(&scope.agent_id, scope.identity_incarnation)?;
        let mut reservations = MutationOwner::reserve_all(vec![owner.clone()]).await?;
        let handoff = host.inner.bootstrap_handoff_lock.lock().await;
        {
            let mut lease = host.inner.runtime_db_maintenance_lock.lock().unwrap();
            *lease = Some(crate::runtime_db::RuntimeDbLock::try_lock(
                host.config().runtime_db_maintenance_lock_path(),
            )?);
            let capability = ExclusiveStartupInputRecovery::for_pre_activation_host(
                host.runtime_db(),
                lease.as_ref().unwrap(),
                &host.config().runtime_db_maintenance_lock_path(),
                &handoff,
            )?;
            let session = StartupEndedMemorySession::new(host.runtime_db(), &capability)?;
            let cold = host.startup_identity_facts(&reservations, false)?;
            MutationReservation::begin_all(&mut reservations, "actual-global-interrupt".into())?;
            let input_scopes = host
                .runtime_db()
                .startup_input_recovery_scopes(&[scope.agent_id.clone()])?;
            let committed = host
                .runtime_db()
                .recover_interrupted_runtime_state_at_startup_in_scopes(
                    &input_scopes,
                    &capability,
                )?;
            MutationReservation::committed_all(&mut reservations, metadata_commits(cold.facts))?;
            host.inner.startup_recovery_report.lock().unwrap().merge(
                committed.report,
                std::slice::from_ref(&scope),
                &host.inner.input_evidence_budget,
            )?;
            host.publish_startup_cold_phase(&mut reservations)?;
            let selected = host
                .select_startup_checkpoint(
                    &CheckpointAddress {
                        scope: scope.clone(),
                        turn_id: binding.key().actual_dispatch_turn_id.clone(),
                        source_message_id: binding.invocation().message_id.clone(),
                        turn_index: binding.invocation().turn_index,
                    },
                    &session,
                )?
                .unwrap();
            let mut cleared = selected.state.clone();
            cleared.current_run_id = None;
            cleared.current_turn_id = None;
            cleared.current_execution_binding = None;
            cleared.status = AgentStatus::AwakeIdle;
            let posture = PreparedInputTransition::posture(
                scope.clone(),
                "startup-before-finish".into(),
                Utc::now(),
                AgentPostureTransitionCommand {
                    agent_id: scope.agent_id.clone(),
                    expected_incarnation: Some(scope.identity_incarnation),
                    agent_state: AgentStateMutation {
                        expected: Some(Box::new(selected.state)),
                        record: Box::new(cleared),
                    },
                    audit_events: vec![],
                    fault: None,
                },
                None,
            )?;
            let request = WorkDomainLoadRequest {
                capture: WorkCaptureRequest::current(),
                record_limit: 512,
                byte_limit: 64 * 1024 * 1024,
            };
            let actual = host
                .commit_startup_phase(&mut reservations, &posture, &selected.identity, &request)?
                .unwrap();
            let finish = PreparedInputTransition::terminal(
                scope.clone(),
                "startup-actual-finish-lost-ack".into(),
                Utc::now(),
                TurnTerminalTransitionCommand {
                    agent_id: scope.agent_id.clone(),
                    agent_state: AgentStateMutation {
                        expected: Some(Box::new(actual.clone())),
                        record: Box::new(actual),
                    },
                    turn_record: selected.turn,
                    terminal_tool_executions: vec![],
                    audit_events: vec![],
                    fault: None,
                },
            )?
            .with_ended_dispatch_memory(
                DispatchMemoryIntent::Finish {
                    binding: selected.binding,
                    frozen: frozen_memory(),
                },
                EndedDispatchAuthorization::from_startup(selected.authorization),
            )?;
            let witness = phase_witness(&finish)?;
            host.register_startup_phase(&scope, &witness)?;
            MutationReservation::begin_all_with_witness(
                &mut reservations,
                finish.descriptor().command_nonce().into(),
                witness.clone(),
            )?;
            host.runtime_db()
                .transitions_in_scope(&scope)
                .commit_prepared_transition_with_work_request_and_memory_budget(
                    &finish,
                    &request,
                    &host.inner.input_evidence_budget,
                )?;
            witness.finish_original_attempt();
            // Actual Finish ACK was lost. Session goes away, but the original
            // immutable whole receipt is sufficient for this phase's recovery.
        }
        drop(handoff);
        drop(reservations);
        assert!(owner.capture().is_err());
        assert_eq!(
            host.inner
                .startup_recovery_report
                .lock()
                .unwrap()
                .report
                .interrupted_execution_attempts,
            1
        );
        let reservations = host
            .reserve_startup_phase_scopes(
                &[(scope.agent_id.clone(), scope.identity_incarnation)],
                None,
            )
            .await?;
        assert!(reservations[0].recovery()?.is_none());
        assert!(reservations[0].capture_ready_snapshot()?.is_none());
        let retained = host.inner.startup_recovery_report.lock().unwrap();
        assert!(retained.phases.is_empty());
        assert_eq!(retained.report.interrupted_execution_attempts, 1);
        drop(retained);
        drop(reservations);
        let checkpoint: String = host.runtime_db().connection()?.query_row(
            "SELECT state FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND turn_id=?2",
            params![scope.agent_id, binding.key().actual_dispatch_turn_id],
            |r| r.get(0),
        )?;
        assert_eq!(checkpoint, "done");
        let receipts: usize = host.runtime_db().connection()?.query_row(
            "SELECT count(*) FROM episode_memory_results WHERE agent_id=?1",
            [&scope.agent_id],
            |r| r.get(0),
        )?;
        assert_eq!(receipts, 1);
        assert_original_memory_notifications_once(&host, &mut published).await?;
        let again = host
            .reserve_startup_phase_scopes(
                &[(scope.agent_id.clone(), scope.identity_incarnation)],
                None,
            )
            .await?;
        drop(again);
        assert!(
            published.try_recv().is_err(),
            "resolved original notifications cannot fire again"
        );
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(10),
            host.inner.memory_index_notify.notified()
        )
        .await
        .is_err());
        Ok(())
    }

    async fn assert_original_memory_notifications_once(
        host: &RuntimeHost,
        published: &mut tokio::sync::broadcast::Receiver<PublishedAuditEvent>,
    ) -> Result<()> {
        let mut original = Vec::new();
        while let Ok(event) = published.try_recv() {
            if event.event.kind == "episode_memory_finalized" {
                original.push(event.event);
            }
        }
        assert_eq!(
            original.len(),
            1,
            "original D1 audit must be published exactly once"
        );
        let (agent_id, payload): (String, String) = host.runtime_db().connection()?.query_row(
            "SELECT agent_id, data_json FROM audit_events WHERE audit_event_id=?1",
            [&original[0].id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(original[0].data["agent_id"], agent_id);
        let actual: crate::types::AuditEvent = serde_json::from_str(&payload)?;
        assert_eq!(
            original[0], actual,
            "publication must preserve allocated original id/seq/epoch"
        );
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            host.inner.memory_index_notify.notified(),
        )
        .await
        .context("original D1 index notification missing")?;
        Ok(())
    }

    #[tokio::test]
    async fn startup_phase_storage_replay_never_reissues_original_notifications() -> Result<()> {
        let (_home, host) = host()?;
        let scope = scope(&host)?;
        host.runtime_db()
            .agent_states()
            .upsert_for_fixture(&AgentState::new(&scope.agent_id))?;
        let _owner = state_only_root(&host, &scope).await?;
        let prepared = posture(&host, &scope, "startup-observed-replay")?;
        let first = host
            .runtime_db()
            .transitions_in_scope(&scope)
            .commit_prepared_transition(&prepared)?;
        let InputMutationExecution::Committed { receipt, .. } = first else {
            anyhow::bail!("first phase must commit");
        };
        assert!(!receipt.outcome.scopes[0].audit_events.is_empty());
        let mut published = host.subscribe_events();
        {
            let mut reservations = host
                .reserve_startup_phase_scopes(
                    &[(scope.agent_id.clone(), scope.identity_incarnation)],
                    None,
                )
                .await?;
            let witness = phase_witness(&prepared)?;
            host.register_startup_phase(&scope, &witness)?;
            MutationReservation::begin_all_with_witness(
                &mut reservations,
                prepared.descriptor().command_nonce().into(),
                witness.clone(),
            )?;
            let replay = host
                .runtime_db()
                .transitions_in_scope(&scope)
                .commit_prepared_transition(&prepared)?;
            assert!(matches!(replay, InputMutationExecution::Replayed { .. }));
            witness.mark_input_storage_replayed()?;
            witness.finish_original_attempt();
            // Lose the typed replay ACK; the shared observation still fences
            // historical event responsibility in the fresh original resolver.
        }
        let reservations = host
            .reserve_startup_phase_scopes(
                &[(scope.agent_id.clone(), scope.identity_incarnation)],
                None,
            )
            .await?;
        assert!(reservations[0].recovery()?.is_none());
        assert!(published.try_recv().is_err());
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(10),
            host.inner.memory_index_notify.notified()
        )
        .await
        .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn startup_memory_finish_keeps_stopped_owner_unactivated() -> Result<()> {
        let (_home, host) = host()?;
        let binding = seed_started_dispatch(&host)?;
        let mut published = host.subscribe_events();
        let scope = binding.scope();
        let before = host
            .runtime_db()
            .agent_states()
            .latest(&scope.agent_id)?
            .unwrap();
        let mut stopped = before.clone();
        stopped.status = AgentStatus::Stopped;
        stopped.current_run_id = None;
        stopped.current_turn_id = None;
        stopped.current_execution_binding = None;
        host.runtime_db()
            .transitions_in_scope(&scope)
            .commit_agent_posture(&AgentPostureTransitionCommand {
                agent_id: scope.agent_id.clone(),
                expected_incarnation: Some(scope.identity_incarnation),
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(before)),
                    record: Box::new(stopped),
                },
                audit_events: vec![],
                fault: None,
            })?;
        host.recover_orphaned_queue_claims_at_startup().await?;
        assert_eq!(
            host.runtime_db()
                .agent_states()
                .latest(&scope.agent_id)?
                .unwrap()
                .status,
            AgentStatus::Stopped
        );
        let registry = host.inner.runtimes.read().await;
        assert!(!registry.ever_activated);
        assert!(registry.agents.is_empty());
        drop(registry);
        let checkpoint: String = host.runtime_db().connection()?.query_row(
            "SELECT state FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND turn_id=?2",
            params![scope.agent_id, binding.key().actual_dispatch_turn_id],
            |r| r.get(0),
        )?;
        assert_eq!(checkpoint, "done");
        assert_original_memory_notifications_once(&host, &mut published).await?;
        Ok(())
    }
}
