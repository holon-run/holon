//! Fixed original observations use Active scope and never authorize State or Task changes.
use super::*;

pub(in crate::runtime) struct OriginalRuntimeObservationMaterial {
    kind: OriginalLifecycleNoopKind,
    scope: super::super::resident_input_evidence::InputEvidenceScope,
    epoch: u64,
    ledger: super::super::input_evidence_budget::InputEvidenceBudgetLedger,
    records: usize,
    guard: Arc<super::super::input_evidence_budget::BudgetViewGuard>,
}
pub(in crate::runtime) struct OriginalRuntimeObservationAudit {
    event: AuditEvent,
    source: OriginalLifecycleNoopAuditSource,
}
impl OriginalRuntimeObservationMaterial {
    pub(in crate::runtime) fn freeze(
        self,
        runtime: &RuntimeHandle,
        event: AuditEvent,
    ) -> Result<OriginalRuntimeObservationAudit> {
        let source = OriginalLifecycleNoopAuditSource {
            scope: self.scope,
            owner_epoch: self.epoch,
            ledger: self.ledger,
            kind: self.kind,
            event_digest: OriginalLifecycleNoopAuditSource::digest(&event)?,
            shutdown_abort_digest: None,
            workspace_request: None,
            material_records: self.records,
            _material: self.guard,
        };
        source.validate_owner(runtime, &event)?;
        Ok(OriginalRuntimeObservationAudit { event, source })
    }
}
impl RuntimeHandle {
    pub(in crate::runtime) fn prepare_original_runtime_observation_material(
        &self,
        kind: OriginalLifecycleNoopKind,
        fields: &impl serde::Serialize,
    ) -> Result<OriginalRuntimeObservationMaterial> {
        anyhow::ensure!(
            kind.is_runtime_observation(),
            "another original Runtime observation family"
        );
        // Before JSON, command_display/boundary or Skill DTO copies. The same
        // reservation then covers the frozen Event and all receipt copies.
        let (encoded, nodes) = super::super::input_evidence_budget::borrowed_json_material(&(
            self.instance_agent_id(),
            fields,
        ))?;
        let records = nodes
            .checked_add(128)
            .and_then(|n| n.checked_mul(8))
            .context("Runtime observation copy records overflow")?;
        let bytes = encoded
            .checked_add(8192)
            .and_then(|n| n.checked_mul(8))
            .and_then(|n| n.checked_add(u64::try_from(records).ok()?.checked_mul(256)?))
            .context("Runtime observation copy bytes overflow")?;
        let ledger = &self.inner.input_evidence_budget;
        let limits = ledger.limits();
        anyhow::ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Runtime observation allowance overflow")?
                    .min(64 * 1024 * 1024)
                && records <= usize::try_from(limits.record_limit)?.min(65536),
            super::super::current_read_facts::CurrentReadError::ResourceExhausted
        );
        let guard = ledger.register_host_owned_bytes(
            super::super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        guard.validate_host_owned_material_binding(ledger, bytes)?;
        Ok(OriginalRuntimeObservationMaterial {
            kind,
            scope: super::super::resident_input_evidence::InputEvidenceScope {
                agent_id: self.instance_agent_id().into(),
                identity_incarnation: self.instance_incarnation(),
            },
            epoch: self.instance_owner_epoch(),
            ledger: ledger.clone(),
            records,
            guard,
        })
    }
    pub(in crate::runtime) fn prepare_original_work_completion_warning(
        &self,
        work_item_id: &str,
        kind: &str,
        message: &str,
        source_turn_index: Option<u64>,
        source_round: Option<usize>,
    ) -> Result<OriginalRuntimeObservationAudit> {
        let material = self.prepare_original_runtime_observation_material(
            OriginalLifecycleNoopKind::WorkCompletionWarning,
            &(work_item_id, kind, message, source_turn_index, source_round),
        )?;
        material.freeze(
            self,
            AuditEvent::legacy(
                "work_item_completion_warning",
                serde_json::json!({
                    "agent_id":self.instance_agent_id(), "work_item_id":work_item_id,
                    "kind":kind, "message":message, "source_turn_index":source_turn_index,
                    "source_round":source_round,
                }),
            ),
        )
    }
    pub(in crate::runtime) async fn finish_owned_runtime_observation_audit(
        &self,
        audit: OriginalRuntimeObservationAudit,
    ) -> Result<()> {
        audit.source.validate_owner(self, &audit.event)?;
        let mut owner = self.begin_mutation().await?;
        owner.append_held_runtime_observation_audit(audit).await
    }
}
impl RuntimeMutation {
    pub(in crate::runtime) async fn append_held_runtime_observation_audit(
        &mut self,
        audit: OriginalRuntimeObservationAudit,
    ) -> Result<()> {
        let OriginalRuntimeObservationAudit { event, source } = audit;
        source.validate_owner(&self.runtime, &event)?;
        anyhow::ensure!(
            self.reservations[self.primary_index].owner_epoch() == source.owner_epoch,
            "Runtime observation owner changed while waiting"
        );
        let root = self.capture_published_root()?;
        root.active_identity()?;
        let prepared =
            crate::runtime_db::transitions::PreparedInputTransition::runtime_observation_audit(
                self.input_evidence_scope(),
                event,
                root.state.clone(),
                source,
                None,
            )?;
        drop(root);
        self.commit_prepared_input_transition(prepared).await?;
        Ok(())
    }
}
#[cfg(test)]
#[path = "runtime_observation_audit_tests.rs"]
mod tests;
