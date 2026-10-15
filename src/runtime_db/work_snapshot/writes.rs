#[path = "wait_cleanup.rs"]
mod wait_cleanup;
pub(crate) use wait_cleanup::{
    prepare_existing_work_wait_cleanup_tx, register_existing_work_wait_cleanup_tx,
};
#[cfg(test)]
pub(crate) use wait_cleanup::{prepare_work_wait_cleanup_tx, register_work_wait_cleanup_tx};

use super::read_preparation::WorkReadMaterial;
use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;

pub(crate) struct WorkWriteProof {
    scope: InputEvidenceScope,
    base_revision: u64,
    before: BTreeMap<WorkKey, Option<StoredRecord>>,
    original_recovery: bool,
    material_budget: Option<InputEvidenceBudgetLedger>,
    preparation_usage: (usize, usize),
    original_material_limits: Option<(usize, usize)>,
    preparation_guards: Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}

impl WorkWriteProof {
    fn read_material<'a>(
        &'a self,
        request: &WorkDomainLoadRequest,
        memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<Option<WorkReadMaterial<'a>>> {
        self.material_budget
            .as_ref()
            .map(|ledger| {
                WorkReadMaterial::resume(
                    &self.scope,
                    request,
                    ledger,
                    memory,
                    self.preparation_usage,
                    self.original_material_limits
                        .context("budgeted Work proof omitted original phase limits")?,
                )
            })
            .transpose()
    }

    pub(super) fn into_preparation_guards(
        self,
    ) -> Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>> {
        self.preparation_guards
    }

    /// Transaction-local preparation ownership, never admission authority.
    pub(crate) fn retain_preparation_budget(
        &mut self,
        guards: Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    ) {
        self.preparation_guards.extend(guards);
    }
    #[cfg(test)]
    pub(super) fn original_phase_usage(&self) -> (usize, usize) {
        self.preparation_usage
    }
    pub(in crate::runtime_db) fn input_writer_material(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        ensure!(self.scope == *scope
            && self.material_budget.as_ref().is_some_and(|actual| actual.same_ledger(ledger))
            && !self.preparation_guards.is_empty(),
            "Native Input Work prior lacks original scope/ledger/guards");
        for guard in &self.preparation_guards {
            guard.validate_owned_material_binding(scope, ledger, 0)?;
        }
        Ok(self.preparation_usage)
    }
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }
    /// A body observed and qualified before this original business SQL.
    pub(crate) fn admitted_work_ids(&self) -> BTreeSet<String> {
        self.before
            .iter()
            .filter_map(|(key, before)| {
                (key.family == WorkFamily::Work && before.is_some()).then(|| key.id.clone())
            })
            .collect()
    }
    /// A typed cleanup read may observe this proof's exact original revision.
    /// This does not authorize a new command or qualify another Work key.
    pub(crate) fn validate_domain_observation_tx(
        &self,
        tx: &Transaction<'_>,
        revision: u64,
    ) -> Result<()> {
        ensure!(
            scope_facts(tx, &self.scope, self.original_recovery)?.is_some(),
            "original Work observation scope retired"
        );
        let _ = binding(tx, &self.scope)?;
        let current = loader::current_coverage(tx, &self.scope)?
            .context("original Work observation coverage missing")?;
        if current.revision != revision {
            return Err(WorkSnapshotError::RevisionMismatch {
                expected: revision,
                actual: current.revision,
            }
            .into());
        }
        ensure!(
            current.revision == self.base_revision,
            "original Work observation proof base changed"
        );
        Ok(())
    }
    pub(crate) fn validate_admission_tx(&self, tx: &Transaction<'_>, work_id: &str) -> Result<()> {
        ensure!(
            scope_facts(tx, &self.scope, self.original_recovery)?.is_some(),
            "original Work admission scope retired"
        );
        let key = WorkKey::new(WorkFamily::Work, work_id);
        let before = self
            .before
            .get(&key)
            .and_then(Option::as_ref)
            .context("derived input has no original qualified Work")?;
        let actual = stored_on(tx, &self.scope.agent_id, &key)?
            .context("original Work admission disappeared")?;
        ensure!(
            actual.payload == before.payload && actual.fingerprint == before.fingerprint,
            "original Work admission changed before its derived-input proof"
        );
        Ok(())
    }
}

