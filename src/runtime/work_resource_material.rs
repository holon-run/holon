//! Borrow actual Resource allocations into one original Work SQL allowance.
use super::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger};
use super::resident_input_evidence::InputEvidenceScope;
use super::*;

/// Only the original owner may freeze its already prepared selectors. This
/// owns bounded request metadata and its guard, never a Root or Input body map.
#[derive(Debug)]
struct OriginalWorkInputSelection {
    scope: InputEvidenceScope,
    ledger: InputEvidenceBudgetLedger,
    request: crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest,
    guard: Arc<BudgetViewGuard>,
    cost: (u64, usize),
}
enum OriginalWorkResource {
    WithInputs {
        original: Option<Box<WorkResourceMaterialLoan>>,
        selection: OriginalWorkInputSelection,
    },
    WithMetadata {
        original: Option<Box<WorkResourceMaterialLoan>>,
        metadata: Arc<super::work_reads::WorkQueryMetadataMaterial>,
    },
    Plan(Arc<resource_refresh::ObservedWorkPlanRead>),
    Result(Arc<WorkItemMutationMaterial>),
    CreatedFile {
        guard: Arc<BudgetViewGuard>,
        cost: (u64, usize),
    },
}
/// Accounting only. All original Work/State/source validators still run.
pub(crate) struct WorkResourceMaterialLoan(OriginalWorkResource);
impl std::fmt::Debug for WorkResourceMaterialLoan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkResourceMaterialLoan")
            .finish_non_exhaustive()
    }
}
impl WorkResourceMaterialLoan {
    pub(crate) fn with_input_selection(
        original: Option<Self>,
        inputs: &super::resident_input_evidence::ResidentInputEvidence,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
    ) -> Result<Self> {
        anyhow::ensure!(
            inputs.belongs_to(scope) && inputs.shares_query_ledger(ledger),
            "original Work Input selection scope/ledger differs"
        );
        let prior = original
            .as_ref()
            .map(|loan| loan.validate_accounting(scope, ledger))
            .transpose()?
            .unwrap_or((0, 0));
        let limits = (limits.0.min(64 * 1024 * 1024), limits.1.min(65_536));
        let mut observed = None;
        let request =
            inputs.work_owner_preparation_request(limits.1, limits.0, |bytes, records| {
                anyhow::ensure!(
                    prior
                        .0
                        .checked_add(usize::try_from(bytes)?)
                        .is_some_and(|n| n <= limits.0)
                        && prior.1.checked_add(records).is_some_and(|n| n <= limits.1),
                    super::resident_work::WorkReadError::ResourceExhausted(
                        "Work original Input selection"
                    )
                );
                let guard = ledger.register_owned_bytes(
                    scope,
                    super::input_evidence_budget::InputMemoryCategory::Candidate,
                    super::input_evidence_budget::OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: bytes,
                    },
                )?;
                observed = Some((guard, (bytes, records)));
                Ok(())
            })?;
        let (guard, cost) =
            observed.context("original Work Input selector omitted actual material")?;
        Ok(Self(OriginalWorkResource::WithInputs {
            original: original.map(Box::new),
            selection: OriginalWorkInputSelection {
                scope: scope.clone(),
                ledger: ledger.clone(),
                request,
                guard,
                cost,
            },
        }))
    }
    pub(crate) fn original_input_request(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Option<&crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest>> {
        match &self.0 {
            OriginalWorkResource::WithInputs { selection, .. } => {
                anyhow::ensure!(
                    &selection.scope == scope && selection.ledger.same_ledger(ledger),
                    "original Work Input request scope/ledger differs"
                );
                selection
                    .guard
                    .validate_owned_material_binding(scope, ledger, selection.cost.0)?;
                Ok(Some(&selection.request))
            }
            OriginalWorkResource::WithMetadata { original, .. } => original
                .as_ref()
                .map(|loan| loan.original_input_request(scope, ledger))
                .transpose()
                .map(Option::flatten),
            _ => Ok(None),
        }
    }
    pub(crate) fn original_result_material(&self) -> Option<&Arc<WorkItemMutationMaterial>> {
        match &self.0 {
            OriginalWorkResource::Result(result) => Some(result),
            OriginalWorkResource::WithInputs { original, .. }
            | OriginalWorkResource::WithMetadata { original, .. } => original
                .as_ref()
                .and_then(|loan| loan.original_result_material()),
            _ => None,
        }
    }
    /// Only the central Task owner's single preserved Input request.
    pub(crate) fn is_original_input_selection_only(&self) -> bool {
        matches!(&self.0, OriginalWorkResource::WithInputs { original: None, .. })
    }

    pub(crate) fn has_original_resource(&self) -> bool {
        match &self.0 {
            OriginalWorkResource::WithInputs { original, .. }
            | OriginalWorkResource::WithMetadata { original, .. } => original
                .as_ref()
                .is_some_and(|loan| loan.has_original_resource()),
            _ => true,
        }
    }

    pub(in crate::runtime) fn plan(read: Arc<resource_refresh::ObservedWorkPlanRead>) -> Self {
        Self(OriginalWorkResource::Plan(read))
    }
    pub(in crate::runtime) fn result(read: Arc<WorkItemMutationMaterial>) -> Self {
        Self(OriginalWorkResource::Result(read))
    }
    pub(in crate::runtime) fn created_file(
        guard: Arc<BudgetViewGuard>,
        cost: (u64, usize),
    ) -> Self {
        Self(OriginalWorkResource::CreatedFile { guard, cost })
    }
    pub(in crate::runtime) fn with_wait_metadata(
        original: Option<Self>,
        metadata: Arc<super::work_reads::WorkQueryMetadataMaterial>,
    ) -> Self {
        Self(OriginalWorkResource::WithMetadata {
            original: original.map(Box::new),
            metadata,
        })
    }
    pub(crate) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        let cost = match &self.0 {
            OriginalWorkResource::WithInputs {
                original,
                selection,
            } => {
                self.original_input_request(scope, ledger)?;
                let prior = original
                    .as_ref()
                    .map(|loan| loan.validate_accounting(scope, ledger))
                    .transpose()?
                    .unwrap_or((0, 0));
                return Ok((
                    prior
                        .0
                        .checked_add(usize::try_from(selection.cost.0)?)
                        .context("Work Input combined bytes overflow")?,
                    prior
                        .1
                        .checked_add(selection.cost.1)
                        .context("Work Input combined records overflow")?,
                ));
            }
            OriginalWorkResource::WithMetadata { original, metadata } => {
                let added = metadata.validate_accounting(scope, ledger)?;
                let original = original
                    .as_ref()
                    .map(|loan| loan.validate_accounting(scope, ledger))
                    .transpose()?
                    .unwrap_or((0, 0));
                return Ok((
                    original
                        .0
                        .checked_add(added.0)
                        .context("Wait loan bytes overflow")?,
                    original
                        .1
                        .checked_add(added.1)
                        .context("Wait loan records overflow")?,
                ));
            }
            OriginalWorkResource::Plan(read) => {
                read.validate_material_binding(scope, ledger)?;
                read.material_estimate()?
            }
            OriginalWorkResource::Result(read) => {
                read.validate_material_binding(scope, ledger)?;
                read.material_estimate()
            }
            OriginalWorkResource::CreatedFile { guard, cost } => {
                guard.validate_owned_material_binding(scope, ledger, cost.0)?;
                *cost
            }
        };
        Ok((usize::try_from(cost.0)?, cost.1))
    }
}

#[cfg(test)]
#[path = "work_resource_material_tests.rs"]
mod tests;
