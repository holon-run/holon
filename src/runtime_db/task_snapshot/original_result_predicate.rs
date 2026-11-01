//! Original admit predicate, bound to one closed selection and its borrowed SQL transaction.
use super::*;
use crate::runtime::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger};
use crate::runtime_db::task_result_settlement::{
    PreparedTaskResultMutation, TaskResultSettlementRecord,
};

struct SelectedPredicate {
    task_id: String,
    result_digest: String,
    typed_digest: String,
    result_projection: String,
    result_revision: Option<u64>,
    result_bytes: usize,
    result_alias_bytes: usize,
    task_alias_bytes: usize,
    task_raw: Option<String>,
    qualified_task: Option<TaskFactLocator>,
    task_bytes: Option<usize>,
    matches: bool,
}
/// Neither Clone nor caller-constructible. It observes the original predicate;
/// Task/Result write qualification and the whole receipt remain separate proofs.
pub(crate) struct OriginalResultPredicateProof<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    scope: InputEvidenceScope,
    scope_bytes: usize,
    rows: BTreeMap<String, SelectedPredicate>,
    guards: Vec<Arc<BudgetViewGuard>>,
    material: TaskMaintenanceMaterial,
}
/// The outer writer retains this observation until finalizer and whole receipt.
/// No transaction, body, root, storage or physical-resource reference survives.
pub(crate) struct OriginalResultPredicateRetention {
    _guards: Vec<Arc<BudgetViewGuard>>,
}

impl OriginalResultPredicateRetention {
    /// Move only owned accounting into the original ACK carrier. No authority
    /// or transaction/body reference remains after predicate execution.
    pub(crate) fn into_budget_views(self) -> Vec<Arc<BudgetViewGuard>> {
        self._guards
    }
}

fn scope_material_tx(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<usize> {
    tx.query_row(
        "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)+COALESCE((SELECT length(CAST(mode AS BLOB))+COALESCE(length(CAST(seed_id AS BLOB)),0) FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2 AND policy_version=1),0)",
        params![scope.agent_id,scope.identity_incarnation], |row| row.get(0),
    ).map_err(Into::into)
}
fn alias_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    key: &TaskKey,
) -> Result<usize> {
    tx.query_row(
        "SELECT COALESCE((SELECT length(CAST(availability AS BLOB))+COALESCE(length(CAST(content_digest AS BLOB)),0)+COALESCE(length(CAST(projection_digest AS BLOB)),0) FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family=?3 AND record_id=?4),0)",
        params![scope.agent_id,scope.identity_incarnation,family_name(key.family),key.id],
        |row| row.get(0),
    ).map_err(Into::into)
}
fn typed_digest(value: &TaskResultSettlementRecord) -> Result<String> {
    struct Writer(Sha256);
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer(Sha256::new());
    serde_json::to_writer(&mut writer, value)?;
    Ok(format!("{:x}", writer.0.finalize()))
}

