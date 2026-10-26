//! Live Input selectors retain allocations, never their runtime or published root.
use super::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_input_evidence::{InputEvidenceScope, InputQuerySelection},
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
};

#[derive(Default)]
struct RegistryInner {
    next: AtomicU64,
    slots: Mutex<BTreeMap<u64, Slot>>,
}
struct Slot {
    lease: Weak<LeaseInner>,
    _budget: Arc<BudgetViewGuard>,
}
#[derive(Clone, Default)]
pub(super) struct InputQueryRegistry(Arc<RegistryInner>);

struct LeaseInner {
    id: u64,
    scope: InputEvidenceScope,
    owner_epoch: u64,
    selection: InputQuerySelection,
    registry: Weak<RegistryInner>,
    _budget: Arc<BudgetViewGuard>,
}
impl Drop for LeaseInner {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            // A snapshot can release the last upgraded lease under this lock.
            if let Ok(mut slots) = registry.slots.try_lock() {
                slots.remove(&self.id);
            };
        }
    }
}
pub(crate) struct InputQueryLease(Arc<LeaseInner>);
impl InputQueryLease {
    pub(super) fn selection(&self) -> &InputQuerySelection {
        &self.0.selection
    }
}

/// Held through the original writer transaction, not stored in durable coverage.
pub(crate) struct InputQueryRoles {
    pub(super) selection: InputQuerySelection,
    scope: InputEvidenceScope,
    owner_epoch: u64,
    _leases: Vec<Arc<LeaseInner>>,
    _budget: Arc<BudgetViewGuard>,
    ledger: InputEvidenceBudgetLedger,
}
impl std::fmt::Debug for InputQueryRoles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputQueryRoles")
            .field("selection", &self.selection)
            .field("owner_epoch", &self.owner_epoch)
            .finish_non_exhaustive()
    }
}
impl InputQueryRoles {
    pub(super) fn validate_binding(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == *scope && self.owner_epoch > 0 && self.ledger.same_ledger(ledger),
            "Input query roles scope/ledger differs"
        );
        for lease in &self._leases {
            lease
                ._budget
                .validate_owned_material_binding(scope, ledger, 0)?;
        }
        Ok(())
    }
}

fn account(
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
    bytes: u64,
    count: usize,
    limits: (usize, usize),
) -> Result<Arc<BudgetViewGuard>> {
    let bytes = bytes.checked_mul(8).context("Input roles bytes overflow")?;
    let count = count.checked_mul(8).context("Input roles count overflow")?;
    ensure!(
        bytes <= u64::try_from(limits.1)? && count <= limits.0,
        crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ResourceExhausted(
            "live Input query selectors"
        )
    );
    Ok(ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: bytes,
        },
    )?)
}

fn footprint(scope: &InputEvidenceScope, selection: &InputQuerySelection) -> Result<(u64, usize)> {
    ensure!(
        scope.identity_incarnation > 0,
        "Input query requires an exact incarnation"
    );
    ensure!(
        selection.required_keys.iter().all(|key| !key.id.is_empty()),
        "Input query selector ID is empty"
    );
    let count = selection
        .required_keys
        .len()
        .checked_add(4)
        .context("Input role nodes overflow")?;
    let bytes = serialized_bytes(&(scope, selection))?
        .checked_add(
            u64::try_from(count)?
                .checked_mul(256)
                .context("Input role nodes overflow")?,
        )
        .context("Input role bytes overflow")?;
    Ok((bytes, count))
}

impl InputQueryRegistry {
    pub(super) fn register(
        &self,
        scope: &InputEvidenceScope,
        owner_epoch: u64,
        selection: &InputQuerySelection,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
    ) -> Result<InputQueryLease> {
        ensure!(
            owner_epoch > 0,
            "Input query requires an exact published owner"
        );
        let (bytes, count) = footprint(scope, selection)?;
        let mut slots = self
            .0
            .slots
            .lock()
            .map_err(|_| anyhow::anyhow!("Input query registry poisoned"))?;
        slots.retain(|_, slot| slot.lease.strong_count() > 0);
        let budget = account(scope, ledger, bytes, count, limits)?;
        let id = self
            .0
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| anyhow::anyhow!("Input query lease ID exhausted"))?;
        let lease = Arc::new(LeaseInner {
            id,
            scope: scope.clone(),
            owner_epoch,
            selection: selection.clone(),
            registry: Arc::downgrade(&self.0),
            _budget: budget,
        });
        slots.insert(
            id,
            Slot {
                lease: Arc::downgrade(&lease),
                _budget: lease._budget.clone(),
            },
        );
        Ok(InputQueryLease(lease))
    }

    pub(super) fn validate_lease(
        &self,
        lease: &InputQueryLease,
        scope: &InputEvidenceScope,
        owner_epoch: u64,
    ) -> Result<()> {
        ensure!(
            lease.0.scope == *scope
                && lease.0.owner_epoch == owner_epoch
                && Weak::ptr_eq(&lease.0.registry, &Arc::downgrade(&self.0)),
            "Input query lease belongs to another runtime scope or owner"
        );
        Ok(())
    }

    pub(super) fn snapshot(
        &self,
        scope: &InputEvidenceScope,
        owner_epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
    ) -> Result<InputQueryRoles> {
        ensure!(
            owner_epoch > 0,
            "Input query requires an exact published owner"
        );
        let mut slots = self
            .0
            .slots
            .lock()
            .map_err(|_| anyhow::anyhow!("Input query registry poisoned"))?;
        slots.retain(|_, slot| slot.lease.strong_count() > 0);
        let (mut bytes, mut count) = footprint(scope, &InputQuerySelection::default())?;
        for slot in slots.values() {
            let Some(lease) = slot.lease.upgrade() else {
                continue;
            };
            if lease.scope == *scope && lease.owner_epoch == owner_epoch {
                lease
                    ._budget
                    .validate_owned_material_binding(scope, ledger, 0)?;
                let (extra_bytes, extra_count) = footprint(scope, &lease.selection)?;
                bytes = bytes
                    .checked_add(extra_bytes)
                    .context("Input role union bytes overflow")?;
                count = count
                    .checked_add(extra_count)
                    .context("Input role union count overflow")?;
            }
        }
        slots.retain(|_, slot| slot.lease.strong_count() > 0);
        let budget = account(scope, ledger, bytes, count, limits)?;
        let mut selection = InputQuerySelection::default();
        let mut leases = Vec::new();
        for slot in slots.values() {
            let Some(lease) = slot.lease.upgrade() else {
                continue;
            };
            if lease.scope != *scope || lease.owner_epoch != owner_epoch {
                continue;
            }
            selection
                .required_keys
                .extend(lease.selection.required_keys.iter().cloned());
            if let Some(after) = lease.selection.birth_after {
                selection.birth_after =
                    Some(selection.birth_after.map_or(after, |base| base.min(after)));
            }
            leases.push(lease);
        }
        slots.retain(|_, slot| slot.lease.strong_count() > 0);
        Ok(InputQueryRoles {
            selection,
            scope: scope.clone(),
            owner_epoch,
            _leases: leases,
            _budget: budget,
            ledger: ledger.clone(),
        })
    }
}

#[cfg(test)]
#[path = "input_query_roles_tests.rs"]
mod tests;
