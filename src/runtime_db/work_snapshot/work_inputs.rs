//! Named Work preparation installs its finite owning Input closure in the same
//! transaction. It neither acquires an owner nor admits an unqualified body.
use super::*;
use crate::runtime::{
    input_evidence_budget::{borrowed_json_material, InputEvidenceBudgetLedger},
    resident_input_evidence::ResidentInputEvidence,
};
use crate::runtime_db::input_evidence_snapshot::{
    prepare_input_evidence_snapshot_with_material_tx, read_material::InputReadMaterial,
    InputEvidenceBootstrapSnapshot, InputEvidenceLoadRequest,
};

pub(crate) fn prepare_work_with_owning_inputs_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &WorkDomainLoadRequest,
    original_recovery: bool,
    ledger: &InputEvidenceBudgetLedger,
    previous_inputs: Option<&ResidentInputEvidence>,
    owner_prior: (usize, usize),
) -> Result<(WorkCurrentSnapshot, Option<ResidentInputEvidence>)> {
    let mut work_material = read_preparation::WorkReadMaterial::resume(
        scope,
        request,
        ledger,
        None,
        owner_prior,
        (request.byte_limit, request.record_limit),
    )?;
    let mut work = loader::prepare_work_domain_snapshot_with_material_tx(
        tx,
        scope,
        request,
        original_recovery,
        true,
        &mut work_material,
    )?
    .require_current()?;
    if previous_inputs.is_some_and(|inputs| {
        inputs.belongs_to(scope)
            && inputs.covers_work_input_sources(work.resident.coverage(), &work.owning_input_roots)
    }) || (previous_inputs.is_none()
        && work.owning_input_roots.is_empty()
        && work
            .resident
            .coverage()
            .latest_results
            .values()
            .all(Option::is_none))
    {
        work.material_usage = work_material.usage();
        work.material_views = work_material.into_guards();
        return Ok((work, None));
    }
    let empty = InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: BTreeSet::new(),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        record_limit: request.record_limit,
        byte_limit: request.byte_limit,
    };
    let mut material = InputReadMaterial::new(scope, &empty, ledger, work_material.usage())?;
    let mut input = if let Some(previous) = previous_inputs {
        ensure!(
            previous.belongs_to(scope),
            "Work preparation Input scope differs"
        );
        previous.work_owner_preparation_request(
            request.record_limit,
            request.byte_limit,
            |bytes, records| material.observe_projected(bytes, records),
        )?
    } else {
        empty
    };
    // These are additional request containers, not a second charge for their
    // immutable Work/Source allocations. Debit before cloning any key.
    let (bytes, nodes) = borrowed_json_material(&(
        &work.owning_input_roots,
        &work.resident.coverage().latest_results,
    ))?;
    let records = nodes
        .checked_add(16)
        .and_then(|n| n.checked_mul(8))
        .context("Work Input roots count overflow")?;
    let bytes = bytes
        .checked_add(
            (nodes as u64)
                .checked_mul(256)
                .context("Work Input roots slots overflow")?,
        )
        .and_then(|n| n.checked_mul(8))
        .context("Work Input roots bytes overflow")?;
    material.observe_projected(bytes, records)?;
    input
        .mandatory_roots
        .extend(work.owning_input_roots.iter().cloned());
    input.selected_completion_brief_ids.extend(
        work.resident
            .coverage()
            .latest_results
            .values()
            .flatten()
            .map(|head| head.brief_id.clone()),
    );
    let InputEvidenceBootstrapSnapshot::Current(prepared) =
        prepare_input_evidence_snapshot_with_material_tx(
            tx,
            scope,
            &input,
            original_recovery,
            &mut material,
        )?
    else {
        return Err(WorkSnapshotError::RetiredScope.into());
    };
    ensure!(
        prepared.facts.identity == work.facts.identity
            && prepared.facts.state == work.facts.state
            && prepared.facts.control_revision == work.facts.control_revision,
        "Work and owning Input preparation canonical views differ"
    );
    ensure!(
        prepared
            .resident
            .covers_work_input_sources(work.resident.coverage(), &work.owning_input_roots),
        "Work owning Input sources differ from their qualified Work heads"
    );
    work.material_usage = material.usage();
    work.material_views = work_material.into_guards();
    work.material_views.extend(material.into_guards());
    Ok((work, Some(prepared.resident)))
}