pub(crate) fn prepare_work_write_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<WorkKey>,
) -> Result<WorkWriteProof> {
    prepare(tx, scope, keys, false, None)
}
/// Only same-incarnation already-admitted cleanup may use this proof. Existing
/// keys must already belong to its immutable/current namespace; no adoption.
pub(crate) fn prepare_original_work_write_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<WorkKey>,
) -> Result<WorkWriteProof> {
    prepare(tx, scope, keys, true, None)
}
/// A transaction-local accounting carrier. It cannot admit a Work key; the
/// unchanged original validator still runs after the exact footprint is frozen.
pub(in crate::runtime_db) struct WorkWriteMaterial<'a> {
    material: WorkReadMaterial<'a>,
}
impl<'a> WorkWriteMaterial<'a> {
    pub(in crate::runtime_db) fn new(
        scope: &'a InputEvidenceScope,
        request: &WorkDomainLoadRequest,
        ledger: &'a InputEvidenceBudgetLedger,
        memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<Self> {
        Ok(Self {
            material: WorkReadMaterial::new(scope, request, ledger, memory)?,
        })
    }
    pub(in crate::runtime_db) fn with_resource_material(
        scope: &'a InputEvidenceScope,
        request: &WorkDomainLoadRequest,
        ledger: &'a InputEvidenceBudgetLedger,
        memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
        loan: Option<&crate::runtime::WorkResourceMaterialLoan>,
    ) -> Result<Self> {
        let Some(loan) = loan else {
            return Self::new(scope, request, ledger, memory);
        };
        let prior = loan.validate_accounting(scope, ledger)?;
        Ok(Self {
            material: WorkReadMaterial::resume(
                scope,
                request,
                ledger,
                memory,
                prior,
                (
                    request.byte_limit.min(MAX_METADATA_BYTES),
                    request.record_limit.min(MAX_METADATA_RECORDS),
                ),
            )?,
        })
    }
    fn value_nodes(value: &serde_json::Value, depth: usize) -> Result<usize> {
        ensure!(
            depth < 128,
            "Work incoming JSON depth exceeds decoder bound"
        );
        let children = match value {
            serde_json::Value::Array(values) => values.iter().try_fold(0usize, |n, value| {
                n.checked_add(Self::value_nodes(value, depth + 1)?)
                    .context("Work incoming nodes overflow")
            })?,
            serde_json::Value::Object(values) => {
                values.values().try_fold(values.len(), |n, value| {
                    n.checked_add(Self::value_nodes(value, depth + 1)?)
                        .context("Work incoming nodes overflow")
                })?
            }
            _ => 0,
        };
        children
            .checked_add(1)
            .context("Work incoming nodes overflow")
    }
    pub(in crate::runtime_db) fn admit_work(
        &mut self,
        record: &crate::types::WorkItemRecord,
    ) -> Result<()> {
        self.material.raw(
            usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
                record,
            )?)?,
            1,
        )?;
        // Fixed Work/plan-artifact/completion-intent structs fit within 64
        // native slots; every variable collection/value is counted below.
        let mut nodes = record
            .todo_list
            .len()
            .checked_mul(3)
            .and_then(|n| {
                record
                    .work_refs
                    .len()
                    .checked_mul(12)
                    .and_then(|refs| n.checked_add(refs))
            })
            .and_then(|n| n.checked_add(64))
            .context("Work incoming record nodes overflow")?;
        for reference in &record.work_refs {
            nodes = nodes
                .checked_add(reference.metadata.len())
                .context("Work metadata keys overflow")?;
            for value in reference.metadata.values() {
                nodes = nodes
                    .checked_add(Self::value_nodes(value, 0)?)
                    .context("Work metadata nodes overflow")?;
            }
        }
        self.material.native(nodes)
    }
    pub(in crate::runtime_db) fn admit_wait(
        &mut self,
        record: &crate::types::WaitConditionRecord,
    ) -> Result<()> {
        self.material.raw(
            usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
                record,
            )?)?,
            1,
        )?;
        let nodes = record
            .wake_sources
            .len()
            .checked_mul(8)
            .and_then(|n| n.checked_add(32))
            .context("Wait incoming nodes overflow")?;
        let nodes = nodes
            .checked_add(
                record
                    .continuation
                    .as_ref()
                    .map(|v| Self::value_nodes(v, 0))
                    .transpose()?
                    .unwrap_or_default(),
            )
            .context("Wait continuation nodes overflow")?;
        self.material.native(nodes)
    }
    pub(in crate::runtime_db) fn admit_frame(
        &mut self,
        record: &crate::types::WorkItemContinuationFrame,
    ) -> Result<()> {
        self.material.raw(
            usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
                record,
            )?)?,
            1,
        )?;
        self.material.native(32)
    }
    pub(in crate::runtime_db) fn for_readback(
        proof: &'a WorkWriteProof,
        request: &WorkDomainLoadRequest,
        memory: Option<&'a mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    ) -> Result<Option<Self>> {
        proof
            .read_material(request, memory)
            .map(|m| m.map(|material| Self { material }))
    }
    pub(in crate::runtime_db) fn reserve_result_briefs(
        &mut self,
        briefs: &[crate::types::BriefRecord],
    ) -> Result<()> {
        self.material.input_ids(
            briefs
                .iter()
                .filter(|b| b.kind == BriefKind::Result)
                .map(|b| b.id.as_str()),
        )
    }
    pub(in crate::runtime_db) fn reserve_actual_ids<'b>(
        &mut self,
        ids: impl IntoIterator<Item = &'b str>,
    ) -> Result<()> {
        self.material.input_ids(ids)
    }
    #[cfg(test)]
    pub(in crate::runtime_db) fn reserve_actual_keys(
        &mut self,
        proof: &WorkWriteProof,
    ) -> Result<()> {
        ensure!(
            self.material.scope() == &proof.scope,
            "Work actual-key accounting scope differs"
        );
        self.material.keys(proof.before.keys(), 8)
    }
    pub(in crate::runtime_db) fn final_state_tx(
        &mut self,
        tx: &Transaction<'_>,
        proof: &WorkWriteProof,
    ) -> Result<AgentState> {
        self.material
            .scope_tx(tx, proof.original_recovery)?
            .context("Work finalizer scope retired")?
            .state
            .context("Work finalizer requires actual final State")
    }
    pub(in crate::runtime_db) fn register_tx(
        self,
        tx: &Transaction<'_>,
        proof: &WorkWriteProof,
        actual: &BTreeSet<WorkKey>,
        result_briefs: &BTreeSet<String>,
        final_state: &AgentState,
        request: &WorkDomainLoadRequest,
        capture_final_view: bool,
    ) -> Result<(CommittedWorkFacts, Option<CapturedWork>)> {
        ensure!(
            self.material.scope() == &proof.scope,
            "Work readback material belongs to another proof scope"
        );
        register_work_writes_inner_tx(
            tx,
            proof,
            actual,
            result_briefs,
            final_state,
            request,
            capture_final_view,
            Some(self.material),
        )
    }
    pub(in crate::runtime_db) fn key(&mut self, family: WorkFamily, id: &str) -> Result<WorkKey> {
        self.material.raw(id.len(), 1)?;
        Ok(WorkKey::new(family, id))
    }
    pub(in crate::runtime_db) fn wait_slots(&mut self, count: usize) -> Result<()> {
        self.material.charge(
            0,
            count
                .checked_mul(8 * 256)
                .context("Work Wait selector slots overflow")?,
            count
                .checked_mul(8)
                .context("Work Wait selector records overflow")?,
        )
    }
    pub(in crate::runtime_db) fn trigger_wait_pool_tx(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
    ) -> Result<()> {
        ensure!(
            self.material.scope() == scope,
            "Wait projection material belongs to another scope"
        );
        self.material.trigger_wait_pool_tx(tx)
    }
    pub(in crate::runtime_db) fn into_guards(
        self,
    ) -> Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>> {
        self.material.into_guards()
    }
    pub(in crate::runtime_db) fn active_frame_keys_tx(
        &mut self,
        tx: &Transaction<'_>,
        keys: &mut BTreeSet<WorkKey>,
    ) -> Result<()> {
        self.material.active_frame_keys_tx(tx, keys)
    }

    pub(in crate::runtime_db) fn wait_keys_tx<'b>(
        &mut self,
        tx: &Transaction<'_>,
        records: impl IntoIterator<Item = &'b crate::types::WaitConditionRecord>,
        keys: &mut BTreeSet<WorkKey>,
    ) -> Result<()> {
        for record in records {
            keys.insert(self.key(WorkFamily::Wait, &record.id)?);
            if matches!(
                record.status,
                crate::types::WaitConditionStatus::Active
                    | crate::types::WaitConditionStatus::Triggered
            ) {
                let sql="SELECT wait_condition_id AS id FROM wait_conditions WHERE agent_id=?1 AND work_item_id IS ?2 AND status IN ('active','triggered') AND wait_condition_id!=?3 LIMIT ?4";
                self.material.ids_query(
                    tx,
                    sql,
                    params![
                        record.agent_id,
                        record.work_item_id,
                        record.id,
                        i64::try_from(MAX_METADATA_RECORDS + 1)?
                    ],
                )?;
                let mut query = tx.prepare(sql)?;
                for id in query.query_map(
                    params![
                        record.agent_id,
                        record.work_item_id,
                        record.id,
                        i64::try_from(MAX_METADATA_RECORDS + 1)?
                    ],
                    |r| r.get::<_, String>(0),
                )? {
                    keys.insert(WorkKey::new(WorkFamily::Wait, id?));
                }
            }
        }
        Ok(())
    }
    pub(in crate::runtime_db) fn prepare_tx(
        self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        keys: &BTreeSet<WorkKey>,
        original_recovery: bool,
    ) -> Result<WorkWriteProof> {
        ensure!(
            self.material.scope() == scope,
            "Work material/proof scope differs"
        );
        prepare(tx, scope, keys, original_recovery, Some(self.material))
    }
}
/// The actual caller's original phase bounds and Host ledger are required.
/// Material accounting never changes the original admission predicate.
#[cfg(test)]
pub(crate) fn prepare_work_write_with_budget_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<WorkKey>,
    request: &WorkDomainLoadRequest,
    original_recovery: bool,
    ledger: &InputEvidenceBudgetLedger,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<WorkWriteProof> {
    let material = WorkReadMaterial::new(scope, request, ledger, memory)?;
    prepare(tx, scope, keys, original_recovery, Some(material))
}
pub(super) fn prepare_with_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<WorkKey>,
    original_recovery: bool,
    material: WorkReadMaterial<'_>,
) -> Result<WorkWriteProof> {
    prepare(tx, scope, keys, original_recovery, Some(material))
}
fn prepare(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<WorkKey>,
    original_recovery: bool,
    mut material: Option<WorkReadMaterial<'_>>,
) -> Result<WorkWriteProof> {
    let facts = match material.as_mut() {
        Some(m) => m.scope_tx(tx, original_recovery)?,
        None => scope_facts(tx, scope, original_recovery)?,
    };
    ensure!(facts.is_some(), "Work write scope retired");
    let seed = match material.as_mut() {
        Some(m) => m.binding_tx(tx)?,
        None => binding(tx, scope)?,
    };
    if let Some(m) = material.as_mut() {
        m.keys(keys, 8)?;
    }
    bound_metadata(
        keys.len(),
        keys.iter()
            .map(|key| key.id.len() + std::mem::size_of::<WorkKey>())
            .sum(),
    )?;
    let mut before = BTreeMap::new();
    let mut bytes = 0usize;
    for key in keys {
        let prior = match material.as_mut() {
            Some(m) => m.alias_tx(tx, key)?,
            None => alias(tx, scope, key)?,
        };
        ensure!(
            !prior
                .as_ref()
                .is_some_and(|alias| alias.availability == WorkAvailability::Pruned),
            "scoped Work writer cannot reuse a pruned ID"
        );
        let (table, id) = family_table(key.family);
        let length: Option<usize> = tx
            .query_row(
                &format!("SELECT length(CAST(payload_json AS BLOB)) FROM {table} WHERE {id}=?1"),
                [&key.id],
                |row| row.get(0),
            )
            .optional()?;
        bytes = bytes.saturating_add(length.unwrap_or_default());
        bound_metadata(keys.len(), bytes)?;
        let stored = match material.as_mut() {
            Some(m) => m.record_tx(tx, key)?,
            None => stored_on(tx, &scope.agent_id, key)?,
        };
        if let Some(stored) = &stored {
            match material.as_mut() {
                Some(m) => m.validate_member_tx(tx, seed.as_deref(), key, stored)?,
                None => qualification::validate_member(tx, scope, seed.as_deref(), key, stored)?,
            };
        } else {
            ensure!(
                !qualification::known_anywhere(tx, key)?,
                "known Work ID has no canonical body"
            );
        }
        if original_recovery {
            ensure!(
                stored.is_some(),
                "original Work cleanup cannot mint a fresh key without a derived admission proof"
            );
        }
        before.insert(key.clone(), stored);
    }
    let coverage = match material.as_mut() {
        Some(m) => m.coverage_tx(tx)?,
        None => loader::current_coverage(tx, scope)?,
    };
    let material_budget = material.as_ref().map(|m| m.ledger().clone());
    Ok(WorkWriteProof {
        scope: scope.clone(),
        base_revision: coverage.as_ref().map_or(1, |coverage| coverage.revision),
        before,
        original_recovery,
        material_budget,
        preparation_usage: material.as_ref().map_or((0, 0), WorkReadMaterial::usage),
        original_material_limits: material.as_ref().map(WorkReadMaterial::limits),
        preparation_guards: material
            .map(WorkReadMaterial::into_guards)
            .unwrap_or_default(),
    })
}

