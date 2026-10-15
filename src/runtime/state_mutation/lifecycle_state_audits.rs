//! Original changed-State Lifecycle tails; this carrier grants no admission.
use super::*;
use crate::types::WorktreeSession;

pub(in crate::runtime) struct OriginalLifecycleStateAudits {
    events: Vec<AuditEvent>,
    material_records: usize,
    _material: Option<Arc<super::super::input_evidence_budget::BudgetViewGuard>>,
}
impl OriginalLifecycleStateAudits {
    pub(super) fn events(&self) -> &[AuditEvent] {
        &self.events
    }
}
impl RuntimeHandle {
    pub(in crate::runtime) fn prepare_original_workspace_detachment_audits<'a>(
        &self,
        agent_id: &str,
        ids: impl Iterator<Item = &'a str> + Clone,
        reason: Option<&str>,
    ) -> Result<OriginalLifecycleStateAudits> {
        anyhow::ensure!(
            agent_id == self.instance_agent_id(),
            "Lifecycle tail belongs to another original Runtime"
        );
        let mut bytes = 0u64;
        let mut nodes = 0usize;
        let mut count = 0usize;
        // Iterate borrowed source fields before allocating the Event Vec,
        // UUIDs, JSON maps, workspace-ID or agent-ID copies.
        for id in ids.clone() {
            let (encoded, native) = super::super::input_evidence_budget::borrowed_json_material(
                &(agent_id, id, reason),
            )?;
            bytes = bytes
                .checked_add(encoded)
                .and_then(|n| n.checked_add(4096))
                .and_then(|n| n.checked_add(u64::try_from(native).ok()?.checked_mul(256)?))
                .context("Lifecycle tail borrowed bytes overflow")?;
            nodes = nodes
                .checked_add(native)
                .and_then(|n| n.checked_add(64))
                .context("Lifecycle tail borrowed records overflow")?;
            count = count
                .checked_add(1)
                .context("Lifecycle tail count overflow")?;
        }
        if count == 0 {
            return Ok(OriginalLifecycleStateAudits {
                events: Vec::new(),
                material_records: 0,
                _material: None,
            });
        }
        let bytes = bytes
            .checked_mul(4)
            .context("Lifecycle tail copy bytes overflow")?;
        let nodes = nodes
            .checked_mul(4)
            .context("Lifecycle tail copy records overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        anyhow::ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Lifecycle tail allowance overflow")?
                    .min(64 * 1024 * 1024)
                && nodes <= usize::try_from(limits.record_limit)?.min(65536),
            super::super::current_read_facts::CurrentReadError::ResourceExhausted
        );
        let material = self.inner.input_evidence_budget.register_host_owned_bytes(
            super::super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let mut events = Vec::with_capacity(count);
        for id in ids {
            let data = match reason {
                Some(reason) => {
                    serde_json::json!({"agent_id":agent_id,"workspace_id":id,"reason":reason})
                }
                None => serde_json::json!({"agent_id":agent_id,"workspace_id":id}),
            };
            events.push(AuditEvent::legacy("workspace_detached", data));
        }
        Ok(OriginalLifecycleStateAudits {
            events,
            material_records: nodes,
            _material: Some(material),
        })
    }
}
impl RuntimeMutation {
    pub(in crate::runtime) async fn persist_state_with_original_lifecycle_audits(
        &mut self,
        audits: OriginalLifecycleStateAudits,
    ) -> Result<()> {
        self.persist_state_with_original_lifecycle_audits_and_workspace_request(audits, None)
            .await
    }
    pub(in crate::runtime) async fn persist_state_with_original_lifecycle_audits_and_workspace_request(
        &mut self,
        audits: OriginalLifecycleStateAudits,
        material: Option<Arc<super::super::OriginalWorkspaceRequestMaterial>>,
    ) -> Result<()> {
        anyhow::ensure!(
            audits.events.is_empty() || self.candidate.state != self.base.state,
            "nonempty Lifecycle State tails require their actual State change"
        );
        if audits.events.is_empty() {
            return Box::pin(
                self.persist_state_inner(None, None, None, None, None, None, material),
            )
            .await;
        }
        Box::pin(self.persist_state_inner(None, None, None, None, None, Some(audits), material))
            .await
    }
}

#[cfg(test)]
#[path = "lifecycle_state_audit_tests.rs"]
mod tests;

