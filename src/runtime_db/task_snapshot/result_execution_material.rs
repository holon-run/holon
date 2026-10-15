//! A consumed Result decoder retains only its actual ACK/receipt envelope.
use super::*;
use crate::runtime::input_evidence_budget::borrowed_json_material;

pub(in crate::runtime_db) fn retire_consumed_original_result_material<'a>(
    scope:&'a InputEvidenceScope, request:&TaskDomainLoadRequest,
    ledger:&'a InputEvidenceBudgetLedger, usage:&mut TaskMaintenanceMaterial,
    guards:&mut Vec<Arc<BudgetViewGuard>>, selection:std::ops::Range<usize>, selected:TaskMaintenanceMaterial,
    predicate:Option<std::ops::Range<usize>>, predicate_cost:TaskMaintenanceMaterial,
    command:&impl serde::Serialize,
    outcome:&crate::runtime_db::task_result_settlement::TaskResultWriteOutcome,
    actual:&BTreeSet<TaskKey>,
)->Result<ReleasedTaskReadbackMaterial<'a>> {
    ensure!(selection.start<selection.end && selection.end<=guards.len(),"Result selection guard interval changed");
    ensure!(predicate.as_ref().is_none_or(|range|range.start>=selection.end && range.end<=guards.len())
        && (predicate.is_some() || (predicate_cost.bytes==0&&predicate_cost.records==0)),
        "Result predicate guard interval changed");
    let retired=(selected.bytes.checked_add(predicate_cost.bytes).context("Result retired bytes overflow")?,
        selected.records.checked_add(predicate_cost.records).context("Result retired nodes overflow")?);
    let mut observed=0usize;
    for index in selection.clone().chain(predicate.clone().into_iter().flatten()) {
        let guard=&guards[index];
        ensure!(Arc::strong_count(guard)==1,"Result decoder guard has another live holder");
        guard.validate_owned_material_binding(scope,ledger,0)?;
        observed=observed.checked_add(usize::try_from(guard.owned_material_bytes()?)?)
            .context("Result guard bytes overflow")?;
    }
    ensure!(observed==retired.0,"Result decoder guards differ from its admitted cost");
    let frozen=borrowed_json_material(command)?;
    let (bytes,nodes)=borrowed_json_material(&(outcome,actual))?;
    // The actual outcome survives, then receipt Value/encoding and finite effect
    // key copies may coexist. Preserve the original eight-copy owned envelope.
    let nodes=nodes.checked_add(8).and_then(|n|n.checked_mul(8))
        .and_then(|n|n.checked_add(frozen.1)).context("Result retained nodes overflow")?;
    // The prepared command remains a distinct allocation. Only its dead
    // operation/serialization copies exit, never its complete native/body floor.
    let bytes=usize::try_from(bytes)?.checked_add(scope.agent_id.len()).and_then(|n|n.checked_add(1024))
        .and_then(|n|n.checked_mul(8)).and_then(|n|usize::try_from(frozen.0).ok().and_then(|f|n.checked_add(f)))
        .and_then(|n|nodes.checked_mul(256).and_then(|native|n.checked_add(native)))
        .context("Result retained bytes overflow")?;
    let released=(retired.0.saturating_sub(bytes),retired.1.saturating_sub(nodes));
    let additional=(bytes.saturating_sub(retired.0),nodes.saturating_sub(retired.1));
    let next=TaskMaintenanceMaterial {
        bytes:usage.bytes.checked_sub(retired.0).and_then(|n|n.checked_add(bytes)).context("Result phase bytes overflow")?,
        records:usage.records.checked_sub(retired.1).and_then(|n|n.checked_add(nodes)).context("Result phase nodes overflow")?,
    };
    bound(next.records,next.bytes,Some(request))?;
    // Execution already admitted the ACK's first allocation. Any larger future
    // receipt envelope is checked above before its copies; only accounting
    // handles overlap while the completed execution envelope is exchanged.
    let retained=ledger.register_owned_bytes(scope,InputMemoryCategory::Candidate,OwnedBytesEstimate {
        payload_bytes:0,metadata_estimate_bytes:u64::try_from(bytes)?,
    })?;
    if let Some(range)=predicate {drop(guards.drain(range));}
    drop(guards.drain(selection));guards.push(retained);
    *usage=next;
    Ok(ReleasedTaskReadbackMaterial::completed_result_exchange(scope,ledger,released,additional))
}
