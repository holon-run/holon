//! Per-attempt Task preparation, separate from the frozen command and nonce.
use super::*;
use crate::runtime::{
    input_evidence_budget::{InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate},
    resident_task::ResidentTaskDomain,
};
use crate::runtime_db::task_snapshot::{self as tasks, TaskWriteBaselineStale};
use std::sync::Arc;

#[cfg(test)]
type ReaderBarrier = Box<dyn FnOnce() -> Result<()>>;

#[cfg(test)]
thread_local! {
    static READER_BARRIER: std::cell::RefCell<Option<ReaderBarrier>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn with_reader_barrier<T>(
    barrier: impl FnOnce() -> Result<()> + 'static,
    run: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<ReaderBarrier>);
    impl Drop for Restore {
        fn drop(&mut self) {
            READER_BARRIER.with(|slot| slot.replace(self.0.take()));
        }
    }
    let _restore = Restore(READER_BARRIER.with(|slot| slot.replace(Some(Box::new(barrier)))));
    run()
}

#[derive(Debug, Default)]
pub(crate) struct PreparedTaskBaselines {
    entries: Vec<PreparedTaskBaseline>,
    // Borrowed body Arcs need no new body charge. Delay coverage encoding until
    // typed preflight: exact replay / NoState / Retired must win over admission.
    roots: Vec<Arc<ResidentTaskDomain>>,
}

impl PreparedTaskBaselines {
    pub(crate) fn from_reservations(
        reservations: &[crate::runtime::mutation_owner::MutationReservation],
        _ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let mut residents = Vec::new();
        for reservation in reservations {
            if let Some(root) = reservation.capture_ready_snapshot()? {
                if let Some(tasks) = &root.tasks {
                    residents.push(Arc::clone(tasks));
                }
            }
        }
        let mut bases = Self::default();
        bases.defer_roots(residents);
        Ok(bases)
    }

    /// Root borrowing must not charge or encode before the exact receipt lookup.
    pub(crate) fn defer_roots(&mut self, residents: Vec<Arc<ResidentTaskDomain>>) {
        for resident in residents {
            if self.entries.iter().any(|entry| {
                entry.resident.scope() == resident.scope()
                    && Arc::ptr_eq(&entry.resident, &resident)
            }) {
                continue;
            }
            self.entries
                .retain(|entry| entry.resident.scope() != resident.scope());
            self.roots.retain(|root| root.scope() != resident.scope());
            self.roots.push(resident);
        }
    }

    #[cfg(test)]
    pub(crate) fn new(
        residents: Vec<Arc<ResidentTaskDomain>>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        Ok(Self {
            entries: residents
                .into_iter()
                .map(|resident| PreparedTaskBaseline::new(resident, ledger))
                .collect::<Result<_>>()?,
            roots: Vec::new(),
        })
    }

    pub(super) fn task_base(&self, scope: &InputEvidenceScope) -> Option<&PreparedTaskBaseline> {
        self.entries
            .iter()
            .find(|entry| entry.resident.scope() == scope)
    }

    /// Only installed roots may be retained without publishing a Task ACK.
    pub(super) fn resident(&self, scope: &InputEvidenceScope) -> Option<&Arc<ResidentTaskDomain>> {
        self.task_base(scope)
            .filter(|base| !base.unpublished)
            .map(|base| &base.resident)
            .or_else(|| self.roots.iter().find(|root| root.scope() == scope))
    }

    pub(crate) fn has_prepared_scope(&self, scope: &InputEvidenceScope) -> bool {
        self.task_base(scope).is_some()
    }

    /// Only a typed, already-authorized preflight miss reaches this reader.
    /// Identity precedes State so a retired producer cannot decode its successor.
    pub(crate) fn reprepare(
        &mut self,
        db: &RuntimeDb,
        stale: &TaskWriteBaselineStale,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        let scope = &stale.scope;
        if let Some(index) = self
            .roots
            .iter()
            .position(|resident| resident.scope() == scope)
        {
            let resident = self.roots.remove(index);
            self.entries
                .push(PreparedTaskBaseline::new(resident, ledger)?);
            return Ok(());
        }
        let _stage = attribution::TASK_DOMAIN_PREPARE.start();
        for attempt in 0..3 {
            let candidate = {
                let _reader = tracing::trace_span!("prepared_task_baseline_reader").entered();
                db.read_for_preparation(|tx| {
                let reader =
                    crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope.agent_id);
                let identity = reader.identity_fact()?;
                if !identity.as_ref().is_some_and(|identity| {
                    identity.incarnation == scope.identity_incarnation
                        && matches!(
                            identity.status,
                            crate::types::AgentRegistryStatus::Active
                                | crate::types::AgentRegistryStatus::Deleting
                        )
                }) {
                    // The original writer owns its exact sparse/retired ACK.
                    return Ok(None);
                }
                let bytes: u64 = tx.query_row(
                    "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1),0)",
                    [&scope.agent_id],
                    |row| row.get(0),
                )?;
                let state_guard = ledger.register_owned_bytes(
                    scope,
                    InputMemoryCategory::Candidate,
                    OwnedBytesEstimate {
                        payload_bytes: bytes.checked_mul(8).context("Task preparation State bytes overflow")?,
                        metadata_estimate_bytes: 256,
                    },
                )?;
                let facts = reader.scope_facts()?;
                let Some(state) = facts.state else {
                    return Ok(None);
                };
                #[cfg(test)]
                if let Some(barrier) = READER_BARRIER.with(|slot| slot.borrow_mut().take()) {
                    barrier()?;
                }
                let candidate = tasks::prepare_task_candidate_tx(
                    tx,
                    scope,
                    &state,
                    &stale.request,
                    ledger,
                )?
                .context("Task preparation requires an initialized original domain")?;
                Ok(Some((candidate, state_guard)))
                })?
            };
            let Some((candidate, _state_guard)) = candidate else {
                return Ok(());
            };
            match db.transaction_once(|tx| tasks::commit_task_candidate_tx(tx, scope, candidate)) {
                Ok(resident) => {
                    let mut entry = PreparedTaskBaseline::new(Arc::new(resident), ledger)?;
                    entry.unpublished = true;
                    if let Some(old) = self
                        .entries
                        .iter_mut()
                        .find(|entry| entry.resident.scope() == scope)
                    {
                        *old = entry;
                    } else {
                        self.entries.push(entry);
                    }
                    return Ok(());
                }
                Err(error)
                    if attempt < 2
                        && error
                            .downcast_ref::<tasks::TaskPreparationCandidateStale>()
                            .is_some() => {}
                Err(error) => return Err(error),
            }
        }
        unreachable!("bounded Task candidate attempts return their final result")
    }
}
