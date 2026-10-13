//! Transaction-local owned material, separate from reclaimable History limits.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_work::CapturedWork,
};
use std::sync::Arc;

/// The guards observe real transient ownership until the outer TX ends. They do
/// not reserve pending input responsibility or impose a shared Candidate limit.
pub(crate) struct DispatchMemoryTransactionBudget {
    scope: InputEvidenceScope,
    ledger: InputEvidenceBudgetLedger,
    guards: Vec<Arc<BudgetViewGuard>>,
    bytes: usize,
    records: usize,
    byte_limit: usize,
    record_limit: usize,
    state_material: usize,
    adapter_material: (usize, usize),
    finish: bool,
}
impl DispatchMemoryTransactionBudget {
    pub(crate) fn prepare_tx(
        tx: &Transaction<'_>,
        memory: &DispatchMemoryIntent,
        request: &super::super::work_snapshot::WorkDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        ensure!(
            request.byte_limit > 0 && request.record_limit > 0,
            DispatchMemoryError::ResourceExhausted
        );
        let scope = memory.binding_scope();
        let mut result = Self {
            scope,
            ledger: ledger.clone(),
            guards: Vec::new(),
            bytes: 0,
            records: 0,
            byte_limit: request.byte_limit.min(MAX_METADATA_BYTES),
            record_limit: request.record_limit.min(MAX_METADATA_RECORDS),
            state_material: 0,
            adapter_material: (0, 0),
            finish: matches!(memory, DispatchMemoryIntent::Finish { .. }),
        };
        // SQL lengths precede the full seed String, typed record and its clones.
        // Factors bound simultaneous decoding, clone, canonical JSON and write
        // readback buffers; this is an encoded footprint estimate, not RSS.
        match memory {
            DispatchMemoryIntent::Started { start } => {
                result.charge(serialized_bytes(start.as_ref())? as usize, 1, 8)?;
            }
            DispatchMemoryIntent::Deferred { binding, .. }
            | DispatchMemoryIntent::Finish { binding, .. } => {
                let bytes:usize=tx.query_row(
                    "SELECT length(CAST(payload_json AS BLOB)) FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
                    params![binding.key().agent_id,binding.key().identity_incarnation,binding.key().actual_dispatch_turn_id],
                    |r|r.get(0))?;
                ensure!(
                    bytes <= MAX_METADATA_BYTES,
                    DispatchMemoryError::ResourceExhausted
                );
                let members:usize=tx.query_row(
                    "SELECT COUNT(*) FROM dispatch_memory_checkpoints c,json_each(json_extract(c.payload_json,'$.obligation.members')) WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.turn_id=?3",
                    params![binding.key().agent_id,binding.key().identity_incarnation,binding.key().actual_dispatch_turn_id],|r|r.get(0))?;
                result.charge(bytes, members.saturating_add(1), 12)?;
                let receipt_bytes:usize=tx.query_row(
                    "SELECT COALESCE(MAX(length(CAST(r.prepared_json AS BLOB))+length(CAST(r.outcome_json AS BLOB))),0) FROM input_mutation_receipts r WHERE r.command_nonce IN (SELECT json_extract(c.payload_json,'$.original_start.command_nonce') FROM dispatch_memory_checkpoints c WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.turn_id=?3 UNION ALL SELECT json_extract(j.value,'$.original.command_nonce') FROM dispatch_memory_checkpoints c,json_each(json_extract(c.payload_json,'$.obligation.members')) j WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.turn_id=?3)",
                    params![binding.key().agent_id,binding.key().identity_incarnation,binding.key().actual_dispatch_turn_id],|r|r.get(0))?;
                // Member receipts are validated sequentially, only one is live.
                result.charge(receipt_bytes, usize::from(receipt_bytes > 0), 6)?;
            }
        }
        let (source, turn) = match memory {
            DispatchMemoryIntent::Started { start } => (
                &start.invocation.message_id,
                &start.key.actual_dispatch_turn_id,
            ),
            DispatchMemoryIntent::Deferred { binding, .. }
            | DispatchMemoryIntent::Finish { binding, .. } => (
                &binding.invocation().message_id,
                &binding.key().actual_dispatch_turn_id,
            ),
        };
        for (table, column, id) in [
            ("messages", "evidence_id", source),
            ("turn_records", "turn_id", turn),
        ] {
            let size: Option<usize> = tx
                .query_row(
                    &format!(
                        "SELECT length(CAST(payload_json AS BLOB)) FROM {table} WHERE {column}=?1"
                    ),
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(size) = size {
                result.charge(size, 1, 8)?;
            }
        }
        result.refresh_state_tx(tx)?;
        if result.finish {
            result.refresh_adapters_tx(tx)?;
        }
        Ok(result)
    }
    /// Original ended authorization shares the same transaction allowance.
    pub(crate) fn additional_material(
        &mut self,
        bytes: usize,
        records: usize,
        copies: usize,
    ) -> Result<()> {
        self.charge(bytes, records, copies)
    }
    fn charge(&mut self, bytes: usize, records: usize, copies: usize) -> Result<()> {
        let payload = bytes
            .checked_mul(copies)
            .context(DispatchMemoryError::ResourceExhausted)?;
        let metadata = records
            .checked_mul(copies)
            .and_then(|n| n.checked_mul(256))
            .context(DispatchMemoryError::ResourceExhausted)?;
        let combined = payload
            .checked_add(metadata)
            .context(DispatchMemoryError::ResourceExhausted)?;
        let next = self
            .bytes
            .checked_add(combined)
            .context(DispatchMemoryError::ResourceExhausted)?;
        let next_records = self
            .records
            .checked_add(
                records
                    .checked_mul(copies)
                    .context(DispatchMemoryError::ResourceExhausted)?,
            )
            .context(DispatchMemoryError::ResourceExhausted)?;
        ensure!(
            next <= self.byte_limit && next_records <= self.record_limit,
            DispatchMemoryError::ResourceExhausted
        );
        if combined > 0 {
            self.guards.push(self.ledger.register_owned_bytes(
                &self.scope,
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: payload as u64,
                    metadata_estimate_bytes: metadata as u64,
                },
            )?);
        }
        self.bytes = next;
        self.records = next_records;
        Ok(())
    }
    fn refresh_state_tx(&mut self, tx: &Transaction<'_>) -> Result<()> {
        let bytes: usize = tx.query_row(
            "SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1",
            [&self.scope.agent_id],
            |r| r.get(0),
        )?;
        if bytes > self.state_material {
            // expected token, actual business State, refreshed State, planner,
            // embedded write/readback and result clones may coexist.
            self.charge(
                bytes - self.state_material,
                usize::from(self.state_material == 0),
                16,
            )?;
            self.state_material = bytes;
        }
        Ok(())
    }
    fn refresh_adapters_tx(&mut self, tx: &Transaction<'_>) -> Result<()> {
        let mut records = 0usize;
        let mut bytes = 0usize;
        for (table, predicate) in [
            (
                "tasks",
                "owner_agent_id=?1 AND status IN ('queued','running','cancelling')",
            ),
            ("external_triggers", "target_agent_id=?1"),
            ("execution_protocol_work_items", "agent_id=?1"),
            ("execution_protocol_attempts", "agent_id=?1"),
            ("execution_protocol_outcomes", "agent_id=?1"),
        ] {
            let (count,size):(usize,usize)=tx.query_row(&format!(
                "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM {table} WHERE {predicate}"),
                [&self.scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            records = records
                .checked_add(count)
                .context(DispatchMemoryError::ResourceExhausted)?;
            bytes = bytes
                .checked_add(size)
                .context(DispatchMemoryError::ResourceExhausted)?;
        }
        let (count,size):(usize,usize)=tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(data_json AS BLOB))),0) FROM (SELECT data_json FROM audit_events WHERE agent_id=?1 ORDER BY event_seq DESC,created_at DESC LIMIT 64)",
            [&self.scope.agent_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        records = records
            .checked_add(count)
            .context(DispatchMemoryError::ResourceExhausted)?;
        bytes = bytes
            .checked_add(size)
            .context(DispatchMemoryError::ResourceExhausted)?;
        let (count, size) =
            super::super::input_evidence_snapshot::qualified_dispatch_brief_material_tx(
                tx,
                &self.scope,
                64,
            )?;
        records = records
            .checked_add(count)
            .context(DispatchMemoryError::ResourceExhausted)?;
        bytes = bytes
            .checked_add(size)
            .context(DispatchMemoryError::ResourceExhausted)?;
        let additional_bytes = bytes.saturating_sub(self.adapter_material.1);
        let additional_records = records.saturating_sub(self.adapter_material.0);
        // Covers decoded legacy adapters and their projection/planner copies.
        self.charge(additional_bytes, additional_records, 8)?;
        self.adapter_material = (
            records.max(self.adapter_material.0),
            bytes.max(self.adapter_material.1),
        );
        Ok(())
    }
    /// Work's original loader gets only the remaining combined preparation
    /// allowance. No alias or body is acquired by this calculation.
    pub(crate) fn work_request(
        &self,
        original: &super::super::work_snapshot::WorkDomainLoadRequest,
    ) -> Result<super::super::work_snapshot::WorkDomainLoadRequest> {
        let mut result = original.clone();
        let copies = if self.finish { 4 } else { 1 };
        result.byte_limit = self.byte_limit.saturating_sub(self.bytes) / copies;
        result.record_limit = self.record_limit.saturating_sub(self.records) / copies;
        ensure!(
            result.byte_limit > 0 && result.record_limit > 0,
            DispatchMemoryError::ResourceExhausted
        );
        Ok(result)
    }
    pub(crate) fn before_finalizer_tx(&mut self, tx: &Transaction<'_>) -> Result<()> {
        self.refresh_state_tx(tx)?;
        if self.finish {
            self.refresh_adapters_tx(tx)?;
        }
        Ok(())
    }
    pub(crate) fn capture_final_work(&mut self, work: &CapturedWork) -> Result<()> {
        let bytes = work.records.iter().try_fold(0usize, |total, record| {
            total
                .checked_add(serialized_bytes(&record.record)? as usize)
                .context(DispatchMemoryError::ResourceExhausted)
        })?;
        let metadata = serialized_bytes(&(
            &work.scope,
            work.revision,
            work.live_complete,
            &work.live_work_ids,
            &work.unresolved_wait_ids,
            &work.active_frame_ids,
            &work.turn_wait_keys,
            &work.task_wait_keys,
            &work.trigger_wait_keys,
            &work.nonopen,
            &work.latest_results,
            work.observations.iter().collect::<Vec<_>>(),
        ))? as usize;
        self.charge(
            bytes
                .checked_add(metadata)
                .context(DispatchMemoryError::ResourceExhausted)?,
            work.records.len(),
            // Projection/current/grouped Work clones: <= 8 encoded copies.
            // A snapshot duplicates each source field at most twice. Its
            // refresh/planner/builder/episode plus canonical digest buffers
            // and embedded write/readback coexist: <= 40 source copies.
            // Reserve all 48 before final_closure/derive/plan creates them;
            // the old State/seed material is accounted separately above.
            48,
        )
    }
    pub(crate) fn detached_member(
        &mut self,
        keys: &BTreeSet<WorkKey>,
        refs: &BTreeSet<InputEvidenceKey>,
        original: &PreparedInputMutation,
    ) -> Result<()> {
        // New member keys are readback facts; no speculative full Tool body is
        // stored. Reserve clones/serialization before growing the Pending map.
        self.charge(
            serialized_bytes(&(keys, refs, original))? as usize,
            keys.len().saturating_add(refs.len()),
            12,
        )
    }
}
