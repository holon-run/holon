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
    final_material: Option<Arc<DispatchAuditMaterial>>,
    bytes: usize,
    records: usize,
    byte_limit: usize,
    record_limit: usize,
    state_material: usize,
    adapter_material: (usize, usize),
    protocol_material: (usize, usize),
    finish: bool,
}

/// The finalizer can only borrow Work while its original TX allowance is held.
#[must_use]
pub(crate) struct AdmittedFinalWork<'a, 'conn> {
    work: &'a CapturedWork,
    _budget: &'a DispatchMemoryTransactionBudget,
    tx: &'a Transaction<'conn>,
    state: &'a AgentState,
}

/// The original finalizer's allocations, moved rather than registered again.
#[derive(Debug)]
pub(in crate::runtime_db) struct DispatchAuditMaterial {
    guards: Vec<Arc<BudgetViewGuard>>,
}

impl DispatchAuditMaterial {
    pub(in crate::runtime_db) fn guard(&self) -> &Arc<BudgetViewGuard> {
        self.guards.first().expect("admitted finalizer material")
    }
}

impl AdmittedFinalWork<'_, '_> {
    pub(super) fn work(&self) -> &CapturedWork {
        self.work
    }

    pub(in crate::runtime_db) fn validate_protocol_read(
        &self,
        tx: &Transaction<'_>,
        agent_id: &str,
    ) -> Result<()> {
        ensure!(
            std::ptr::eq(self.tx, tx),
            "closure protocol changed original transaction"
        );
        self.validate_closure_scope(agent_id)
    }

    pub(in crate::runtime_db) fn validate_protocol_material(
        &self,
        bytes: usize,
        records: usize,
    ) -> Result<()> {
        ensure!(
            bytes <= self._budget.protocol_material.1
                && records <= self._budget.protocol_material.0,
            "closure protocol exceeds its original material admission"
        );
        Ok(())
    }

    pub(super) fn validate_final_state(&self, state: &AgentState) -> Result<()> {
        ensure!(
            std::ptr::eq(self.state, state),
            "final memory State changed after material admission"
        );
        Ok(())
    }

    pub(crate) fn validate_closure_scope(&self, agent_id: &str) -> Result<()> {
        ensure!(
            self.work.scope == self._budget.scope && self.work.scope.agent_id == agent_id,
            "closure differs from original admitted final Work scope"
        );
        Ok(())
    }

    pub(in crate::runtime_db) fn audit_material(
        &self,
        tx: &Transaction<'_>,
        agent_id: &str,
        incarnation: u64,
    ) -> Result<Arc<DispatchAuditMaterial>> {
        ensure!(
            std::ptr::eq(self.tx, tx),
            "Audit material changed original finalizer transaction"
        );
        self.validate_closure_scope(agent_id)?;
        ensure!(
            self._budget.scope.identity_incarnation == incarnation,
            "Audit material differs from original finalizer incarnation"
        );
        let material = self
            ._budget
            .final_material
            .as_ref()
            .context("finalizer omitted original Audit material")?;
        for guard in &material.guards {
            guard.validate_owned_material_binding(&self._budget.scope, &self._budget.ledger, 0)?;
        }
        Ok(material.clone())
    }
}

struct WorkObservationEntries<'a>(&'a CapturedWork);

