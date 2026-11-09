//! Same-view original identity/State-presence observation, never Task readiness.
use super::*;
use crate::runtime_db::task_snapshot::TaskDomainLoadRequest;

pub(crate) struct OriginalTaskStopOwnerState {
    unavailable: Option<TaskStopPhaseInstallMode>,
    _material: Arc<BudgetViewGuard>,
}
impl OriginalTaskStopOwnerState {
    pub(crate) fn owner_state_unavailable(&self) -> bool {
        matches!(
            self.unavailable,
            Some(TaskStopPhaseInstallMode::NoState | TaskStopPhaseInstallMode::Retired)
        )
    }
}
pub(crate) fn observe_original_task_stop_owner_state_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    _request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<OriginalTaskStopOwnerState> {
    let bytes = phase::task_stop_phase_identity_material_tx(tx, scope)?;
    let bytes = bytes
        .checked_mul(8)
        .context("original Stop identity material overflow")?;

    let material = ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: u64::try_from(bytes)?,
            metadata_estimate_bytes: 0,
        },
    )?;
    let unavailable = phase::observe_original_task_stop_state_tx(tx, scope)?;
    Ok(OriginalTaskStopOwnerState {
        unavailable,
        _material: material,
    })
}
