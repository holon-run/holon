//! Standalone D3 evidence refreshes completion metadata in its original TX.
//! This never expands Work coverage or loads unrelated historical Work bodies.
use super::*;

pub(crate) struct PreparedWorkCompletionWrite {
    scope: InputEvidenceScope,
    coverage: Option<WorkCoverage>,
    proof: Option<WorkWriteProof>,
}

pub(crate) fn prepare_work_completion_write_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    result: Option<&BriefRecord>,
    request: &WorkDomainLoadRequest,
) -> Result<PreparedWorkCompletionWrite> {
    ensure!(
        scope_facts(tx, scope, false)?.is_some(),
        "completion writer scope retired"
    );
    let coverage = loader::current_coverage(tx, scope)?;
    if let Some(coverage) = &coverage {
        bound_completion_coverage(coverage, request)?;
    }
    let proof = if let Some(work) = result
        .filter(|brief| brief.kind == BriefKind::Result && !brief.text.is_empty())
        .and_then(|brief| brief.work_item_id.as_deref())
    {
        let proof = prepare_work_write_tx(
            tx,
            scope,
            &BTreeSet::from([WorkKey::new(WorkFamily::Work, work)]),
        )?;
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
    })
}

/// Input qualification/pruning has already completed. Only exact current head
/// references change; all Work bodies and historical coverage stay untouched.
pub(crate) fn finish_work_completion_write_tx(
    tx: &Transaction<'_>,
    prepared: PreparedWorkCompletionWrite,
    actual_result_brief_ids: &BTreeSet<String>,
    request: &WorkDomainLoadRequest,
) -> Result<Option<CommittedWorkDelta>> {
    let scope = &prepared.scope;
    ensure!(
        scope_facts(tx, scope, false)?.is_some(),
        "completion writer identity changed"
    );
    ensure!(
        loader::current_coverage(tx, scope)? == prepared.coverage,
        "completion Work coverage changed inside its original transaction"
    );
    if !actual_result_brief_ids.is_empty() {
        let proof = prepared
            .proof
            .as_ref()
            .context("Result omitted original Work proof")?;
        for work in proof.admitted_work_ids() {
            proof.validate_admission_tx(tx, &work)?;
        }
    }
    let original = prepared.coverage;
    let base_revision = original.as_ref().map_or(1, |coverage| coverage.revision);
    let mut coverage = original
        .clone()
        .unwrap_or_else(|| writes::empty_coverage(base_revision));
    writes::update_result_heads(tx, scope, &mut coverage, actual_result_brief_ids)?;
    let Some(original) = original else {
        // The finite by-key index is durable, but an unprepared scope cannot
        // acquire a fake empty complete Work domain from an evidence write.
        return Ok(None);
    };
    coverage
        .latest_results
        .retain(|work, _| original.latest_results.contains_key(work));
    for (work, head) in &mut coverage.latest_results {
        if let Some(head) = head {
            let next = completion::completion_head_refs_tx(tx, scope, &head.brief_id)?;
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
    Ok(Some(CommittedWorkDelta {
        scope: scope.clone(),
        base_revision,
        revision: coverage.revision,
        records: Vec::new(),
        removals: BTreeMap::new(),
        coverage,
    }))
}

fn bound_completion_coverage(
    coverage: &WorkCoverage,
    request: &WorkDomainLoadRequest,
) -> Result<()> {
    let bytes = serde_json::to_vec(coverage)?.len();
    bound_metadata(coverage.observations.len(), bytes)?;
    ensure!(
        coverage.observations.len() <= request.record_limit && bytes <= request.byte_limit,
        WorkSnapshotError::ResourceExhausted("completion coverage metadata")
    );
    Ok(())
}