impl Serialize for WorkObservationEntries<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.observations.iter())
    }
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
            final_material: None,
            bytes: 0,
            records: 0,
            byte_limit: request.byte_limit.min(MAX_METADATA_BYTES),
            record_limit: request.record_limit.min(MAX_METADATA_RECORDS),
            state_material: 0,
            adapter_material: (0, 0),
            protocol_material: (0, 0),
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
    pub(in crate::runtime_db) fn validate_input_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == *scope && self.ledger.same_ledger(ledger),
            "Input material differs from original Memory scope/ledger"
        );
        Ok(())
    }

    pub(in crate::runtime_db) fn input_material_usage(&self) -> (usize, usize) {
        (self.bytes, self.records)
    }

    pub(in crate::runtime_db) fn additional_observed_input_material(
        &mut self,
        material: &crate::runtime_db::input_evidence_snapshot::read_material::ObservedInputReadMaterial<'_>,
    ) -> Result<()> {
        let usage = material.validate_accounting(&self.scope, &self.ledger)?;
        let bytes = self
            .bytes
            .checked_add(usage.0)
            .context(DispatchMemoryError::ResourceExhausted)?;
        let records = self
            .records
            .checked_add(usage.1)
            .context(DispatchMemoryError::ResourceExhausted)?;
        if bytes > self.byte_limit || records > self.record_limit {
            tracing::warn!(agent_id = %self.scope.agent_id, used_bytes = self.bytes,
                requested_bytes = usage.0, used_records = self.records,
                requested_records = usage.1, byte_limit = self.byte_limit,
                record_limit = self.record_limit,
                "dispatch memory Input material allowance exceeded");
            return Err(DispatchMemoryError::ResourceExhausted.into());
        }
        // The original Input phase owns the guard; Memory only borrows it.
        self.bytes = bytes;
        self.records = records;
        Ok(())
    }

    #[track_caller]
    pub(crate) fn additional_material(
        &mut self,
        bytes: usize,
        records: usize,
        copies: usize,
    ) -> Result<()> {
        self.charge(bytes, records, copies)
    }
    pub(in crate::runtime_db) fn preflight_observed_task_material(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        bytes: usize,
        records: usize,
    ) -> Result<()> {
        ensure!(
            self.scope == *scope && self.ledger.same_ledger(ledger),
            "Task material differs from original Memory scope/ledger"
        );
        if !self
            .bytes
            .checked_add(bytes)
            .is_some_and(|n| n <= self.byte_limit)
            || !self
                .records
                .checked_add(records)
                .is_some_and(|n| n <= self.record_limit)
        {
            tracing::warn!(agent_id = %self.scope.agent_id, used_bytes = self.bytes,
                requested_bytes = bytes, used_records = self.records,
                requested_records = records, byte_limit = self.byte_limit,
                record_limit = self.record_limit,
                "dispatch memory Task material allowance exceeded");
            return Err(DispatchMemoryError::ResourceExhausted.into());
        }
        Ok(())
    }
    pub(in crate::runtime_db) fn additional_observed_task_material(
        &mut self,
        material: &crate::runtime_db::task_snapshot::read_preparation::ObservedTaskReadMaterial<'_>,
    ) -> Result<()> {
        let (bytes, records, guard) = material.validate_accounting(&self.scope, &self.ledger)?;
        self.preflight_observed_task_material(&self.scope, &self.ledger, bytes, records)?;
        // Task has already admitted its complete envelope. Retain that same
        // owning guard, rather than registering a second copy plus row overhead.
        if !self
            .guards
            .iter()
            .any(|existing| Arc::ptr_eq(existing, &guard))
        {
            self.guards.push(guard);
        }
        self.bytes += bytes;
        self.records += records;
        Ok(())
    }
    #[track_caller]
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
        if next > self.byte_limit || next_records > self.record_limit {
            tracing::warn!(agent_id = %self.scope.agent_id,
                used_bytes = self.bytes, requested_bytes = combined,
                used_records = self.records, requested_records = records.saturating_mul(copies),
                byte_limit = self.byte_limit, record_limit = self.record_limit,
                copies, source_line = std::panic::Location::caller().line(),
                "dispatch memory preparation allowance exceeded");
            return Err(DispatchMemoryError::ResourceExhausted.into());
        }
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
            if table.starts_with("execution_protocol_") {
                continue;
            }
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
        let (protocol_bytes, protocol_records) =
            super::super::transitions::execution_protocol_repository::closure_protocol_material_tx(
                tx,
                &self.scope.agent_id,
            )?;
        self.charge(
            protocol_bytes.saturating_sub(self.protocol_material.1),
            protocol_records.saturating_sub(self.protocol_material.0),
            1,
        )?;
        self.protocol_material = (
            protocol_records.max(self.protocol_material.0),
            protocol_bytes.max(self.protocol_material.1),
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
    pub(crate) fn capture_final_work<'a, 'conn>(
        &'a mut self,
        tx: &'a Transaction<'conn>,
        work: &'a CapturedWork,
        state: &'a AgentState,
    ) -> Result<AdmittedFinalWork<'a, 'conn>> {
        ensure!(
            self.final_material.is_none(),
            "final Work was already admitted"
        );
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
            WorkObservationEntries(work),
        ))? as usize;
        // Full display/grouped queue projections retain at most eight copies.
        self.charge(
            bytes
                .checked_add(metadata)
                .context(DispatchMemoryError::ResourceExhausted)?,
            work.records.len(),
            8,
        )?;
        ensure!(
            state.id == self.scope.agent_id,
            "final memory State differs from original scope"
        );
        let mut current_work = 0usize;
        let mut wait_bytes = 0usize;
        let mut wait_records = 0usize;
        let mut has_work = false;
        for record in &work.records {
            let bytes = serialized_bytes(&record.record)? as usize;
            match &record.record {
                crate::runtime::resident_work::CanonicalWorkRecord::Work(_) => {
                    if state.current_work_item_id.as_deref() == Some(record.key.id.as_str())
                        && matches!(&record.record, crate::runtime::resident_work::CanonicalWorkRecord::Work(work) if work.state == crate::types::WorkItemState::Open)
                    {
                        has_work = true;
                        current_work = bytes;
                    }
                }
                crate::runtime::resident_work::CanonicalWorkRecord::Wait(_) => {
                    wait_bytes = wait_bytes
                        .checked_add(bytes)
                        .context(DispatchMemoryError::ResourceExhausted)?;
                    wait_records += 1;
                }
                _ => {}
            }
        }
        // Memory snapshots derive from one current Work and its waits, never
        // completed display history. Reserve the exact State-selected Open Work
        // plus all selected Waits before deriving that snapshot.
        self.charge(
            current_work
                .checked_add(wait_bytes)
                .and_then(|n| n.checked_add(metadata))
                .context(DispatchMemoryError::ResourceExhausted)?,
            usize::from(has_work)
                .checked_add(wait_records)
                .context(DispatchMemoryError::ResourceExhausted)?,
            40,
        )?;
        // Move the Vec buffer and all guards intact; only the shared header is new.
        self.charge(
            std::mem::size_of::<DispatchAuditMaterial>() + 2 * std::mem::size_of::<usize>(),
            1,
            1,
        )?;
        self.final_material = Some(Arc::new(DispatchAuditMaterial {
            guards: std::mem::take(&mut self.guards),
        }));
        Ok(AdmittedFinalWork {
            work,
            _budget: self,
            tx,
            state,
        })
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
