//! A closed instance's original registered producers, not reconstructed OS IDs.
use super::super::{
    command_task::OriginalManagedProcess,
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_input_evidence::InputEvidenceScope,
};
use super::*;

pub(crate) struct RetiredProducerExit {
    scope: InputEvidenceScope,
    owner_epoch: u64,
    registry_id: uuid::Uuid,
    registration_revision: u64,
    processes: Vec<Arc<OriginalManagedProcess>>,
    _material: Arc<BudgetViewGuard>,
}
impl RetiredProducerExit {
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }
    pub(crate) fn owner_epoch(&self) -> u64 {
        self.owner_epoch
    }
    pub(crate) fn registry_id(&self) -> uuid::Uuid {
        self.registry_id
    }
    pub(crate) fn registration_revision(&self) -> u64 {
        self.registration_revision
    }
    /// Re-probe the same frozen groups before cleanup. A reused/nonempty group,
    /// EPERM or unsupported backend grants no physical permission.
    pub(crate) fn registered_groups_still_empty(&self) -> bool {
        self.processes
            .iter()
            .all(|process| process.group_is_empty())
    }
}
pub(in crate::runtime) struct RetiringProducerSnapshot {
    scope: InputEvidenceScope,
    owner_epoch: u64,
    revision: u64,
    registry_id: uuid::Uuid,
    leases: Vec<Arc<ControlJoinLease>>,
    runs: Vec<Arc<ControlRunLease>>,
    processes: Vec<Arc<OriginalManagedProcess>>,
    material: Arc<BudgetViewGuard>,
}
impl RetiringProducerSnapshot {
    pub(in crate::runtime) fn finish(
        self,
        registry: &ControlCancellationRegistry,
    ) -> Result<Option<RetiredProducerExit>> {
        if registry.original_process_registry_id() != self.registry_id
            || !registry.retiring.load(Ordering::Acquire)
            || registry
                .producer_registration_revision
                .load(Ordering::Acquire)
                != self.revision
            || !registry
                .joins
                .lock()
                .map_err(|_| anyhow!("Control leases poisoned"))?
                .is_empty()
            || !registry
                .runs
                .lock()
                .map_err(|_| anyhow!("Control runs poisoned"))?
                .is_empty()
            || self.runs.iter().any(|run| !run.is_complete())
        {
            return Ok(None);
        }
        for lease in self.leases {
            if !lease.is_finished() {
                return Ok(None);
            }
            if lease.is_command_producer() {
                let Some(fact) = lease.original_command_process_exit() else {
                    return Ok(None);
                };
                if !fact.matches_lease(&lease)
                    || fact.scope() != &self.scope
                    || !fact.group_is_empty()
                {
                    return Ok(None);
                }
            }
        }
        if self
            .processes
            .iter()
            .any(|process| process.scope() != &self.scope || !process.group_is_empty())
        {
            return Ok(None);
        }
        Ok(Some(RetiredProducerExit {
            scope: self.scope,
            owner_epoch: self.owner_epoch,
            registry_id: self.registry_id,
            registration_revision: self.revision,
            processes: self.processes,
            _material: self.material,
        }))
    }
}
impl ControlCancellationRegistry {
    pub(in crate::runtime) fn original_process_registry_id(&self) -> uuid::Uuid {
        *self.process_registry_id.get_or_init(uuid::Uuid::new_v4)
    }
    pub(in crate::runtime) fn original_process_registration_revision(&self) -> u64 {
        self.producer_registration_revision.load(Ordering::Acquire)
    }
    pub(in crate::runtime) fn original_registered_groups_quiescent(&self) -> bool {
        let processes_empty =
            self.managed_processes.lock().ok().is_some_and(|processes| {
                processes.values().all(|process| process.group_is_empty())
            });
        processes_empty
            && self.joins.lock().ok().is_some_and(|joins| {
                joins.values().all(|lease| {
                    !lease.is_command_producer()
                        || lease
                            .original_command_process_exit()
                            .is_some_and(|fact| fact.group_is_empty())
                })
            })
    }
    pub(in crate::runtime) fn retain_managed_process(
        &self,
        process: Arc<OriginalManagedProcess>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        let mut processes = self
            .managed_processes
            .lock()
            .map_err(|_| anyhow!("original processes poisoned"))?;
        // Successful wait plus ESRCH is the only eager release. Live/unknown
        // groups and cancelled spawn windows remain owned by this registry.
        processes.retain(|_, original| !original.group_is_empty());
        let count = processes
            .len()
            .checked_add(1)
            .ok_or_else(|| anyhow!("original process count overflow"))?;
        let bytes = processes
            .values()
            .try_fold(process.material_bytes(), |sum, p| {
                sum.checked_add(p.material_bytes())
                    .ok_or_else(|| anyhow!("original process bytes overflow"))
            })?;
        let limits = ledger.limits();
        ensure!(
            u64::try_from(count)? <= limits.record_limit
                && bytes
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .ok_or_else(|| anyhow!("original process limit overflow"))?
                        .min(64 * 1024 * 1024),
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "original process responsibilities"
            )
        );
        self.producer_registration_revision
            .fetch_add(1, Ordering::AcqRel);
        ensure!(
            processes
                .insert(process.token().to_owned(), process)
                .is_none(),
            "original process token reused"
        );
        Ok(())
    }
    pub(in crate::runtime) fn capture_retiring_producers(
        &self,
        scope: &InputEvidenceScope,
        owner_epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<RetiringProducerSnapshot> {
        ensure!(
            self.retiring.load(Ordering::Acquire),
            "producer snapshot requires closed original registry"
        );
        let leases = self
            .joins
            .lock()
            .map_err(|_| anyhow!("Control leases poisoned"))?;
        let runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("Control runs poisoned"))?;
        let processes = self
            .managed_processes
            .lock()
            .map_err(|_| anyhow!("original processes poisoned"))?;
        let revision = self.producer_registration_revision.load(Ordering::Acquire);
        let mut bytes = serialized_bytes(&(scope, owner_epoch, revision))?
            .checked_add(512)
            .ok_or_else(|| anyhow!("producer snapshot overflow"))?;
        for lease in leases.values() {
            bytes = bytes
                .checked_add(serialized_bytes(&(
                    lease.lease_id(),
                    lease.target_id(),
                    lease.domain_generation(),
                ))?)
                .and_then(|n| n.checked_add(256))
                .ok_or_else(|| anyhow!("producer snapshot overflow"))?;
        }
        for run in runs.values() {
            bytes = bytes
                .checked_add(serialized_bytes(&(
                    run.lease_id.as_str(),
                    run.run_id.as_str(),
                ))?)
                .and_then(|n| n.checked_add(256))
                .ok_or_else(|| anyhow!("producer snapshot overflow"))?;
        }
        for process in processes.values() {
            bytes = bytes
                .checked_add(serialized_bytes(&(process.scope(), process.token()))?)
                .and_then(|n| n.checked_add(256))
                .ok_or_else(|| anyhow!("producer snapshot overflow"))?;
        }
        let bytes = bytes
            .checked_mul(4)
            .ok_or_else(|| anyhow!("producer snapshot overflow"))?;
        let records = leases
            .len()
            .checked_add(runs.len())
            .and_then(|n| n.checked_add(processes.len()))
            .and_then(|n| n.checked_add(1))
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| anyhow!("producer snapshot overflow"))?;
        let limits = ledger.limits();
        ensure!(
            u64::try_from(records)? <= limits.record_limit
                && bytes
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .ok_or_else(|| anyhow!("producer snapshot limit overflow"))?
                        .min(64 * 1024 * 1024),
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "retiring producer metadata"
            )
        );
        let material = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        Ok(RetiringProducerSnapshot {
            scope: scope.clone(),
            owner_epoch,
            revision,
            registry_id: self.original_process_registry_id(),
            leases: leases.values().cloned().collect(),
            runs: runs.values().cloned().collect(),
            processes: processes.values().cloned().collect(),
            material,
        })
    }
}

#[cfg(test)]
mod instance_id_tests {
    use super::*;
    #[test]
    fn distinct_original_registries_do_not_share_an_instance_even_with_same_owner_epoch(
    ) -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "registry-instance".into(),
            identity_incarnation: 1,
        };
        let a = ControlCancellationRegistry::default();
        let b = ControlCancellationRegistry::default();
        a.cancel_local_producers();
        b.cancel_local_producers();
        let a = a
            .capture_retiring_producers(&scope, 7, &ledger)?
            .finish(&a)?
            .expect("empty closed original registry");
        let b = b
            .capture_retiring_producers(&scope, 7, &ledger)?
            .finish(&b)?
            .expect("empty closed original registry");
        assert_eq!(a.owner_epoch(), b.owner_epoch());
        assert_ne!(a.registry_id(), b.registry_id());
        drop((a, b));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
}