pub(crate) fn prepare_original_result_predicate_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    scope: &InputEvidenceScope,
    prepared: &PreparedTaskResultMutation<'_, '_>,
    request: &TaskDomainLoadRequest,
    budget: &InputEvidenceBudgetLedger,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<OriginalResultPredicateProof<'tx, 'connection>> {
    // Check the closed operation and actual transaction before any new material.
    let selected = prepared.original_predicate_rows_tx(tx)?;
    let mut material = read_preparation::TaskReadMaterial::new(scope, request, Some(budget))
        .with_memory_budget(memory);
    let scope_bytes = scope_material_tx(tx, scope)?;
    material.charge_relation(scope_bytes)?;
    maintenance::require_existing_maintenance_scope_tx(tx, scope)?;
    let seed = binding(tx, scope)?;
    let mut rows = BTreeMap::new();
    for (record, raw) in selected {
        ensure!(
            record.agent_id == scope.agent_id,
            "TaskResult predicate scope mismatch"
        );
        let key = TaskKey::new(TaskFamily::ResultSettlement, &record.result_identity);
        let bytes = checkpoint_material_size_on(tx, &key)?.context("selected Result missing")?;
        material.charge_record(&key, bytes)?;
        let task_key = TaskKey::new(TaskFamily::Task, &record.task_id);
        let result_alias_bytes = alias_material_tx(tx, scope, &key)?;
        let task_alias_bytes = alias_material_tx(tx, scope, &task_key)?;
        material.charge_relation(result_alias_bytes)?;
        material.charge_relation(task_alias_bytes)?;
        let stored = stored_on(tx, &scope.agent_id, &key)?.context("selected Result missing")?;
        ensure!(
            stored.payload == raw
                && stored.fact == CanonicalTaskFact::ResultSettlement(record.clone()),
            "TaskResult original predicate selection changed"
        );
        let current = alias(tx, scope, &key)?;
        ensure!(
            !current
                .as_ref()
                .is_some_and(|a| a.availability != TaskAvailability::Present),
            "original predicate cannot revive absent or pruned Result"
        );
        let fixed = qualification::eligible_digest(tx, seed.as_deref(), &key)?;
        let result_revision = if current.is_some() || fixed.is_some() {
            Some(qualification::validate_member(
                tx,
                scope,
                seed.as_deref(),
                &key,
                &stored,
            )?)
        } else {
            // Raw legacy cleanup may keep its business outcome without becoming
            // a member. It still cannot borrow a different incarnation's member.
            ensure!(
                !qualification::known_anywhere(tx, &key)?,
                "unqualified Result belongs to another scope"
            );
            None
        };
        let (relation, task_raw, qualified_task) = result_relation::observe_task_predicate_tx(
            tx,
            scope,
            seed.as_deref(),
            record,
            request,
            &mut material,
        )?;
        let task_bytes = qualified_task
            .as_ref()
            .map(|task| {
                checkpoint_material_size_on(tx, &task.key)?.context("qualified Task disappeared")
            })
            .transpose()?;
        let matches = matches!(
            relation,
            TaskResultDependencyRelation::QualifiedCurrent { .. }
        );
        let row = SelectedPredicate {
            task_id: record.task_id.clone(),
            result_digest: stored.digest(),
            typed_digest: typed_digest(record)?,
            result_projection: stored.projection_digest,
            result_revision,
            result_bytes: bytes,
            result_alias_bytes,
            task_alias_bytes,
            task_raw,
            qualified_task,
            task_bytes,
            matches,
        };
        ensure!(
            rows.insert(record.result_identity.clone(), row).is_none(),
            "duplicate Result predicate target"
        );
    }
    let estimate = material.material_estimate();
    Ok(OriginalResultPredicateProof {
        tx,
        scope: scope.clone(),
        scope_bytes,
        rows,
        guards: material.into_guards(),
        material: estimate,
    })
}

