//! Existing observation selectors and owned material for named Task preparation.
use super::*;
use crate::runtime::input_evidence_budget::{
    BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
};

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
pub(in crate::runtime_db) struct TaskReadMaterial<'a> {
    scope: &'a InputEvidenceScope,
    request: &'a TaskDomainLoadRequest,
    budget: Option<&'a InputEvidenceBudgetLedger>,
    memory_budget:
        Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    bytes: usize,
    records: usize,
    guards: Vec<Arc<BudgetViewGuard>>,
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
        Ok(self)
    }
    pub(in crate::runtime_db) fn charge(
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
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("Task read material total overflow")?;
        self.records = self
            .records
            .checked_add(records)
            .context("Task read record total overflow")?;
        bound(self.records, self.bytes, Some(self.request))?;
        if let Some(memory) = &mut self.memory_budget {
            memory.additional_material(bytes, records, 1)?;
        }
        let bytes = u64::try_from(bytes)?;
        self.guards.push(budget.register_owned_bytes(
            self.scope,
            InputMemoryCategory::Candidate,
            if metadata {
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: bytes,
                }
            } else {
                OwnedBytesEstimate {
                    payload_bytes: bytes,
                    metadata_estimate_bytes: 0,
                }
            },
        )?);
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
        let size: Option<usize> = tx.query_row(
            "SELECT length(CAST(coverage_json AS BLOB)) FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope.agent_id,scope.identity_incarnation], |row| row.get(0),
        ).optional()?;
        let Some(size) = size else { return Ok(None) };
        self.charge_ids(size, 1)?;
        // Byte admission precedes every native JSON traversal.
        let records: usize = tx.query_row("SELECT
            (SELECT COUNT(*) FROM json_each(coverage_json,'$.recovery_sources')) +
            COALESCE((SELECT SUM((SELECT COUNT(*) FROM json_each(source.value,'$.paused_replays')))
                FROM json_each(coverage_json,'$.recovery_sources') AS source),0) +
            8*(SELECT COUNT(*) FROM json_each(coverage_json,'$.running.sources')) + (SELECT COUNT(*) FROM json_each(coverage_json,'$.query.born_task_seqs')) + (SELECT COUNT(*) FROM json_each(coverage_json,'$.query.reply_task_ids')) + COALESCE((SELECT SUM(json_array_length(value)) FROM json_each(coverage_json,'$.query.reply_task_ids')),0) + CASE WHEN EXISTS(SELECT 1 FROM json_each(coverage_json,'$.query.parent_turn_tasks')) THEN (SELECT COUNT(*)-1 FROM json_tree(coverage_json,'$.query.parent_turn_tasks')) ELSE 0 END + CASE WHEN EXISTS(SELECT 1 FROM json_each(coverage_json,'$.status.heads')) THEN (SELECT COUNT(*) FROM json_tree(coverage_json,'$.status')) ELSE 0 END +         (SELECT COUNT(*) FROM json_each(coverage_json,'$.output.heads')) +         COALESCE(json_array_length(coverage_json,'$.output.window.message_ids'),0) +         (SELECT COUNT(*) FROM json_each(coverage_json,'$.task_metadata')) +         (SELECT COUNT(*) FROM json_each(coverage_json,'$.result_metadata')) +         (SELECT COUNT(*) FROM json_each(coverage_json,'$.result_dependencies')) +         COALESCE(json_array_length(coverage_json,'$.observations'),0) +         COALESCE((SELECT SUM(json_array_length(value)) FROM json_each(coverage_json,'$.task_result_ids')),0) +         COALESCE((SELECT SUM(json_array_length(value)) FROM json_each(coverage_json,'$.activation_result_ids')),0) +         COALESCE((SELECT SUM(json_array_length(value)) FROM json_each(coverage_json,'$.reply_result_ids')),0) +         (SELECT COUNT(*) FROM json_each(coverage_json,'$.stop.records')) +         (SELECT COUNT(*) FROM json_each(coverage_json,'$.stop.heads')) +         COALESCE(json_array_length(coverage_json,'$.stop.pending_effect_seqs'),0)         FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope.agent_id,scope.identity_incarnation], |row| row.get(0),
        )?;
        self.charge_ids(0, records)?;
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
        let mut query = tx.prepare(
            "SELECT CASE WHEN capture_phase='first_import' THEN length(CAST(i.roots_json AS BLOB)) ELSE length(CAST(s.roots_json AS BLOB)) END,CASE WHEN capture_phase='first_import' THEN (SELECT COUNT(*) FROM json_each(i.roots_json,'$.task_domain_checkpoint.record_digests')) ELSE (SELECT COUNT(*) FROM json_each(s.roots_json,'$.task_domain_checkpoint.record_digests')) END FROM task_domain_captures c JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id LEFT JOIN episode_continuity_imports i ON i.seed_id=c.seed_id WHERE c.seed_id=?1 ORDER BY c.capture_phase",
        )?;
        for value in query.query_map([seed], |row| {
            Ok((row.get::<_, usize>(0)?, row.get::<_, usize>(1)?))
        })? {
            let (bytes, records) = value?;
            self.charge(bytes, records.max(1), 4, true)?;
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
