//! Existing observation selectors and owned material for named Task preparation.
use super::*;
use crate::runtime::input_evidence_budget::{
    BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
};

#[path = "input_read_bridge.rs"]
mod input_read_bridge;
#[path = "preflight_material.rs"]
mod preflight_material;
#[path = "readback_material.rs"]
mod readback_material;
#[path = "scalar_material.rs"]
mod scalar_material;
#[path = "decoder_material.rs"]
mod decoder_material;
pub(in crate::runtime_db) use readback_material::ReleasedTaskReadbackMaterial;
#[path = "read_body_material.rs"]
mod read_body_material;
pub(in crate::runtime_db) use preflight_material::ReleasedTaskPreflightMaterial;

/// Historical receipt selectors name observations, not Task admission. An
/// existing tombstone stays strict; a raw or another-incarnation row is omitted.
pub(crate) fn qualified_task_recovery_selectors_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<TaskKey>,
) -> Result<BTreeSet<TaskKey>> {
    ensure!(
        scope_facts(tx, scope, true)?.is_some(),
        TaskSnapshotError::RetiredScope
    );
    bound(
        keys.len(),
        keys.iter().map(|key| key.id.len() + 128).sum(),
        None,
    )?;
    let seed = binding(tx, scope)?;
    let mut qualified = BTreeSet::new();
    for key in keys {
        match alias(tx, scope, key)? {
            Some(value) if value.availability == TaskAvailability::Present => {
                qualified.insert(key.clone());
            }
            Some(value) if value.availability == TaskAvailability::Pruned => {
                return Err(TaskSnapshotError::NeedsPreparation(key.clone()).into());
            }
            Some(_) => {}
            None if qualification::eligible_digest(tx, seed.as_deref(), key)?.is_some() => {
                qualified.insert(key.clone());
            }
            None => {}
        }
    }
    Ok(qualified)
}

/// Decode creates raw + typed + expected/actual projection + qualified/encoded
/// copies. Eight is a conservative concurrent-material bound, not RSS or unique
/// canonical-key counting. Guards own estimates only and never retain a record.
/// The original Task reader owns this guard before Memory borrows its cost.
pub(in crate::runtime_db) struct ObservedTaskReadMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    guard: Arc<BudgetViewGuard>,
    bytes: usize,
    records: usize,
}
impl ObservedTaskReadMaterial<'_> {
    pub(in crate::runtime_db) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        ensure!(
            self.scope == scope && self.ledger.same_ledger(ledger),
            "Task material differs from original Memory scope/ledger"
        );
        self.guard
            .validate_owned_material_binding(scope, ledger, u64::try_from(self.bytes)?)?;
        Ok((self.bytes, self.records))
    }
}