/// An actual Input ACK may change an already selected Legacy source. Refresh
/// only those finite heads; no Work body, domain or ordinary window is created.
pub(crate) fn refresh_work_delivery_sources_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    delta: &crate::runtime::resident_input_evidence::CommittedInputEvidenceDelta,
    request: &WorkDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<Option<CommittedWorkDelta>> {
    if !delta
        .records
        .iter()
        .any(|record| record.locator.key.family == InputEvidenceFamily::LegacyDelivery)
        && !delta
            .removals
            .iter()
            .any(|record| record.key.family == InputEvidenceFamily::LegacyDelivery)
    {
        return Ok(None);
    }
    for record in &delta.records {
        if record.locator.key.family == InputEvidenceFamily::LegacyDelivery {
            ensure!(
                &record.locator.scope == scope,
                "Work Delivery record scope differs"
            );
            let crate::runtime::resident_input_evidence::PreparedInputEvidence::LegacyDelivery(
                body,
            ) = &record.record
            else {
                anyhow::bail!("Work Delivery locator family differs from body")
            };
            ensure!(
                body.id == record.locator.key.id && body.agent_id == scope.agent_id,
                "Work Delivery body address differs"
            );
        }
    }
    for removed in &delta.removals {
        if removed.key.family == InputEvidenceFamily::LegacyDelivery {
            ensure!(
                &removed.scope == scope,
                "Work Delivery removal scope differs"
            );
        }
    }
    if let Some(invalidation) = &delta.output_recall_invalidation {
        ensure!(
            invalidation.belongs_to(scope),
            "Work Source denial scope differs"
        );
    }
    let (mut source_bytes, mut source_nodes) = borrowed_json_material(&(
        scope,
        &delta.execution.initialized,
        &delta.execution.sources,
        &delta.execution.open_attempts,
        &delta.execution.nonterminal_work_items,
        &delta.execution.work_items,
        delta.output_recall_invalidation.as_ref().map(|_| scope),
        &delta.turn_transcript_keys,
        &delta.message_turn_keys,
    ))?;
    source_bytes = source_bytes
        .checked_add(
            (source_nodes as u64)
                .checked_mul(256)
                .context("Work Source delta metadata overflow")?,
        )
        .context("Work Source delta bytes overflow")?;
    for removed in &delta.removals {
        ensure!(
            &removed.scope == scope,
            "Work Source removal original scope differs"
        );
        let cost = borrowed_json_material(&(
            &removed.scope,
            &removed.key,
            removed.revision,
            &removed.previous_content_digest,
        ))?;
        source_bytes = source_bytes
            .checked_add(cost.0)
            .and_then(|n| n.checked_add((cost.1 as u64).checked_mul(256)?))
            .context("Work Source removal metadata overflow")?;
        source_nodes = source_nodes
            .checked_add(cost.1)
            .context("Work Source removal nodes overflow")?;
    }
    let slots = delta
        .records
        .capacity()
        .checked_add(delta.removals.capacity())
        .context("Work Source retained slot count overflow")?;
    source_bytes = source_bytes
        .checked_add(u64::try_from(
            slots
                .checked_mul(256)
                .context("Work Source retained slots overflow")?,
        )?)
        .context("Work Source retained bytes overflow")?;
    source_nodes = source_nodes
        .checked_add(slots)
        .context("Work Source retained nodes overflow")?;
    for record in &delta.records {
        ensure!(
            &record.locator.scope == scope,
            "Work Source delta original scope differs"
        );
        let cost = record.resident_material_estimate()?;
        source_bytes = source_bytes
            .checked_add(u64::try_from(cost.0)?)
            .context("Work Source delta body cost overflow")?;
        source_nodes = source_nodes
            .checked_add(cost.1)
            .context("Work Source delta nodes overflow")?;
    }
    let prior = (usize::try_from(source_bytes)?, source_nodes);
    let mut material = read_preparation::WorkReadMaterial::resume(
        scope,
        request,
        ledger,
        memory,
        prior,
        (request.byte_limit, request.record_limit),
    )?;
    // The original delta retains these bodies; this additional actual view is
    // Weak in the ledger and deduplicates the identical physical Arc allocation.
    let source_pins = ledger.register_view(
        scope,
        crate::runtime::input_evidence_budget::InputMemoryCategory::Candidate,
        &delta.records,
    )?;
    if material.scope_tx(tx, true)?.is_none() {
        return Ok(None);
    }
    let Some(previous) = material.coverage_tx(tx)? else {
        return Ok(None);
    };
    material.coverage_copy(&previous, 4)?;
    let mut coverage = previous.clone();
    let changed_id = |id: &str| {
        delta.records.iter().any(|record| {
            record.locator.key.family == InputEvidenceFamily::LegacyDelivery
                && record.locator.key.id == id
        }) || delta.removals.iter().any(|record| {
            record.key.family == InputEvidenceFamily::LegacyDelivery && record.key.id == id
        })
    };
    let mut changed = false;
    for (work_id, heads) in &mut coverage.legacy_delivery_heads {
        let new_source = delta.records.iter().any(|record|
            matches!(&record.record, crate::runtime::resident_input_evidence::PreparedInputEvidence::LegacyDelivery(body)
                if body.work_item_id == *work_id));
        if !new_source && ![&heads.latest_any, &heads.latest_nonempty].into_iter().any(|head|
            matches!(head, crate::runtime::resident_work::WorkDeliveryHead::Present{record_id,..} if changed_id(record_id)))
        { continue }
        use crate::runtime::resident_input_evidence::LegacyDeliveryHeadSelector as Selector;
        use crate::runtime::resident_work::{WorkDeliveryHead, WorkDeliveryHeads};
        let any =
            crate::runtime_db::input_evidence_snapshot::legacy_delivery::heads::observe_head_tx(
                tx,
                scope,
                work_id,
                Selector::LatestAny,
                |b, r| material.observe_projected(b, r),
            )?;
        let nonempty =
            crate::runtime_db::input_evidence_snapshot::legacy_delivery::heads::observe_head_tx(
                tx,
                scope,
                work_id,
                Selector::LatestNonempty,
                |b, r| material.observe_projected(b, r),
            )?;
        let next = WorkDeliveryHeads {
            latest_any: WorkDeliveryHead::from_observation(scope, any)?,
            latest_nonempty: WorkDeliveryHead::from_observation(scope, nonempty)?,
        };
        changed |= *heads != next;
        *heads = next;
    }
    if !changed {
        return Ok(None);
    }
    coverage.revision = previous
        .revision
        .checked_add(1)
        .context("Work Delivery revision overflow")?;
    material.coverage_copy(&coverage, 4)?;
    loader::persist_coverage(tx, scope, &coverage)?;
    let mut guards = material.into_guards();
    guards.push(source_pins);
    Ok(Some(CommittedWorkDelta {
        _material_views: Some(Arc::new(guards)),
        scope: scope.clone(),
        base_revision: previous.revision,
        revision: coverage.revision,
        records: Vec::new(),
        removals: BTreeMap::new(),
        coverage,
    }))
}