pub(crate) fn preflight_work_write_snapshot_tx(
    tx: &Transaction<'_>,
    proof: &WorkWriteProof,
    request: &WorkDomainLoadRequest,
) -> Result<WorkBootstrapSnapshot> {
    preflight_work_write_snapshot_with_memory_tx(tx, proof, request, None)
}
pub(crate) fn preflight_work_write_snapshot_with_memory_tx(
    tx: &Transaction<'_>,
    proof: &WorkWriteProof,
    request: &WorkDomainLoadRequest,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<WorkBootstrapSnapshot> {
    let mut material = proof.read_material(request, memory)?;
    let coverage = match material.as_mut() {
        Some(m) => m.coverage_tx(tx)?,
        None => loader::current_coverage(tx, &proof.scope)?,
    };
    ensure!(
        coverage.as_ref().map_or(1, |c| c.revision) == proof.base_revision,
        "Work preflight proof base changed"
    );
    let mut snapshot = match material.as_mut() {
        Some(m) => loader::prepare_work_domain_snapshot_with_material_tx(
            tx,
            &proof.scope,
            request,
            proof.original_recovery,
            false,
            m,
        )?,
        None => loader::preflight_original_work_snapshot_tx(
            tx,
            &proof.scope,
            request,
            proof.original_recovery,
        )?,
    };
    if let WorkBootstrapSnapshot::Current(current) = &mut snapshot {
        current.material_views = material
            .map(WorkReadMaterial::into_guards)
            .unwrap_or_default();
    }
    Ok(snapshot)
}

/// Check the complete view while its original proof is held, then release
/// this temporary phase before business SQL. The proof retains its own guards.
pub(in crate::runtime_db) fn preflight_work_write_snapshot_with_shared_phase_tx(
    tx: &Transaction<'_>,
    proof: &WorkWriteProof,
    request: &WorkDomainLoadRequest,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<(usize, usize)> {
    let mut material = proof.read_material(request, memory)?;
    let coverage = match material.as_mut() {
        Some(m) => m.coverage_tx(tx)?,
        None => loader::current_coverage(tx, &proof.scope)?,
    };
    ensure!(
        coverage.as_ref().map_or(1, |c| c.revision) == proof.base_revision,
        "Work preflight proof base changed"
    );
    let snapshot = match material.as_mut() {
        Some(m) => loader::prepare_work_domain_snapshot_with_material_tx(
            tx,
            &proof.scope,
            request,
            proof.original_recovery,
            false,
            m,
        )?,
        None => loader::preflight_original_work_snapshot_tx(
            tx,
            &proof.scope,
            request,
            proof.original_recovery,
        )?,
    };
    #[cfg(test)]
    if let Some(m) = material.as_ref() {
        m.report_phase_usage("preflight", proof.preparation_usage);
    }
    let peak = material.as_ref().map_or((0, 0), WorkReadMaterial::usage);
    drop(snapshot.require_current()?);
    drop(coverage);
    if let Some(material) = material {
        material.finish_transient()?;
    }
    Ok(peak)
}

/// Freeze primary and all implicit owner-cancellation candidates before SQL.
/// Returning a superset never qualifies records; register receives actual IDs.
pub(crate) fn work_wait_write_keys_tx(
    tx: &Transaction<'_>,
    records: &[crate::types::WaitConditionRecord],
) -> Result<BTreeSet<WorkKey>> {
    let mut keys = BTreeSet::new();
    for record in records {
        keys.insert(WorkKey::new(WorkFamily::Wait, &record.id));
        if matches!(
            record.status,
            crate::types::WaitConditionStatus::Active
                | crate::types::WaitConditionStatus::Triggered
        ) {
            let mut query=tx.prepare("SELECT wait_condition_id FROM wait_conditions WHERE agent_id=?1 AND work_item_id IS ?2 AND status IN ('active','triggered') AND wait_condition_id!=?3 LIMIT ?4")?;
            for id in query.query_map(
                params![
                    record.agent_id,
                    record.work_item_id,
                    record.id,
                    i64::try_from(MAX_METADATA_RECORDS + 1)?
                ],
                |row| row.get::<_, String>(0),
            )? {
                keys.insert(WorkKey::new(WorkFamily::Wait, id?));
            }
        }
        bound_metadata(keys.len(), keys.iter().map(|key| key.id.len()).sum())?;
    }
    Ok(keys)
}

#[derive(Debug)]
pub(crate) struct CommittedWorkFacts {
    pub delta: CommittedWorkDelta,
    pub owning_input_roots: BTreeSet<InputEvidenceKey>,
    /// Includes the original loan/proof/preflight/final readback and capture.
    pub(in crate::runtime_db) material_usage: Option<(usize, usize)>,
}

#[cfg(test)]
impl CommittedWorkFacts {
    pub(super) fn input_roots(&self) -> BTreeSet<InputEvidenceKey> {
        self.owning_input_roots
            .iter()
            .cloned()
            .chain(
                self.delta
                    .coverage
                    .latest_results
                    .values()
                    .flatten()
                    .flat_map(WorkCompletionRef::input_roots),
            )
            .collect()
    }
}

pub(crate) fn register_work_writes_tx(
    tx: &Transaction<'_>,
    proof: &WorkWriteProof,
    actual: &BTreeSet<WorkKey>,
    actual_result_brief_ids: &BTreeSet<String>,
    final_state: &AgentState,
    request: &WorkDomainLoadRequest,
) -> Result<CommittedWorkFacts> {
    register_work_writes_inner_tx(
        tx,
        proof,
        actual,
        actual_result_brief_ids,
        final_state,
        request,
        false,
        proof.read_material(request, None)?,
    )
    .map(|(facts, _)| facts)
}

/// Reuses the finalizer's exact selected view; it does not load a second view
/// or advance another revision. No-op commits also return their full view.
pub(crate) fn register_work_writes_with_view_tx(
    tx: &Transaction<'_>,
    proof: &WorkWriteProof,
    actual: &BTreeSet<WorkKey>,
    actual_result_brief_ids: &BTreeSet<String>,
    final_state: &AgentState,
    request: &WorkDomainLoadRequest,
) -> Result<(CommittedWorkFacts, CapturedWork)> {
    let (facts, captured) = register_work_writes_inner_tx(
        tx,
        proof,
        actual,
        actual_result_brief_ids,
        final_state,
        request,
        true,
        proof.read_material(request, None)?,
    )?;
    Ok((
        facts,
        captured.context("Work finalizer did not capture its final view")?,
    ))
}

fn register_work_writes_inner_tx(
    tx: &Transaction<'_>,
    proof: &WorkWriteProof,
    actual: &BTreeSet<WorkKey>,
    actual_result_brief_ids: &BTreeSet<String>,
    final_state: &AgentState,
    request: &WorkDomainLoadRequest,
    capture_final_view: bool,
    mut material: Option<WorkReadMaterial<'_>>,
) -> Result<(CommittedWorkFacts, Option<CapturedWork>)> {
    if let Some(m) = material.as_mut() {
        m.keys(actual, 8)?;
    }
    ensure!(
        final_state.id == proof.scope.agent_id,
        "Work final State scope mismatch"
    );
    let facts = match material.as_mut() {
        Some(m) => m.scope_tx(tx, proof.original_recovery)?,
        None => scope_facts(tx, &proof.scope, proof.original_recovery)?,
    }
    .context("Work writer identity changed during original transaction")?;
    ensure!(
        facts.state.as_ref() == Some(final_state),
        "Work delta does not use exact final canonical State"
    );
    let original_coverage = match material.as_mut() {
        Some(m) => m.coverage_tx(tx)?,
        None => loader::current_coverage(tx, &proof.scope)?,
    };
    if let (Some(m), Some(c)) = (material.as_mut(), original_coverage.as_ref()) {
        m.coverage_copy(c, 1)?;
    }
    ensure!(
        original_coverage
            .as_ref()
            .map_or(1, |coverage| coverage.revision)
            == proof.base_revision,
        "Work proof coverage changed before finalization"
    );
    let mut coverage = original_coverage
        .clone()
        .or_else(|| Some(empty_coverage(proof.base_revision)));
    let revision = proof
        .base_revision
        .checked_add(1)
        .context("Work revision overflow")?;
    let mut changed = Vec::new();
    let mut removals = BTreeMap::new();
    for key in actual {
        let before = proof
            .before
            .get(key)
            .context("actual Work footprint omitted from pre-SQL proof")?;
        match match material.as_mut() {
            Some(m) => m.record_tx(tx, key)?,
            None => stored_on(tx, &proof.scope.agent_id, key)?,
        } {
            Some(stored) => {
                ensure!(
                    !match material.as_mut() {
                        Some(m) => m.alias_tx(tx, key)?,
                        None => alias(tx, &proof.scope, key)?,
                    }
                    .is_some_and(|alias| alias.availability == WorkAvailability::Pruned),
                    "Work write resurrected a pruned ID"
                );
                let unchanged = before.as_ref().is_some_and(|prior| {
                    prior.payload == stored.payload && prior.fingerprint == stored.fingerprint
                });
                if let CanonicalWorkRecord::Wait(wait) = &stored.record {
                    let prior_alias = match material.as_mut() {
                        Some(m) => m.alias_tx(tx, key)?,
                        None => alias(tx, &proof.scope, key)?,
                    };
                    if !unchanged || prior_alias.is_some() {
                        let edge_revision = if unchanged {
                            prior_alias.as_ref().expect("checked alias").revision
                        } else {
                            revision
                        };
                        super::relations::register_wait_edges_tx(
                            tx,
                            &proof.scope,
                            wait,
                            edge_revision,
                            false,
                        )?;
                    }
                }
                if before.as_ref().is_some_and(|prior| {
                    prior.payload == stored.payload && prior.fingerprint == stored.fingerprint
                }) {
                    continue;
                }
                upsert_alias(
                    tx,
                    &proof.scope,
                    key,
                    &Alias {
                        revision,
                        availability: WorkAvailability::Present,
                        digest: Some(stored.digest()),
                        fingerprint: Some(stored.fingerprint.clone()),
                    },
                )?;
                if key.family == WorkFamily::Work && before.is_none() {
                    let coverage =
                        coverage.get_or_insert_with(|| empty_coverage(proof.base_revision));
                    coverage
                        .latest_results
                        .entry(key.id.clone())
                        .or_insert(None);
                    loader::persist_result_head(tx, &proof.scope, &key.id, revision, &None)?;
                }
                changed.push(stored.qualified(&proof.scope, revision)?);
            }
            None => {
                ensure!(before.is_some(), "missing-to-missing Work is not a prune");
                upsert_alias(
                    tx,
                    &proof.scope,
                    key,
                    &Alias {
                        revision,
                        availability: WorkAvailability::Pruned,
                        digest: None,
                        fingerprint: None,
                    },
                )?;
                if let Some(StoredRecord {
                    record: CanonicalWorkRecord::Wait(wait),
                    ..
                }) = before
                {
                    super::relations::register_wait_edges_tx(
                        tx,
                        &proof.scope,
                        wait,
                        revision,
                        true,
                    )?;
                }
                removals.insert(key.clone(), WorkAvailability::Pruned);
            }
        }
    }
    if !actual_result_brief_ids.is_empty() {
        let coverage = coverage.get_or_insert_with(|| empty_coverage(proof.base_revision));
        update_result_heads_with_material_tx(
            tx,
            &proof.scope,
            coverage,
            actual_result_brief_ids,
            material.as_mut(),
        )?;
    }
    if let Some(coverage) = &mut coverage {
        coverage.revision = revision;
        loader::persist_coverage(tx, &proof.scope, coverage)?;
    }
    #[cfg(test)]
    if let Some(m) = material.as_ref() {
        m.report_phase_usage("before final snapshot", proof.preparation_usage);
    }
    let snapshot = match material.as_mut() {
        Some(m) => loader::prepare_work_domain_snapshot_with_material_tx(
            tx,
            &proof.scope,
            request,
            proof.original_recovery,
            true,
            m,
        )?,
        None => {
            if proof.original_recovery {
                loader::prepare_original_work_recovery_snapshot_tx(tx, &proof.scope, request)?
            } else {
                loader::prepare_work_domain_snapshot_tx(tx, &proof.scope, request)?
            }
        }
    }
    .require_current()?;
    #[cfg(test)]
    if let Some(m) = material.as_ref() {
        m.report_phase_usage("after final snapshot", proof.preparation_usage);
    }
    // Only ACK bodies outside the ordinary final selection need an extra
    // root. Promoting an already-live key would make its next no-op change
    // coverage just by removing that unnecessary mandatory membership.
    let extra = changed
        .iter()
        .filter_map(|record| {
            (snapshot.resident.coverage().observations.get(&record.key)
                != Some(&WorkAvailability::Present))
            .then(|| record.key.clone())
        })
        .collect::<BTreeSet<_>>();
    let snapshot = if extra.is_empty() {
        snapshot
    } else {
        drop(snapshot);
        if let Some(m) = material.as_mut() {
            m.selectors(&request.capture)?;
            m.keys(&extra, 8)?;
        }
        let mut request = request.clone();
        request.capture.mandatory_roots.extend(extra);
        match material.as_mut() {
            Some(m) => loader::prepare_work_domain_snapshot_with_material_tx(
                tx,
                &proof.scope,
                &request,
                proof.original_recovery,
                true,
                m,
            )?,
            None => {
                if proof.original_recovery {
                    loader::prepare_original_work_recovery_snapshot_tx(tx, &proof.scope, &request)?
                } else {
                    loader::prepare_work_domain_snapshot_tx(tx, &proof.scope, &request)?
                }
            }
        }
        .require_current()?
    };
    let (mut facts, view) = finish_delta_inner(
        tx,
        &proof.scope,
        proof.base_revision,
        original_coverage.as_ref(),
        snapshot,
        changed,
        removals,
        capture_final_view,
        material.as_mut(),
    )?;
    if let Some(material) = material {
        facts.material_usage = Some(material.usage());
        facts.delta._material_views = Some(Arc::new(material.into_guards()));
    }
    Ok((facts, view))
}

#[cfg(test)]
fn finish_delta(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    base_revision: u64,
    original: Option<&WorkCoverage>,
    snapshot: WorkCurrentSnapshot,
    changed: Vec<Arc<QualifiedWorkRecord>>,
    removals: BTreeMap<WorkKey, WorkAvailability>,
) -> Result<CommittedWorkFacts> {
    finish_delta_inner(
        tx,
        scope,
        base_revision,
        original,
        snapshot,
        changed,
        removals,
        false,
        None,
    )
    .map(|(facts, _)| facts)
}

#[allow(clippy::too_many_arguments)]
fn finish_delta_inner(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    base_revision: u64,
    original: Option<&WorkCoverage>,
    snapshot: WorkCurrentSnapshot,
    changed: Vec<Arc<QualifiedWorkRecord>>,
    removals: BTreeMap<WorkKey, WorkAvailability>,
    capture_final_view: bool,
    mut material: Option<&mut WorkReadMaterial<'_>>,
) -> Result<(CommittedWorkFacts, Option<CapturedWork>)> {
    if let Some(m) = material.as_deref_mut() {
        m.coverage_copy(snapshot.resident.coverage(), 4)?;
    }
    let mut coverage = snapshot.resident.coverage().clone();
    coverage.revision = base_revision;
    let is_changed = !changed.is_empty() || !removals.is_empty() || original != Some(&coverage);
    coverage.revision = if is_changed {
        base_revision
            .checked_add(1)
            .context("Work revision overflow")?
    } else {
        base_revision
    };
    loader::persist_coverage(tx, scope, &coverage)?;
    let mut records = BTreeMap::new();
    if is_changed {
        records.extend(
            changed
                .into_iter()
                .map(|record| (record.key.clone(), record)),
        );
    }
    let captured = if is_changed || capture_final_view {
        let capture_request = WorkCaptureRequest {
            include_live: true,
            nonopen_limit: coverage.nonopen.limit,
            mandatory_roots: coverage.mandatory_roots.clone(),
            latest_result_work_ids: coverage.latest_results.keys().cloned().collect(),
            required_turn_ids: coverage.turn_wait_keys.keys().cloned().collect(),
            required_task_ids: coverage.task_wait_keys.keys().cloned().collect(),
            required_trigger_message_ids: coverage.trigger_wait_keys.keys().cloned().collect(),
        };
        let mut captured = match material.as_deref_mut() {
            Some(m) => m.capture_resident(&snapshot.resident, &capture_request)?,
            None => snapshot.resident.capture(&capture_request)?,
        };
        // The loader temporarily uses base+1; a true no-op retains base.
        captured.revision = coverage.revision;
        if is_changed {
            for record in &captured.records {
                records
                    .entry(record.key.clone())
                    .or_insert_with(|| record.clone());
            }
        }
        capture_final_view.then_some(captured)
    } else {
        None
    };
    let material_usage = (!snapshot.material_views.is_empty()).then_some(snapshot.material_usage);
    Ok((
        CommittedWorkFacts {
            delta: CommittedWorkDelta {
                _material_views: (!snapshot.material_views.is_empty())
                    .then(|| Arc::new(snapshot.material_views)),
                scope: scope.clone(),
                base_revision,
                revision: coverage.revision,
                records: records.into_values().collect(),
                removals,
                coverage,
            },
            owning_input_roots: snapshot.owning_input_roots,
            material_usage,
        },
        captured,
    ))
}

pub(super) fn empty_coverage(revision: u64) -> WorkCoverage {
    WorkCoverage {
        revision,
        live_complete: true,
        live_work_ids: BTreeSet::new(),
        unresolved_wait_ids: BTreeSet::new(),
        active_frame_ids: BTreeSet::new(),
        mandatory_roots: BTreeSet::new(),
        observations: BTreeMap::new(),
        nonopen: WorkHistoryCoverage {
            limit: WORK_NONOPEN_WINDOW,
            keys: Vec::new(),
            is_exhaustive: true,
        },
        latest_results: BTreeMap::new(),
        legacy_delivery_heads: BTreeMap::new(),
        turn_wait_keys: BTreeMap::new(),
        task_wait_keys: BTreeMap::new(),
        trigger_wait_keys: BTreeMap::new(),
    }
}

/// Actual Result writes are already qualified by D3 in this same transaction.
/// Only their owning Work heads are considered; raw late history is excluded.
#[cfg(test)]
pub(crate) fn register_work_result_writes_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    brief_ids: &BTreeSet<String>,
    request: &WorkDomainLoadRequest,
) -> Result<CommittedWorkFacts> {
    ensure!(
        scope_facts(tx, scope, false)?.is_some(),
        "Result Work scope retired"
    );
    let mut coverage = loader::current_coverage(tx, scope)?
        .context("Result writer needs prepared Work coverage")?;
    let base_revision = coverage.revision;
    let original_coverage = coverage.clone();
    update_result_heads(tx, scope, &mut coverage, brief_ids)?;
    coverage.revision = base_revision
        .checked_add(1)
        .context("Work revision overflow")?;
    loader::persist_coverage(tx, scope, &coverage)?;
    let snapshot =
        loader::prepare_work_domain_snapshot_tx(tx, scope, request)?.require_current()?;
    finish_delta(
        tx,
        scope,
        base_revision,
        Some(&original_coverage),
        snapshot,
        Vec::new(),
        BTreeMap::new(),
    )
}

pub(super) fn update_result_heads(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    coverage: &mut WorkCoverage,
    brief_ids: &BTreeSet<String>,
) -> Result<()> {
    update_result_heads_with_material_tx(tx, scope, coverage, brief_ids, None)
}
pub(super) fn update_result_heads_with_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    coverage: &mut WorkCoverage,
    brief_ids: &BTreeSet<String>,
    mut material: Option<&mut WorkReadMaterial<'_>>,
) -> Result<()> {
    if let Some(m) = material.as_deref_mut() {
        m.input_ids(brief_ids.iter().map(String::as_str))?;
    }
    let mut touched = BTreeSet::new();
    bound_metadata(brief_ids.len(), brief_ids.iter().map(String::len).sum())?;
    for id in brief_ids {
        let key = InputEvidenceKey::new(InputEvidenceFamily::Brief, id);
        let present:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='brief' AND record_id=?3 AND availability='present')",params![scope.agent_id,scope.identity_incarnation,id],|row|row.get(0))?;
        ensure!(
            present,
            "actual Result Brief was not qualified by its original D3 writer"
        );
        if let Some(m) = material.as_deref_mut() {
            m.input_key_tx(tx, &key)?;
        }
        let _ = super::completion::proven_payload(tx, scope, &key)?;
        let payload: String = tx.query_row(
            "SELECT payload_json FROM briefs WHERE evidence_id=?1",
            [id],
            |row| row.get(0),
        )?;
        let brief: BriefRecord = serde_json::from_str(&payload)?;
        ensure!(
            brief.agent_id == scope.agent_id && brief.id == *id,
            "Result writer Brief scope mismatch"
        );
        if brief.kind != BriefKind::Result || brief.text.is_empty() {
            continue;
        }
        let Some(work) = brief.work_item_id else {
            continue;
        };
        let key = WorkKey::new(WorkFamily::Work, &work);
        let bytes: usize = tx.query_row(
            "SELECT length(CAST(payload_json AS BLOB)) FROM work_items WHERE work_item_id=?1",
            [&work],
            |row| row.get(0),
        )?;
        bound_metadata(1, bytes)?;
        if let Some(m) = material.as_deref_mut() {
            m.keys(std::iter::once(&key), 8)?;
        }
        let stored = match material.as_deref_mut() {
            Some(m) => m.record_tx(tx, &key)?,
            None => stored_on(tx, &scope.agent_id, &key)?,
        }
        .context("actual Result has no original Work")?;
        let seed = match material.as_deref_mut() {
            Some(m) => m.binding_tx(tx)?,
            None => binding(tx, scope)?,
        };
        match material.as_deref_mut() {
            Some(m) => m.validate_member_tx(tx, seed.as_deref(), &key, &stored)?,
            None => qualification::validate_member(tx, scope, seed.as_deref(), &key, &stored)?,
        };
        touched.insert(work);
    }
    for work in touched {
        if let Some(m) = material.as_deref_mut() {
            m.completion_result_pool_tx(tx, &work)?;
        }
        // Qualification is explicit. Existing raw same-name rows never win.
        let head:Option<String>=tx.query_row("SELECT b.evidence_id FROM briefs b WHERE b.agent_id=?1 AND b.work_item_id=?3 AND b.kind='result' AND length(json_extract(b.payload_json,'$.text'))>0 AND (EXISTS(SELECT 1 FROM input_evidence_current a WHERE a.family='brief' AND a.record_id=b.evidence_id AND a.agent_id=?1 AND a.identity_incarnation=?2 AND a.availability='present') OR (NOT EXISTS(SELECT 1 FROM input_evidence_current a WHERE a.family='brief' AND a.record_id=b.evidence_id AND a.agent_id=?1 AND a.identity_incarnation=?2) AND EXISTS(SELECT 1 FROM agent_episode_continuity c JOIN input_evidence_eligibility e USING(seed_id) WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND e.family='brief' AND e.record_id=b.evidence_id))) ORDER BY b.created_at DESC,b.evidence_id DESC LIMIT 1",params![scope.agent_id,scope.identity_incarnation,work],|row|row.get(0)).optional()?;
        let head = head.context("actual qualified Result has no current head")?;
        let result = super::completion::completion_head_refs_with_material_tx(
            tx,
            scope,
            &head,
            material.as_deref_mut(),
        )?;
        loader::persist_result_head(
            tx,
            scope,
            &work,
            coverage
                .revision
                .checked_add(1)
                .context("Work revision overflow")?,
            &Some(result.clone()),
        )?;
        coverage.latest_results.insert(work, Some(result));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn register_work_pruned_tx(
    tx: &Transaction<'_>,
    proof: &WorkWriteProof,
    keys: &BTreeSet<WorkKey>,
    state: &AgentState,
    request: &WorkDomainLoadRequest,
) -> Result<CommittedWorkFacts> {
    ensure!(
        keys.iter()
            .all(|key| proof.before.get(key).is_some_and(Option::is_some)),
        "Work prune lacks original existing footprint"
    );
    for key in keys {
        ensure!(
            stored_on(tx, &proof.scope.agent_id, key)?.is_none(),
            "Work prune still has a canonical body"
        );
    }
    register_work_writes_tx(tx, proof, keys, &BTreeSet::new(), state, request)
}
