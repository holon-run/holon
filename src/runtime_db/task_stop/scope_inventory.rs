//! Startup discovery of original scopes; no State, Task or signal permission.
use super::*;
use crate::runtime_db::task_snapshot::{TaskDomainLoadRequest, TaskSnapshotError};

pub(crate) struct PendingTaskStopScopeInventory {
    scopes: Vec<(InputEvidenceScope, usize)>,
    _pending_effect_count: usize,
    _material: Arc<BudgetViewGuard>,
}
impl PendingTaskStopScopeInventory {
    pub(crate) fn scopes(&self) -> impl ExactSizeIterator<Item = (&InputEvidenceScope, usize)> {
        self.scopes.iter().map(|(scope, count)| (scope, *count))
    }
    #[cfg(test)]
    pub(crate) fn pending_effect_count(&self) -> usize {
        self._pending_effect_count
    }
}

/// Accounting scope belongs to the caller's real actor; it is only the ledger
/// address for this global metadata allocation, never a business authorization.
/// Bodies are acquired later under each original scope's actual reservation.
#[cfg(test)]
pub(crate) fn pending_task_stop_scope_inventory_tx(
    tx: &Transaction<'_>,
    accounting_scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<PendingTaskStopScopeInventory> {
    pending_scope_inventory_inner_tx(tx, Some(accounting_scope), request, ledger)
}

/// Host metadata has no agent identity or incarnation authority.
pub(crate) fn pending_task_stop_host_scope_inventory_tx(
    tx: &Transaction<'_>,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<PendingTaskStopScopeInventory> {
    pending_scope_inventory_inner_tx(tx, None, request, ledger)
}
fn pending_scope_inventory_inner_tx(
    tx: &Transaction<'_>,
    accounting_scope: Option<&InputEvidenceScope>,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<PendingTaskStopScopeInventory> {
    let (scope_count,bytes,effects):(usize,usize,usize)=tx.query_row(&format!(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(agent_id AS BLOB))+64),0),COALESCE(SUM(effect_count),0) FROM (SELECT agent_id,identity_incarnation,COUNT(*) AS effect_count FROM task_stop_effects WHERE {STOP_PENDING} GROUP BY agent_id,identity_incarnation)"),
        [],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let exhausted = || TaskSnapshotError::ResourceExhausted("Stop startup scope inventory");
    // SQL integers precede every String, scope clone and Vec allocation. Four
    // copies cover row tuple, DTO, reservation selector and sorted group metadata.
    let records = scope_count.checked_mul(4).context(exhausted())?;
    let bytes = bytes
        .checked_add(scope_count.checked_mul(256).context(exhausted())?)
        .and_then(|n| n.checked_mul(4))
        .context(exhausted())?;
    ensure!(
        records <= request.record_limit.min(MAX_RECORDS)
            && bytes <= request.byte_limit.min(MAX_BYTES),
        exhausted()
    );
    let estimate = OwnedBytesEstimate {
        payload_bytes: u64::try_from(bytes)?,
        metadata_estimate_bytes: 0,
    };
    let material = match accounting_scope {
        Some(scope) => {
            ledger.register_owned_bytes(scope, InputMemoryCategory::Candidate, estimate)?
        }
        None => ledger.register_host_owned_bytes(InputMemoryCategory::Candidate, estimate)?,
    };
    let mut statement=tx.prepare(&format!(
        "SELECT agent_id,identity_incarnation,COUNT(*) FROM task_stop_effects WHERE {STOP_PENDING} GROUP BY agent_id,identity_incarnation ORDER BY agent_id,identity_incarnation LIMIT ?1"))?;
    let rows = statement.query_map([i64::try_from(scope_count)?], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u64>(1)?,
            r.get::<_, usize>(2)?,
        ))
    })?;
    let mut scopes = Vec::with_capacity(scope_count);
    let mut actual_effects = 0usize;
    for row in rows {
        let (agent_id, identity_incarnation, count) = row?;
        ensure!(
            !agent_id.is_empty() && identity_incarnation > 0 && count > 0,
            "Stop scope inventory projection corrupt"
        );
        actual_effects = actual_effects.checked_add(count).context(exhausted())?;
        scopes.push((
            InputEvidenceScope {
                agent_id,
                identity_incarnation,
            },
            count,
        ));
    }
    ensure!(
        scopes.len() == scope_count && actual_effects == effects,
        "Stop scope inventory changed in read view"
    );
    Ok(PendingTaskStopScopeInventory {
        scopes,
        _pending_effect_count: effects,
        _material: material,
    })
}
