//! Live Event birth and finite ordinary windows belong to the original owner slot.
//! Weak registrations retain neither a consumer nor a published Root or body.
use super::{
    current_read_facts::{CurrentReadError, CURRENT_READ_MAX_BYTES, CURRENT_READ_MAX_RECORDS},
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_input_evidence::InputEvidenceScope,
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
};

#[derive(Debug, Default)]
struct RegistryInner {
    next: AtomicU64,
    slots: Mutex<BTreeMap<u64, Slot>>,
}
#[derive(Debug)]
struct Slot {
    lease: Weak<LeaseInner>,
    _material: Arc<BudgetViewGuard>,
}
#[derive(Debug, Default, Clone)]
pub(crate) struct CurrentEventQueryRegistry(Arc<RegistryInner>);
#[derive(Debug, Clone, Copy)]
enum EventRole {
    NativeBirth { after: u64 },
    OrdinaryWindow { body: usize, headers: usize },
}
#[derive(Debug)]
struct LeaseInner {
    id: u64,
    scope: InputEvidenceScope,
    epoch: u64,
    role: EventRole,
    material: (usize, usize),
    registry: Weak<RegistryInner>,
    budget: Arc<BudgetViewGuard>,
    slot_budget: Arc<BudgetViewGuard>,
}
impl Drop for LeaseInner {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            if let Ok(mut slots) = registry.slots.try_lock() {
                slots.remove(&self.id);
            }
        }
    }
}
#[derive(Debug)]
pub(crate) struct CurrentEventQueryLease(Arc<LeaseInner>);
impl CurrentEventQueryLease {
    pub(crate) fn cursor(&self) -> u64 {
        match self.0.role {
            EventRole::NativeBirth { after } => after,
            EventRole::OrdinaryWindow { .. } => unreachable!("birth lease requires birth role"),
        }
    }
    pub(crate) fn material_estimate(&self) -> (usize, usize) {
        self.0.material
    }
}
#[derive(Debug)]
pub(crate) struct CurrentOrdinaryEventQueryLease(Arc<LeaseInner>);
impl CurrentOrdinaryEventQueryLease {
    pub(crate) fn material_estimate(&self) -> (usize, usize) {
        self.0.material
    }
    pub(crate) fn window_limits(&self) -> (usize, usize) {
        match self.0.role {
            EventRole::OrdinaryWindow { body, headers } => (body, headers),
            EventRole::NativeBirth { .. } => unreachable!("ordinary lease requires finite window"),
        }
    }
}
/// Read selection only. Existing business admission still owns every mutation.
#[derive(Debug)]
pub(crate) struct CurrentEventRoleSnapshot {
    scope: InputEvidenceScope,
    epoch: u64,
    after: Option<u64>,
    window: Option<(usize, usize)>,
    material: (usize, usize),
    _leases: Vec<Arc<LeaseInner>>,
    budget: Arc<BudgetViewGuard>,
    own_material: (usize, usize),
}
impl CurrentEventRoleSnapshot {
    /// Borrow each original allocation; observers add no Event/body authority.
    pub(crate) fn visit_material(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        mut observe: impl FnMut(&Arc<BudgetViewGuard>, usize, usize) -> Result<()>,
    ) -> Result<()> {
        self.validate(scope, self.epoch, ledger)?;
        for lease in &self._leases {
            ensure!(
                lease.scope == *scope && lease.epoch == self.epoch,
                "Event loan owner differs"
            );
            lease
                .budget
                .validate_owned_material_binding(scope, ledger, 0)?;
            lease
                .slot_budget
                .validate_owned_material_binding(scope, ledger, 0)?;
            observe(
                &lease.budget,
                usize::try_from(lease.budget.owned_material_bytes()?)?,
                64,
            )?;
            observe(
                &lease.slot_budget,
                usize::try_from(lease.slot_budget.owned_material_bytes()?)?,
                16,
            )?;
        }
        observe(&self.budget, self.own_material.0, self.own_material.1)
    }
    pub(crate) fn birth_after(&self) -> Option<u64> {
        self.after
    }
    pub(crate) fn window_limits(&self) -> Option<(usize, usize)> {
        self.window
    }
    pub(crate) fn material_estimate(&self) -> (usize, usize) {
        self.material
    }
    pub(crate) fn prepare_request(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        request: &mut super::current_read_facts::CurrentReadRequest,
    ) -> Result<(usize, usize)> {
        self.validate(scope, self.epoch, ledger)?;
        request.check_material(self.material.0, self.material.1)?;
        request.event_birth_after = self.after;
        if let Some((body, headers)) = self.window {
            request.event_limit = request.event_limit.max(body);
            request.event_count_limit = request.event_count_limit.max(headers);
        }
        Ok(self.material)
    }
    pub(crate) fn prepare_address_request(
        &self,
        agent_id: &str,
        incarnation: u64,
        ledger: &InputEvidenceBudgetLedger,
        request: &mut super::current_read_facts::CurrentReadRequest,
    ) -> Result<(usize, usize)> {
        ensure!(
            self.scope.agent_id == agent_id && self.scope.identity_incarnation == incarnation,
            CurrentReadError::CanonicalChanged
        );
        self.prepare_request(&self.scope, ledger, request)
    }
    pub(crate) fn validate(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == *scope && self.epoch == epoch,
            "Event role snapshot belongs to another owner"
        );
        self.budget
            .validate_owned_material_binding(scope, ledger, 0)?;
        Ok(())
    }
}
fn combined(a: (usize, usize), b: (usize, usize)) -> Result<(usize, usize)> {
    Ok((
        a.0.checked_add(b.0).context("Event role bytes overflow")?,
        a.1.checked_add(b.1).context("Event role nodes overflow")?,
    ))
}
fn check(material: (usize, usize), limits: (usize, usize)) -> Result<()> {
    ensure!(
        material.0 <= limits.0.min(CURRENT_READ_MAX_BYTES)
            && material.1 <= limits.1.min(CURRENT_READ_MAX_RECORDS),
        CurrentReadError::ResourceExhausted
    );
    Ok(())
}
fn observed(
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
    bytes: usize,
) -> Result<Arc<BudgetViewGuard>> {
    Ok(ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: u64::try_from(bytes)?,
        },
    )?)
}
impl CurrentEventQueryRegistry {
    pub(crate) fn register(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        after: u64,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
    ) -> Result<CurrentEventQueryLease> {
        Ok(CurrentEventQueryLease(self.register_role(
            scope,
            epoch,
            EventRole::NativeBirth { after },
            ledger,
            limits,
        )?))
    }
    pub(crate) fn register_ordinary_window(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        body: usize,
        headers: usize,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
    ) -> Result<CurrentOrdinaryEventQueryLease> {
        ensure!(
            body > 0
                && body <= headers
                && headers <= super::current_read_facts::CURRENT_EVENT_WINDOW,
            CurrentReadError::WindowExpansion
        );
        Ok(CurrentOrdinaryEventQueryLease(self.register_role(
            scope,
            epoch,
            EventRole::OrdinaryWindow { body, headers },
            ledger,
            limits,
        )?))
    }
    fn register_role(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        role: EventRole,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
    ) -> Result<Arc<LeaseInner>> {
        ensure!(
            scope.identity_incarnation > 0 && epoch > 0 && !scope.agent_id.is_empty(),
            "Event role requires an exact original owner"
        );
        let scope_bytes = usize::try_from(serialized_bytes(scope)?)?;
        let lease_bytes = scope_bytes
            .checked_add(1024)
            .and_then(|n| n.checked_mul(8))
            .context("Event lease bytes overflow")?;
        let slot_bytes = scope_bytes
            .checked_mul(4)
            .and_then(|n| n.checked_add(512))
            .context("Event role slot bytes overflow")?;
        let material = combined((lease_bytes, 64), (slot_bytes, 16))?;
        let mut slots = self
            .0
            .slots
            .lock()
            .map_err(|_| anyhow::anyhow!("Event role registry poisoned"))?;
        slots.retain(|_, slot| slot.lease.strong_count() > 0);
        // Registration itself is bounded, including still-live older epochs.
        // Waiting for a writer snapshot cannot permit an unbounded lease pool.
        let mut total = material;
        for slot in slots.values() {
            if let Some(lease) = slot.lease.upgrade() {
                total = combined(total, lease.material)?;
            }
        }
        check(total, limits)?;
        let budget = observed(scope, ledger, lease_bytes)?;
        let slot_budget = observed(scope, ledger, slot_bytes)?;
        let id = self
            .0
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| anyhow::anyhow!("Event role ID exhausted"))?;
        let lease = Arc::new(LeaseInner {
            id,
            scope: scope.clone(),
            epoch,
            role,
            material,
            registry: Arc::downgrade(&self.0),
            budget,
            slot_budget: slot_budget.clone(),
        });
        slots.insert(
            id,
            Slot {
                lease: Arc::downgrade(&lease),
                _material: slot_budget,
            },
        );
        Ok(lease)
    }
    pub(crate) fn validate_lease(
        &self,
        lease: &CurrentEventQueryLease,
        scope: &InputEvidenceScope,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            lease.0.scope == *scope
                && lease.0.epoch == epoch
                && Weak::ptr_eq(&lease.0.registry, &Arc::downgrade(&self.0)),
            "Event role belongs to another owner or registry"
        );
        lease
            .0
            .budget
            .validate_owned_material_binding(scope, ledger, 0)?;
        Ok(())
    }
    pub(crate) fn validate_ordinary_lease(
        &self,
        lease: &CurrentOrdinaryEventQueryLease,
        scope: &InputEvidenceScope,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            lease.0.scope == *scope
                && lease.0.epoch == epoch
                && Weak::ptr_eq(&lease.0.registry, &Arc::downgrade(&self.0))
                && matches!(lease.0.role, EventRole::OrdinaryWindow { .. }),
            "ordinary Event window belongs to another owner or registry"
        );
        lease
            .0
            .budget
            .validate_owned_material_binding(scope, ledger, 0)?;
        Ok(())
    }
    pub(crate) fn snapshot(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
    ) -> Result<Option<CurrentEventRoleSnapshot>> {
        let mut slots = self
            .0
            .slots
            .lock()
            .map_err(|_| anyhow::anyhow!("Event role registry poisoned"))?;
        slots.retain(|_, slot| slot.lease.strong_count() > 0);
        let mut held = (0, 0);
        let mut count = 0usize;
        let mut after = None;
        let mut window = None;
        for slot in slots.values() {
            let Some(lease) = slot.lease.upgrade() else {
                continue;
            };
            if lease.scope != *scope || lease.epoch != epoch {
                continue;
            }
            lease
                .budget
                .validate_owned_material_binding(scope, ledger, 0)?;
            held = combined(held, lease.material)?;
            count = count
                .checked_add(1)
                .context("Event role snapshot slots overflow")?;
            match lease.role {
                EventRole::NativeBirth { after: cursor } => {
                    after = Some(after.map_or(cursor, |old: u64| old.min(cursor)));
                }
                EventRole::OrdinaryWindow { body, headers } => {
                    let (old_body, old_headers) = window.unwrap_or((0, 0));
                    window = Some((old_body.max(body), old_headers.max(headers)));
                }
            }
        }
        if count == 0 {
            return Ok(None);
        }
        let copied = (
            usize::try_from(serialized_bytes(scope)?)?
                .checked_add(2048)
                .and_then(|n| {
                    count
                        .checked_mul(128)
                        .and_then(|slots| n.checked_add(slots))
                })
                .context("Event role snapshot bytes overflow")?,
            count
                .checked_mul(4)
                .and_then(|n| n.checked_add(32))
                .context("Event role snapshot nodes overflow")?,
        );
        let material = combined(held, copied)?;
        check(material, limits)?;
        let budget = observed(scope, ledger, copied.0)?;
        let mut leases = Vec::new();
        for slot in slots.values() {
            let Some(lease) = slot.lease.upgrade() else {
                continue;
            };
            if lease.scope == *scope && lease.epoch == epoch {
                leases.push(lease);
            }
        }
        slots.retain(|_, slot| slot.lease.strong_count() > 0);
        // A concurrent consumer drop may make the borrowed first pass larger;
        // the held snapshot can conservatively keep that min cursor for this ACK.
        Ok(Some(CurrentEventRoleSnapshot {
            scope: scope.clone(),
            epoch,
            after,
            window,
            material,
            _leases: leases,
            budget,
            own_material: copied,
        }))
    }
}

