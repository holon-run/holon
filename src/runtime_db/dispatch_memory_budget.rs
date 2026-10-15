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
    original_input_prefix: Option<(std::sync::Weak<BudgetViewGuard>, (usize,usize))>,
}
impl DispatchMemoryTransactionBudget {
    pub(in crate::runtime_db) fn validate_input_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(&self.scope == scope && self.ledger.same_ledger(ledger),
            "original Input writer changed Memory scope/ledger");
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn material_usage_for_test(&self) -> (usize, usize) {
        (self.bytes, self.records)
    }
    pub(crate) fn prepare_tx(
        tx: &Transaction<'_>,
        memory: &DispatchMemoryIntent,
        request: &super::super::work_snapshot::WorkDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        Self::prepare_with_work_resource_tx(tx, memory, request, ledger, None)
    }
    pub(in crate::runtime_db) fn prepare_with_work_resource_tx(
        tx: &Transaction<'_>,
        memory: &DispatchMemoryIntent,
        request: &super::super::work_snapshot::WorkDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
        loan: Option<&crate::runtime::WorkResourceMaterialLoan>,
    ) -> Result<Self> {
        Self::prepare_with_original_input_tx(tx,memory,request,ledger,loan,None)
    }
    pub(in crate::runtime_db) fn prepare_with_original_input_tx(
        tx:&Transaction<'_>, memory:&DispatchMemoryIntent,
        request:&super::super::work_snapshot::WorkDomainLoadRequest,
        ledger:&InputEvidenceBudgetLedger,
        loan:Option<&crate::runtime::WorkResourceMaterialLoan>,
        input:Option<&crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial<'_>>,
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
            original_input_prefix: None,
        };
        if let Some(loan) = loan {
            let (bytes, records) = loan.validate_accounting(&result.scope, ledger)?;
            ensure!(
                bytes <= result.byte_limit && records <= result.record_limit,
                DispatchMemoryError::ResourceExhausted
            );
            result.bytes = bytes;
            result.records = records;
        }
        if let Some(input)=input {
            input.validate_accounting(&result.scope,ledger)?;
            let usage=input.usage();
            ensure!(usage.0>=result.bytes && usage.1>=result.records
                && usage.0<=result.byte_limit && usage.1<=result.record_limit,
                DispatchMemoryError::ResourceExhausted);
            // The original generic factory already included this exact Work
            // loan in the prefix. Keep that shared allocation only once.
            result.original_input_prefix=Some((Arc::downgrade(input.original_writer_phase_anchor()?),usage));
            result.bytes=usage.0;result.records=usage.1;
        }
        // SQL lengths precede the full seed String, typed record and its clones.
        // Factors bound simultaneous decoding, clone, canonical JSON and write
        // readback buffers; this is an encoded footprint estimate, not RSS.
        match memory {
            DispatchMemoryIntent::Started { start } => {
                let (bytes,nodes)=crate::runtime::input_evidence_budget::borrowed_json_material(start.as_ref())?;
                result.charge(usize::try_from(bytes)?,nodes,8)?;
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
                // Even SQLite's first full JSON traversal follows the actual
                // raw allowance. Count all values, object keys and containers
                // before any checkpoint DTO/native copy can be constructed.
                result.charge(bytes,1,12)?;
                let nodes:usize=tx.query_row(
                    "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM dispatch_memory_checkpoints c,json_tree(c.payload_json) j WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.turn_id=?3",
                    params![binding.key().agent_id,binding.key().identity_incarnation,binding.key().actual_dispatch_turn_id],|r|r.get(0))?;
                result.charge(0,nodes,12)?;
                result.admit_member_receipts_tx(tx, binding)?;
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
    /// Original whole receipts are checked one at a time. Admission includes
    /// every normalized cell before any JSON traversal or receipt String/DTO.
    fn admit_member_receipts_tx(&mut self, tx:&Transaction<'_>, binding:&DispatchMemoryBinding) -> Result<()> {
        let receipt_bytes:usize=tx.query_row(
            "SELECT COALESCE(MAX(length(CAST(r.family AS BLOB))+length(CAST(r.phase_version AS BLOB))+length(CAST(r.command_digest AS BLOB))+length(CAST(r.scope_digest AS BLOB))+length(CAST(r.scopes_json AS BLOB))+length(CAST(r.prepared_at AS BLOB))+length(CAST(r.prepared_json AS BLOB))+length(CAST(r.outcome_json AS BLOB))+length(CAST(r.committed_at AS BLOB))+length(CAST(r.result_digest AS BLOB))+256),0) FROM input_mutation_receipts r WHERE r.command_nonce IN (SELECT json_extract(c.payload_json,'$.original_start.command_nonce') FROM dispatch_memory_checkpoints c WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.turn_id=?3 UNION ALL SELECT json_extract(j.value,'$.original.command_nonce') FROM dispatch_memory_checkpoints c,json_each(json_extract(c.payload_json,'$.obligation.members')) j WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.turn_id=?3)",
            params![binding.key().agent_id,binding.key().identity_incarnation,binding.key().actual_dispatch_turn_id],|r|r.get(0))?;
        self.charge(receipt_bytes,usize::from(receipt_bytes>0),6)?;
        // Malformed JSON retains the original receipt reader's error. Its raw
        // buffer is bounded above; valid trees include object keys/containers.
        let native:usize=tx.query_row(
            "SELECT COALESCE(MAX(20+CASE WHEN json_valid(r.scopes_json) THEN (SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(r.scopes_json)) ELSE 0 END+CASE WHEN json_valid(r.prepared_json) THEN (SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(r.prepared_json)) ELSE 0 END+CASE WHEN json_valid(r.outcome_json) THEN (SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(r.outcome_json)) ELSE 0 END),0) FROM input_mutation_receipts r WHERE r.command_nonce IN (SELECT json_extract(c.payload_json,'$.original_start.command_nonce') FROM dispatch_memory_checkpoints c WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.turn_id=?3 UNION ALL SELECT json_extract(j.value,'$.original.command_nonce') FROM dispatch_memory_checkpoints c,json_each(json_extract(c.payload_json,'$.obligation.members')) j WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND c.turn_id=?3)",
            params![binding.key().agent_id,binding.key().identity_incarnation,binding.key().actual_dispatch_turn_id],|r|r.get(0))?;
        self.charge(0,native,6)
    }
    pub(in crate::runtime_db) fn original_input_phase_usage(
        &self, input:&crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial<'_>,
    ) -> Result<(usize,usize)> {
        input.validate_accounting(&self.scope,&self.ledger)?;
        let (anchor,prefix)=self.original_input_prefix.as_ref().context("Memory omitted its original Input prefix")?;
        let anchor=anchor.upgrade().context("original Input prefix anchor was released")?;
        ensure!(Arc::ptr_eq(&anchor,input.original_writer_phase_anchor()?) && *prefix==input.usage(),
            "Memory cannot resume another Input decoder phase");
        ensure!(self.bytes>=prefix.0 && self.records>=prefix.1 && !self.guards.is_empty(),
            "Memory lost actual original cumulative material");
        for guard in &self.guards {guard.validate_owned_material_binding(&self.scope,&self.ledger,0)?;}
        Ok((self.bytes,self.records))
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

    /// Same original Input allocation, already registered before its decoder.
    /// Do not add another owned guard or reobserve the caller's prior material.
    pub(in crate::runtime_db) fn additional_observed_input_material(
        &mut self,
        observed:&crate::runtime_db::input_evidence_snapshot::read_material::ObservedInputReadMaterial<'_>,
    ) -> Result<()> {
        let (bytes, records) = observed.validate_accounting(&self.scope, &self.ledger)?;
        let next_bytes = self
            .bytes
            .checked_add(bytes)
            .context(DispatchMemoryError::ResourceExhausted)?;
        let next_records = self
            .records
            .checked_add(records)
            .context(DispatchMemoryError::ResourceExhausted)?;
        ensure!(
            next_bytes <= self.byte_limit && next_records <= self.record_limit,
            DispatchMemoryError::ResourceExhausted
        );
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
    }

    /// Input Scheduling already registered this exact allocation before decode.
    pub(in crate::runtime_db) fn additional_observed_scheduling_material(
        &mut self,
        observed: &crate::runtime_db::scheduling_snapshot::ObservedSchedulingAllocation<'_>,
    ) -> Result<()> {
        let (bytes, records) = observed.validate_accounting(&self.scope, &self.ledger)?;
        let next_bytes = self
            .bytes
            .checked_add(bytes)
            .context(DispatchMemoryError::ResourceExhausted)?;
        let next_records = self
            .records
            .checked_add(records)
            .context(DispatchMemoryError::ResourceExhausted)?;
        ensure!(
            next_bytes <= self.byte_limit && next_records <= self.record_limit,
            DispatchMemoryError::ResourceExhausted
        );
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
    }

    /// Task already owns the raw/native allocation guard. Memory only borrows
    /// this same cost; adding another node floor or guard would count it twice.
    pub(in crate::runtime_db) fn additional_observed_task_material(
        &mut self,
        observed: &crate::runtime_db::task_snapshot::read_preparation::ObservedTaskReadMaterial<'_>,
    ) -> Result<()> {
        let (bytes, records) = observed.validate_accounting(&self.scope, &self.ledger)?;
        let next_bytes = self
            .bytes
            .checked_add(bytes)
            .context(DispatchMemoryError::ResourceExhausted)?;
        let next_records = self
            .records
            .checked_add(records)
            .context(DispatchMemoryError::ResourceExhausted)?;
        ensure!(
            next_bytes <= self.byte_limit && next_records <= self.record_limit,
            DispatchMemoryError::ResourceExhausted
        );
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
    }

    pub(in crate::runtime_db) fn remaining_material_limits(&self) -> Result<(usize, usize)> {
        Ok((
            self.byte_limit
                .checked_sub(self.bytes)
                .context(DispatchMemoryError::ResourceExhausted)?,
            self.record_limit
                .checked_sub(self.records)
                .context(DispatchMemoryError::ResourceExhausted)?,
        ))
    }

    /// Same actual Current allocation, already guarded by its original decoder.
    pub(in crate::runtime_db) fn additional_observed_current_material(
        &mut self,
        observed: &crate::runtime_db::current_read_snapshot::ObservedCurrentReadMaterial<'_>,
    ) -> Result<()> {
        let (bytes, records) = observed.validate_accounting(&self.scope, &self.ledger)?;
        let next_bytes = self
            .bytes
            .checked_add(bytes)
            .context(DispatchMemoryError::ResourceExhausted)?;
        let next_records = self
            .records
            .checked_add(records)
            .context(DispatchMemoryError::ResourceExhausted)?;
        ensure!(
            next_bytes <= self.byte_limit && next_records <= self.record_limit,
            DispatchMemoryError::ResourceExhausted
        );
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
    }

    /// The original Work material owns this actual guard. Debit the same
    /// transaction allowance without registering that allocation a second time.
    pub(in crate::runtime_db) fn additional_observed_work_material(
        &mut self,
        observed: &super::super::work_snapshot::read_preparation::ObservedWorkMaterial<'_>,
    ) -> Result<()> {
        let (bytes, records) = observed.validate_accounting(&self.scope, &self.ledger)?;
        let next_bytes = self
            .bytes
            .checked_add(bytes)
            .context(DispatchMemoryError::ResourceExhausted)?;
        let next_records = self
            .records
            .checked_add(records)
            .context(DispatchMemoryError::ResourceExhausted)?;
        ensure!(
            next_bytes <= self.byte_limit && next_records <= self.record_limit,
            DispatchMemoryError::ResourceExhausted
        );
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
    }

    pub(in crate::runtime_db) fn release_task_readback_material(
        &mut self,
        released:crate::runtime_db::task_snapshot::read_preparation::ReleasedTaskReadbackMaterial<'_>,
    )->Result<()> {
        let (bytes,records)=released.validate_accounting(&self.scope,&self.ledger)?;
        self.bytes=self.bytes.checked_sub(bytes).context("Task readback Memory bytes underflow")?;
        self.records=self.records.checked_sub(records).context("Task readback Memory nodes underflow")?;
        Ok(())
    }

    pub(in crate::runtime_db) fn release_task_preflight_material(
        &mut self,
        released: crate::runtime_db::task_snapshot::read_preparation::ReleasedTaskPreflightMaterial<
            '_,
        >,
    ) -> Result<()> {
        let (bytes, records) = released.validate_accounting(&self.scope, &self.ledger)?;
        let next_bytes = self
            .bytes
            .checked_sub(bytes)
            .context("Task preflight Memory bytes underflow")?;
        let next_records = self
            .records
            .checked_sub(records)
            .context("Task preflight Memory nodes underflow")?;
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
    }

    pub(in crate::runtime_db) fn release_input_transient_material(
        &mut self,
        released: crate::runtime_db::input_evidence_snapshot::read_material::ReleasedInputMaterial<
            '_,
        >,
    ) -> Result<()> {
        let (bytes, records) = released.validate_accounting(&self.scope, &self.ledger)?;
        let next_bytes = self
            .bytes
            .checked_sub(bytes)
            .context("Input transient Memory byte debit underflow")?;
        let next_records = self
            .records
            .checked_sub(records)
            .context("Input transient Memory node debit underflow")?;
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
    }

    pub(in crate::runtime_db) fn release_work_transient_material(
        &mut self,
        released: super::super::work_snapshot::read_preparation::ReleasedWorkMaterial<'_>,
    ) -> Result<()> {
        let (bytes, records) = released.validate_accounting(&self.scope, &self.ledger)?;
        let next_bytes = self
            .bytes
            .checked_sub(bytes)
            .context("Work transient Memory byte debit underflow")?;
        let next_records = self
            .records
            .checked_sub(records)
            .context("Work transient Memory record debit underflow")?;
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
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
        let (bytes, records) =
            work.records
                .iter()
                .try_fold((0usize, 0usize), |(bytes, records), record| {
                    let (native_bytes, native_records) =
                        record.record.resident_material_estimate()?;
                    Ok::<_, anyhow::Error>((
                        bytes
                            .checked_add(native_bytes.max(record.resident_bytes))
                            .context(DispatchMemoryError::ResourceExhausted)?,
                        records
                            .checked_add(native_records)
                            .context(DispatchMemoryError::ResourceExhausted)?,
                    ))
                })?;
        let metadata = usize::try_from(work.encoded_selected_metadata_bytes()?)
            .context(DispatchMemoryError::ResourceExhausted)?;
        self.charge(
            bytes
                .checked_add(metadata)
                .context(DispatchMemoryError::ResourceExhausted)?,
            records,
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
