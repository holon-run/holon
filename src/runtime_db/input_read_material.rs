//! Actual owned material of one original Input preparation, without read authority.
use super::*;
use crate::runtime::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};

#[path = "input_read_joint.rs"]
mod joint;

#[path = "input_read_domain_joint.rs"]
mod domain_joint;

#[path = "input_read_scheduling_joint.rs"]
mod scheduling_joint;

#[path = "input_read_query.rs"]
mod query;

#[path = "input_read_native.rs"]
mod native;

/// Only this original Input material can mint the observation after its
/// actual guard exists. Memory borrows that same allocation, not a second view.
pub(in crate::runtime_db) struct ObservedInputReadMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    guard: Arc<BudgetViewGuard>,
    bytes: usize,
    records: usize,
}
impl ObservedInputReadMaterial<'_> {
    pub(in crate::runtime_db) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        ensure!(
            self.scope == scope && self.ledger.same_ledger(ledger),
            "Input material differs from original Memory scope/ledger"
        );
        self.guard
            .validate_owned_material_binding(scope, ledger, u64::try_from(self.bytes)?)?;
        Ok((self.bytes, self.records))
    }
}

/// Private marks describe only this material's registered Tool decoder guards.
pub(in crate::runtime_db::input_evidence_snapshot) struct ToolScratchMark {
    bytes: usize,
    records: usize,
    guards: usize,
    original: std::sync::Weak<BudgetViewGuard>,
}
pub(in crate::runtime_db) struct ReleasedInputMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    bytes: usize,
    records: usize,
}
impl ReleasedInputMaterial<'_> {
    pub(in crate::runtime_db) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        ensure!(
            self.scope == scope && self.ledger.same_ledger(ledger),
            "released Input material differs from original Memory scope/ledger"
        );
        Ok((self.bytes, self.records))
    }
}