/// Only its completed original Lifecycle branch can mint this source.
/// It carries no Root, Runtime, database or historical State body.
#[derive(Debug)]
pub(crate) struct OriginalLifecycleNoopAuditSource {
    scope: super::super::resident_input_evidence::InputEvidenceScope,
    owner_epoch: u64,
    ledger: super::super::input_evidence_budget::InputEvidenceBudgetLedger,
    kind: OriginalLifecycleNoopKind,
    event_digest: [u8; 32],
    shutdown_abort_digest: Option<[u8; 32]>,
    workspace_request: Option<Arc<super::super::OriginalWorkspaceRequestMaterial>>,
    material_records: usize,
    _material: Arc<super::super::input_evidence_budget::BudgetViewGuard>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OriginalLifecycleNoopKind {
    ModelSet,
    ModelRequested,
    ModelCleared,
    ModelClearRequested,
    RuntimeShutdown,
    RuntimeLoopFailure,
    TaskInputCommand,
    TaskInputChild,
    CommandExecutionRequested,
    WorkCompletionWarning,
    SkillReadActivated,
    OperatorAbortStop,
    OperatorAbortIdle,
    WorkspaceAttached,
    WorkspaceUsed,
    WorkspaceExited,
    WorkspaceEntered,
    RegisteredWorktreeCreated,
    RegisteredWorktreeRetained,
    RegisteredWorktreeRemoved,
    LegacyWorktreeEntered,
    LegacyWorktreeExited,
    TaskWorktreeCreated,
    TaskWorktreeMetadataRecorded,
    TaskWorktreeCleanupFailed,
    TaskWorktreeCleanupAlreadyRemoved,
    TaskWorktreeCleanupRetained,
    WorktreeRetainedForReview,
    TaskWorktreeBranchCleanupRetained,
    WorktreeAutoCleanedUp,
    WorktreeAutoCleanupFailed,
}
impl OriginalLifecycleNoopKind {
    fn event_kind(self) -> &'static str {
        match self {
            Self::ModelSet => "agent_model_override_set",
            Self::ModelRequested => "agent_model_override_requested",
            Self::ModelCleared => "agent_model_override_cleared",
            Self::ModelClearRequested => "agent_model_override_clear_requested",
            Self::RuntimeShutdown => "runtime_service_shutdown_requested",
            Self::RuntimeLoopFailure => "agent_runtime_loop_failed",
            Self::TaskInputCommand | Self::TaskInputChild => "task_input_delivered",
            Self::CommandExecutionRequested => "process_execution_requested",
            Self::WorkCompletionWarning => "work_item_completion_warning",
            Self::SkillReadActivated => "skill_activated",
            Self::OperatorAbortStop | Self::OperatorAbortIdle => "current_run_aborted",
            Self::WorkspaceAttached => "workspace_attached",
            Self::WorkspaceUsed => "workspace_used",
            Self::WorkspaceExited => "workspace_exited",
            Self::WorkspaceEntered => "workspace_entered",
            Self::RegisteredWorktreeCreated => "worktree_created",
            Self::RegisteredWorktreeRetained => "worktree_cleanup_retained",
            Self::RegisteredWorktreeRemoved => "worktree_cleanup_removed",
            Self::LegacyWorktreeEntered => "worktree_entered",
            Self::LegacyWorktreeExited => "worktree_exited",
            Self::TaskWorktreeCreated => "worktree_created_for_task",
            Self::TaskWorktreeMetadataRecorded => "task_worktree_metadata_recorded",
            Self::TaskWorktreeCleanupFailed => "task_worktree_cleanup_failed",
            Self::TaskWorktreeCleanupAlreadyRemoved => "task_worktree_cleanup_already_removed",
            Self::TaskWorktreeCleanupRetained => "task_worktree_cleanup_retained",
            Self::WorktreeRetainedForReview => "worktree_retained_for_review",
            Self::TaskWorktreeBranchCleanupRetained => "task_worktree_branch_cleanup_retained",
            Self::WorktreeAutoCleanedUp => "worktree_auto_cleaned_up",
            Self::WorktreeAutoCleanupFailed => "worktree_auto_cleanup_failed",
        }
    }
    pub(crate) fn is_task_input(self) -> bool {
        matches!(self, Self::TaskInputCommand | Self::TaskInputChild)
    }
    pub(crate) fn is_runtime_observation(self) -> bool {
        matches!(
            self,
            Self::CommandExecutionRequested
                | Self::WorkCompletionWarning
                | Self::SkillReadActivated
        )
    }
    pub(crate) fn requires_active_observation_scope(self) -> bool {
        self.is_task_input() || self.is_runtime_observation()
    }
    pub(crate) fn is_model(self) -> bool {
        matches!(
            self,
            Self::ModelSet | Self::ModelRequested | Self::ModelCleared | Self::ModelClearRequested
        )
    }
    pub(crate) fn is_operator_abort(self) -> bool {
        matches!(self, Self::OperatorAbortStop | Self::OperatorAbortIdle)
    }
    pub(crate) fn is_workspace(self) -> bool {
        matches!(
            self,
            Self::WorkspaceAttached
                | Self::WorkspaceUsed
                | Self::WorkspaceExited
                | Self::WorkspaceEntered
                | Self::RegisteredWorktreeCreated
                | Self::RegisteredWorktreeRetained
                | Self::RegisteredWorktreeRemoved
                | Self::LegacyWorktreeEntered
                | Self::LegacyWorktreeExited
                | Self::TaskWorktreeCreated
                | Self::TaskWorktreeMetadataRecorded
                | Self::TaskWorktreeCleanupFailed
                | Self::TaskWorktreeCleanupAlreadyRemoved
                | Self::TaskWorktreeCleanupRetained
                | Self::WorktreeRetainedForReview
                | Self::TaskWorktreeBranchCleanupRetained
                | Self::WorktreeAutoCleanedUp
                | Self::WorktreeAutoCleanupFailed
        )
    }
    fn from_event_kind(kind: &str) -> Result<Self> {
        match kind {
            "agent_model_override_set" => Ok(Self::ModelSet),
            "agent_model_override_requested" => Ok(Self::ModelRequested),
            "agent_model_override_cleared" => Ok(Self::ModelCleared),
            "agent_model_override_clear_requested" => Ok(Self::ModelClearRequested),
            _ => anyhow::bail!("original Model noop source has another closed Event kind"),
        }
    }
}
impl OriginalLifecycleNoopAuditSource {
    fn digest(event: &AuditEvent) -> Result<[u8; 32]> {
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
        // Validation borrows the original tree before the joint phase gate;
        // it must not first allocate a full transient JSON/String copy.
        let mut writer = HashWriter(Sha256::new());
        serde_json::to_writer(&mut writer, event)?;
        Ok(writer.0.finalize().into())
    }
    pub(crate) fn kind(&self) -> OriginalLifecycleNoopKind {
        self.kind
    }
    pub(crate) fn validate_event(
        &self,
        scope: &super::super::resident_input_evidence::InputEvidenceScope,
        event: &AuditEvent,
    ) -> Result<()> {
        anyhow::ensure!(
            self.scope == *scope
                && event.kind == self.kind.event_kind()
                && (self.kind == OriginalLifecycleNoopKind::RuntimeShutdown
                    || self.kind.is_workspace()
                    || self.kind.is_task_input()
                    || self.kind == OriginalLifecycleNoopKind::CommandExecutionRequested
                    || event.data["agent_id"].as_str() == Some(scope.agent_id.as_str()))
                && Self::digest(event)? == self.event_digest,
            "original Lifecycle noop Event/scope changed"
        );
        if self.kind.is_task_input() {
            anyhow::ensure!(
                event.data["task_id"].as_str().is_some()
                    && event.data["bytes_written"].as_u64().is_some()
                    && match self.kind {
                        OriginalLifecycleNoopKind::TaskInputCommand =>
                            event.data["task_kind"] == "command_task"
                                && event.data.get("child_agent_id").is_none()
                                && event.data["input_target"].as_str().is_some(),
                        OriginalLifecycleNoopKind::TaskInputChild =>
                            event.data["child_agent_id"].as_str().is_some()
                                && event.data["input_target"] == "child_followup",
                        _ => unreachable!(),
                    },
                "TaskInput Audit differs from its original delivery family"
            );
        }
        let operator_mode = match self.kind {
            OriginalLifecycleNoopKind::OperatorAbortStop => Some("stop_after_abort"),
            OriginalLifecycleNoopKind::OperatorAbortIdle => Some("idle_after_abort"),
            _ => None,
        };
        if let Some(mode) = operator_mode {
            anyhow::ensure!(
                event.data["mode"] == mode && event.data["reason"] == "operator_aborted",
                "original operator abort Audit has another mode/reason"
            );
        }
        Ok(())
    }
    pub(crate) fn validate_shutdown_companion(
        &self,
        event: &AuditEvent,
        aborted: Option<&AuditEvent>,
    ) -> Result<()> {
        match (self.kind, self.shutdown_abort_digest, aborted) {
            (OriginalLifecycleNoopKind::RuntimeShutdown, Some(digest), Some(aborted)) => {
                anyhow::ensure!(
                    aborted.id != event.id
                        && aborted.kind == "current_run_aborted"
                        && aborted.data["mode"] == "shutdown"
                        && aborted.data["reason"] == "daemon_shutdown"
                        && aborted.data["agent_id"].as_str() == Some(self.scope.agent_id.as_str())
                        && aborted.data["run_id"] == event.data["aborted_run_id"]
                        && Self::digest(aborted)? == digest,
                    "original shutdown companion changed"
                );
            }
            (OriginalLifecycleNoopKind::RuntimeShutdown, None, None) => {
                anyhow::ensure!(
                    event.data["aborted_run_id"].is_null(),
                    "original shutdown lost its actual aborted-run companion"
                );
            }
            (_, None, None) => {}
            _ => anyhow::bail!("original Lifecycle source has another shutdown companion"),
        }
        Ok(())
    }
    pub(crate) fn validate_accounting(
        &self,
        scope: &super::super::resident_input_evidence::InputEvidenceScope,
        ledger: &super::super::input_evidence_budget::InputEvidenceBudgetLedger,
        event: &AuditEvent,
    ) -> Result<()> {
        self.validate_event(scope, event)?;
        anyhow::ensure!(
            self.ledger.same_ledger(ledger) && self._material.matches_resource_ledger(ledger),
            "original Lifecycle noop source belongs to another actual ledger"
        );
        Ok(())
    }
    pub(crate) fn material_prior(
        &self,
        scope: &super::super::resident_input_evidence::InputEvidenceScope,
        ledger: &super::super::input_evidence_budget::InputEvidenceBudgetLedger,
        event: &AuditEvent,
    ) -> Result<(usize, usize)> {
        self.validate_accounting(scope, ledger, event)?;
        // The original source allocation remains alive in Prepared. Borrow its
        // entire actual cost, never register its Event/body a second time.
        let prior = match self.workspace_request.as_ref() {
            Some(material) => {
                material.validate_binding(scope, self.owner_epoch, ledger)?;
                material.material_estimate()?
            }
            None => (0, 0),
        };
        Ok((
            usize::try_from(self._material.owned_material_bytes()?)?
                .checked_add(prior.0)
                .context("Workspace Audit combined bytes overflow")?,
            self.material_records
                .checked_add(prior.1)
                .context("Workspace Audit combined records overflow")?,
        ))
    }
    fn validate_owner(&self, runtime: &RuntimeHandle, event: &AuditEvent) -> Result<()> {
        self.validate_accounting(
            &super::super::resident_input_evidence::InputEvidenceScope {
                agent_id: runtime.instance_agent_id().into(),
                identity_incarnation: runtime.inner.identity_incarnation,
            },
            &runtime.inner.input_evidence_budget,
            event,
        )?;
        anyhow::ensure!(
            self.owner_epoch == runtime.instance_owner_epoch(),
            "original Lifecycle noop source belongs to another owner epoch"
        );
        Ok(())
    }
}
pub(in crate::runtime) struct OriginalLifecycleNoopAudit {
    event: AuditEvent,
    source: OriginalLifecycleNoopAuditSource,
    fault: Option<TransitionFaultPoint>,
}
impl RuntimeHandle {
    pub(in crate::runtime) fn prepare_original_model_override_audit(
        &self,
        agent_id: &str,
        model: &crate::types::AgentModelState,
        pending: bool,
        clearing: bool,
    ) -> Result<OriginalLifecycleStateAudits> {
        anyhow::ensure!(
            agent_id == self.instance_agent_id(),
            "original Model Audit belongs to another Runtime"
        );
        // Borrow exactly the original Audit DTO fields before its first clone.
        let (encoded, nodes) = super::super::input_evidence_budget::borrowed_json_material(&(
            agent_id,
            &model.source,
            &model.effective_model,
            &model.requested_model,
            &model.active_model,
            &model.override_model,
            &model.override_reasoning_effort,
            model.override_service_tier,
            model.effective_service_tier,
            model.fallback_active,
            pending,
        ))?;
        let bytes = encoded
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("Model Audit native bytes overflow")?,
            )
            .and_then(|n| n.checked_add(8192))
            .and_then(|n| n.checked_mul(4))
            .context("Model Audit copy bytes overflow")?;
        let records = nodes
            .checked_add(128)
            .and_then(|n| n.checked_mul(4))
            .context("Model Audit copy records overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        anyhow::ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Model Audit cap overflow")?
                    .min(64 * 1024 * 1024)
                && records <= usize::try_from(limits.record_limit)?.min(65536),
            super::super::current_read_facts::CurrentReadError::ResourceExhausted
        );
        let material = self.inner.input_evidence_budget.register_host_owned_bytes(
            super::super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let kind = match (clearing, pending) {
            (false, false) => OriginalLifecycleNoopKind::ModelSet,
            (false, true) => OriginalLifecycleNoopKind::ModelRequested,
            (true, false) => OriginalLifecycleNoopKind::ModelCleared,
            (true, true) => OriginalLifecycleNoopKind::ModelClearRequested,
        };
        Ok(OriginalLifecycleStateAudits {
            events: vec![AuditEvent::legacy(
                kind.event_kind(),
                to_json_value(&AgentModelOverrideAuditEvent::from_model_state(
                    agent_id, model, pending,
                )),
            )],
            material_records: records,
            _material: Some(material),
        })
    }
    pub(in crate::runtime) async fn finish_owned_lifecycle_noop_audit(
        &self,
        audit: OriginalLifecycleNoopAudit,
    ) -> Result<()> {
        let OriginalLifecycleNoopAudit {
            event,
            source,
            fault,
        } = audit;
        // Reject another instance/epoch/ledger before beginning its SQL owner.
        source.validate_owner(self, &event)?;
        let mut owner = self.begin_mutation().await?;
        let reservation = &owner.reservations[owner.primary_index];
        reservation.validate_owner(&self.inner.mutation_owner)?;
        anyhow::ensure!(
            reservation.owner_epoch() == source.owner_epoch,
            "original Lifecycle noop owner changed while waiting"
        );
        let root = owner.capture_published_root()?;
        let prepared =
            crate::runtime_db::transitions::PreparedInputTransition::runtime_lifecycle_noop_audit(
                owner.input_evidence_scope(),
                event,
                root.state.clone(),
                source,
                fault,
            )?;
        // This fresh State is the OCC witness only. The original Event is never
        // regenerated from a later State or from a replacement Runtime.
        drop(root);
        owner.commit_prepared_input_transition(prepared).await?;
        Ok(())
    }
}
impl RuntimeMutation {
    fn freeze_original_model_noop_source(
        &self,
        event: &AuditEvent,
        material: Arc<super::super::input_evidence_budget::BudgetViewGuard>,
        material_records: usize,
    ) -> Result<OriginalLifecycleNoopAuditSource> {
        self.validate_candidate_identity()?;
        anyhow::ensure!(
            self.candidate.state == self.base.state,
            "original Model noop source requires its unchanged actual State"
        );
        let reservation = &self.reservations[self.primary_index];
        reservation.validate_owner(&self.runtime.inner.mutation_owner)?;
        let source = OriginalLifecycleNoopAuditSource {
            scope: self.input_evidence_scope(),
            owner_epoch: reservation.owner_epoch(),
            ledger: self.runtime.inner.input_evidence_budget.clone(),
            kind: OriginalLifecycleNoopKind::from_event_kind(&event.kind)?,
            event_digest: OriginalLifecycleNoopAuditSource::digest(event)?,
            shutdown_abort_digest: None,
            workspace_request: None,
            material_records,
            _material: material,
        };
        source.validate_owner(&self.runtime, event)?;
        Ok(source)
    }
    pub(in crate::runtime) async fn persist_model_override_with_original_audit(
        &mut self,
        audits: OriginalLifecycleStateAudits,
    ) -> Result<Option<OriginalLifecycleNoopAudit>> {
        anyhow::ensure!(
            audits.events.len() == 1,
            "original Model branch requires exactly its frozen Audit"
        );
        let event = &audits.events[0];
        OriginalLifecycleNoopKind::from_event_kind(&event.kind)?;
        anyhow::ensure!(
            event.data["agent_id"].as_str() == Some(self.candidate.state.id.as_str())
                && event.data["override_model"]
                    == serde_json::to_value(&self.candidate.state.model_override)?
                && event.data["override_reasoning_effort"]
                    == serde_json::to_value(&self.candidate.state.model_override_reasoning_effort)?
                && event.data["override_service_tier"]
                    == serde_json::to_value(self.candidate.state.model_override_service_tier)?,
            "original Model Audit differs from its actual business candidate"
        );
        if self.candidate.state != self.base.state {
            self.persist_state_with_original_lifecycle_audits(audits)
                .await?;
            return Ok(None);
        }
        let OriginalLifecycleStateAudits {
            mut events,
            material_records,
            _material,
        } = audits;
        let event = events.pop().context("original Model Audit missing")?;
        let source = self.freeze_original_model_noop_source(
            &event,
            _material.context("original Model Audit material missing")?,
            material_records,
        )?;
        let audit = OriginalLifecycleNoopAudit {
            event,
            source,
            fault: self.runtime.take_transition_fault(),
        };
        // The public entry or Host creation actor already owns the complete
        // request. Retain the original transient install without new admission.
        Box::pin(self.persist_state_inner(None, None, None, None, None, None, None)).await?;
        Ok(Some(audit))
    }
}