/// Original bootstrap already owns the Work snapshot and request. This narrow
/// wrapper keeps the Input material private to the actual SQL preparation.
pub(crate) fn prepare_work_owning_input_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &InputEvidenceLoadRequest,
    original_recovery: bool,
    work: &WorkCurrentSnapshot,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(
    InputEvidenceBootstrapSnapshot,
    Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
)> {
    prepare_work_owning_input_snapshot_inner_tx(
        tx,
        scope,
        request,
        original_recovery,
        work,
        ledger,
        None,
    )
}
/// The actual bootstrap reservation initializes only its accepted native cursor.
/// No live birth interval or historical body is selected by this named cause.
pub(crate) fn prepare_bootstrap_work_owning_input_snapshot_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &InputEvidenceLoadRequest,
    work: &WorkCurrentSnapshot,
    ledger: &InputEvidenceBudgetLedger,
    owner_epoch: u64,
) -> Result<(
    InputEvidenceBootstrapSnapshot,
    Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
)> {
    ensure!(
        owner_epoch > 0,
        "Input bootstrap requires its actual reserved owner epoch"
    );
    prepare_work_owning_input_snapshot_inner_tx(
        tx,
        scope,
        request,
        false,
        work,
        ledger,
        Some(owner_epoch),
    )
}
fn prepare_work_owning_input_snapshot_inner_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    request: &InputEvidenceLoadRequest,
    original_recovery: bool,
    work: &WorkCurrentSnapshot,
    ledger: &InputEvidenceBudgetLedger,
    bootstrap_epoch: Option<u64>,
) -> Result<(
    InputEvidenceBootstrapSnapshot,
    Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
)> {
    let mut material = InputReadMaterial::new(scope, request, ledger, (0, 0))?;
    material.observe_work_prior(work)?;
    let prepared = match bootstrap_epoch {
        Some(epoch)=>crate::runtime_db::input_evidence_snapshot::prepare_input_evidence_snapshot_with_query_material_tx(
            tx,scope,request,original_recovery,&mut material,None,epoch)?,
        None=>prepare_input_evidence_snapshot_with_material_tx(
            tx,scope,request,original_recovery,&mut material)?,
    };
    Ok((prepared, material.into_guards()))
}