#[derive(Debug)]
pub(crate) struct CurrentEventBirthSession {
    pub(crate) cursor: u64,
    lease: CurrentEventQueryLease,
}
impl CurrentEventBirthSession {
    /// Validates only borrowed scope/lease/guards; actual capture still checks State.
    pub(in crate::runtime) fn validate_held_material(
        &self,
        runtime: &super::RuntimeHandle,
        root: &Arc<super::mutation_owner::PostureObservation>,
    ) -> Result<()> {
        validate_captured_event_birth_lease(&self.lease.0, self.cursor, runtime, root)
    }
    pub(in crate::runtime) fn material_estimate(&self) -> Result<(u64, usize)> {
        let actual = self.lease.material_estimate();
        Ok((u64::try_from(actual.0)?, actual.1))
    }
}

#[cfg(test)]
mod held_material_tests {
    use super::*;
    use crate::runtime::tests::support::LifecycleHarness;

    #[tokio::test]
    async fn captured_event_material_validates_original_slot_and_selected_pins_without_allocation(
    ) -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime
            .quiesce_context_resource_observer_task_for_test()
            .await?;
        let held = runtime.inner.mutation_owner.reserve().await?;
        let initial = held
            .capture_ready_snapshot()?
            .context("actual Event Root absent")?;
        let session =
            runtime.register_current_event_birth_session_from_snapshot(&held, &initial)?;
        drop((held, initial));
        runtime
            .append_owned_runtime_audit_event(crate::types::AuditEvent::legacy(
                "runtime_error",
                serde_json::json!({"actual":"captured Event material"}),
            ))
            .await?;
        let root = runtime
            .inner
            .mutation_owner
            .capture()?
            .context("actual Event Root absent")?;
        let ledger = &runtime.inner.input_evidence_budget;
        let before = ledger.report()?.observed_bytes;
        let selected =
            runtime.capture_current_event_birth_session_from_snapshot(&session, &root)?;
        assert!(selected.selected.events.iter().any(|record| matches!(&record.fact,
            super::super::current_read_facts::CurrentReadFact::Event(event) if event.kind == "runtime_error")));
        let strong = Arc::strong_count(&root);
        let used = ledger.report()?.observed_bytes;
        runtime.runtime_db().connection()?.execute_batch(
            "ALTER TABLE audit_events RENAME TO captured_event_forbidden_events;
             ALTER TABLE agent_states RENAME TO captured_event_forbidden_states;",
        )?;
        selected.validate_material_binding(runtime, &root)?;
        assert_eq!(Arc::strong_count(&root), strong);
        assert_eq!(ledger.report()?.observed_bytes, used);
        let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
        let foreign_guard = foreign.register_owned_bytes(
            &selected.selected.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: session.material_estimate()?.0,
                metadata_estimate_bytes: 0,
            },
        )?;
        let registry = runtime.inner.mutation_owner.current_event_queries()?;
        let original = {
            let mut slots = registry.0.slots.lock().unwrap();
            std::mem::replace(
                &mut slots.get_mut(&session.lease.0.id).unwrap()._material,
                foreign_guard,
            )
        };
        assert!(selected.validate_material_binding(runtime, &root).is_err());
        assert_eq!(ledger.report()?.observed_bytes, used);
        let foreign_guard = {
            let mut slots = registry.0.slots.lock().unwrap();
            std::mem::replace(
                &mut slots.get_mut(&session.lease.0.id).unwrap()._material,
                original,
            )
        };
        drop(foreign_guard);
        assert_eq!(foreign.report()?.observed_bytes, 0);
        selected.validate_material_binding(runtime, &root)?;
        drop(selected);
        assert_eq!(ledger.report()?.observed_bytes, before);
        Ok(())
    }

    #[tokio::test]
    async fn event_held_validation_is_pure_and_rejects_a_foreign_actual_slot() -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let _pause = runtime.pause_resource_observer_for_test().await;
        let held = runtime.inner.mutation_owner.reserve().await?;
        let root = held.capture_ready_snapshot()?.unwrap();
        let session = runtime.register_current_event_birth_session_from_snapshot(&held, &root)?;
        drop(held);
        let ledger = &runtime.inner.input_evidence_budget;
        let before = ledger.report()?.observed_bytes;
        let strong = Arc::strong_count(&root);
        runtime.runtime_db().connection()?.execute_batch(
            "ALTER TABLE audit_events RENAME TO held_event_forbidden_events;
             ALTER TABLE agent_states RENAME TO held_event_forbidden_states;",
        )?;
        session.validate_held_material(runtime, &root)?;
        assert_eq!(Arc::strong_count(&root), strong);
        assert_eq!(ledger.report()?.observed_bytes, before);
        let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
        let actual = foreign.register_owned_bytes(
            root.current_reads.as_ref().unwrap().scope(),
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 1024,
                metadata_estimate_bytes: 0,
            },
        )?;
        let registry = runtime.inner.mutation_owner.current_event_queries()?;
        let original = {
            let mut slots = registry.0.slots.lock().unwrap();
            std::mem::replace(
                &mut slots.get_mut(&session.lease.0.id).unwrap()._material,
                actual,
            )
        };
        assert!(session.validate_held_material(runtime, &root).is_err());
        assert_eq!(ledger.report()?.observed_bytes, before);
        let actual = {
            let mut slots = registry.0.slots.lock().unwrap();
            std::mem::replace(
                &mut slots.get_mut(&session.lease.0.id).unwrap()._material,
                original,
            )
        };
        drop(actual);
        assert_eq!(foreign.report()?.observed_bytes, 0);
        session.validate_held_material(runtime, &root)?;
        Ok(())
    }
}