impl OriginalResultPredicateProof<'_, '_> {
    pub(crate) fn material_estimate(&self) -> TaskMaintenanceMaterial {
        self.material
    }
    pub(in crate::runtime_db) fn validate_for_tx<'a>(
        &self,
        tx: &Transaction<'_>,
        selected: impl Iterator<Item = (&'a TaskResultSettlementRecord, &'a str)>,
    ) -> Result<()> {
        ensure!(
            std::ptr::eq(tx, self.tx),
            "original Result predicate belongs to another transaction"
        );
        ensure!(
            scope_material_tx(tx, &self.scope)? <= self.scope_bytes,
            "TaskResult predicate identity material changed"
        );
        maintenance::require_existing_maintenance_scope_tx(tx, &self.scope)?;
        let seed = binding(tx, &self.scope)?;
        let mut count = 0usize;
        for (record, raw) in selected {
            let row = self
                .rows
                .get(&record.result_identity)
                .context("Result is outside original predicate selection")?;
            ensure!(
                record.agent_id == self.scope.agent_id
                    && record.task_id == row.task_id
                    && digest(raw.as_bytes()) == row.result_digest
                    && typed_digest(record)? == row.typed_digest,
                "TaskResult original predicate selection changed"
            );
            let key = TaskKey::new(TaskFamily::ResultSettlement, &record.result_identity);
            ensure!(
                checkpoint_material_size_on(tx, &key)?.is_some_and(|n| n <= row.result_bytes),
                "TaskResult predicate material changed"
            );
            // No body may grow between preparation and the original write. The
            // admitted eight copies cover this final raw/projection validation.
            let actual =
                stored_on(tx, &self.scope.agent_id, &key)?.context("selected Result missing")?;
            ensure!(
                actual.digest() == row.result_digest
                    && actual.projection_digest == row.result_projection,
                "TaskResult original predicate selection changed"
            );
            let task_key = TaskKey::new(TaskFamily::Task, &row.task_id);
            ensure!(
                alias_material_tx(tx, &self.scope, &key)? <= row.result_alias_bytes
                    && alias_material_tx(tx, &self.scope, &task_key)? <= row.task_alias_bytes,
                "TaskResult predicate qualification material changed"
            );
            if let Some(revision) = row.result_revision {
                ensure!(
                    qualification::validate_member(
                        tx,
                        &self.scope,
                        seed.as_deref(),
                        &key,
                        &actual
                    )? == revision,
                    "TaskResult predicate qualification changed"
                );
            } else {
                ensure!(
                    !qualification::known_anywhere(tx, &key)?,
                    "raw Result qualification changed"
                );
            }
            let size: Option<usize> = tx
                .query_row(
                    &format!(
                        "SELECT length(CAST({} AS BLOB)) FROM tasks WHERE task_id=?1",
                        result_relation::TASK_PREDICATE_SQL
                    ),
                    [&row.task_id],
                    |r| r.get(0),
                )
                .optional()?;
            ensure!(
                size == row.task_raw.as_ref().map(String::len),
                "Task predicate changed after preparation"
            );
            let raw: Option<String> = tx
                .query_row(
                    &format!(
                        "SELECT {} FROM tasks WHERE task_id=?1",
                        result_relation::TASK_PREDICATE_SQL
                    ),
                    [&row.task_id],
                    |r| r.get(0),
                )
                .optional()?;
            ensure!(
                raw == row.task_raw,
                "Task predicate changed after preparation"
            );
            if let Some(locator) = &row.qualified_task {
                ensure!(
                    checkpoint_material_size_on(tx, &locator.key)?
                        .zip(row.task_bytes)
                        .is_some_and(|(n, max)| n <= max),
                    "qualified Task predicate material changed"
                );
                let stored = stored_on(tx, &self.scope.agent_id, &locator.key)?
                    .context("qualified Task disappeared")?;
                ensure!(
                    stored.digest() == locator.content_digest
                        && stored.projection_digest == locator.projection_digest
                        && qualification::validate_member(
                            tx,
                            &self.scope,
                            seed.as_deref(),
                            &locator.key,
                            &stored
                        )? == locator.revision,
                    "qualified Task predicate locator changed"
                );
            }
            count += 1;
        }
        ensure!(
            count == self.rows.len(),
            "original Result predicate selection incomplete"
        );
        Ok(())
    }
    pub(in crate::runtime_db) fn matches(
        &self,
        record: &TaskResultSettlementRecord,
    ) -> Result<bool> {
        let row = self
            .rows
            .get(&record.result_identity)
            .context("Result is outside original predicate selection")?;
        ensure!(
            record.agent_id == self.scope.agent_id && typed_digest(record)? == row.typed_digest,
            "Result changed before original predicate consumption"
        );
        Ok(row.matches)
    }
    pub(in crate::runtime_db) fn into_retention(self) -> OriginalResultPredicateRetention {
        OriginalResultPredicateRetention {
            _guards: self.guards,
        }
    }
}
