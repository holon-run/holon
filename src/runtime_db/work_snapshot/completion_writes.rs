//! Standalone D3 evidence refreshes completion metadata in its original TX.
//! This never expands Work coverage or loads unrelated historical Work bodies.
use super::read_preparation::WorkReadMaterial;
use super::*;
use crate::runtime::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger};

pub(crate) struct PreparedWorkCompletionWrite {
    scope: InputEvidenceScope,
    coverage: Option<WorkCoverage>,
    proof: Option<WorkWriteProof>,
    ledger: Option<InputEvidenceBudgetLedger>,
    preparation_guards: Vec<Arc<BudgetViewGuard>>,
}

pub(crate) fn prepare_work_completion_write_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    result: Option<&BriefRecord>,
    request: &WorkDomainLoadRequest,
    ledger: Option<&InputEvidenceBudgetLedger>,
) -> Result<PreparedWorkCompletionWrite> {
    let mut material = ledger
        .map(|ledger| WorkReadMaterial::new(scope, request, ledger, None))
        .transpose()?;
    ensure!(
        match material.as_mut() {
            Some(material) => material.scope_tx(tx, false)?,
            None => scope_facts(tx, scope, false)?,
        }
        .is_some(),
        "completion writer scope retired"
    );
    let coverage = match material.as_mut() {
        Some(material) => material.coverage_tx(tx)?,
        None => loader::current_coverage(tx, scope)?,
    };
    if let Some(coverage) = &coverage {
        bound_completion_coverage(coverage, request)?;
    }
    // Actual D3 readback preserves these identifiers. Observe their future
    // selector/result-work copies before the original body acknowledgement.
    if let (Some(material), Some(result)) = (material.as_mut(), result) {
        material
            .input_ids(std::iter::once(result.id.as_str()).chain(result.work_item_id.as_deref()))?;
    }
    let proof = if let Some(work) = result
        .filter(|brief| brief.kind == BriefKind::Result && !brief.text.is_empty())
        .and_then(|brief| brief.work_item_id.as_deref())
    {
        let key = match material.as_mut() {
            Some(material) => {
                material.input_ids(std::iter::once(work))?;
                WorkKey::new(WorkFamily::Work, work)
            }
            None => WorkKey::new(WorkFamily::Work, work),
        };
        let keys = BTreeSet::from([key]);
        let proof = match material.take() {
            Some(material) => writes::prepare_with_material_tx(tx, scope, &keys, false, material)?,
            None => prepare_work_write_tx(tx, scope, &keys)?,
        };
        ensure!(
            proof.admitted_work_ids().contains(work),
            "standalone Result requires its original qualified Work"
        );
        Some(proof)
    } else {
        None
    };
    Ok(PreparedWorkCompletionWrite {
        scope: scope.clone(),
        coverage,
        proof,
        ledger: ledger.cloned(),
        preparation_guards: material
            .map(WorkReadMaterial::into_guards)
            .unwrap_or_default(),
    })
}

/// Input qualification/pruning has already completed. Only exact current head
/// references change; all Work bodies and historical coverage stay untouched.
/// Missing Work coverage retains accounting in the original ACK, not a domain.
pub(crate) fn finish_work_completion_write_tx(
    tx: &Transaction<'_>,
    mut prepared: PreparedWorkCompletionWrite,
    actual_result_brief_ids: &BTreeSet<String>,
    request: &WorkDomainLoadRequest,
) -> Result<(Option<CommittedWorkDelta>, Vec<Arc<BudgetViewGuard>>)> {
    let scope = &prepared.scope;
    let mut material = prepared
        .ledger
        .as_ref()
        .map(|ledger| WorkReadMaterial::new(scope, request, ledger, None))
        .transpose()?;
    ensure!(
        match material.as_mut() {
            Some(material) => material.scope_tx(tx, false)?,
            None => scope_facts(tx, scope, false)?,
        }
        .is_some(),
        "completion writer identity changed"
    );
    ensure!(
        match material.as_mut() {
            Some(material) => material.coverage_tx(tx)?,
            None => loader::current_coverage(tx, scope)?,
        } == prepared.coverage,
        "completion Work coverage changed inside its original transaction"
    );
    if !actual_result_brief_ids.is_empty() {
        let proof = prepared
            .proof
            .as_ref()
            .context("Result omitted original Work proof")?;
        // The original preproof's raw/native guards remain held in this same
        // transaction; D3 business writes cannot modify the proven Work row.
        for work in proof.admitted_work_ids() {
            proof.validate_admission_tx(tx, &work)?;
        }
    }
    if let (Some(material), Some(coverage)) = (material.as_mut(), prepared.coverage.as_ref()) {
        material.coverage_copy(coverage, 8)?;
    }
    let original = prepared.coverage;
    let base_revision = original.as_ref().map_or(1, |coverage| coverage.revision);
    let mut coverage = original
        .clone()
        .unwrap_or_else(|| writes::empty_coverage(base_revision));
    writes::update_result_heads_with_material_tx(
        tx,
        scope,
        &mut coverage,
        actual_result_brief_ids,
        material.as_mut(),
    )?;
    if original.is_some() {
        coverage
            .latest_results
            .retain(|work, _| original.as_ref().unwrap().latest_results.contains_key(work));
        for (work, head) in &mut coverage.latest_results {
            if let Some(head) = head {
                let next = completion::completion_head_refs_with_material_tx(
                    tx,
                    scope,
                    &head.brief_id,
                    material.as_mut(),
                )?;
                if *head != next {
                    loader::persist_result_head(
                        tx,
                        scope,
                        work,
                        base_revision
                            .checked_add(1)
                            .context("Work revision overflow")?,
                        &Some(next.clone()),
                    )?;
                    *head = next;
                }
            }
        }
        bound_completion_coverage(&coverage, request)?;
        if let Some(material) = material.as_mut() {
            material.coverage_copy(&coverage, 8)?;
        }
    }
    let mut guards = prepared.preparation_guards;
    if let Some(proof) = prepared.proof.take() {
        guards.extend(proof.into_preparation_guards());
    }
    if let Some(material) = material {
        guards.extend(material.into_guards());
    }
    let Some(original) = original else {
        // The finite by-key index is durable, but an unprepared scope cannot
        // acquire a fake empty complete Work domain from an evidence write.
        return Ok((None, guards));
    };
    let changed = coverage != original;
    coverage.revision = if changed {
        base_revision
            .checked_add(1)
            .context("Work revision overflow")?
    } else {
        base_revision
    };
    if changed {
        loader::persist_coverage(tx, scope, &coverage)?;
    }
    Ok((
        Some(CommittedWorkDelta {
            _material_views: (!guards.is_empty()).then(|| Arc::new(guards)),
            scope: scope.clone(),
            base_revision,
            revision: coverage.revision,
            records: Vec::new(),
            removals: BTreeMap::new(),
            coverage,
        }),
        Vec::new(),
    ))
}

fn bound_completion_coverage(
    coverage: &WorkCoverage,
    request: &WorkDomainLoadRequest,
) -> Result<()> {
    let bytes = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
        coverage,
    )?)?;
    bound_metadata(coverage.observations.len(), bytes)?;
    ensure!(
        coverage.observations.len() <= request.record_limit && bytes <= request.byte_limit,
        WorkSnapshotError::ResourceExhausted("completion coverage metadata")
    );
    Ok(())
}