fn validate_captured_event_birth_lease(
    lease: &Arc<LeaseInner>,
    cursor: u64,
    runtime: &super::RuntimeHandle,
    root: &Arc<super::mutation_owner::PostureObservation>,
) -> Result<()> {
    let registry = runtime.inner.mutation_owner.current_event_queries()?;
    let events = root
        .current_reads
        .as_ref()
        .ok_or(CurrentReadError::NeedsPreparation)?;
    let scope = events.scope();
    let ledger = &runtime.inner.input_evidence_budget;
    ensure!(
        scope.agent_id == root.state.id
            && root.state.id == runtime.inner.mutation_owner.agent_id()
            && scope.identity_incarnation == root.token.identity_incarnation
            && root.token.identity_incarnation == runtime.inner.identity_incarnation
            && root.token.owner_epoch == runtime.inner.mutation_owner.owner_epoch()
            && events.uses_ledger(ledger),
        "Event birth held scope or ledger differs"
    );
    ensure!(
        lease.scope == *scope
            && lease.epoch == root.token.owner_epoch
            && Weak::ptr_eq(&lease.registry, &Arc::downgrade(&registry.0)),
        "Event role belongs to another owner or registry"
    );
    lease
        .budget
        .validate_owned_material_binding(scope, ledger, 0)?;
    ensure!(
        matches!(lease.role,EventRole::NativeBirth{after} if after==cursor),
        CurrentReadError::CanonicalChanged
    );
    lease
        .slot_budget
        .validate_owned_material_binding(scope, ledger, 0)?;
    let slot_bytes = usize::try_from(lease.slot_budget.owned_material_bytes()?)?;
    let lease_bytes = lease
        .material
        .0
        .checked_sub(slot_bytes)
        .context("Event birth slot exceeds original material")?;
    lease
        .budget
        .validate_owned_material_binding(scope, ledger, u64::try_from(lease_bytes)?)?;
    let slots = registry
        .0
        .slots
        .lock()
        .map_err(|_| anyhow::anyhow!("Event role registry poisoned"))?;
    let slot = slots
        .get(&lease.id)
        .context("Event birth original slot absent")?;
    ensure!(
        Weak::ptr_eq(&slot.lease, &Arc::downgrade(lease))
            && Arc::ptr_eq(&slot._material, &lease.slot_budget),
        "Event birth original slot differs"
    );
    slot._material
        .validate_owned_material_binding(scope, ledger, u64::try_from(slot_bytes)?)?;
    Ok(())
}