pub(in crate::runtime_db) struct TaskReadMaterial<'a> {
    scope: &'a InputEvidenceScope,
    request: &'a TaskDomainLoadRequest,
    budget: Option<&'a InputEvidenceBudgetLedger>,
    memory_budget:
        Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    bytes: usize,
    records: usize,
    guards: Vec<Arc<BudgetViewGuard>>,
    #[cfg(test)]
    peak: (usize, usize),
}
impl<'a> TaskReadMaterial<'a> {
    pub(in crate::runtime_db) fn new(
        scope: &'a InputEvidenceScope,
        request: &'a TaskDomainLoadRequest,
        budget: Option<&'a InputEvidenceBudgetLedger>,
    ) -> Self {
        Self {
            scope,
            request,
            budget,
            memory_budget: None,
            bytes: 0,
            records: 0,
            guards: Vec::new(),
            #[cfg(test)]
            peak: (0, 0),
        }
    }
    pub(in crate::runtime_db) fn with_memory_budget(
        mut self,
        memory_budget: Option<
            &'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget,
        >,
    ) -> Self {
        self.memory_budget = memory_budget;
        self
    }
    /// This material was admitted earlier in the same original transaction.
    /// Its guards remain with the caller; do not double register or debit Memory.
    pub(in crate::runtime_db) fn with_admitted_material(
        mut self,
        admitted: TaskMaintenanceMaterial,
    ) -> Result<Self> {
        ensure!(
            self.budget.is_some(),
            "original phase requires an actual owner ledger"
        );
        bound(admitted.records, admitted.bytes, Some(self.request))?;
        self.bytes = admitted.bytes;
        self.records = admitted.records;
        #[cfg(test)]
        { self.peak = (self.bytes, self.records); }
        Ok(self)
    }
    fn charge(
        &mut self,
        bytes: usize,
        records: usize,
        copies: usize,
        metadata: bool,
    ) -> Result<()> {
        let Some(budget) = self.budget else {
            return Ok(());
        };
        let bytes = bytes
            .checked_mul(copies)
            .context("Task read material copies overflow")?;
        let records = records
            .checked_mul(copies)
            .context("Task read record copies overflow")?;
        // This is the same native-node floor Memory already charged. Include
        // it in Task's own allowance and guard before any decoder allocation.
        let native = records
            .checked_mul(256)
            .context("Task native material overflow")?;
        let total = bytes
            .checked_add(native)
            .context("Task native material total overflow")?;
        self.bytes = self
            .bytes
            .checked_add(total)
            .context("Task read material total overflow")?;
        self.records = self
            .records
            .checked_add(records)
            .context("Task read record total overflow")?;
        bound(self.records, self.bytes, Some(self.request))?;
        #[cfg(test)]
        { self.peak = (self.peak.0.max(self.bytes), self.peak.1.max(self.records)); }
        let guard = budget.register_owned_bytes(
            self.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: if metadata { 0 } else { u64::try_from(bytes)? },
                metadata_estimate_bytes: u64::try_from(
                    native
                        .checked_add(if metadata { bytes } else { 0 })
                        .context("Task metadata overflow")?,
                )?,
            },
        )?;
        let observed = ObservedTaskReadMaterial {
            scope: self.scope,
            ledger: budget,
            guard,
            bytes: total,
            records,
        };
        if let Some(memory) = self.memory_budget.as_deref_mut() {
            memory.additional_observed_task_material(&observed)?;
        }
        self.guards.push(observed.guard);
        Ok(())
    }
    /// Borrowed phase identity only; this grants no Task/body membership.
    pub(in crate::runtime_db) fn validate_accounting_binding(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(self.scope == scope && self.budget.is_some_and(|actual| actual.same_ledger(ledger)),
            "Task material differs from original scope/ledger");
        bound(self.records, self.bytes, Some(self.request))
    }
    pub(in crate::runtime_db) fn charge_writer_material(
        &mut self,
        bytes: usize,
        records: usize,
        copies: usize,
    ) -> Result<()> {
        self.charge(bytes, records, copies, false)
    }
    pub(in crate::runtime_db) fn admit_original_metadata_tx(
        &mut self, tx: &Transaction<'_>, target: &InputEvidenceScope, keys: &BTreeSet<TaskKey>,
    ) -> Result<()> {
        self.admit_original_metadata_header_tx(tx, target, keys)?;
        for key in keys {
            self.admit_stored_tx(tx, &target.agent_id, key)?;
        }
        Ok(())
    }
    pub(in crate::runtime_db::task_snapshot) fn admit_original_metadata_header_tx(
        &mut self, tx: &Transaction<'_>, target: &InputEvidenceScope, keys: &BTreeSet<TaskKey>,
    ) -> Result<()> {
        let binding_bytes:usize=tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(mode AS BLOB))+COALESCE(length(CAST(seed_id AS BLOB)),0)),0) FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1",
            params![target.agent_id,target.identity_incarnation],|row|row.get(0))?;
        self.charge(binding_bytes, 3, 8, true)?;
        let seed = super::binding(tx, target)?;
        self.charge_checkpoint_metadata(tx, seed.as_deref())?;
        self.admit_original_aliases_tx(tx, target, keys)?;
        Ok(())
    }
    pub(in crate::runtime_db) fn admit_original_aliases_tx(
        &mut self,
        tx: &Transaction<'_>,
        target: &InputEvidenceScope,
        keys: &BTreeSet<TaskKey>,
    ) -> Result<()> {
        for key in keys {
            let bytes:usize=tx.query_row(
                "SELECT COALESCE(SUM(length(CAST(availability AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)+COALESCE(length(CAST(projection_digest AS BLOB)),0)),0) FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4",
                params![target.agent_id,target.identity_incarnation,family_name(key.family),key.id],|row|row.get(0))?;
            self.charge(bytes, 4, 8, true)?;
        }
        Ok(())
    }
    pub(super) fn charge_stops(
        &mut self,
        bytes: usize,
        records: usize,
    ) -> Result<Arc<BudgetViewGuard>> {
        ensure!(
            self.budget.is_some(),
            "Stop observation requires actual owner ledger"
        );
        self.charge(
            bytes
                .checked_add(
                    records
                        .checked_mul(512)
                        .context("Stop node estimate overflow")?,
                )
                .context("Stop material overflow")?,
            records,
            8,
            false,
        )?;
        self.guards
            .last()
            .cloned()
            .context("Stop observation budget missing")
    }
    /// Scope is the original actor's accounting scope. Related selected heads
    /// keep their own exact alias authority; this allowance never grants it.
    pub(in crate::runtime_db) fn validate_original_allowance(
        &self,
        original: &InputEvidenceScope,
        request: &TaskDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == original
                && std::ptr::eq(self.request, request)
                && self.budget.is_some_and(|actual| actual.same_ledger(ledger))
                && original.identity_incarnation > 0
                && !original.agent_id.is_empty(),
            "Task metadata material differs from original scope/request/ledger"
        );
        bound(self.records, self.bytes, Some(request))
    }
    pub(in crate::runtime_db) fn charge_record(
        &mut self,
        key: &TaskKey,
        bytes: usize,
    ) -> Result<()> {
        self.charge_record_address(key.id.len(), bytes)
    }
    pub(super) fn charge_record_address(&mut self, key_bytes: usize, bytes: usize) -> Result<()> {
        self.charge(
            bytes
                .checked_add(key_bytes)
                .and_then(|n| n.checked_add(128))
                .context("Task key material overflow")?,
            1,
            8,
            false,
        )
    }
    pub(in crate::runtime_db) fn charge_ids(&mut self, bytes: usize, count: usize) -> Result<()> {
        self.charge(bytes, count, 4, true)
    }
    /// The dependency sets borrow this phase's already registered allocation.
    pub(in crate::runtime_db) fn charge_ids_for_handoff(
        &mut self,
        bytes: usize,
        count: usize,
    ) -> Result<Arc<BudgetViewGuard>> {
        ensure!(
            self.budget.is_some(),
            "Task handoff requires its original ledger"
        );
        self.charge_ids(bytes, count)?;
        self.guards
            .last()
            .cloned()
            .context("Task dependency material missing")
    }

    /// Accounting follows the original actor; the unchanged scope reader owns
    /// identity/State and related-scope authorization after these material gates.
    pub(in crate::runtime_db) fn scope_facts_tx(
        &mut self,
        tx: &Transaction<'_>,
        target: &InputEvidenceScope,
        original: bool,
    ) -> Result<Option<ContextScopeFacts>> {
        if self.budget.is_none() {
            return super::scope_facts(tx, target, original);
        }
        let identity_bytes: usize = tx.query_row(
            "SELECT COALESCE((SELECT length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",
            [&target.agent_id], |row|row.get(0),
        )?;
        self.charge_record_address(0, identity_bytes)?;
        let identity_nodes: usize = tx.query_row(
            "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_identities i,json_tree(i.payload_json) j WHERE i.agent_id=?1",
            [&target.agent_id], |row|row.get(0),
        )?;
        self.charge_ids(0, identity_nodes)?;
        // Never touch successor State merely to discover a retired predecessor.
        // The original reader still checks all normalized/payload projections.
        let current: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_id=?1 AND (status='active' OR (?3 AND status='deleting')) AND CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' AND json_extract(payload_json,'$.incarnation')>0 THEN json_extract(payload_json,'$.incarnation') END=?2)",
            params![target.agent_id,target.identity_incarnation,original], |row|row.get(0),
        )?;
        if current {
            let bytes: usize = tx.query_row(
                "SELECT COALESCE((SELECT length(CAST(agent_id AS BLOB))+length(CAST(payload_json AS BLOB))+length(CAST(control_revision AS BLOB)) FROM agent_states WHERE agent_id=?1),0)",
                [&target.agent_id], |row|row.get(0),
            )?;
            self.charge_record_address(0, bytes)?;
            let nodes: usize = tx.query_row(
                "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_states s,json_tree(s.payload_json) j WHERE s.agent_id=?1",
                [&target.agent_id], |row|row.get(0),
            )?;
            self.charge_ids(0, nodes)?;
        }
        super::scope_facts(tx, target, original)
    }

    pub(super) fn charge_relation(&mut self, bytes: usize) -> Result<()> {
        self.charge(
            bytes
                .checked_add(256)
                .context("Task relation bytes overflow")?,
            1,
            4,
            true,
        )
    }
    #[cfg(test)]
    pub(in crate::runtime_db) fn phase_peak_for_test(&self) -> TaskMaintenanceMaterial {
        TaskMaintenanceMaterial { bytes: self.peak.0, records: self.peak.1 }
    }
    pub(in crate::runtime_db) fn material_estimate(&self) -> TaskMaintenanceMaterial {
        TaskMaintenanceMaterial {
            bytes: self.bytes,
            records: self.records,
        }
    }
    pub(in crate::runtime_db) fn into_guards(self) -> Vec<Arc<BudgetViewGuard>> {
        self.guards
    }
    pub(super) fn charge_message(&mut self, id: &str, bytes: usize) -> Result<()> {
        self.charge(
            bytes
                .checked_add(id.len() + 128)
                .context("Task reply material overflow")?,
            1,
            4,
            false,
        )
    }
    /// Complete collection/node admission before deserializing a current view.
    pub(in crate::runtime_db) fn read_current_metadata(
        &mut self,
        tx: &Transaction<'_>,
    ) -> Result<Option<TaskCoverage>> {
        let scope = self.scope;
        self.read_current_metadata_for_scope(tx, scope)
    }
    pub(in crate::runtime_db) fn read_current_metadata_for_scope(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
    ) -> Result<Option<TaskCoverage>> {
        self.read_current_metadata_for_scope_inner(tx, scope, 4)
    }
    // The closed decoder has one typed DTO; public callers retain their
    // original allowance for subsequent independent metadata copies.
    fn read_current_metadata_for_scope_inner(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        typed_copies: usize,
    ) -> Result<Option<TaskCoverage>> {
        let size: Option<usize> = tx.query_row(
            "SELECT length(CAST(coverage_json AS BLOB)) FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope.agent_id,scope.identity_incarnation], |row| row.get(0),
        ).optional()?;
        let Some(size) = size else { return Ok(None) };
        self.charge_ids(size, 1)?;
        // Byte admission precedes every native JSON traversal.
        let records: usize = tx.query_row("SELECT         8*(SELECT COUNT(*) FROM json_each(coverage_json,'$.running.sources')) + (SELECT COUNT(*) FROM json_each(coverage_json,'$.query.born_task_seqs')) + (SELECT COUNT(*) FROM json_each(coverage_json,'$.query.reply_task_ids')) + COALESCE((SELECT SUM(json_array_length(value)) FROM json_each(coverage_json,'$.query.reply_task_ids')),0) + CASE WHEN EXISTS(SELECT 1 FROM json_each(coverage_json,'$.query.parent_turn_tasks')) THEN (SELECT COUNT(*)-1 FROM json_tree(coverage_json,'$.query.parent_turn_tasks')) ELSE 0 END + CASE WHEN EXISTS(SELECT 1 FROM json_each(coverage_json,'$.status.heads')) THEN (SELECT COUNT(*) FROM json_tree(coverage_json,'$.status')) ELSE 0 END +         (SELECT COUNT(*) FROM json_each(coverage_json,'$.output.heads')) +         COALESCE(json_array_length(coverage_json,'$.output.window.message_ids'),0) +         (SELECT COUNT(*) FROM json_each(coverage_json,'$.task_metadata')) +         (SELECT COUNT(*) FROM json_each(coverage_json,'$.result_metadata')) +         (SELECT COUNT(*) FROM json_each(coverage_json,'$.result_dependencies')) +         COALESCE(json_array_length(coverage_json,'$.observations'),0) +         COALESCE((SELECT SUM(json_array_length(value)) FROM json_each(coverage_json,'$.task_result_ids')),0) +         COALESCE((SELECT SUM(json_array_length(value)) FROM json_each(coverage_json,'$.activation_result_ids')),0) +         COALESCE((SELECT SUM(json_array_length(value)) FROM json_each(coverage_json,'$.reply_result_ids')),0) +         (SELECT COUNT(*) FROM json_each(coverage_json,'$.stop.records')) +         (SELECT COUNT(*) FROM json_each(coverage_json,'$.stop.heads')) +         COALESCE(json_array_length(coverage_json,'$.stop.pending_effect_seqs'),0)         FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope.agent_id,scope.identity_incarnation], |row| row.get(0),
        )?;
        let nodes: usize = tx.query_row(
            "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM task_domain_current c,json_tree(c.coverage_json) j WHERE c.agent_id=?1 AND c.identity_incarnation=?2",
            params![scope.agent_id,scope.identity_incarnation], |row| row.get(0),
        )?;
        self.charge(0, nodes, typed_copies, true)?;
        let coverage =
            loader::current_coverage(tx, scope)?.context("current Task metadata disappeared")?;
        ensure!(
            coverage.owned_metadata_record_count()? == records,
            "current Task metadata count differs from its original view"
        );
        Ok(Some(coverage))
    }

    pub(super) fn admitted_material(&self) -> (usize, usize) {
        (self.bytes, self.records)
    }
    pub(super) fn charge_checkpoint_metadata(
        &mut self,
        tx: &Transaction<'_>,
        seed: Option<&str>,
    ) -> Result<()> {
        if self.budget.is_none() {
            return Ok(());
        }
        let Some(seed) = seed else { return Ok(()) };
        // The fixed loader decodes the enclosing immutable roots once per
        // capture phase. Account that actual allocation, not just Task IDs.
        for phase in ["upgrade", "legacy_import", "first_import"] {
            let bytes: Option<usize> = tx.query_row(
                "SELECT CASE WHEN capture_phase='first_import' THEN length(CAST(i.roots_json AS BLOB)) ELSE length(CAST(s.roots_json AS BLOB)) END FROM task_domain_captures c JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id LEFT JOIN episode_continuity_imports i ON i.seed_id=c.seed_id WHERE c.seed_id=?1 AND c.capture_phase=?2",
                params![seed, phase], |row| row.get(0),
            ).optional()?;
            let Some(bytes) = bytes else { continue };
            self.charge(bytes, 1, 4, true)?;
            let nodes: usize = tx.query_row(
                "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM task_domain_captures c JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id LEFT JOIN episode_continuity_imports i ON i.seed_id=c.seed_id,json_tree(CASE WHEN capture_phase='first_import' THEN i.roots_json ELSE s.roots_json END) j WHERE c.seed_id=?1 AND c.capture_phase=?2",
                params![seed, phase], |row| row.get(0),
            )?;
            self.charge(0, nodes, 4, true)?;
        }
        Ok(())
    }
    pub(in crate::runtime_db) fn charge_final_metadata(
        &mut self,
        bytes: usize,
        records: usize,
    ) -> Result<()> {
        self.charge(bytes, records, 4, true)
    }
}

/// Count without allocating another encoded coverage copy before its reservation.
pub(super) fn encoded_metadata_bytes(value: &impl serde::Serialize) -> Result<usize> {
    #[derive(Default)]
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("Task metadata byte count overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
}
