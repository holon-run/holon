//! An original Runtime-loop failure observation; it grants no execution or exit.
use super::*;

pub(in crate::runtime) struct OriginalRuntimeLoopFailureAudit {
    event: AuditEvent,
    source: OriginalLifecycleNoopAuditSource,
}
impl RuntimeHandle {
    pub(in crate::runtime) fn prepare_original_runtime_loop_failure_audit(
        &self,
        agent_id: &str,
        summary: &str,
        descriptor: &crate::runtime_error::RuntimeErrorDescriptor,
        released_claims: usize,
    ) -> Result<OriginalRuntimeLoopFailureAudit> {
        anyhow::ensure!(
            agent_id == self.instance_agent_id() && released_claims == 0,
            "Runtime failure observation differs from its original owner/claim policy"
        );
        let (encoded, nodes) = crate::runtime::input_evidence_budget::borrowed_json_material(&(
            agent_id,
            summary,
            descriptor.domain,
            &descriptor.code,
            descriptor.retryable,
            &descriptor.recovery_hint,
            &descriptor.safe_context,
            &descriptor.source_chain,
            "bounded_restart",
            released_claims,
        ))?;
        let bytes = encoded
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("Runtime failure native bytes overflow")?,
            )
            .and_then(|n| n.checked_add(8192))
            .and_then(|n| n.checked_mul(4))
            .context("Runtime failure copy bytes overflow")?;
        let records = nodes
            .checked_add(128)
            .and_then(|n| n.checked_mul(4))
            .context("Runtime failure copy nodes overflow")?;
        let ledger = &self.inner.input_evidence_budget;
        let limits = ledger.limits();
        anyhow::ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Runtime failure limit overflow")?
                    .min(64 * 1024 * 1024)
                && records <= usize::try_from(limits.record_limit)?.min(65536),
            crate::runtime::current_read_facts::CurrentReadError::ResourceExhausted
        );
        let material = ledger.register_host_owned_bytes(
            crate::runtime::input_evidence_budget::InputMemoryCategory::Candidate,
            crate::runtime::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        // The original observation is frozen before waiting for any owner.
        // Only the later same-scope State is read as its exact OCC witness.
        let event = AuditEvent::legacy(
            "agent_runtime_loop_failed",
            serde_json::json!({
                "agent_id":agent_id,"error":summary,"domain":descriptor.domain,"code":descriptor.code,
                "retryable":descriptor.retryable,"recovery_hint":descriptor.recovery_hint,
                "safe_context":descriptor.safe_context,"source_chain":descriptor.source_chain,
                "recovery":"bounded_restart","released_claims":released_claims,
            }),
        );
        let source = OriginalLifecycleNoopAuditSource {
            scope: crate::runtime::resident_input_evidence::InputEvidenceScope {
                agent_id: agent_id.into(),
                identity_incarnation: self.instance_incarnation(),
            },
            owner_epoch: self.instance_owner_epoch(),
            ledger: ledger.clone(),
            kind: OriginalLifecycleNoopKind::RuntimeLoopFailure,
            event_digest: OriginalLifecycleNoopAuditSource::digest(&event)?,
            shutdown_abort_digest: None,
            workspace_request: None,
            material_records: records,
            _material: material,
        };
        source.validate_owner(self, &event)?;
        Ok(OriginalRuntimeLoopFailureAudit { event, source })
    }
    pub(in crate::runtime) async fn finish_owned_runtime_loop_failure_audit(
        &self,
        audit: OriginalRuntimeLoopFailureAudit,
    ) -> Result<()> {
        let OriginalRuntimeLoopFailureAudit { event, source } = audit;
        source.validate_owner(self, &event)?;
        let mut owner = self.begin_mutation().await?;
        anyhow::ensure!(
            owner.reservations[owner.primary_index].owner_epoch() == source.owner_epoch,
            "Runtime failure Audit owner changed while waiting"
        );
        let root = owner.capture_published_root()?;
        let prepared =
            crate::runtime_db::transitions::PreparedInputTransition::runtime_loop_failure_audit(
                owner.input_evidence_scope(),
                event,
                root.state.clone(),
                source,
            )?;
        drop(root);
        owner.commit_prepared_input_transition(prepared).await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "runtime_loop_failure_audit_tests.rs"]
mod tests;