/// Complete this original Work ACK's Input closure before COMMIT. The caller
/// retained its original prepared request and the actual Work material guards.
pub(in crate::runtime_db) fn prepare_committed_work_inputs_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    original: &InputEvidenceLoadRequest,
    work: &CommittedWorkDelta,
    owning_roots: &BTreeSet<InputEvidenceKey>,
    work_usage: (usize, usize),
    request: &WorkDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
    memory: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
    result: Option<(
        &Arc<crate::runtime::WorkItemMutationMaterial>,
        &crate::runtime_db::input_mutation_receipts::PreparedInputMutation,
    )>,
) -> Result<(
    ResidentInputEvidence,
    Option<crate::runtime::OriginalWorkReportHandoff>,
    Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
)> {
    ensure!(work.scope == *scope, "Work ACK Input scope differs");
    if let Some(guards) = work._material_views.as_ref() {
        ensure!(
            guards
                .iter()
                .all(|guard| guard.matches_resource_ledger(ledger)),
            "Work ACK Input material ledger differs"
        );
    }
    let limits = InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: BTreeSet::new(),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        record_limit: request.record_limit.min(65_536),
        byte_limit: request.byte_limit.min(64 * 1024 * 1024),
    };
    let mut material =
        InputReadMaterial::new_with_memory(scope, &limits, ledger, work_usage, memory)?;
    // A second request allocation is distinct from the original held selector.
    material.selectors(original)?;
    let mut input = original.clone();
    input.record_limit = limits.record_limit;
    input.byte_limit = limits.byte_limit;
    let (bytes, nodes) = borrowed_json_material(&(owning_roots, &work.coverage.latest_results))?;
    material.observe_projected(
        bytes
            .checked_add(
                (nodes as u64)
                    .checked_mul(256)
                    .context("Work ACK Input native slots overflow")?,
            )
            .and_then(|n| n.checked_mul(8))
            .context("Work ACK Input roots bytes overflow")?,
        nodes
            .checked_add(16)
            .and_then(|n| n.checked_mul(8))
            .context("Work ACK Input roots count overflow")?,
    )?;
    input.mandatory_roots.extend(owning_roots.iter().cloned());
    input.selected_completion_brief_ids.extend(
        work.coverage
            .latest_results
            .values()
            .flatten()
            .map(|head| head.brief_id.clone()),
    );
    let InputEvidenceBootstrapSnapshot::Current(prepared) =
        prepare_input_evidence_snapshot_with_material_tx(tx, scope, &input, true, &mut material)?
    else {
        return Err(WorkSnapshotError::RetiredScope.into());
    };
    ensure!(
        prepared
            .resident
            .covers_work_input_sources(&work.coverage, owning_roots),
        "original Work ACK Input sources differ from actual qualified heads"
    );
    // The same loader admitted metadata/Arc containers before decoding; these
    // views retain and classify those actual allocations through publication.
    let resident = prepared.resident.with_budget(ledger.clone())?;
    let report = if let Some((result, attempt)) = result {
        let target = result.report_target_id().and_then(|id| {
            work.records.iter().find(|target|
            matches!(&target.record,crate::runtime::resident_work::CanonicalWorkRecord::Work(record)
                if record.id==id&&record.state==crate::types::WorkItemState::Completed))
        });
        if let Some(target) = target {
            Some(result.prepare_original_report_handoff(
                target,
                &work.coverage.legacy_delivery_heads,
                &resident,
                attempt,
                |bytes, records| material.observe_retained_projection(bytes, records),
            )?)
        } else {
            None
        }
    } else {
        None
    };
    Ok((resident, report, material.into_guards()))
}