pub(crate) struct CapturedCurrentEventQuery {
    pub(crate) selected: super::current_read_facts::CapturedCurrentEvents,
    _lease: Arc<LeaseInner>,
}
impl CapturedCurrentEventQuery {
    /// Validate the held native role, actual slot and same-publication body pins.
    pub(in crate::runtime) fn validate_material_binding(
        &self,
        runtime: &super::RuntimeHandle,
        root: &Arc<super::mutation_owner::PostureObservation>,
    ) -> Result<()> {
        let EventRole::NativeBirth { after } = self._lease.role else {
            anyhow::bail!("captured Event does not hold its original birth role")
        };
        validate_captured_event_birth_lease(&self._lease, after, runtime, root)?;
        let domain = root
            .current_reads
            .as_ref()
            .ok_or(CurrentReadError::NeedsPreparation)?;
        ensure!(
            self.selected.revision == domain.coverage().revision,
            "captured Event publication differs"
        );
        self.selected
            .validate_scheduler_body_loan(domain, &runtime.inner.input_evidence_budget)
    }
    /// This phase's own held lease and selected material, excluding caller prior.
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        let selected = self.selected.material_estimate()?;
        Ok((
            selected
                .0
                .checked_add(u64::try_from(self._lease.material.0)?)
                .context("Event birth capture bytes overflow")?,
            selected
                .1
                .checked_add(self._lease.material.1)
                .context("Event birth capture nodes overflow")?,
        ))
    }
}
fn runtime_limits(ledger: &InputEvidenceBudgetLedger) -> Result<(usize, usize)> {
    let limits = ledger.limits();
    Ok((
        usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("Event query limits overflow")?,
        )?
        .min(CURRENT_READ_MAX_BYTES),
        usize::try_from(limits.record_limit)?.min(CURRENT_READ_MAX_RECORDS),
    ))
}
impl super::RuntimeHandle {
    pub(in crate::runtime) fn register_current_event_birth_session_from_snapshot(
        &self,
        reservation: &super::mutation_owner::MutationReservation,
        root: &Arc<super::mutation_owner::PostureObservation>,
    ) -> Result<CurrentEventBirthSession> {
        self.register_current_event_birth_session_from_snapshot_with_prior(
            reservation,
            root,
            (usize::MAX, usize::MAX),
            (0, 0),
        )
    }
    pub(in crate::runtime) fn register_current_event_birth_session_from_snapshot_with_prior(
        &self,
        reservation: &super::mutation_owner::MutationReservation,
        root: &Arc<super::mutation_owner::PostureObservation>,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<CurrentEventBirthSession> {
        reservation.validate_owner(&self.inner.mutation_owner)?;
        let current = reservation
            .capture_ready_snapshot()?
            .context("Event query root unpublished")?;
        ensure!(
            Arc::ptr_eq(&current, root),
            "Event query registration publication changed"
        );
        let events = root
            .current_reads
            .as_ref()
            .ok_or(CurrentReadError::NeedsPreparation)?;
        ensure!(
            events.scope().agent_id == root.state.id
                && events.scope().identity_incarnation == root.token.identity_incarnation
                && root.token.identity_incarnation == self.inner.identity_incarnation
                && root.token.owner_epoch == reservation.owner_epoch()
                && root.state.id == self.inner.mutation_owner.agent_id()
                && events.uses_ledger(&self.inner.input_evidence_budget),
            "Event query held publication scope or ledger differs"
        );
        events.validate_state(&root.state)?;
        let cursor = events
            .coverage()
            .event_query
            .birth_cursor
            .ok_or(CurrentReadError::NeedsPreparation)?;
        let actual = runtime_limits(&self.inner.input_evidence_budget)?;
        let remaining = super::run_birth_sessions::remaining_material(
            &self.inner.input_evidence_budget,
            (limits.0.min(actual.0), limits.1.min(actual.1)),
            prior,
        )?;
        let lease = self
            .inner
            .mutation_owner
            .current_event_queries()?
            .register(
                events.scope(),
                root.token.owner_epoch,
                cursor,
                &self.inner.input_evidence_budget,
                remaining,
            )?;
        Ok(CurrentEventBirthSession { cursor, lease })
    }
    pub(in crate::runtime) fn capture_current_event_birth_session_from_snapshot(
        &self,
        session: &CurrentEventBirthSession,
        root: &Arc<super::mutation_owner::PostureObservation>,
    ) -> Result<CapturedCurrentEventQuery> {
        self.capture_current_event_birth_session_from_snapshot_with_prior(
            session,
            root,
            runtime_limits(&self.inner.input_evidence_budget)?,
            (0, 0),
        )
    }
    pub(in crate::runtime) fn capture_current_event_birth_session_from_snapshot_with_prior(
        &self,
        session: &CurrentEventBirthSession,
        root: &Arc<super::mutation_owner::PostureObservation>,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<CapturedCurrentEventQuery> {
        session.validate_held_material(self, root)?;
        let events = root
            .current_reads
            .as_ref()
            .ok_or(CurrentReadError::NeedsPreparation)?;
        events.validate_state(&root.state)?;
        let runtime_limits = runtime_limits(&self.inner.input_evidence_budget)?;
        let limits = (
            limits.0.min(runtime_limits.0),
            limits.1.min(runtime_limits.1),
        );
        let lease = session.lease.material_estimate();
        let prior = (
            usize::try_from(prior.0)?
                .checked_add(lease.0)
                .context("Event birth prior bytes overflow")?,
            prior
                .1
                .checked_add(lease.1)
                .context("Event birth prior nodes overflow")?,
        );
        check(prior, limits)?;
        let request = super::current_read_facts::CurrentReadRequest::current(
            limits.0 - prior.0,
            limits.1 - prior.1,
        );
        let selected = events.capture_event_births(session.cursor, &request)?;
        Ok(CapturedCurrentEventQuery {
            selected,
            _lease: session.lease.0.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_event_snapshot_lends_original_lease_slot_and_snapshot_guards_without_registration(
    ) -> Result<()> {
        let scope = scope();
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let registry = CurrentEventQueryRegistry::default();
        let lease = registry.register(&scope, 7, 19, &ledger, limits())?;
        let slot = Arc::downgrade(&lease.0.slot_budget);
        let roles = registry
            .snapshot(&scope, 7, &ledger, limits())?
            .context("actual roles absent")?;
        let baseline = ledger.report()?.observed_bytes;
        let mut total = (0, 0);
        let mut guards = Vec::new();
        roles.visit_material(&scope, &ledger, |guard, bytes, records| {
            total.0 += bytes;
            total.1 += records;
            guards.push(guard.clone());
            Ok(())
        })?;
        assert_eq!(total, roles.material_estimate());
        assert_eq!(guards.len(), 3);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
        assert!(roles
            .visit_material(&scope, &foreign, |_, _, _| anyhow::bail!(
                "foreign callback reached"
            ))
            .is_err());
        assert_eq!(foreign.report()?.observed_bytes, 0);
        drop((lease, registry));
        assert!(slot.upgrade().is_some());
        roles.visit_material(&scope, &ledger, |guard, bytes, _| {
            guard.validate_owned_material_binding(&scope, &ledger, u64::try_from(bytes)?)?;
            Ok(())
        })?;
        drop(roles);
        assert!(
            slot.upgrade().is_some(),
            "actual loan must retain its original slot guard"
        );
        drop(guards);
        assert!(slot.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    fn scope() -> InputEvidenceScope {
        InputEvidenceScope {
            agent_id: "event-role-owner".into(),
            identity_incarnation: 1,
        }
    }
    fn limits() -> (usize, usize) {
        (CURRENT_READ_MAX_BYTES, CURRENT_READ_MAX_RECORDS)
    }
    #[test]
    fn owner_shared_registry_keeps_min_live_cursor_and_only_weak_consumer_slots() -> Result<()> {
        let owners = super::super::mutation_owner::MutationOwnerRegistry::default();
        let owner = owners.claim(&scope().agent_id, 1)?;
        let peer = owners.current_owner(&scope().agent_id, 1)?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let registry = owner.current_event_queries()?;
        assert!(registry
            .snapshot(&scope(), owner.owner_epoch(), &ledger, limits())?
            .is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let first = registry.register(&scope(), owner.owner_epoch(), 5, &ledger, limits())?;
        let second = peer.current_event_queries()?.register(
            &scope(),
            peer.owner_epoch(),
            10,
            &ledger,
            limits(),
        )?;
        let selected = registry
            .snapshot(&scope(), owner.owner_epoch(), &ledger, limits())?
            .context("live snapshot")?;
        assert_eq!(selected.birth_after(), Some(5));
        selected.validate(&scope(), owner.owner_epoch(), &ledger)?;
        drop(first);
        assert_eq!(
            registry
                .snapshot(&scope(), owner.owner_epoch(), &ledger, limits())?
                .unwrap()
                .birth_after(),
            Some(5)
        );
        // Only the real in-flight phase retains that lease after consumer drop.
        drop(selected);
        assert_eq!(
            registry
                .snapshot(&scope(), owner.owner_epoch(), &ledger, limits())?
                .unwrap()
                .birth_after(),
            Some(10)
        );
        drop(second);
        assert!(registry
            .snapshot(&scope(), owner.owner_epoch(), &ledger, limits())?
            .is_none());
        assert!(registry.0.slots.lock().unwrap().is_empty());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[test]
    fn owner_event_registration_and_snapshot_bound_live_material_before_more_slots() -> Result<()> {
        let registry = CurrentEventQueryRegistry::default();
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let first = registry.register(&scope(), 1, 0, &ledger, (CURRENT_READ_MAX_BYTES, 150))?;
        let before = ledger.report()?.observed_bytes;
        let error = registry
            .register(&scope(), 1, 1, &ledger, (CURRENT_READ_MAX_BYTES, 150))
            .err()
            .context("joint roles were admitted")?;
        assert!(matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ));
        assert_eq!(ledger.report()?.observed_bytes, before);
        assert_eq!(registry.0.slots.lock().unwrap().len(), 1);
        // The live lease and the next acquisition are individually small, but
        // the actual snapshot must count both before allocating its Arc slots.
        let error = registry
            .snapshot(&scope(), 1, &ledger, (CURRENT_READ_MAX_BYTES, 100))
            .err()
            .context("snapshot prior ignored")?;
        assert!(matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ));
        assert_eq!(ledger.report()?.observed_bytes, before);
        drop(first);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[test]
    fn replacement_epoch_foreign_registry_and_wrong_ledger_cannot_reuse_event_role() -> Result<()> {
        let owners = super::super::mutation_owner::MutationOwnerRegistry::default();
        let original = owners.claim(&scope().agent_id, 1)?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let lease = original.current_event_queries()?.register(
            &scope(),
            original.owner_epoch(),
            3,
            &ledger,
            limits(),
        )?;
        let replacement = owners.claim(&scope().agent_id, 1)?;
        assert!(original.current_event_queries().is_err());
        let registry = replacement.current_event_queries()?;
        assert!(registry
            .snapshot(&scope(), replacement.owner_epoch(), &ledger, limits())?
            .is_none());
        assert!(registry
            .validate_lease(&lease, &scope(), replacement.owner_epoch(), &ledger)
            .is_err());
        assert!(CurrentEventQueryRegistry::default()
            .validate_lease(&lease, &scope(), original.owner_epoch(), &ledger)
            .is_err());
        let wrong = InputEvidenceBudgetLedger::new(Default::default())?;
        assert!(registry
            .validate_lease(&lease, &scope(), original.owner_epoch(), &wrong)
            .is_err());
        drop(lease);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
}

#[cfg(test)]
#[path = "current_event_window_tests.rs"]
mod ordinary_window_tests;

#[cfg(test)]
mod birth_capture_joint_tests {
    use super::*;
    use crate::{runtime::tests::support::LifecycleHarness, types::AuditEvent};
    #[tokio::test]
    async fn birth_event_capture_debits_actual_lease_and_caller_before_arc_or_selector_copy(
    ) -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let _pause = runtime.pause_resource_observer_for_test().await;
        runtime
            .append_owned_runtime_audit_event(AuditEvent::legacy(
                "before_event_birth",
                serde_json::json!({"large":"x".repeat(256 * 1024)}),
            ))
            .await?;
        let reservation = runtime.inner.mutation_owner.reserve().await?;
        let first = reservation
            .capture_ready_snapshot()?
            .context("published root missing")?;
        let session =
            runtime.register_current_event_birth_session_from_snapshot(&reservation, &first)?;
        drop((first, reservation));
        let event = AuditEvent::legacy("after_event_birth", serde_json::json!({"actual":true}));
        let event_id = event.id.clone();
        runtime.append_owned_runtime_audit_event(event).await?;
        let root = runtime
            .inner
            .mutation_owner
            .capture()?
            .context("published root missing")?;
        let selected = runtime.capture_current_event_birth_session_from_snapshot_with_prior(
            &session,
            &root,
            (64 * 1024 * 1024, 65536),
            (0, 0),
        )?;
        assert_eq!(selected.selected.events.len(), 1);
        assert_eq!(selected.selected.events[0].locator.key.id, event_id);
        let actual = selected.material_estimate()?;
        assert!(
            actual.0 < 256 * 1024,
            "unselected historical body leaked into Event birth phase"
        );
        drop(selected);
        let prior = (256 * 1024u64, 32usize);
        let ledger = &runtime.inner.input_evidence_budget;
        let guard = ledger.register_owned_bytes(
            &session.lease.0.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: prior.0,
                metadata_estimate_bytes: 0,
            },
        )?;
        let held = vec![5u8; prior.0 as usize];
        let baseline = ledger.report()?.observed_bytes;
        let total = (usize::try_from(actual.0 + prior.0)?, actual.1 + prior.1);
        runtime.runtime_db().connection()?.execute_batch(
            "ALTER TABLE audit_events RENAME TO birth_joint_forbidden_events;
             ALTER TABLE agent_states RENAME TO birth_joint_forbidden_states;
             ALTER TABLE agent_identities RENAME TO birth_joint_forbidden_identities;",
        )?;
        for limits in [(total.0 - 1, total.1), (total.0, total.1 - 1)] {
            let error = runtime
                .capture_current_event_birth_session_from_snapshot_with_prior(
                    &session, &root, limits, prior,
                )
                .err()
                .context("joint Event capture admitted too much material")?;
            assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::ResourceExhausted)
                ),
                "{error:#}"
            );
            assert_eq!(ledger.report()?.observed_bytes, baseline);
        }
        let selected = runtime.capture_current_event_birth_session_from_snapshot_with_prior(
            &session, &root, total, prior,
        )?;
        assert_eq!(selected.material_estimate()?, actual);
        let weak = Arc::downgrade(&session.lease.0);
        drop(session);
        assert!(weak.upgrade().is_some());
        drop(selected);
        assert!(weak.upgrade().is_none());
        drop((guard, held));
        Ok(())
    }
    #[tokio::test]
    async fn birth_event_capture_rejects_foreign_owner_and_ledger_before_material_allocation(
    ) -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let _pause = runtime.pause_resource_observer_for_test().await;
        let reservation = runtime.inner.mutation_owner.reserve().await?;
        let root = reservation
            .capture_ready_snapshot()?
            .context("published root missing")?;
        let session =
            runtime.register_current_event_birth_session_from_snapshot(&reservation, &root)?;
        drop(reservation);
        let other = LifecycleHarness::new();
        let _pause_other = other.runtime().pause_resource_observer_for_test().await;
        let foreign_root = other.runtime().inner.mutation_owner.capture()?.unwrap();
        let ledger = &runtime.inner.input_evidence_budget;
        let baseline = ledger.report()?.observed_bytes;
        assert!(runtime
            .capture_current_event_birth_session_from_snapshot_with_prior(
                &session,
                &foreign_root,
                (64 * 1024 * 1024, 65536),
                (0, 0)
            )
            .is_err());
        let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
        let forged = CurrentEventBirthSession {
            cursor: session.cursor,
            lease: runtime
                .inner
                .mutation_owner
                .current_event_queries()?
                .register(
                    &session.lease.0.scope,
                    root.token.owner_epoch,
                    session.cursor,
                    &foreign,
                    (64 * 1024 * 1024, 65536),
                )?,
        };
        assert!(runtime
            .capture_current_event_birth_session_from_snapshot_with_prior(
                &forged,
                &root,
                (64 * 1024 * 1024, 65536),
                (0, 0)
            )
            .is_err());
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        drop(forged);
        assert_eq!(foreign.report()?.observed_bytes, 0);
        Ok(())
    }
}