#[cfg(test)]
#[path = "lifecycle_model_audit_tests.rs"]
mod model_tests;

/// Only the original Host shutdown/drain actor consumes this fixed pair.
/// The Event source observes local retirement; it is no process-exit proof.
pub(in crate::runtime) struct OriginalRuntimeShutdownAudits {
    event: AuditEvent,
    aborted: Option<AuditEvent>,
    source: OriginalLifecycleNoopAuditSource,
}
impl RuntimeMutation {
    pub(in crate::runtime) fn freeze_original_runtime_shutdown_audits(
        &self,
        had_abort_handle: bool,
    ) -> Result<OriginalRuntimeShutdownAudits> {
        self.validate_candidate_identity()?;
        anyhow::ensure!(
            self.candidate.state == self.base.state,
            "shutdown Audit source requires its actual acknowledged State"
        );
        let reservation = &self.reservations[self.primary_index];
        reservation.validate_owner(&self.runtime.inner.mutation_owner)?;
        let state = &self.candidate.state;
        // Posture keeps the original abort handle in candidate/facts. Borrow
        // its run ID after the State ACK, instead of retaining an extra String
        // or a new Audit allocation across that preceding transaction.
        let aborted_run_id = if had_abort_handle {
            Some(
                self.candidate
                    .current_run_abort
                    .as_ref()
                    .context("original shutdown lost its acknowledged abort handle")?
                    .run_id
                    .as_str(),
            )
        } else {
            anyhow::ensure!(
                self.candidate.current_run_abort.is_none(),
                "original shutdown acquired another abort handle"
            );
            None
        };
        let (encoded, nodes) = super::super::input_evidence_budget::borrowed_json_material(&(
            &state.id,
            &state.status,
            &state.current_run_id,
            aborted_run_id,
        ))?;
        let bytes = encoded
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("shutdown Audit native bytes overflow")?,
            )
            .and_then(|n| n.checked_add(16384))
            .and_then(|n| n.checked_mul(4))
            .context("shutdown Audit copies overflow")?;
        let records = nodes
            .checked_add(256)
            .and_then(|n| n.checked_mul(4))
            .context("shutdown Audit records overflow")?;
        let ledger = &self.runtime.inner.input_evidence_budget;
        let limits = ledger.limits();
        anyhow::ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("shutdown Audit cap overflow")?
                    .min(64 * 1024 * 1024)
                && records <= usize::try_from(limits.record_limit)?.min(65536),
            super::super::current_read_facts::CurrentReadError::ResourceExhausted
        );
        let material = ledger.register_host_owned_bytes(
            super::super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let event = AuditEvent::legacy(
            "runtime_service_shutdown_requested",
            serde_json::json!({
                "aborted_run_id":aborted_run_id,"status":state.status,
                "current_run_id":state.current_run_id,
            }),
        );
        let aborted = aborted_run_id.map(|run_id| {
            AuditEvent::legacy(
                "current_run_aborted",
                serde_json::json!({"agent_id":state.id,"run_id":run_id,
                "mode":"shutdown","reason":"daemon_shutdown"}),
            )
        });
        let source = OriginalLifecycleNoopAuditSource {
            scope: self.input_evidence_scope(),
            owner_epoch: reservation.owner_epoch(),
            ledger: ledger.clone(),
            kind: OriginalLifecycleNoopKind::RuntimeShutdown,
            event_digest: OriginalLifecycleNoopAuditSource::digest(&event)?,
            shutdown_abort_digest: aborted
                .as_ref()
                .map(OriginalLifecycleNoopAuditSource::digest)
                .transpose()?,
            workspace_request: None,
            material_records: records,
            _material: material,
        };
        source.validate_owner(&self.runtime, &event)?;
        source.validate_shutdown_companion(&event, aborted.as_ref())?;
        Ok(OriginalRuntimeShutdownAudits {
            event,
            aborted,
            source,
        })
    }
}
impl RuntimeHandle {
    pub(in crate::runtime) async fn finish_owned_runtime_shutdown_audits(
        &self,
        audits: OriginalRuntimeShutdownAudits,
    ) -> Result<()> {
        let OriginalRuntimeShutdownAudits {
            event,
            aborted,
            source,
        } = audits;
        source.validate_owner(self, &event)?;
        source.validate_shutdown_companion(&event, aborted.as_ref())?;
        // The original Host shutdown operation or temporary cleanup worker
        // already supervises this continuation after normal admission closes.
        let mut owner = self.begin_mutation().await?;
        let reservation = &owner.reservations[owner.primary_index];
        reservation.validate_owner(&self.inner.mutation_owner)?;
        anyhow::ensure!(
            reservation.owner_epoch() == source.owner_epoch,
            "original shutdown owner changed while waiting"
        );
        let root = owner.capture_published_root()?;
        let prepared =
            crate::runtime_db::transitions::PreparedInputTransition::runtime_shutdown_audits(
                owner.input_evidence_scope(),
                event,
                aborted,
                root.state.clone(),
                source,
                None,
            )?;
        drop(root);
        owner.commit_prepared_input_transition(prepared).await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "runtime_shutdown_audit_tests.rs"]
mod shutdown_tests;

/// Frozen only after the original operator reducer's State/abort ACK.
struct OriginalOperatorAbortAudit {
    event: AuditEvent,
    source: OriginalLifecycleNoopAuditSource,
    outcome: CurrentRunAbortOutcome,
}
impl RuntimeMutation {
    fn freeze_original_operator_abort_audit(
        &self,
        mode: CurrentRunAbortMode,
    ) -> Result<OriginalOperatorAbortAudit> {
        self.validate_candidate_identity()?;
        anyhow::ensure!(
            self.candidate.state == self.base.state,
            "operator abort Audit requires its actual acknowledged State"
        );
        let reservation = &self.reservations[self.primary_index];
        reservation.validate_owner(&self.runtime.inner.mutation_owner)?;
        let handle = self
            .candidate
            .current_run_abort
            .as_ref()
            .context("operator abort lost its original acknowledged handle")?;
        let (encoded, nodes) = super::super::input_evidence_budget::borrowed_json_material(&(
            self.candidate.state.id.as_str(),
            handle.run_id.as_str(),
            mode.as_str(),
        ))?;
        let bytes = encoded
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("operator abort native bytes overflow")?,
            )
            .and_then(|n| n.checked_add(8192))
            .and_then(|n| n.checked_mul(4))
            .context("operator abort copies overflow")?;
        let records = nodes
            .checked_add(128)
            .and_then(|n| n.checked_mul(4))
            .context("operator abort records overflow")?;
        let ledger = &self.runtime.inner.input_evidence_budget;
        let limits = ledger.limits();
        anyhow::ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("operator abort cap overflow")?
                    .min(64 * 1024 * 1024)
                && records <= usize::try_from(limits.record_limit)?.min(65536),
            super::super::current_read_facts::CurrentReadError::ResourceExhausted
        );
        let material = ledger.register_host_owned_bytes(
            super::super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        // First additional outcome/Event Strings and JSON are covered by this
        // post-State phase; the original abort handle itself remains unchanged.
        let outcome = CurrentRunAbortOutcome {
            agent_id: self.candidate.state.id.clone(),
            run_id: handle.run_id.clone(),
            mode,
        };
        let event = AuditEvent::legacy(
            "current_run_aborted",
            serde_json::json!({
                "agent_id":outcome.agent_id,"run_id":outcome.run_id,
                "mode":mode.as_str(),"reason":"operator_aborted",
            }),
        );
        let source = OriginalLifecycleNoopAuditSource {
            scope: self.input_evidence_scope(),
            owner_epoch: reservation.owner_epoch(),
            ledger: ledger.clone(),
            kind: match mode {
                CurrentRunAbortMode::StopAfterAbort => OriginalLifecycleNoopKind::OperatorAbortStop,
                CurrentRunAbortMode::IdleAfterAbort => OriginalLifecycleNoopKind::OperatorAbortIdle,
            },
            event_digest: OriginalLifecycleNoopAuditSource::digest(&event)?,
            shutdown_abort_digest: None,
            workspace_request: None,
            material_records: records,
            _material: material,
        };
        source.validate_owner(&self.runtime, &event)?;
        Ok(OriginalOperatorAbortAudit {
            event,
            source,
            outcome,
        })
    }
}
impl RuntimeHandle {
    pub(in crate::runtime) async fn abort_current_run_owned(
        &self,
        request: CurrentRunAbortRequest,
    ) -> Result<CurrentRunAbortOutcome> {
        let mut guard = self.begin_mutation().await?;
        let Some(handle) = guard.current_run_abort.as_ref() else {
            return Err(CurrentRunAbortError::NoCurrentRun {
                agent_id: guard.state.id.clone(),
            }
            .into());
        };
        if let Some(requested_run_id) = request.run_id {
            if requested_run_id != handle.run_id {
                return Err(CurrentRunAbortError::StaleRunId {
                    requested_run_id,
                    current_run_id: handle.run_id.clone(),
                }
                .into());
            }
        }
        match request.mode {
            CurrentRunAbortMode::StopAfterAbort => {
                scheduler::apply_stop_projection(&mut guard.state)
            }
            CurrentRunAbortMode::IdleAfterAbort => {
                scheduler::apply_idle_projection(&mut guard.state, &self.inner.storage)?
            }
        }
        guard.defer_abort_current_run("operator_aborted");
        guard.persist_state(&self.inner.storage).await?;
        let audit = guard.freeze_original_operator_abort_audit(request.mode)?;
        drop(guard);
        let outcome = self.finish_owned_operator_abort_audit(audit).await?;
        self.inner.notify.notify_waiters();
        Ok(outcome)
    }
    async fn finish_owned_operator_abort_audit(
        &self,
        audit: OriginalOperatorAbortAudit,
    ) -> Result<CurrentRunAbortOutcome> {
        let OriginalOperatorAbortAudit {
            event,
            source,
            outcome,
        } = audit;
        source.validate_owner(self, &event)?;
        let mut owner = self.begin_mutation().await?;
        let reservation = &owner.reservations[owner.primary_index];
        reservation.validate_owner(&self.inner.mutation_owner)?;
        anyhow::ensure!(
            reservation.owner_epoch() == source.owner_epoch,
            "operator abort owner changed while waiting"
        );
        let root = owner.capture_published_root()?;
        let prepared =
            crate::runtime_db::transitions::PreparedInputTransition::runtime_operator_abort_audit(
                owner.input_evidence_scope(),
                event,
                root.state.clone(),
                source,
                None,
            )?;
        drop(root);
        owner.commit_prepared_input_transition(prepared).await?;
        Ok(outcome)
    }
}

