#[path = "wait_cleanup.rs"]
mod wait_cleanup;
pub(crate) use wait_cleanup::{
    prepare_existing_work_wait_cleanup_tx, register_existing_work_wait_cleanup_tx,
};
#[cfg(test)]
pub(crate) use wait_cleanup::{prepare_work_wait_cleanup_tx, register_work_wait_cleanup_tx};

use super::*;

pub(crate) struct WorkWriteProof {
    scope: InputEvidenceScope,
    base_revision: u64,
    before: BTreeMap<WorkKey, Option<StoredRecord>>,
    original_recovery: bool,
    decoded_state: Option<super::super::context_inputs::ContextStateDecode>,
    preparation_guards: Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}

impl WorkWriteProof {
    /// Transaction-local preparation ownership, never admission authority.
    pub(crate) fn retain_preparation_budget(
        &mut self,
        guards: Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    ) {
        self.preparation_guards.extend(guards);
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
    prepare(tx, scope, keys, false)
}
/// Only same-incarnation already-admitted cleanup may use this proof. Existing
/// keys must already belong to its immutable/current namespace; no adoption.
pub(crate) fn prepare_original_work_write_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<WorkKey>,
) -> Result<WorkWriteProof> {
    prepare(tx, scope, keys, true)
}
fn prepare(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<WorkKey>,
    original_recovery: bool,
) -> Result<WorkWriteProof> {
    ensure!(
        !scope.agent_id.is_empty() && scope.identity_incarnation > 0,
        "invalid Work scope"
    );
    let scope_read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
    ensure!(
        scope_is_current(tx, scope, scope_read.facts(), original_recovery)?,
        "Work write scope retired"
    );
    let seed = binding(tx, scope)?;
    bound_metadata(
        keys.len(),
        keys.iter()
            .map(|key| key.id.len() + std::mem::size_of::<WorkKey>())
            .sum(),
    )?;
    let mut before = BTreeMap::new();
    let mut bytes = 0usize;
    for key in keys {
        let prior = alias(tx, scope, key)?;
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
        let stored = stored_on(tx, &scope.agent_id, key)?;
        if let Some(stored) = &stored {
            qualification::validate_member(tx, scope, seed.as_deref(), key, stored)?;
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
    Ok(WorkWriteProof {
        scope: scope.clone(),
        base_revision: loader::current_coverage(tx, scope)?.map_or(1, |coverage| coverage.revision),
        before,
        original_recovery,
        decoded_state: scope_read.decoded_state(),
        preparation_guards: Vec::new(),
    })
}

pub(crate) fn preflight_work_write_snapshot_tx(
    tx: &Transaction<'_>,
    proof: &WorkWriteProof,
    request: &WorkDomainLoadRequest,
) -> Result<WorkBootstrapSnapshot> {
    // Preflight uses the proof's original scope and never advances its base.
    ensure!(
        loader::current_coverage(tx, &proof.scope)?
            .as_ref()
            .map_or(1, |coverage| coverage.revision)
            == proof.base_revision,
        "Work preflight proof base changed"
    );
    loader::preflight_original_work_snapshot_tx(
        tx,
        &proof.scope,
        request,
        proof.original_recovery,
        proof.decoded_state.as_ref(),
    )
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
) -> Result<(CommittedWorkFacts, Option<CapturedWork>)> {
    ensure!(
        final_state.id == proof.scope.agent_id,
        "Work final State scope mismatch"
    );
    let scope_read = ContextDurableRead::new(tx, &proof.scope.agent_id)
        .scope_read_using_decode(proof.decoded_state.as_ref())?;
    let facts = scope_read.facts();
    ensure!(
        scope_is_current(tx, &proof.scope, facts, proof.original_recovery)?,
        "Work writer identity changed during original transaction"
    );
    ensure!(
        facts.state.as_deref() == Some(final_state),
        "Work delta does not use exact final canonical State"
    );
    let original_coverage = loader::current_coverage(tx, &proof.scope)?;
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
        match stored_on(tx, &proof.scope.agent_id, key)? {
            Some(stored) => {
                ensure!(
                    !alias(tx, &proof.scope, key)?
                        .is_some_and(|alias| alias.availability == WorkAvailability::Pruned),
                    "Work write resurrected a pruned ID"
                );
                let unchanged = before.as_ref().is_some_and(|prior| {
                    prior.payload == stored.payload && prior.fingerprint == stored.fingerprint
                });
                if let CanonicalWorkRecord::Wait(wait) = &stored.record {
                    let prior_alias = alias(tx, &proof.scope, key)?;
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
        update_result_heads(tx, &proof.scope, coverage, actual_result_brief_ids)?;
    }
    if let Some(coverage) = &mut coverage {
        coverage.revision = revision;
        loader::persist_coverage(tx, &proof.scope, coverage)?;
    }
    let snapshot = loader::load_with_scope_read(
        tx,
        &proof.scope,
        request,
        true,
        proof.original_recovery,
        false,
        Some(scope_read),
    )?
    .require_current()?;
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
        let mut request = request.clone();
        request.capture.mandatory_roots.extend(extra);
        if proof.original_recovery {
            loader::prepare_original_work_recovery_snapshot_tx(tx, &proof.scope, &request)?
        } else {
            loader::prepare_work_domain_snapshot_tx(tx, &proof.scope, &request)?
        }
        .require_current()?
    };
    finish_delta_inner(
        tx,
        &proof.scope,
        proof.base_revision,
        original_coverage.as_ref(),
        snapshot,
        changed,
        removals,
        capture_final_view,
    )
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
) -> Result<(CommittedWorkFacts, Option<CapturedWork>)> {
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
        let mut captured = snapshot.resident.capture(&WorkCaptureRequest {
            include_live: true,
            nonopen_limit: coverage.nonopen.limit,
            mandatory_roots: coverage.mandatory_roots.clone(),
            latest_result_work_ids: coverage.latest_results.keys().cloned().collect(),
            required_turn_ids: coverage.turn_wait_keys.keys().cloned().collect(),
            required_task_ids: coverage.task_wait_keys.keys().cloned().collect(),
            required_trigger_message_ids: coverage.trigger_wait_keys.keys().cloned().collect(),
        })?;
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
    Ok((
        CommittedWorkFacts {
            delta: CommittedWorkDelta {
                scope: scope.clone(),
                base_revision,
                revision: coverage.revision,
                records: records.into_values().collect(),
                removals,
                coverage,
            },
            owning_input_roots: snapshot.owning_input_roots,
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
    let mut touched = BTreeSet::new();
    bound_metadata(brief_ids.len(), brief_ids.iter().map(String::len).sum())?;
    for id in brief_ids {
        let key = InputEvidenceKey::new(InputEvidenceFamily::Brief, id);
        let present:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='brief' AND record_id=?3 AND availability='present')",params![scope.agent_id,scope.identity_incarnation,id],|row|row.get(0))?;
        ensure!(
            present,
            "actual Result Brief was not qualified by its original D3 writer"
        );
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
        let stored =
            stored_on(tx, &scope.agent_id, &key)?.context("actual Result has no original Work")?;
        qualification::validate_member(tx, scope, binding(tx, scope)?.as_deref(), &key, &stored)?;
        touched.insert(work);
    }
    for work in touched {
        // Qualification is explicit. Existing raw same-name rows never win.
        let head:Option<String>=tx.query_row("SELECT b.evidence_id FROM briefs b WHERE b.agent_id=?1 AND b.work_item_id=?3 AND b.kind='result' AND length(json_extract(b.payload_json,'$.text'))>0 AND (EXISTS(SELECT 1 FROM input_evidence_current a WHERE a.family='brief' AND a.record_id=b.evidence_id AND a.agent_id=?1 AND a.identity_incarnation=?2 AND a.availability='present') OR (NOT EXISTS(SELECT 1 FROM input_evidence_current a WHERE a.family='brief' AND a.record_id=b.evidence_id AND a.agent_id=?1 AND a.identity_incarnation=?2) AND EXISTS(SELECT 1 FROM agent_episode_continuity c JOIN input_evidence_eligibility e USING(seed_id) WHERE c.agent_id=?1 AND c.identity_incarnation=?2 AND e.family='brief' AND e.record_id=b.evidence_id))) ORDER BY b.created_at DESC,b.evidence_id DESC LIMIT 1",params![scope.agent_id,scope.identity_incarnation,work],|row|row.get(0)).optional()?;
        let head = head.context("actual qualified Result has no current head")?;
        let result = super::completion::completion_head_refs_tx(tx, scope, &head)?;
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

#[test]
fn original_work_proof_reuses_canonical_state_and_rejects_changed_final_state() -> Result<()> {
    use crate::{
        diagnostics::resource_io::ResourceReadMaterial,
        runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    };
    let (_home, db) = super::tests::fresh()?;
    let scope = InputEvidenceScope {
        agent_id: "agent-a".into(),
        identity_incarnation: 1,
    };
    let request = WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: 1024,
        byte_limit: 1024 * 1024,
    };
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let decoded = db.transaction(|tx| {
        let _entry = phase.enter();
        let keys = BTreeSet::new();
        let proof = prepare_work_write_tx(tx, &scope, &keys)?;
        let read = ContextDurableRead::new(tx, &scope.agent_id)
            .scope_read_using_decode(proof.decoded_state.as_ref())?;
        let original = read.facts().state.as_ref().unwrap();
        let before: i64 = tx.query_row("SELECT total_changes()", [], |row| row.get(0))?;
        for _ in 0..2 {
            let snapshot = preflight_work_write_snapshot_tx(tx, &proof, &request)?.require_current()?;
            assert!(Arc::ptr_eq(original, snapshot.facts.state.as_ref().unwrap()));
        }
        assert_eq!(tx.query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))?, before);
        register_work_writes_tx(tx, &proof, &keys, &BTreeSet::new(),
            original, &request)?;
        tx.execute("UPDATE agent_states SET payload_json=json_set(payload_json,'$.pending',17) WHERE agent_id=?1",
            [&scope.agent_id])?;
        let error = register_work_writes_tx(tx, &proof, &keys, &BTreeSet::new(), original, &request)
            .err().context("changed State reused its original decode")?;
        assert!(error.to_string().contains("exact final canonical State"), "{error:#}");
        Ok(proof.decoded_state.clone().unwrap())
    })?;
    phase.finish()?;
    drop(phase);
    assert!(
        ledger.report()?.observed_bytes > 0,
        "original decode retains its phase"
    );
    drop(decoded);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
