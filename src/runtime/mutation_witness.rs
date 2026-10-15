//! Durable result addresses for explicitly migrated mutation families.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use anyhow::{ensure, Context, Result};

use crate::runtime_db::{
    control_transition::{ControlInvocation, ControlKey, PreparedControl},
    episode_memory::{EpisodeMemoryInvocation, EpisodeMemoryKey, EpisodeMemoryTransitionCommand},
};

/// The address and command digest are frozen before the original SQL attempt.
/// Finishing the synchronous attempt proves only that it cannot still commit;
/// receipt presence or absence must be read on a fresh coherent transaction.
#[derive(Debug, Clone)]
pub(crate) struct MutationWitness {
    operation_id: String,
    family: MutationWitnessFamily,
    original_attempt_finished: Arc<AtomicBool>,
    // Unknown COMMIT retains the original physical targets with the owner ticket.
    control_cancellation: Option<Arc<super::control::PreparedControlCancellation>>,
    control_owner_epoch: Option<u64>,
    input_attempt: Option<crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt>,
    input_pending_accepted: Arc<AtomicBool>,
    // This invocation observed typed Replayed; it owns no old side effects.
    input_storage_replayed: Arc<AtomicBool>,
    input_runtime: Vec<Arc<super::state_mutation::RuntimeMutationFacts>>,
    input_stop_readonly: Vec<Arc<super::state_mutation::InitialTaskStopReadonlyMember>>,
    input_stop_parent: Option<super::mutation_owner::KnownCommit>,
    input_work_report: Option<Arc<super::OriginalWorkReportContinuation>>,
    // Host request allocation is never relabeled as an agent guard.
    input_task_input_child_material: Option<(Arc<super::OriginalTaskInputChildMaterial>, u64)>,
    input_status_parent: Option<super::mutation_owner::KnownCommit>,
    input_command_producer: Option<std::sync::Weak<super::OriginalCommandTaskProducer>>,
    input_child_producer: Option<std::sync::Weak<super::OriginalChildTaskProducer>>,
    input_child_binding: Option<(
        std::sync::Weak<super::OriginalChildTaskProducer>,
        Arc<crate::runtime_db::transitions::PreparedInputTransition>,
    )>,
    input_child_binding_observation: Option<super::mutation_owner::KnownCommit>,
    input_child_observations: Vec<(
        super::resident_input_evidence::InputEvidenceScope,
        super::mutation_owner::KnownCommit,
    )>,
    input_pending: Vec<(
        super::input_evidence_budget::InputEvidenceBudgetLedger,
        super::input_evidence_budget::PendingAdmissionTicket,
    )>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MutationWitnessKind {
    EpisodeMemory,
    Control,
    InputMutation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MutationWitnessFamily {
    InputMutation {
        prepared: crate::runtime_db::input_mutation_receipts::PreparedInputMutation,
    },
    EpisodeMemory {
        key: EpisodeMemoryKey,
        invocation: EpisodeMemoryInvocation,
        command_digest: String,
    },
    Control {
        key: ControlKey,
        invocation: ControlInvocation,
        command_digest: String,
    },
}

impl MutationWitness {
    pub(crate) fn input_mutation(
        operation_id: String,
        attempt: crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt,
        pending: Vec<(
            super::input_evidence_budget::InputEvidenceBudgetLedger,
            super::input_evidence_budget::PendingAdmissionTicket,
        )>,
    ) -> Result<Self> {
        ensure!(
            operation_id == attempt.prepared().command_nonce(),
            "input witness operation differs from original nonce"
        );
        ensure!(
            !attempt.original_attempt_finished(),
            "input witness attempt already finished"
        );
        if let Some((ledger, _)) = pending.first() {
            ensure!(
                pending.iter().all(|(other, _)| ledger.same_ledger(other)),
                "original pending tickets require one coordinator ledger"
            );
        }
        Ok(Self {
            operation_id,
            family: MutationWitnessFamily::InputMutation {
                prepared: attempt.prepared().clone(),
            },
            original_attempt_finished: Arc::new(AtomicBool::new(false)),
            control_cancellation: None,
            control_owner_epoch: None,
            input_attempt: Some(attempt),
            input_pending: pending,
            input_runtime: Vec::new(),
            input_stop_readonly: Vec::new(),
            input_stop_parent: None,
            input_work_report: None,
            input_task_input_child_material: None,
            input_status_parent: None,
            input_command_producer: None,
            input_child_producer: None,
            input_child_observations: Vec::new(),
            input_child_binding: None,
            input_child_binding_observation: None,
            input_pending_accepted: Arc::new(AtomicBool::new(false)),
            input_storage_replayed: Arc::new(AtomicBool::new(false)),
        })
    }
    pub(crate) fn with_original_command_producer(
        mut self,
        source: &Arc<super::OriginalCommandTaskProducer>,
        prepared: &crate::runtime_db::transitions::PreparedInputTransition,
    ) -> Result<Self> {
        let intent = prepared
            .original_command_terminal()
            .context("Command witness needs original producer facet")?;
        ensure!(
            self.input_attempt()
                .is_some_and(|attempt| attempt.prepared() == prepared.descriptor())
                && &intent.original == source.address()
                && !self.original_attempt_finished(),
            "Command witness differs from original producer"
        );
        self.input_command_producer = Some(Arc::downgrade(source));
        Ok(self)
    }
    pub(crate) fn with_original_child_producer(
        mut self,
        source: &Arc<super::OriginalChildTaskProducer>,
        prepared: &crate::runtime_db::transitions::PreparedInputTransition,
        reservations: &[super::mutation_owner::MutationReservation],
    ) -> Result<Self> {
        let intent = prepared
            .original_child_terminal()
            .context("child witness needs original producer facet")?;
        ensure!(
            self.input_attempt()
                .is_some_and(|attempt| attempt.prepared() == prepared.descriptor())
                && &intent.original == source.address()
                && !self.original_attempt_finished()
                && prepared.descriptor().scopes().len() == reservations.len(),
            "child witness differs from original producer group"
        );
        for scope in prepared.descriptor().scopes() {
            let reservation = reservations
                .iter()
                .find(|r| {
                    r.agent_id() == scope.agent_id
                        && r.identity_incarnation() == scope.identity_incarnation
                })
                .context("child witness owner group incomplete")?;
            if let Some(root) = reservation.capture_ready_snapshot()? {
                ensure!(
                    root.token.owner_epoch == reservation.owner_epoch()
                        && root.token.identity_incarnation == scope.identity_incarnation,
                    "child original publication epoch differs"
                );
                self.input_child_observations.push((
                    scope.clone(),
                    super::mutation_owner::KnownCommit::State {
                        current_reads: root.current_reads.clone(),
                        state: root.state.clone(),
                        episodes: root.episodes.clone(),
                        input_evidence: root.input_evidence.clone(),
                        work: root.work.clone(),
                        tasks: root.tasks.clone(),
                        scheduling: root.scheduling.clone(),
                        runtime: None,
                    },
                ));
            }
        }
        self.input_child_producer = Some(Arc::downgrade(source));
        Ok(self)
    }
    pub(crate) fn with_original_child_binding(
        mut self,
        source: &Arc<super::OriginalChildTaskProducer>,
        prepared: &Arc<crate::runtime_db::transitions::PreparedInputTransition>,
        reservation: &super::mutation_owner::MutationReservation,
    ) -> Result<Self> {
        let intent = prepared
            .original_child_monitor_binding()
            .context("binding witness lacks original facet")?;
        ensure!(
            self.input_attempt()
                .is_some_and(|attempt| attempt.prepared() == prepared.descriptor())
                && &intent.original == source.address()
                && !self.original_attempt_finished()
                && prepared.descriptor().scopes() == std::slice::from_ref(&source.address().scope)
                && reservation.agent_id() == source.address().scope.agent_id
                && reservation.identity_incarnation()
                    == source.address().scope.identity_incarnation,
            "binding witness changed original source/owner"
        );
        if let Some(root) = reservation.capture_ready_snapshot()? {
            self.input_child_binding_observation =
                Some(super::mutation_owner::KnownCommit::State {
                    current_reads: root.current_reads.clone(),
                    state: root.state.clone(),
                    episodes: root.episodes.clone(),
                    input_evidence: root.input_evidence.clone(),
                    work: root.work.clone(),
                    tasks: root.tasks.clone(),
                    scheduling: root.scheduling.clone(),
                    runtime: None,
                });
        }
        self.input_child_binding = Some((Arc::downgrade(source), prepared.clone()));
        Ok(self)
    }
    pub(in crate::runtime) fn input_child_binding(
        &self,
    ) -> Option<(
        Arc<super::OriginalChildTaskProducer>,
        &Arc<crate::runtime_db::transitions::PreparedInputTransition>,
    )> {
        let (source, prepared) = self.input_child_binding.as_ref()?;
        Some((source.upgrade()?, prepared))
    }
    pub(in crate::runtime) fn input_child_binding_observation(
        &self,
    ) -> Option<&super::mutation_owner::KnownCommit> {
        self.input_child_binding_observation.as_ref()
    }
    pub(in crate::runtime) fn input_child_producer(
        &self,
    ) -> Option<Arc<super::OriginalChildTaskProducer>> {
        self.input_child_producer
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
    }
    pub(in crate::runtime) fn input_child_observation(
        &self,
        scope: &super::resident_input_evidence::InputEvidenceScope,
    ) -> Option<&super::mutation_owner::KnownCommit> {
        self.input_child_observations
            .iter()
            .find(|(s, _)| s == scope)
            .map(|(_, known)| known)
    }
    pub(super) fn retain_original_command_notifications(
        &self,
        proof: &crate::runtime_db::command_task_terminal::OriginalCommandTaskSparseRecovery,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<()> {
        let attempt = self
            .input_attempt()
            .context("Command notification lacks original attempt")?;
        ensure!(
            proof.matches_original(attempt) && self.original_attempt_finished(),
            "Command notification requires same finished attempt"
        );
        if self.input_storage_replayed() {
            return Ok(());
        }
        let source = self
            .input_command_producer
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .context("original Command notification responsibility unavailable")?;
        source.retain_sparse_notifications(proof, ledger)
    }
    /// Freeze only the true parent phase's already published observation before SQL.
    pub(crate) fn with_task_stop_parent_observation(
        mut self,
        reservation: &super::mutation_owner::MutationReservation,
        prepared: &crate::runtime_db::transitions::PreparedInputTransition,
    ) -> Result<Self> {
        let scope = prepared
            .task_stop_phase_scope()
            .context("Stop parent observation needs its true phase")?;
        let attempt = self
            .input_attempt()
            .context("Stop parent observation lacks original attempt")?;
        ensure!(
            !self.original_attempt_finished()
                && attempt.prepared() == prepared.descriptor()
                && prepared.descriptor().scopes() == std::slice::from_ref(scope)
                && scope.agent_id == reservation.agent_id()
                && scope.identity_incarnation == reservation.identity_incarnation(),
            "Stop parent observation differs from original held scope"
        );
        if let Some(root) = reservation.capture_ready_snapshot()? {
            ensure!(
                root.token.identity_incarnation == scope.identity_incarnation
                    && root.token.owner_epoch == reservation.owner_epoch(),
                "Stop parent observation epoch changed"
            );
            if root.tasks.is_none() {
                self.input_stop_parent = Some(super::mutation_owner::KnownCommit::State {
                    current_reads: root.current_reads.clone(),
                    state: root.state.clone(),
                    episodes: root.episodes.clone(),
                    input_evidence: root.input_evidence.clone(),
                    work: root.work.clone(),
                    tasks: None,
                    scheduling: root.scheduling.clone(),
                    runtime: None,
                });
            }
        }
        Ok(self)
    }
    pub(super) fn input_task_stop_parent(&self) -> Option<&super::mutation_owner::KnownCommit> {
        self.input_stop_parent.as_ref()
    }
    /// Keep this exact metadata-only actor observation before closing its gate.
    pub(crate) fn with_task_status_parent_observation(
        mut self,
        reservation: &super::mutation_owner::MutationReservation,
        prepared: &crate::runtime_db::transitions::PreparedInputTransition,
    ) -> Result<Self> {
        let scope = prepared
            .task_status_metadata_scope()
            .context("Status observation requires its focused phase")?;
        ensure!(
            !self.original_attempt_finished()
                && self
                    .input_attempt()
                    .is_some_and(|a| a.prepared() == prepared.descriptor())
                && prepared.descriptor().scopes() == std::slice::from_ref(scope)
                && scope.agent_id == reservation.agent_id()
                && scope.identity_incarnation == reservation.identity_incarnation(),
            "Status observation differs from its original scope"
        );
        if let Some(root) = reservation.capture_ready_snapshot()? {
            ensure!(
                root.token.owner_epoch == reservation.owner_epoch()
                    && root.token.identity_incarnation == scope.identity_incarnation,
                "Status observation epoch differs"
            );
            self.input_status_parent = Some(super::mutation_owner::KnownCommit::State {
                current_reads: root.current_reads.clone(),
                state: root.state.clone(),
                episodes: root.episodes.clone(),
                input_evidence: root.input_evidence.clone(),
                work: root.work.clone(),
                tasks: root.tasks.clone(),
                scheduling: root.scheduling.clone(),
                runtime: None,
            });
        }
        Ok(self)
    }
    pub(in crate::runtime) fn input_task_status_parent(
        &self,
    ) -> Option<&super::mutation_owner::KnownCommit> {
        self.input_status_parent.as_ref()
    }
    pub(crate) fn with_original_work_report(
        mut self,
        result: Option<&Arc<super::WorkItemMutationMaterial>>,
        limits: (usize, usize),
        prior: (usize, usize),
    ) -> Result<Self> {
        if let Some(result) = result.filter(|result| result.expects_completed_report()) {
            self.input_work_report = result.retain_original_report_attempt(
                self.input_attempt()
                    .context("report witness lacks original attempt")?,
                limits,
                prior,
            )?;
        }
        Ok(self)
    }
    pub(crate) fn with_original_task_input_child_material(
        mut self,
        prepared: &crate::runtime_db::transitions::PreparedInputTransition,
        reservation: &super::mutation_owner::MutationReservation,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let Some(material) = prepared.original_task_input_child_material() else {
            return Ok(self);
        };
        let scope = prepared
            .descriptor()
            .scopes()
            .first()
            .context("child input witness scope absent")?;
        ensure!(
            !self.original_attempt_finished()
                && self
                    .input_attempt()
                    .is_some_and(|attempt| attempt.prepared() == prepared.descriptor())
                && prepared.descriptor().scopes().len() == 1
                && self.input_task_input_child_material.is_none()
                && reservation.agent_id() == scope.agent_id
                && reservation.identity_incarnation() == scope.identity_incarnation,
            "child input witness differs from its original attempt/owner"
        );
        material.validate_binding(scope, reservation.owner_epoch(), ledger)?;
        self.input_task_input_child_material = Some((material.clone(), reservation.owner_epoch()));
        Ok(self)
    }
    /// Only actual still-live original allocations enter the first fresh gate.
    /// The exact receipt retains its old authority after a later identity change.
    pub(crate) fn original_input_material_prior(
        &self,
        scope: &super::resident_input_evidence::InputEvidenceScope,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        let work = self.original_work_report_material_prior(scope, ledger)?;
        let child = self
            .input_task_input_child_material
            .as_ref()
            .map(|(material, epoch)| {
                material.validate_binding(scope, *epoch, ledger)?;
                material.material_estimate()
            })
            .transpose()?
            .unwrap_or((0, 0));
        Ok((
            work.0
                .checked_add(child.0)
                .context("original Input prior bytes overflow")?,
            work.1
                .checked_add(child.1)
                .context("original Input prior nodes overflow")?,
        ))
    }
    /// Borrow actual retained allocations before a fresh reader's first gate.
    /// This does not read SQL, clone bodies or register the same guard again.
    pub(crate) fn original_work_report_material_prior(
        &self,
        scope: &super::resident_input_evidence::InputEvidenceScope,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        self.input_work_report
            .as_ref()
            .map(|source| source.original_material_prior(scope, ledger))
            .transpose()
            .map(|cost| cost.unwrap_or((0, 0)))
    }
    pub(crate) fn confirm_original_work_report(
        &self,
        receipt: &crate::runtime_db::input_mutation_receipts::InputMutationReceipt,
    ) -> Result<()> {
        if let Some(report) = &self.input_work_report {
            report.confirm_receipt(
                self.input_attempt()
                    .context("report receipt lacks original attempt")?,
                receipt,
            )?;
        }
        Ok(())
    }
    pub(crate) fn release_original_work_report(&self) -> Result<()> {
        if let Some(report) = &self.input_work_report {
            report.definitely_not_committed(
                self.input_attempt()
                    .context("report absence lacks original attempt")?,
            )?;
        }
        Ok(())
    }
    pub(crate) fn with_input_runtime(
        mut self,
        runtime: Vec<Arc<super::state_mutation::RuntimeMutationFacts>>,
    ) -> Result<Self> {
        ensure!(
            self.kind() == MutationWitnessKind::InputMutation,
            "runtime input targets require input witness"
        );
        let mut ids = std::collections::BTreeSet::new();
        let MutationWitnessFamily::InputMutation { prepared } = &self.family else {
            unreachable!()
        };
        for facts in &runtime {
            ensure!(
                ids.insert(facts.state.id.clone())
                    && prepared
                        .scopes()
                        .iter()
                        .any(|scope| scope.agent_id == facts.state.id),
                "saved input runtime is outside original scope group"
            );
        }
        self.input_runtime = runtime;
        Ok(self)
    }
    pub(super) fn input_runtime(
        &self,
        agent_id: &str,
    ) -> Option<&Arc<super::state_mutation::RuntimeMutationFacts>> {
        self.input_runtime
            .iter()
            .find(|facts| facts.state.id == agent_id)
    }
    pub(in crate::runtime) fn with_initial_task_stop_readonly(
        mut self,
        members: Vec<Arc<super::state_mutation::InitialTaskStopReadonlyMember>>,
    ) -> Result<Self> {
        ensure!(
            self.kind() == MutationWitnessKind::InputMutation || members.is_empty(),
            "readonly Stop requires input witness"
        );
        for member in &members {
            ensure!(
                self.matches_scope_group(
                    self.input_attempt
                        .as_ref()
                        .context("readonly Stop attempt missing")?
                        .prepared()
                        .scopes()
                ) && self
                    .input_attempt
                    .as_ref()
                    .unwrap()
                    .prepared()
                    .scopes()
                    .iter()
                    .any(|scope| scope == &member.target().scope),
                "readonly Stop witness member is outside original group"
            );
        }
        ensure!(
            members.len() <= 1,
            "initial Stop has multiple readonly children"
        );
        self.input_stop_readonly = members;
        Ok(self)
    }
    pub(super) fn initial_task_stop_readonly(
        &self,
        agent_id: &str,
    ) -> Option<&Arc<super::state_mutation::InitialTaskStopReadonlyMember>> {
        self.input_stop_readonly
            .iter()
            .find(|member| member.target().scope.agent_id == agent_id)
    }
    pub(crate) fn input_attempt(
        &self,
    ) -> Option<&crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt> {
        self.input_attempt.as_ref()
    }
    pub(crate) fn mark_input_storage_replayed(&self) -> Result<()> {
        ensure!(
            self.kind() == MutationWitnessKind::InputMutation,
            "storage replay observation requires an input witness"
        );
        ensure!(
            !self.original_attempt_finished(),
            "storage replay must be recorded before original attempt completion"
        );
        self.input_storage_replayed.store(true, Ordering::Release);
        Ok(())
    }
    pub(crate) fn input_storage_replayed(&self) -> bool {
        self.input_storage_replayed.load(Ordering::Acquire)
    }
    fn resolve_pending(
        &self,
        resolution: super::input_evidence_budget::PendingTicketResolution,
    ) -> Result<()> {
        if let Some((ledger, _)) = self.input_pending.first() {
            ledger.resolve_original_tickets(
                &self
                    .input_pending
                    .iter()
                    .map(|(_, ticket)| ticket)
                    .collect::<Vec<_>>(),
                resolution,
            )?;
        }
        Ok(())
    }
    pub(crate) fn mark_pending_started(&self) -> Result<()> {
        self.resolve_pending(super::input_evidence_budget::PendingTicketResolution::Started)
    }
    pub(crate) fn mark_pending_unknown(&self) -> Result<()> {
        self.resolve_pending(super::input_evidence_budget::PendingTicketResolution::Unknown)
    }
    #[cfg(test)]
    pub(crate) fn mark_pending_accepted(&self) -> Result<()> {
        self.mark_pending_result(true, false)
    }
    /// A committed Stop continuation may deliberately admit no input. Only its
    /// original typed ACK can resolve this attempt's new admission tickets.
    pub(crate) fn mark_original_input_receipt_pending(
        &self,
        receipt: &crate::runtime_db::input_mutation_receipts::InputMutationReceipt,
        fresh_receipt: bool,
    ) -> Result<()> {
        use crate::runtime_db::{
            input_mutation_receipts::InputMutationResult, task_stop::TaskStopResultOutcome,
        };
        let attempt = self
            .input_attempt()
            .context("input receipt has no original attempt")?;
        ensure!(
            self.kind() == MutationWitnessKind::InputMutation
                && receipt.prepared == *attempt.prepared(),
            "pending ACK differs from original input attempt"
        );
        ensure!(
            !fresh_receipt || self.original_attempt_finished(),
            "fresh pending ACK requires the original finished attempt"
        );
        let accepted = match &receipt.outcome.result {
            InputMutationResult::ChildTaskTerminal { ack } => {
                ensure!(
                    receipt.prepared.scopes().contains(&ack.original.scope),
                    "child pending ACK original scope differs"
                );
                matches!(
                    &ack.result,
                    crate::runtime_db::child_task_terminal::ChildTaskTerminalResult::Delivered { .. }
                )
            }
            InputMutationResult::CommandTaskTerminal { ack } => {
                ensure!(
                    ack.original.scope == receipt.prepared.scopes()[0],
                    "Command pending ACK original scope differs"
                );
                matches!(&ack.result,crate::runtime_db::command_task_terminal::CommandTaskTerminalResult::Delivered{..})
            }
            InputMutationResult::TaskStopPhase { ack } => match &ack.result {
                Some(result) => {
                    ensure!(
                        result.phase_nonce == receipt.prepared.command_nonce(),
                        "Stop Result pending ACK has another phase identity"
                    );
                    matches!(&result.outcome, TaskStopResultOutcome::Delivered { .. })
                }
                None => {
                    ensure!(
                        self.input_pending.is_empty(),
                        "Stop progress ACK cannot account for Result admission tickets"
                    );
                    false
                }
            },
            _ => receipt.outcome.applied,
        };
        self.mark_pending_result(accepted, fresh_receipt)
    }
    pub(crate) fn mark_pending_result(&self, accepted: bool, fresh_receipt: bool) -> Result<()> {
        if !self.input_pending_accepted.load(Ordering::Acquire) {
            self.resolve_pending(if accepted {
                super::input_evidence_budget::PendingTicketResolution::Accepted
            } else if fresh_receipt {
                super::input_evidence_budget::PendingTicketResolution::FreshNotCommitted
            } else {
                super::input_evidence_budget::PendingTicketResolution::DefinitelyNotCommitted
            })?;
            self.input_pending_accepted.store(true, Ordering::Release);
        }
        Ok(())
    }
    pub(crate) fn release_pending_not_committed(&self, fresh_proof: bool) -> Result<()> {
        self.release_original_work_report()?;
        self.resolve_pending(if fresh_proof {
            super::input_evidence_budget::PendingTicketResolution::FreshNotCommitted
        } else {
            super::input_evidence_budget::PendingTicketResolution::DefinitelyNotCommitted
        })
    }
    pub(crate) fn matches_scope_group(
        &self,
        scopes: &[super::resident_input_evidence::InputEvidenceScope],
    ) -> bool {
        match &self.family {
            MutationWitnessFamily::InputMutation { prepared } => prepared.scopes() == scopes,
            _ => {
                scopes.len() == 1
                    && self.owner() == (scopes[0].agent_id.as_str(), scopes[0].identity_incarnation)
            }
        }
    }

    pub(crate) fn episode_memory(
        operation_id: String,
        command: &EpisodeMemoryTransitionCommand,
    ) -> Result<Self> {
        ensure!(
            !operation_id.trim().is_empty(),
            "mutation operation is empty"
        );
        Ok(Self {
            operation_id,
            family: MutationWitnessFamily::EpisodeMemory {
                key: command.key().clone(),
                invocation: command.invocation().clone(),
                command_digest: command.command_digest().to_owned(),
            },
            original_attempt_finished: Arc::new(AtomicBool::new(false)),
            control_cancellation: None,
            control_owner_epoch: None,
            input_attempt: None,
            input_pending: Vec::new(),
            input_runtime: Vec::new(),
            input_stop_readonly: Vec::new(),
            input_stop_parent: None,
            input_work_report: None,
            input_task_input_child_material: None,
            input_status_parent: None,
            input_command_producer: None,
            input_child_producer: None,
            input_child_observations: Vec::new(),
            input_child_binding: None,
            input_child_binding_observation: None,
            input_pending_accepted: Arc::new(AtomicBool::new(false)),
            input_storage_replayed: Arc::new(AtomicBool::new(false)),
        })
    }

    pub(super) fn control(
        operation_id: String,
        command: &PreparedControl,
        cancellation: Arc<super::control::PreparedControlCancellation>,
        owner_epoch: u64,
    ) -> Result<Self> {
        ensure!(
            !operation_id.trim().is_empty(),
            "mutation operation is empty"
        );
        Ok(Self {
            operation_id,
            family: MutationWitnessFamily::Control {
                key: command.key().clone(),
                invocation: command.invocation().clone(),
                command_digest: command.command_digest().to_owned(),
            },
            original_attempt_finished: Arc::new(AtomicBool::new(false)),
            control_cancellation: Some(cancellation),
            control_owner_epoch: Some(owner_epoch),
            input_attempt: None,
            input_pending: Vec::new(),
            input_runtime: Vec::new(),
            input_stop_readonly: Vec::new(),
            input_stop_parent: None,
            input_work_report: None,
            input_task_input_child_material: None,
            input_status_parent: None,
            input_command_producer: None,
            input_child_producer: None,
            input_child_observations: Vec::new(),
            input_child_binding: None,
            input_child_binding_observation: None,
            input_pending_accepted: Arc::new(AtomicBool::new(false)),
            input_storage_replayed: Arc::new(AtomicBool::new(false)),
        })
    }

    pub(crate) fn kind(&self) -> MutationWitnessKind {
        match self.family {
            MutationWitnessFamily::InputMutation { .. } => MutationWitnessKind::InputMutation,
            MutationWitnessFamily::EpisodeMemory { .. } => MutationWitnessKind::EpisodeMemory,
            MutationWitnessFamily::Control { .. } => MutationWitnessKind::Control,
        }
    }

    pub(crate) fn control_owner_epoch(&self) -> Option<u64> {
        self.control_owner_epoch
    }

    pub(super) fn control_cancellation(
        &self,
    ) -> Option<Arc<super::control::PreparedControlCancellation>> {
        self.control_cancellation.clone()
    }

    pub(crate) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub(crate) fn family(&self) -> &MutationWitnessFamily {
        &self.family
    }

    pub(crate) fn owner(&self) -> (&str, u64) {
        match &self.family {
            MutationWitnessFamily::EpisodeMemory { key, .. } => {
                (&key.agent_id, key.identity_incarnation)
            }
            MutationWitnessFamily::Control { key, .. } => (&key.agent_id, key.identity_incarnation),
            MutationWitnessFamily::InputMutation { prepared } => {
                let scope = &prepared.scopes()[0];
                (&scope.agent_id, scope.identity_incarnation)
            }
        }
    }

    /// Called immediately when the synchronous storage invocation returns, on
    /// both success and failure, before any async installation can be cancelled.
    pub(crate) fn finish_original_attempt(&self) {
        if let Some(attempt) = &self.input_attempt {
            attempt.finish_original_attempt();
        }
        self.original_attempt_finished
            .store(true, Ordering::Release);
    }

    pub(crate) fn original_attempt_finished(&self) -> bool {
        if let Some(attempt) = &self.input_attempt {
            return attempt.original_attempt_finished();
        }
        self.original_attempt_finished.load(Ordering::Acquire)
    }

    pub(crate) fn same_attempt(&self, other: &Self) -> bool {
        self.operation_id == other.operation_id
            && self.family == other.family
            && match (&self.input_attempt, &other.input_attempt) {
                (Some(a), Some(b)) => a.same_attempt(b),
                (None, None) => true,
                _ => false,
            }
            && Arc::ptr_eq(
                &self.original_attempt_finished,
                &other.original_attempt_finished,
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn witness() -> MutationWitness {
        MutationWitness {
            operation_id: "operation-a".into(),
            family: MutationWitnessFamily::EpisodeMemory {
                key: EpisodeMemoryKey {
                    agent_id: "agent-a".into(),
                    identity_incarnation: 2,
                    actual_dispatch_turn_id: "turn-a".into(),
                },
                invocation: EpisodeMemoryInvocation {
                    message_id: "message-a".into(),
                    turn_index: 1,
                },
                command_digest: "digest-a".into(),
            },
            original_attempt_finished: Arc::new(AtomicBool::new(false)),
            control_cancellation: None,
            control_owner_epoch: None,
            input_attempt: None,
            input_pending: Vec::new(),
            input_runtime: Vec::new(),
            input_stop_readonly: Vec::new(),
            input_stop_parent: None,
            input_work_report: None,
            input_task_input_child_material: None,
            input_status_parent: None,
            input_command_producer: None,
            input_child_producer: None,
            input_child_observations: Vec::new(),
            input_child_binding: None,
            input_child_binding_observation: None,
            input_pending_accepted: Arc::new(AtomicBool::new(false)),
            input_storage_replayed: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn an_address_alone_does_not_prove_the_original_attempt_finished() {
        let original = witness();
        let saved = original.clone();
        let same_address_new_attempt = witness();
        assert!(!saved.original_attempt_finished());
        assert!(saved.same_attempt(&original));
        assert!(!saved.same_attempt(&same_address_new_attempt));
        original.finish_original_attempt();
        assert!(saved.original_attempt_finished());
        assert!(!same_address_new_attempt.original_attempt_finished());
        assert_eq!(saved.owner(), ("agent-a", 2));
    }
}
