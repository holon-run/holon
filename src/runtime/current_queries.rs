//! Weak live-role registry; frozen roles retain only selectors and accounting.
use super::input_evidence_budget::{serialized_bytes, BudgetViewGuard, InputMemoryCategory};
use super::resident_input_evidence::InputEvidenceScope;
use super::*;
use crate::runtime_db::current_query::{CurrentQuerySource, CurrentSelection};
use std::sync::{Mutex as StdMutex, Weak};

#[derive(Default)]
pub(super) struct CurrentQueryRegistry(Arc<StdMutex<Vec<Weak<CurrentLeaseInner>>>>);

struct CurrentLeaseInner {
    scope: InputEvidenceScope,
    epoch: u64,
    selection: Arc<CurrentSelection>,
    registry: Weak<StdMutex<Vec<Weak<CurrentLeaseInner>>>>,
    _budget: Arc<BudgetViewGuard>,
}

#[cfg(test)]
#[path = "current_queries_tests.rs"]
mod tests;

pub(crate) struct CurrentQueryLease(Arc<CurrentLeaseInner>);

impl CurrentQueryLease {
    pub(super) fn selection(&self) -> &CurrentSelection {
        &self.0.selection
    }
}

pub(crate) struct CurrentRoles {
    scope: InputEvidenceScope,
    epoch: u64,
    selection: Arc<CurrentSelection>,
    leases: Vec<Arc<CurrentLeaseInner>>,
    ledger: super::input_evidence_budget::InputEvidenceBudgetLedger,
    _budget: Arc<BudgetViewGuard>,
}

impl std::fmt::Debug for CurrentRoles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CurrentRoles")
            .field("scope", &self.scope)
            .field("epoch", &self.epoch)
            .field("selection", &self.selection)
            .finish()
    }
}

impl CurrentRoles {
    pub(super) fn has_live_roles(&self) -> bool {
        !self.leases.is_empty()
    }

    pub(crate) fn selection(&self) -> &Arc<CurrentSelection> {
        &self.selection
    }

    pub(crate) fn ledger(&self) -> &super::input_evidence_budget::InputEvidenceBudgetLedger {
        &self.ledger
    }
    pub(crate) fn validate(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        selection: &CurrentSelection,
    ) -> Result<()> {
        anyhow::ensure!(
            self.scope == *scope
                && self.epoch == epoch
                && epoch > 0
                && self.selection.as_ref() == selection,
            "Current roles scope, owner or selection differs"
        );
        self._budget
            .validate_owned_material_binding(scope, &self.ledger, 0)?;
        for lease in &self.leases {
            anyhow::ensure!(
                lease.scope == *scope && lease.epoch == epoch,
                "Current live role binding differs"
            );
            lease
                ._budget
                .validate_owned_material_binding(scope, &self.ledger, 0)?;
        }
        Ok(())
    }
}

impl CurrentQueryRegistry {
    fn account(
        scope: &InputEvidenceScope,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
        bytes: usize,
        count: usize,
    ) -> Result<Arc<BudgetViewGuard>> {
        let limits = ledger.limits();
        anyhow::ensure!(
            u64::try_from(bytes)?
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Current roles budget overflow")?
                && u64::try_from(count)? <= limits.record_limit,
            "Current roles resource exhausted"
        );
        Ok(ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(bytes)?,
            },
        )?)
    }

    pub(super) fn register(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        selection: CurrentSelection,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<CurrentQueryLease> {
        anyhow::ensure!(epoch > 0, "Current role requires a published owner");
        let bytes = usize::try_from(serialized_bytes(&selection)?)?
            .checked_add(1024)
            .context("Current lease size overflow")?;
        let guard = Self::account(scope, ledger, bytes, selection.queue_ids.len() + 8)?;
        let lease = Arc::new(CurrentLeaseInner {
            scope: scope.clone(),
            epoch,
            selection: Arc::new(selection),
            registry: Arc::downgrade(&self.0),
            _budget: guard,
        });
        let mut slots = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Current roles poisoned"))?;
        slots.retain(|slot| slot.strong_count() > 0);
        slots.push(Arc::downgrade(&lease));
        Ok(CurrentQueryLease(lease))
    }

    pub(super) fn validate(
        &self,
        lease: &CurrentQueryLease,
        scope: &InputEvidenceScope,
        epoch: u64,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<()> {
        anyhow::ensure!(
            lease.0.scope == *scope
                && lease.0.epoch == epoch
                && Weak::ptr_eq(&lease.0.registry, &Arc::downgrade(&self.0)),
            "Current lease belongs to a foreign runtime"
        );
        lease
            .0
            ._budget
            .validate_owned_material_binding(scope, ledger, 0)?;
        Ok(())
    }

    pub(super) fn snapshot(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        extra: Option<&CurrentSelection>,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<CurrentRoles> {
        anyhow::ensure!(epoch > 0, "Current roles require a published owner");
        let mut slots = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Current roles poisoned"))?;
        slots.retain(|slot| slot.strong_count() > 0);
        let mut bytes = 1024usize;
        let mut count = 8usize;
        if let Some(extra) = extra {
            bytes = bytes
                .checked_add(usize::try_from(serialized_bytes(extra)?)? + 1024)
                .context("Current union bytes overflow")?;
            count = count
                .checked_add(extra.queue_ids.len() + 8)
                .context("Current union count overflow")?;
        }
        for lease in slots
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|lease| lease.scope == *scope && lease.epoch == epoch)
        {
            lease
                ._budget
                .validate_owned_material_binding(scope, ledger, 0)?;
            bytes = bytes
                .checked_add(usize::try_from(serialized_bytes(lease.selection.as_ref())?)? + 1024)
                .context("Current union bytes overflow")?;
            count = count
                .checked_add(lease.selection.queue_ids.len() + 8)
                .context("Current union count overflow")?;
        }
        let guard = Self::account(scope, ledger, bytes, count)?;
        let leases = slots
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|lease| lease.scope == *scope && lease.epoch == epoch)
            .collect::<Vec<_>>();
        let mut selection = CurrentSelection::default();
        if let Some(extra) = extra {
            selection.queue_ids.extend(extra.queue_ids.iter().cloned());
            selection.events_after = extra.events_after;
        }
        for lease in &leases {
            selection
                .queue_ids
                .extend(lease.selection.queue_ids.iter().cloned());
            if let Some(after) = lease.selection.events_after {
                selection.events_after =
                    Some(selection.events_after.map_or(after, |old| old.min(after)));
            }
        }
        Ok(CurrentRoles {
            scope: scope.clone(),
            epoch,
            selection: Arc::new(selection),
            leases,
            ledger: ledger.clone(),
            _budget: guard,
        })
    }

    pub(super) fn source(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        baseline: Option<&Arc<crate::runtime_db::current_query::CurrentQuery>>,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<Option<Arc<CurrentQuerySource>>> {
        let mut roles = self.snapshot(scope, epoch, None, ledger)?;
        let Some(baseline) = baseline else {
            anyhow::ensure!(
                roles.leases.is_empty(),
                "Current active roles need preparation"
            );
            return Ok(None);
        };
        if roles.selection.events_after.is_none() {
            Arc::make_mut(&mut roles.selection).events_after = Some(baseline.event_cursor);
        }
        baseline.validate_binding(scope, ledger)?;
        anyhow::ensure!(
            baseline.selection.covers(&roles.selection),
            "Current active union needs preparation"
        );
        Ok(Some(Arc::new(CurrentQuerySource {
            baseline: baseline.clone(),
            selection: roles.selection.clone(),
            owner_epoch: epoch,
            roles,
        })))
    }
}