pub(in crate::runtime_db) struct InputReadMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    byte_limit: usize,
    record_limit: usize,
    bytes: usize,
    records: usize,
    guards: Vec<Arc<BudgetViewGuard>>,
    memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
}
impl<'a> InputReadMaterial<'a> {
    /// The first guard is this decoder phase's unique actual anchor. Selectors
    /// and other borrowed guards cannot identify another Input phase.
    pub(in crate::runtime_db) fn original_writer_phase_anchor(&self) -> Result<&Arc<BudgetViewGuard>> {
        let guard=self.guards.first().context("first Input writer phase has no actual anchor")?;
        guard.validate_owned_material_binding(self.scope,self.ledger,0)?;
        Ok(guard)
    }
    /// Memory already borrowed this exact prefix before its first decoder.
    /// Continue with its true cumulative usage; no extra registration or debit.
    pub(in crate::runtime_db) fn resume_after_original_memory<'b>(self,
        memory:&'b mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
    ) -> Result<InputReadMaterial<'b>> where 'a:'b {
        let usage=memory.original_input_phase_usage(&self)?;
        let scope=self.scope;let ledger=self.ledger;let limits=(self.byte_limit,self.record_limit);
        InputReadMaterial::resume_native_phase(scope,ledger,limits,usage,self.guards,Some(memory))
    }
    /// Prior is actual material whose original guards remain with the caller.
    /// It is debited from this phase, never registered a second time.
    pub(in crate::runtime_db) fn new(
        scope: &'a InputEvidenceScope,
        request: &InputEvidenceLoadRequest,
        ledger: &'a InputEvidenceBudgetLedger,
        prior: (usize, usize),
    ) -> Result<Self> {
        Self::new_with_memory(scope, request, ledger, prior, None)
    }
    pub(in crate::runtime_db) fn new_with_memory(
        scope: &'a InputEvidenceScope,
        request: &InputEvidenceLoadRequest,
        ledger: &'a InputEvidenceBudgetLedger,
        prior: (usize, usize),
        memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<Self> {
        Self::new_with_memory_and_query_prior(scope, request, ledger, prior, memory, None)
    }
    pub(in crate::runtime_db) fn new_with_memory_and_query_prior(
        scope: &'a InputEvidenceScope,
        request: &InputEvidenceLoadRequest,
        ledger: &'a InputEvidenceBudgetLedger,
        prior: (usize, usize),
        memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
        query: Option<(&crate::runtime::input_queries::InputQuerySnapshot, u64)>,
    ) -> Result<Self> {
        ensure!(
            !scope.agent_id.is_empty() && scope.identity_incarnation > 0,
            "invalid Input material scope"
        );
        let mut result = Self {
            scope,
            ledger,
            byte_limit: request.byte_limit.min(MAX_METADATA_BYTES),
            record_limit: request.record_limit.min(MAX_RECORDS),
            bytes: prior.0,
            records: prior.1,
            guards: Vec::new(),
            memory,
        };
        // Borrow original live selector guards before even the first Input
        // scope allocation. The existing same-Arc observer debits each once.
        if let Some((query, epoch)) = query {
            result.observe_query_prior(query, epoch)?;
        }
        // The original ledger's scope and dynamic statement/guard slots are
        // admitted before constructing an owned query or extending this Vec.
        let scope_bytes = usize::try_from(serialized_bytes(scope)?)?
            .checked_mul(8)
            .context(InputEvidenceSnapshotError::ResourceExhausted(
                "Input scope material",
            ))?;
        let metadata = scope_bytes
            .checked_add(4096)
            .and_then(|n| n.checked_add(std::mem::size_of::<Self>()))
            .context(InputEvidenceSnapshotError::ResourceExhausted(
                "Input scope material",
            ))?;
        result.charge(0, metadata, 1)?;
        // Decoder marks belong to this phase's unique scope guard. Shared
        // selector loans follow it and cannot identify two distinct phases.
        result.guards.rotate_right(1);
        Ok(result)
    }
    pub(in crate::runtime_db) fn validate_binding(&self, scope: &InputEvidenceScope) -> Result<()> {
        ensure!(
            self.scope == scope,
            "Input material belongs to another scope"
        );
        Ok(())
    }
    fn charge(&mut self, payload: usize, metadata: usize, records: usize) -> Result<()> {
        let exhausted = || InputEvidenceSnapshotError::ResourceExhausted("Input read material");
        let metadata = metadata
            .checked_add(std::mem::size_of::<BudgetViewGuard>())
            .and_then(|n| n.checked_add(2 * std::mem::size_of::<Arc<BudgetViewGuard>>()))
            .context(exhausted())?;
        let records = records.checked_add(1).context(exhausted())?;
        let next_bytes = self
            .bytes
            .checked_add(payload)
            .and_then(|n| n.checked_add(metadata))
            .context(exhausted())?;
        let next_records = self.records.checked_add(records).context(exhausted())?;
        ensure!(
            next_bytes <= self.byte_limit && next_records <= self.record_limit,
            exhausted()
        );
        let guard = self.ledger.register_owned_bytes(
            self.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(payload)?,
                metadata_estimate_bytes: u64::try_from(metadata)?,
            },
        )?;
        let observed = ObservedInputReadMaterial {
            scope: self.scope,
            ledger: self.ledger,
            guard,
            bytes: payload.checked_add(metadata).context(exhausted())?,
            records,
        };
        if let Some(memory) = self.memory.as_deref_mut() {
            memory.additional_observed_input_material(&observed)?;
        }
        self.guards.push(observed.guard);
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
    }
    pub(in crate::runtime_db::input_evidence_snapshot) fn validate_tool_agent(
        &self,
        agent: &str,
    ) -> Result<()> {
        ensure!(
            self.scope.agent_id == agent,
            "Tool material belongs to another agent"
        );
        Ok(())
    }
    pub(in crate::runtime_db::input_evidence_snapshot) fn tool_scratch_mark(
        &self,
    ) -> ToolScratchMark {
        ToolScratchMark {
            bytes: self.bytes,
            records: self.records,
            guards: self.guards.len(),
            original: Arc::downgrade(self.guards.first().expect("original Input scope guard")),
        }
    }
    /// The Tool reader and selected Values have been dropped. Only its final
    /// separately guarded report may survive; this grants no Input membership.
    pub(in crate::runtime_db::input_evidence_snapshot) fn retire_tool_scratch(
        &mut self,
        mark: ToolScratchMark,
        retained: Option<ToolScratchMark>,
    ) -> Result<()> {
        let original = Arc::downgrade(
            self.guards
                .first()
                .context("original Input scope guard missing")?,
        );
        ensure!(
            std::sync::Weak::ptr_eq(&mark.original, &original)
                && retained
                    .as_ref()
                    .is_none_or(|retained| std::sync::Weak::ptr_eq(&retained.original, &original))
                && mark.guards <= self.guards.len(),
            "Tool scratch mark belongs to another Input phase"
        );
        // A Tool decoder emits at most 20 guards. Their vector capacity fits
        // the phase's existing 4096-byte constant metadata guard after drain.
        ensure!(
            self.guards.len() <= mark.guards + 20,
            "Tool scratch material escaped"
        );
        ensure!(
            self.guards[mark.guards..]
                .iter()
                .all(|guard| Arc::strong_count(guard) == 1),
            "Tool scratch guard has another live holder"
        );
        let kept = if let Some(ref retained) = retained {
            ensure!(
                retained.guards + 1 == self.guards.len() && retained.guards >= mark.guards,
                "Tool retained report guard differs from final allocation"
            );
            (
                self.bytes
                    .checked_sub(retained.bytes)
                    .context("Tool retained byte underflow")?,
                self.records
                    .checked_sub(retained.records)
                    .context("Tool retained node underflow")?,
            )
        } else {
            (0, 0)
        };
        let released = ReleasedInputMaterial {
            scope: self.scope,
            ledger: self.ledger,
            bytes: self
                .bytes
                .checked_sub(mark.bytes)
                .and_then(|n| n.checked_sub(kept.0))
                .context("Tool scratch byte underflow")?,
            records: self
                .records
                .checked_sub(mark.records)
                .and_then(|n| n.checked_sub(kept.1))
                .context("Tool scratch node underflow")?,
        };
        let next_bytes = self
            .bytes
            .checked_sub(released.bytes)
            .context("Tool phase byte underflow")?;
        let next_records = self
            .records
            .checked_sub(released.records)
            .context("Tool phase node underflow")?;
        let guard = if retained.is_some() {
            self.guards.pop()
        } else {
            None
        };
        drop(self.guards.drain(mark.guards..));
        if let Some(guard) = guard {
            self.guards.push(guard);
        }
        if let Some(memory) = self.memory.as_deref_mut() {
            memory.release_input_transient_material(released)?;
        }
        self.bytes = next_bytes;
        self.records = next_records;
        Ok(())
    }
    /// The source already supplies actual raw/native/copy costs. Do not apply
    /// another copy factor or register the same borrowed prior again.
    /// Return only this newly admitted guard to the original Source result;
    /// Memory already borrowed it during charge before any report clone.
    pub(in crate::runtime_db) fn observe_retained_projection(
        &mut self,
        bytes: u64,
        records: usize,
    ) -> Result<(Arc<BudgetViewGuard>, (usize, usize))> {
        let before = self.usage();
        self.charge(usize::try_from(bytes)?, 0, records)?;
        Ok((
            self.guards
                .last()
                .expect("charge always retains its guard")
                .clone(),
            (self.bytes - before.0, self.records - before.1),
        ))
    }
    pub(in crate::runtime_db) fn observe_projected(
        &mut self,
        bytes: u64,
        records: usize,
    ) -> Result<()> {
        self.charge(usize::try_from(bytes)?, 0, records)
    }
    /// Debit the same transaction's already observed Work allocations. Their
    /// original caller retains the actual guards; this creates no new view.
    pub(in crate::runtime_db) fn observe_work_prior(
        &mut self,
        work: &crate::runtime_db::work_snapshot::WorkCurrentSnapshot,
    ) -> Result<()> {
        ensure!(
            work.resident.scope() == self.scope
                && work
                    .material_views
                    .iter()
                    .all(|guard| guard.matches_resource_ledger(self.ledger)),
            "Input Work material belongs to another scope/ledger"
        );
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("Input and Work combined material");
        let bytes = self
            .bytes
            .checked_add(work.material_usage.0)
            .context(exhausted())?;
        let records = self
            .records
            .checked_add(work.material_usage.1)
            .context(exhausted())?;
        ensure!(
            bytes <= self.byte_limit && records <= self.record_limit,
            exhausted()
        );
        self.bytes = bytes;
        self.records = records;
        Ok(())
    }
    pub(in crate::runtime_db) fn selectors(
        &mut self,
        request: &InputEvidenceLoadRequest,
    ) -> Result<()> {
        let exhausted = || InputEvidenceSnapshotError::ResourceExhausted("Input selector material");
        let mut encoded = usize::try_from(serialized_bytes(&(
            &request.mandatory_roots,
            &request.interrupted_sources,
            &request.selected_completion_brief_ids,
        ))?)?;
        for window in &request.windows {
            encoded = encoded
                .checked_add(usize::try_from(serialized_bytes(&(
                    &window.selector,
                    window.limit,
                ))?)?)
                .context(exhausted())?;
        }
        let count = request
            .mandatory_roots
            .len()
            .checked_add(request.interrupted_sources.len())
            .and_then(|n| n.checked_add(request.selected_completion_brief_ids.len()))
            .and_then(|n| n.checked_add(request.windows.len()))
            .context(exhausted())?;
        self.charge(
            encoded.checked_mul(8).context(exhausted())?,
            count
                .checked_mul(8 * 256)
                .and_then(|n| n.checked_add(8 * std::mem::size_of::<InputEvidenceLoadRequest>()))
                .context(exhausted())?,
            count.checked_mul(8).context(exhausted())?,
        )
    }
    /// Raw scalar lengths precede SQLite JSON traversal and the original DTO
    /// decoder. Missing rows remain missing; this never creates State/identity.
    pub(in crate::runtime_db) fn scope_facts_tx(
        &mut self,
        tx: &Transaction<'_>,
    ) -> Result<ContextScopeFacts> {
        let (identity_bytes, identity_rows): (usize, usize) = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))),0),COUNT(*) FROM agent_identities WHERE agent_id=?1",
            [&self.scope.agent_id], |row| Ok((row.get(0)?,row.get(1)?)),
        )?;
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("Input scope raw material");
        self.charge(
            identity_bytes.checked_mul(8).context(exhausted())?,
            identity_rows.checked_mul(8 * 256).context(exhausted())?,
            identity_rows.checked_mul(8).context(exhausted())?,
        )?;
        let identity_nodes: usize = tx.query_row(
            "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_identities r,json_tree(r.payload_json) j WHERE r.agent_id=?1",
            [&self.scope.agent_id], |row| row.get(0),
        )?;
        let identity_nodes = identity_nodes.checked_mul(2).context(exhausted())?;
        self.charge(
            0,
            identity_nodes.checked_mul(256).context(exhausted())?,
            identity_nodes,
        )?;
        // Admit only the original identity before its header check. A retired
        // request must not observe the successor's State, even its raw size.
        let current: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_id=?1 AND CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' THEN json_extract(payload_json,'$.incarnation') END=?2)",
            params![self.scope.agent_id,self.scope.identity_incarnation], |row|row.get(0),
        )?;
        ensure!(current, InputEvidenceSnapshotError::RetiredScope);
        let (state_bytes, state_rows): (usize, usize) = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+COALESCE(length(CAST(control_revision AS BLOB)),0)),0),COUNT(*) FROM agent_states WHERE agent_id=?1",
            [&self.scope.agent_id], |row| Ok((row.get(0)?,row.get(1)?)),
        )?;
        self.charge(
            state_bytes.checked_mul(8).context(exhausted())?,
            state_rows.checked_mul(8 * 256).context(exhausted())?,
            state_rows.checked_mul(8).context(exhausted())?,
        )?;
        let state_nodes: usize = tx.query_row(
            "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_states r,json_tree(r.payload_json) j WHERE r.agent_id=?1",
            [&self.scope.agent_id], |row| row.get(0),
        )?;
        // One typed body and SQLite working tree; Arc pins share that body.
        let state_nodes = state_nodes.checked_mul(2).context(exhausted())?;
        self.charge(
            0,
            state_nodes.checked_mul(256).context(exhausted())?,
            state_nodes,
        )?;
        ContextDurableRead::new(tx, &self.scope.agent_id).scope_facts()
    }
    /// The opaque original receipt phase re-borrows its validated Source after
    /// confirmation. Its own guards and Event prior remain unchanged.
    pub(in crate::runtime_db) fn replace_borrowed_recovery_prior(
        &mut self,
        previous: (usize, usize),
        actual: (usize, usize),
    ) -> Result<()> {
        ensure!(
            self.memory.is_none(),
            "original receipt prior cannot reset a Memory debit"
        );
        let exhausted =
            || InputEvidenceSnapshotError::ResourceExhausted("fresh Source retained prior");
        let bytes = self
            .bytes
            .checked_sub(previous.0)
            .and_then(|n| n.checked_add(actual.0))
            .context(exhausted())?;
        let records = self
            .records
            .checked_sub(previous.1)
            .and_then(|n| n.checked_add(actual.1))
            .context(exhausted())?;
        ensure!(
            bytes <= self.byte_limit && records <= self.record_limit,
            exhausted()
        );
        self.bytes = bytes;
        self.records = records;
        Ok(())
    }
    pub(in crate::runtime_db) fn usage(&self) -> (usize, usize) {
        (self.bytes, self.records)
    }
    pub(in crate::runtime_db) fn into_guards(self) -> Vec<Arc<BudgetViewGuard>> {
        self.guards
    }
}

#[path = "input_read_records.rs"]
mod records;

#[cfg(test)]
#[path = "input_read_material_tests.rs"]
mod tests;

#[path = "input_read_execution.rs"]
mod execution_material;

#[path = "input_read_relations.rs"]
mod relations;

#[path = "input_read_origin.rs"]
mod origin_material;

#[path = "input_read_windows.rs"]
mod window_material;

#[path = "input_read_episode_pending.rs"]
mod episode_pending;

#[cfg(test)]
pub(crate) fn probe_original_report_input_prior_for_test(
    scope: &InputEvidenceScope,
    request: &InputEvidenceLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    prior: (usize, usize),
    projected: (u64, usize),
) -> Result<(usize, usize)> {
    let mut material = InputReadMaterial::new(scope, request, ledger, prior)?;
    material.observe_projected(projected.0, projected.1)?;
    Ok(material.usage())
}

#[path = "input_read_writer.rs"]
mod writer;