#[cfg(test)]
#[path = "runtime_operator_abort_audit_tests.rs"]
mod operator_abort_tests;

pub(in crate::runtime) struct OriginalWorkspaceObservationAudit {
    event: AuditEvent,
    source: OriginalLifecycleNoopAuditSource,
}
impl RuntimeHandle {
    pub(in crate::runtime) fn prepare_original_workspace_attached_observation(
        &self,
        request: Arc<super::super::OriginalWorkspaceRequestMaterial>,
        workspace: &WorkspaceEntry,
    ) -> Result<OriginalWorkspaceObservationAudit> {
        self.prepare_original_workspace_observation(request,OriginalLifecycleNoopKind::WorkspaceAttached,
            &(&workspace.workspace_id,&workspace.workspace_anchor),||AuditEvent::legacy(
                "workspace_attached",serde_json::json!({
                    "workspace_id":workspace.workspace_id,"workspace_anchor":workspace.workspace_anchor,
                })))
    }
    pub(in crate::runtime) fn prepare_original_workspace_used_observation(
        &self,
        request: Arc<super::super::OriginalWorkspaceRequestMaterial>,
        workspace: &WorkspaceEntry,
        execution_root_id: &str,
        execution_root: &Path,
        access_mode: WorkspaceAccessMode,
        selected_cwd: &Path,
    ) -> Result<OriginalWorkspaceObservationAudit> {
        self.prepare_original_workspace_observation(request,OriginalLifecycleNoopKind::WorkspaceUsed,
            &(&workspace.workspace_id,&workspace.workspace_anchor,execution_root_id,execution_root,access_mode,selected_cwd),
            ||AuditEvent::legacy("workspace_used",serde_json::json!({
                "workspace_id":workspace.workspace_id,"workspace_anchor":workspace.workspace_anchor,
                "execution_root_id":execution_root_id,"execution_root":execution_root,
                "projection_kind":WorkspaceProjectionKind::CanonicalRoot,"access_mode":access_mode,"cwd":selected_cwd,
            })))
    }
    pub(in crate::runtime) fn prepare_original_workspace_exited_observation(
        &self,
        request: Arc<super::super::OriginalWorkspaceRequestMaterial>,
        entry: &ActiveWorkspaceEntry,
        profile: &crate::system::ExecutionProfile,
    ) -> Result<OriginalWorkspaceObservationAudit> {
        self.prepare_original_workspace_observation(request,OriginalLifecycleNoopKind::WorkspaceExited,
            &(&entry.workspace_id,&entry.execution_root_id,entry.projection_kind,entry.access_mode,
                &entry.execution_root_id,profile),
            ||AuditEvent::legacy("workspace_exited",serde_json::json!({
                "workspace_id":entry.workspace_id,"execution_root_id":entry.execution_root_id,
                "projection_kind":entry.projection_kind,"access_mode":entry.access_mode,
                "boundary":crate::system::HostLocalBoundary::from_parts(profile,Some(entry.projection_kind),
                    Some(entry.access_mode),Some(entry.execution_root_id.clone())).audit_metadata(),
            })))
    }
    pub(in crate::runtime) fn prepare_original_workspace_entered_observation(
        &self,
        request: Arc<super::super::OriginalWorkspaceRequestMaterial>,
        workspace: &WorkspaceEntry,
        execution_root_id: &str,
        execution_root: &Path,
        projection_kind: WorkspaceProjectionKind,
        access_mode: WorkspaceAccessMode,
        selected_cwd: &Path,
        profile: &crate::system::ExecutionProfile,
        existing: bool,
    ) -> Result<OriginalWorkspaceObservationAudit> {
        anyhow::ensure!(
            !existing || projection_kind == WorkspaceProjectionKind::GitWorktreeRoot,
            "existing worktree observation has another projection"
        );
        self.prepare_original_workspace_observation(request, OriginalLifecycleNoopKind::WorkspaceEntered,
            &(&workspace.workspace_id, &workspace.workspace_anchor, execution_root_id, execution_root,
                projection_kind, access_mode, selected_cwd, execution_root_id, profile, existing),
            || {
                let mut data=serde_json::json!({
                    "workspace_id":workspace.workspace_id,"workspace_anchor":workspace.workspace_anchor,
                    "execution_root_id":execution_root_id,"execution_root":execution_root,
                    "projection_kind":projection_kind,"access_mode":access_mode,"cwd":selected_cwd,
                    "boundary":crate::system::HostLocalBoundary::from_parts(profile,Some(projection_kind),Some(access_mode),
                        Some(execution_root_id.into())).audit_metadata(),
                });
                if existing {
                    data["detected_kind"]=serde_json::json!("existing_git_worktree");
                    data["ownership"]=serde_json::json!("external");
                }
                AuditEvent::legacy("workspace_entered",data)
            })
    }
    pub(in crate::runtime) fn prepare_original_registered_worktree_created_observation(
        &self,
        request: Arc<super::super::OriginalWorkspaceRequestMaterial>,
        workspace_id: &str,
        root_id: &str,
        branch: &str,
        base_ref: &str,
        base_commit: &str,
        activated: bool,
    ) -> Result<OriginalWorkspaceObservationAudit> {
        self.prepare_original_workspace_observation(request, OriginalLifecycleNoopKind::RegisteredWorktreeCreated,
            &(workspace_id, root_id, branch, base_ref, base_commit, activated), || AuditEvent::legacy("worktree_created",
                serde_json::json!({"workspace_id":workspace_id,"execution_root_id":root_id,"branch":branch,
                    "requested_base_ref":base_ref,"resolved_base_commit":base_commit,"activated":activated})))
    }
    pub(in crate::runtime) fn prepare_original_registered_worktree_retained_observation(
        &self,
        request: Arc<super::super::OriginalWorkspaceRequestMaterial>,
        root_id: &str,
        changed_files: &[String],
    ) -> Result<OriginalWorkspaceObservationAudit> {
        self.prepare_original_workspace_observation(
            request,
            OriginalLifecycleNoopKind::RegisteredWorktreeRetained,
            &(root_id, changed_files),
            || {
                AuditEvent::legacy(
                    "worktree_cleanup_retained",
                    serde_json::json!({"execution_root_id":root_id,"changed_files":changed_files}),
                )
            },
        )
    }
    pub(in crate::runtime) fn prepare_original_registered_worktree_removed_observation(
        &self,
        request: Arc<super::super::OriginalWorkspaceRequestMaterial>,
        root_id: &str,
        branch: Option<&str>,
        deleted: bool,
        retained_reason: Option<&str>,
    ) -> Result<OriginalWorkspaceObservationAudit> {
        self.prepare_original_workspace_observation(request, OriginalLifecycleNoopKind::RegisteredWorktreeRemoved,
            &(root_id, branch, deleted, retained_reason), || AuditEvent::legacy("worktree_cleanup_removed",
                serde_json::json!({"execution_root_id":root_id,"branch":branch,"branch_deleted":deleted,"branch_retained_reason":retained_reason})))
    }
    pub(in crate::runtime) fn prepare_original_legacy_worktree_entered_observation(
        &self,
        request: Arc<super::super::OriginalWorkspaceRequestMaterial>,
        session: &WorktreeSession,
        profile: &crate::system::ExecutionProfile,
        root_id: &str,
    ) -> Result<OriginalWorkspaceObservationAudit> {
        self.prepare_original_workspace_observation(request, OriginalLifecycleNoopKind::LegacyWorktreeEntered,
            &(session,profile,root_id),||AuditEvent::legacy("worktree_entered",serde_json::json!({
                "worktree":to_json_value(session),"boundary":crate::system::HostLocalBoundary::from_parts(profile,
                    Some(WorkspaceProjectionKind::GitWorktreeRoot),Some(WorkspaceAccessMode::ExclusiveWrite),Some(root_id.into())).audit_metadata(),
            })))
    }
    pub(in crate::runtime) fn prepare_original_legacy_worktree_exited_observation(
        &self,
        request: Arc<super::super::OriginalWorkspaceRequestMaterial>,
        session: Option<&WorktreeSession>,
        removed: bool,
    ) -> Result<OriginalWorkspaceObservationAudit> {
        self.prepare_original_workspace_observation(request,OriginalLifecycleNoopKind::LegacyWorktreeExited,
            &(session.map(|s|(&s.worktree_path,&s.worktree_branch)),removed),||AuditEvent::legacy("worktree_exited",serde_json::json!({
                "worktree_path":session.map(|s|&s.worktree_path),"worktree_branch":session.map(|s|&s.worktree_branch),"removed":removed,
            })))
    }
    pub(in crate::runtime) fn prepare_original_task_worktree_created_observation(
        &self,
        request: Arc<super::super::OriginalWorkspaceRequestMaterial>,
        task_id: &str,
        original_cwd: &Path,
        original_branch: &str,
        worktree_path: &Path,
        worktree_branch: &str,
    ) -> Result<OriginalWorkspaceObservationAudit> {
        self.prepare_original_workspace_observation(request,OriginalLifecycleNoopKind::TaskWorktreeCreated,
            &(task_id,original_cwd,original_branch,worktree_path,worktree_branch),||AuditEvent::legacy("worktree_created_for_task",
                serde_json::json!({"task_id":task_id,"worktree_path":worktree_path,"worktree_branch":worktree_branch,"original_cwd":original_cwd,"original_branch":original_branch})))
    }
    pub(in crate::runtime) fn prepare_original_task_worktree_metadata_observation(
        &self,
        request: Arc<super::super::OriginalWorkspaceRequestMaterial>,
        task_id: &str,
        seed: &super::super::ManagedWorktreeSeed,
    ) -> Result<OriginalWorkspaceObservationAudit> {
        self.prepare_original_workspace_observation(request,OriginalLifecycleNoopKind::TaskWorktreeMetadataRecorded,
            &(task_id,&seed.worktree_path,&seed.worktree_branch),||AuditEvent::legacy("task_worktree_metadata_recorded",
                serde_json::json!({"task_id":task_id,"worktree_path":seed.worktree_path,"worktree_branch":seed.worktree_branch})))
    }
    #[cfg(test)]
    pub(in crate::runtime) fn prepare_original_task_worktree_cleanup_observation(
        &self,
        request: Arc<super::super::OriginalWorkspaceRequestMaterial>,
        kind: OriginalLifecycleNoopKind,
        borrowed: &impl serde::Serialize,
        build: impl FnOnce() -> AuditEvent,
    ) -> Result<OriginalWorkspaceObservationAudit> {
        anyhow::ensure!(
            matches!(
                kind,
                OriginalLifecycleNoopKind::TaskWorktreeCleanupFailed
                    | OriginalLifecycleNoopKind::TaskWorktreeCleanupAlreadyRemoved
                    | OriginalLifecycleNoopKind::TaskWorktreeCleanupRetained
                    | OriginalLifecycleNoopKind::WorktreeRetainedForReview
                    | OriginalLifecycleNoopKind::TaskWorktreeBranchCleanupRetained
                    | OriginalLifecycleNoopKind::WorktreeAutoCleanedUp
                    | OriginalLifecycleNoopKind::WorktreeAutoCleanupFailed
            ),
            "Task cleanup observation has another original family"
        );
        self.prepare_original_workspace_observation(request, kind, borrowed, build)
    }
    fn prepare_original_workspace_observation(
        &self,
        request: Arc<super::super::OriginalWorkspaceRequestMaterial>,
        kind: OriginalLifecycleNoopKind,
        borrowed: &impl serde::Serialize,
        build: impl FnOnce() -> AuditEvent,
    ) -> Result<OriginalWorkspaceObservationAudit> {
        anyhow::ensure!(
            kind.is_workspace(),
            "Workspace observation has another family"
        );
        let scope = super::super::resident_input_evidence::InputEvidenceScope {
            agent_id: self.instance_agent_id().into(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        let ledger = &self.inner.input_evidence_budget;
        request.validate_binding(&scope, self.instance_owner_epoch(), ledger)?;
        let prior = request.material_estimate()?;
        let (encoded, nodes) =
            super::super::input_evidence_budget::borrowed_json_material(borrowed)?;
        let bytes = encoded
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("Workspace Audit native bytes overflow")?,
            )
            .and_then(|n| n.checked_add(16384))
            .and_then(|n| n.checked_mul(4))
            .context("Workspace Audit copies overflow")?;
        let records = nodes
            .checked_add(128)
            .and_then(|n| n.checked_mul(4))
            .context("Workspace Audit records overflow")?;
        let limits = ledger.limits();
        anyhow::ensure!(
            bytes
                .checked_add(u64::try_from(prior.0)?)
                .context("Workspace joint bytes overflow")?
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Workspace joint cap overflow")?
                    .min(64 * 1024 * 1024)
                && records
                    .checked_add(prior.1)
                    .context("Workspace joint records overflow")?
                    <= usize::try_from(limits.record_limit)?.min(65536),
            super::super::current_read_facts::CurrentReadError::ResourceExhausted
        );
        let guard = ledger.register_host_owned_bytes(
            super::super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let event = build();
        let source = OriginalLifecycleNoopAuditSource {
            scope,
            owner_epoch: self.instance_owner_epoch(),
            ledger: ledger.clone(),
            kind,
            event_digest: OriginalLifecycleNoopAuditSource::digest(&event)?,
            shutdown_abort_digest: None,
            workspace_request: Some(request),
            material_records: records,
            _material: guard,
        };
        source.validate_owner(self, &event)?;
        Ok(OriginalWorkspaceObservationAudit { event, source })
    }
    pub(in crate::runtime) async fn finish_owned_original_workspace_observation(
        &self,
        audit: OriginalWorkspaceObservationAudit,
    ) -> Result<()> {
        let OriginalWorkspaceObservationAudit { event, source } = audit;
        source.validate_owner(self, &event)?;
        let mut owner = self.begin_mutation().await?;
        anyhow::ensure!(
            owner.reservations[owner.primary_index].owner_epoch() == source.owner_epoch,
            "Workspace observation owner changed while waiting"
        );
        let root = owner.capture_published_root()?;
        let p=crate::runtime_db::transitions::PreparedInputTransition::runtime_original_workspace_audit(
            owner.input_evidence_scope(),event,root.state.clone(),source,None)?;
        drop(root);
        owner.commit_prepared_input_transition(p).await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "workspace_observation_audit_tests.rs"]
mod workspace_tests;

#[cfg(test)]
#[path = "workspace_home_exit_audit_tests.rs"]
mod workspace_home_exit_tests;

#[cfg(test)]
#[path = "workspace_enter_audit_tests.rs"]
mod workspace_enter_tests;

#[cfg(test)]
#[path = "workspace_control_audit_tests.rs"]
mod workspace_control_tests;

#[cfg(test)]
#[path = "physical_worktree_audit_tests.rs"]
mod physical_worktree_tests;

#[cfg(test)]
#[path = "physical_worktree_parent_audit_tests.rs"]
mod physical_worktree_parent_tests;

#[path = "runtime_loop_failure_audits.rs"]
mod runtime_loop_failure_audits;

#[path = "task_input_audits.rs"]
mod task_input_audits;

#[path = "runtime_observation_audits.rs"]
mod runtime_observation_audits;
