//! Live Input selectors; the registry owns no Root, Runtime, DB or body.
use super::{
    input_evidence_budget::{
        borrowed_json_material, serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_input_evidence::{InputEvidenceKey, InputEvidenceScope},
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
};

/// Actual allocation policy, independent of any body/read permission.
pub(in crate::runtime) fn actual_query_material_limits(
    ledger: &InputEvidenceBudgetLedger, caller: (usize, usize),
) -> Result<(usize, usize)> {
    let configured = ledger.limits();
    let exhausted = || InputQueryError::ResourceExhausted("actual ledger limits");
    let bytes = configured.history_per_agent_bytes.checked_add(configured.pending_per_agent_bytes)
        .ok_or_else(exhausted)?;
    let bytes = bytes.min(u64::try_from(caller.0).map_err(|_|exhausted())?).min(64*1024*1024);
    let nodes = configured.record_limit.min(u64::try_from(caller.1).map_err(|_|exhausted())?).min(65_536);
    Ok((usize::try_from(bytes).map_err(|_|exhausted())?,usize::try_from(nodes).map_err(|_|exhausted())?))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InputQueryError {
    #[error("Input query birth source incomplete")]
    Incomplete,
    #[error("Input query complete birth frontier not prepared")]
    FrontierNotPrepared,
    #[error("Input query resource exhausted: {0}")]
    ResourceExhausted(&'static str),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct InputQuerySelection {
    pub required_keys: BTreeSet<InputEvidenceKey>,
    pub birth_after: Option<u64>,
}
/// Same-view accepted metadata, never an authority to adopt a raw address.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct InputQueryCoverage {
    #[serde(default)]
    pub required_keys: BTreeSet<InputEvidenceKey>,
    pub birth_cursor: Option<u64>,
    pub birth_after: Option<u64>,
    #[serde(with = "birth_members")]
    pub born_key_seqs: BTreeMap<InputEvidenceKey, u64>,
}
// Struct keys have no JSON object-key encoding. Stream borrowed entries for
// material measurement; old empty maps remain readable but never imply Ready.
mod birth_members {
    use super::*;
    pub(super) fn serialize<S: serde::Serializer>(
        members: &BTreeMap<InputEvidenceKey, u64>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_seq(members.iter())
    }
    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<BTreeMap<InputEvidenceKey, u64>, D::Error> {
        struct Members;
        impl<'de> serde::de::Visitor<'de> for Members {
            type Value = BTreeMap<InputEvidenceKey, u64>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("Input birth member sequence or old empty object")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut members = BTreeMap::new();
                while let Some((key, seq)) = sequence.next_element::<(InputEvidenceKey, u64)>()? {
                    if members.insert(key, seq).is_some() {
                        return Err(serde::de::Error::custom("duplicate Input birth member"));
                    }
                }
                Ok(members)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                if map.next_key::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(serde::de::Error::custom("old Input birth map is not empty"));
                }
                Ok(BTreeMap::new())
            }
        }
        deserializer.deserialize_any(Members)
    }
}
impl InputQueryCoverage {
    /// Observation roots preserve typed unavailability; they are independent
    /// of the original business mandatory-body contract.
    pub(crate) fn observation_keys(&self) -> impl Iterator<Item = &InputEvidenceKey> {
        self.required_keys.iter().chain(self.born_key_seqs.keys())
    }
    pub(crate) fn validate_selection(&self, selection: &InputQuerySelection) -> Result<()> {
        let cursor = self.birth_cursor.ok_or(InputQueryError::Incomplete)?;
        if let Some(after) = selection.birth_after {
            ensure!(
                after <= cursor
                    && (after == cursor || self.birth_after.is_some_and(|cut| cut <= after)),
                InputQueryError::FrontierNotPrepared
            );
        }
        ensure!(
            self.born_key_seqs
                .values()
                .all(|seq| self.birth_after.is_some_and(|after| *seq > after) && *seq <= cursor),
            "Input query accepted birth metadata differs"
        );
        Ok(())
    }
}
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct InputQuerySpec<'a> {
    pub required_keys: &'a [InputEvidenceKey],
    pub birth_after: Option<u64>,
}
#[derive(Debug, Default)]
struct RegistryInner {
    next: AtomicU64,
    slots: Mutex<BTreeMap<u64, QuerySlot>>,
}
#[derive(Debug)]
struct QuerySlot {
    lease: Weak<LeaseInner>,
    // A dead slot may await lock release. Retain only its actual small map
    // allocation, never the entire now-dead selector allocation.
    _slot_budget: Arc<BudgetViewGuard>,
}
#[derive(Debug, Default, Clone)]
pub(in crate::runtime) struct InputQueryRegistry(Arc<RegistryInner>);
#[derive(Debug)]
struct LeaseInner {
    id: u64,
    scope: InputEvidenceScope,
    epoch: u64,
    selection: InputQuerySelection,
    material: (u64, usize),
    registry: Weak<RegistryInner>,
    budget: Arc<BudgetViewGuard>,
    _slot_budget: Arc<BudgetViewGuard>,
}
impl Drop for LeaseInner {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            if let Ok(mut slots) = registry.slots.try_lock() {
                slots.remove(&self.id);
            };
        }
    }
}
pub(crate) struct InputQueryLease(Arc<LeaseInner>);
impl InputQueryLease {
    pub(in crate::runtime) fn validate_material_binding(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.0.scope == *scope,
            "Input query material belongs to another scope"
        );
        self.0
            .budget
            .validate_owned_material_binding(scope, ledger, 0)?;
        Ok(())
    }
    pub(crate) fn selection(&self) -> &InputQuerySelection {
        &self.0.selection
    }
    pub(crate) fn material_estimate(&self) -> (u64, usize) {
        self.0.material
    }
}
#[derive(Debug)]
pub(crate) struct InputQuerySnapshot {
    scope: InputEvidenceScope,
    epoch: u64,
    pub(crate) selection: InputQuerySelection,
    _leases: Vec<Arc<LeaseInner>>,
    _budget: Arc<BudgetViewGuard>,
    material: (u64, usize),
    // Only query metadata from the original published Input; no Root/body backref.
    original_base: Option<OriginalInputQueryBase>,
    original_acquisition: Option<Arc<super::OriginalMutationAcquisitionMaterial>>,
    original_publication: Option<super::resident_input_evidence::OriginalInputPublicationMaterial>,
}
#[derive(Debug)]
struct OriginalInputQueryBase {
    coverage: InputQueryCoverage,
    material: (u64, usize),
    guard: Arc<BudgetViewGuard>,
}
impl InputQuerySnapshot {
    pub(crate) fn original_base(&self) -> Option<&InputQueryCoverage> {
        self.original_base.as_ref().map(|base| &base.coverage)
    }
    pub(in crate::runtime) fn with_original_acquisition(
        mut self, material: Arc<super::OriginalMutationAcquisitionMaterial>,
        ledger: &InputEvidenceBudgetLedger, limits: (usize,usize),
    ) -> Result<Self> {
        ensure!(self.original_acquisition.is_none(), "Input acquisition material already attached");
        material.validate(&self.scope,self.epoch,ledger)?;
        let limits=actual_query_material_limits(ledger,limits)?;
        let cost=material.material_estimate();
        let next=(self.material.0.checked_add(cost.0),self.material.1.checked_add(cost.1));
        let (Some(bytes),Some(nodes))=next else {
            anyhow::bail!(InputQueryError::ResourceExhausted("mutation acquisition prior"));
        };
        ensure!(bytes<=u64::try_from(limits.0)? && nodes<=limits.1,
            InputQueryError::ResourceExhausted("mutation acquisition prior"));
        self.original_acquisition=Some(material);
        self.material=(bytes,nodes);
        Ok(self)
    }
    pub(in crate::runtime) fn with_original_base(
        mut self,
        coverage: &InputQueryCoverage,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
    ) -> Result<Self> {
        let limits = actual_query_material_limits(ledger, limits)?;
        self.validate_binding(&self.scope, self.epoch, ledger)?;
        if coverage.birth_cursor.is_none() { return Ok(self); }
        coverage.validate_selection(&InputQuerySelection::default())?;
        let (raw, nodes) = borrowed_json_material(coverage)?;
        let nodes = nodes.checked_add(8).and_then(|n| n.checked_mul(8))
            .context(InputQueryError::ResourceExhausted("original query base"))?;
        let bytes = raw.checked_add(1024).and_then(|n| n.checked_mul(8))
            .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
            .context(InputQueryError::ResourceExhausted("original query base"))?;
        let next = (self.material.0.checked_add(bytes), self.material.1.checked_add(nodes));
        let (Some(bytes_total), Some(nodes_total)) = next else {
            anyhow::bail!(InputQueryError::ResourceExhausted("original query base"));
        };
        ensure!(bytes_total <= u64::try_from(limits.0.min(64*1024*1024))?
            && nodes_total <= limits.1.min(65_536),
            InputQueryError::ResourceExhausted("original query base"));
        // The actual guard precedes the first key/map/ID clone.
        let guard = observe(ledger, &self.scope, bytes)?;
        self.original_base = Some(OriginalInputQueryBase {
            coverage: coverage.clone(), material: (bytes, nodes), guard,
        });
        self.material = (bytes_total, nodes_total);
        Ok(self)
    }
    pub(in crate::runtime) fn with_original_publication(
        mut self, material: super::resident_input_evidence::OriginalInputPublicationMaterial,
        ledger: &InputEvidenceBudgetLedger, limits: (usize, usize),
    ) -> Result<Self> {
        ensure!(self.original_publication.is_none(), "Input publication material already attached");
        material.validate_binding(&self.scope, ledger)?;
        let limits = actual_query_material_limits(ledger, limits)?;
        let cost = material.material_estimate();
        let next = (self.material.0.checked_add(cost.0), self.material.1.checked_add(cost.1));
        let (Some(bytes), Some(nodes)) = next else { anyhow::bail!(InputQueryError::ResourceExhausted("publication metadata")); };
        ensure!(bytes<=limits.0 as u64 && nodes<=limits.1, InputQueryError::ResourceExhausted("publication metadata"));
        self.original_publication=Some(material);
        self.material=(bytes,nodes);
        Ok(self)
    }
    pub(crate) fn original_publication_matches(
        &self, inputs: &super::resident_input_evidence::ResidentInputEvidence,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<bool> {
        match &self.original_publication {
            Some(material) => material.matches_original(inputs, ledger),
            None => Ok(false),
        }
    }
    pub(crate) fn validate_binding(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == *scope && self.epoch == epoch,
            "Input role snapshot belongs to another owner"
        );
        self._budget
            .validate_owned_material_binding(scope, ledger, 0)?;
        if let Some(base) = &self.original_base {
            base.guard.validate_owned_material_binding(scope, ledger, base.material.0)?;
        }
        if let Some(material)=&self.original_acquisition { material.validate(scope,epoch,ledger)?; }
        if let Some(material) = &self.original_publication { material.validate_binding(scope, ledger)?; }
        Ok(())
    }
    /// Original actual selector allocations, including registry slots. The
    /// observer borrows their guards; it cannot mint a selector or body grant.
    pub(crate) fn visit_material(
        &self,
        mut observe: impl FnMut(&Arc<BudgetViewGuard>, u64, usize) -> Result<()>,
    ) -> Result<()> {
        let mut held = (0u64, 0usize);
        for lease in &self._leases {
            let slot = lease._slot_budget.owned_material_bytes()?;
            let bytes = lease
                .material
                .0
                .checked_sub(slot)
                .context("Input lease slot cost differs")?;
            let records = lease
                .material
                .1
                .checked_sub(16)
                .context("Input lease slot count differs")?;
            observe(&lease.budget, bytes, records)?;
            observe(&lease._slot_budget, slot, 16)?;
            held.0 = held
                .0
                .checked_add(lease.material.0)
                .context("Input held material overflow")?;
            held.1 = held
                .1
                .checked_add(lease.material.1)
                .context("Input held nodes overflow")?;
        }
        if let Some(material)=&self.original_acquisition {
            material.visit_material(&mut observe)?;
            let cost=material.material_estimate();
            held.0=held.0.checked_add(cost.0).context("Input acquisition prior bytes overflow")?;
            held.1=held.1.checked_add(cost.1).context("Input acquisition prior nodes overflow")?;
        }
        if let Some(base) = &self.original_base {
            observe(&base.guard, base.material.0, base.material.1)?;
            held.0 = held.0.checked_add(base.material.0).context("Input base bytes overflow")?;
            held.1 = held.1.checked_add(base.material.1).context("Input base nodes overflow")?;
        }
        if let Some(material) = &self.original_publication {
            material.visit_material(&mut observe)?;
            let cost=material.material_estimate();
            held.0=held.0.checked_add(cost.0).context("Input publication prior bytes overflow")?;
            held.1=held.1.checked_add(cost.1).context("Input publication prior nodes overflow")?;
        }
        observe(
            &self._budget,
            self.material
                .0
                .checked_sub(held.0)
                .context("Input copied material differs")?,
            self.material
                .1
                .checked_sub(held.1)
                .context("Input copied node count differs")?,
        )
    }
    pub(in crate::runtime) fn material_estimate(&self) -> (u64, usize) {
        self.material
    }
}
fn add_key(bytes: &mut u64, count: &mut usize, key: &InputEvidenceKey) -> Result<()> {
    ensure!(!key.id.is_empty(), "Input query selector ID is empty");
    *bytes = bytes
        .checked_add(serialized_bytes(key)?)
        .and_then(|n| n.checked_add(256))
        .context("Input query selector bytes overflow")?;
    *count = count
        .checked_add(3)
        .context("Input query selector nodes overflow")?;
    Ok(())
}
fn measured(bytes: u64, count: usize, limits: (usize, usize)) -> Result<(u64, usize)> {
    let bytes = bytes
        .checked_mul(8)
        .context("Input query copies overflow")?;
    let count = count
        .checked_mul(8)
        .context("Input query node copies overflow")?;
    ensure!(
        bytes <= u64::try_from(limits.0.min(64 * 1024 * 1024))? && count <= limits.1.min(65_536),
        InputQueryError::ResourceExhausted("selectors")
    );
    Ok((bytes, count))
}
fn observe(
    ledger: &InputEvidenceBudgetLedger,
    scope: &InputEvidenceScope,
    bytes: u64,
) -> Result<Arc<BudgetViewGuard>> {
    Ok(ledger.register_owned_bytes(
        scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: bytes,
        },
    )?)
}
impl InputQueryRegistry {
    pub(in crate::runtime) fn snapshot_if_live(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
    ) -> Result<Option<InputQuerySnapshot>> {
        let mut slots = self
            .0
            .slots
            .lock()
            .map_err(|_| anyhow::anyhow!("Input query registry poisoned"))?;
        slots.retain(|_, slot| slot.lease.strong_count() > 0);
        let live = slots.values().any(|slot| {
            slot.lease
                .upgrade()
                .is_some_and(|lease| lease.scope == *scope && lease.epoch == epoch)
        });
        drop(slots);
        if !live {
            return Ok(None);
        }
        // A concurrent lease release may narrow this snapshot to empty. It
        // cannot authorize a key or make a dead registration live again.
        self.snapshot(scope, epoch, ledger, limits).map(Some)
    }
    pub(in crate::runtime) fn validate_lease(
        &self,
        lease: &InputQueryLease,
        scope: &InputEvidenceScope,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            lease.0.scope == *scope
                && lease.0.epoch == epoch
                && Weak::ptr_eq(&lease.0.registry, &Arc::downgrade(&self.0)),
            "Input query belongs to another owner or registry"
        );
        lease
            .0
            .budget
            .validate_owned_material_binding(scope, ledger, 0)?;
        Ok(())
    }
    pub(in crate::runtime) fn register(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        spec: InputQuerySpec<'_>,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
    ) -> Result<InputQueryLease> {
        ensure!(
            epoch > 0 && scope.identity_incarnation > 0 && !scope.agent_id.is_empty(),
            "Input query requires an exact actual owner"
        );
        let mut bytes = serialized_bytes(scope)?
            .checked_add(1024)
            .context("Input query scope overflow")?;
        let mut count = 8usize;
        for key in spec.required_keys {
            add_key(&mut bytes, &mut count, key)?;
        }
        let mut slots = self
            .0
            .slots
            .lock()
            .map_err(|_| anyhow::anyhow!("Input query registry poisoned"))?;
        slots.retain(|_, slot| slot.lease.strong_count() > 0);
        let selectors = measured(bytes, count, limits)?;
        let slot_bytes = serialized_bytes(scope)?
            .checked_mul(4)
            .and_then(|n| n.checked_add(512))
            .context("Input query actual slot scope overflow")?;
        let cost = (
            selectors
                .0
                .checked_add(slot_bytes)
                .context("Input query slot bytes overflow")?,
            selectors
                .1
                .checked_add(16)
                .context("Input query slot count overflow")?,
        );
        ensure!(
            cost.0 <= u64::try_from(limits.0.min(64 * 1024 * 1024))?
                && cost.1 <= limits.1.min(65_536),
            InputQueryError::ResourceExhausted("slot")
        );
        let budget = observe(ledger, scope, selectors.0)?;
        let slot_budget = observe(ledger, scope, slot_bytes)?;
        let id = self
            .0
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| anyhow::anyhow!("Input query lease ID exhausted"))?;
        let lease = Arc::new(LeaseInner {
            id,
            scope: scope.clone(),
            epoch,
            selection: InputQuerySelection {
                required_keys: spec.required_keys.iter().cloned().collect(),
                birth_after: spec.birth_after,
            },
            material: cost,
            registry: Arc::downgrade(&self.0),
            budget,
            _slot_budget: slot_budget.clone(),
        });
        slots.insert(
            id,
            QuerySlot {
                lease: Arc::downgrade(&lease),
                _slot_budget: slot_budget,
            },
        );
        Ok(InputQueryLease(lease))
    }
    /// Borrow/count all matching live selectors before the merged sets or Arc
    /// slots allocate. The snapshot keeps the original leases through use.
    pub(in crate::runtime) fn snapshot(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
        limits: (usize, usize),
    ) -> Result<InputQuerySnapshot> {
        let mut slots = self
            .0
            .slots
            .lock()
            .map_err(|_| anyhow::anyhow!("Input query registry poisoned"))?;
        slots.retain(|_, slot| slot.lease.strong_count() > 0);
        let mut bytes = serialized_bytes(scope)?
            .checked_add(1024)
            .context("Input query snapshot scope overflow")?;
        let mut count = 8usize;
        let mut held = (0u64, 0usize);
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
            held.0 = held
                .0
                .checked_add(lease.material.0)
                .context("Input held query bytes overflow")?;
            held.1 = held
                .1
                .checked_add(lease.material.1)
                .context("Input held query nodes overflow")?;
            bytes = bytes
                .checked_add(128)
                .context("Input query lease slot bytes overflow")?;
            count = count
                .checked_add(1)
                .context("Input query lease slot count overflow")?;
            for key in &lease.selection.required_keys {
                add_key(&mut bytes, &mut count, key)?;
            }
        }
        slots.retain(|_, slot| slot.lease.strong_count() > 0);
        let copied = measured(bytes, count, limits)?;
        let material = (
            held.0
                .checked_add(copied.0)
                .context("Input snapshot combined bytes overflow")?,
            held.1
                .checked_add(copied.1)
                .context("Input snapshot combined nodes overflow")?,
        );
        ensure!(
            material.0 <= u64::try_from(limits.0.min(64 * 1024 * 1024))?
                && material.1 <= limits.1.min(65_536),
            InputQueryError::ResourceExhausted("held and copied selectors")
        );
        let budget = observe(ledger, scope, copied.0)?;
        let mut selection = InputQuerySelection::default();
        let mut leases = Vec::new();
        for slot in slots.values() {
            let Some(lease) = slot.lease.upgrade() else {
                continue;
            };
            if lease.scope != *scope || lease.epoch != epoch {
                continue;
            }
            selection
                .required_keys
                .extend(lease.selection.required_keys.iter().cloned());
            if let Some(cursor) = lease.selection.birth_after {
                selection.birth_after = Some(
                    selection
                        .birth_after
                        .map_or(cursor, |prior| prior.min(cursor)),
                );
            }
            leases.push(lease);
        }
        slots.retain(|_, slot| slot.lease.strong_count() > 0);
        Ok(InputQuerySnapshot {
            original_acquisition: None,
            original_base: None,
            original_publication: None,
            scope: scope.clone(),
            epoch,
            selection,
            _leases: leases,
            _budget: budget,
            material,
        })
    }
}

#[cfg(test)]
#[path = "input_queries_tests.rs"]
mod tests;

pub(crate) struct InputBirthSession {
    pub(crate) cursor: u64,
    lease: InputQueryLease,
}
impl InputBirthSession {
    /// Incomplete remains explicit before any selected allocation or prior loan.
    pub(in crate::runtime) fn validate_held_material(
        &self,
        runtime: &super::RuntimeHandle,
        root: &Arc<super::mutation_owner::PostureObservation>,
    ) -> Result<()> {
        let input = root
            .input_evidence
            .as_ref()
            .ok_or(InputQueryError::Incomplete)?;
        input
            .query_coverage()
            .validate_selection(self.lease.selection())?;
        let registry = runtime.inner.mutation_owner.input_queries()?;
        let scope = input.scope();
        let ledger = &runtime.inner.input_evidence_budget;
        ensure!(
            root.state.id == runtime.inner.mutation_owner.agent_id()
                && scope.agent_id == root.state.id
                && scope.identity_incarnation == root.token.identity_incarnation
                && root.token.identity_incarnation == runtime.inner.identity_incarnation
                && root.token.owner_epoch == runtime.inner.mutation_owner.owner_epoch()
                && input.shares_query_ledger(ledger),
            "Input birth held scope or ledger differs"
        );
        registry.validate_lease(&self.lease, scope, root.token.owner_epoch, ledger)?;
        ensure!(
            self.lease.selection().birth_after == Some(self.cursor),
            InputQueryError::FrontierNotPrepared
        );
        self.lease
            .0
            ._slot_budget
            .validate_owned_material_binding(scope, ledger, 0)?;
        let slot_bytes = self.lease.0._slot_budget.owned_material_bytes()?;
        let lease_bytes = self
            .lease
            .material_estimate()
            .0
            .checked_sub(slot_bytes)
            .context("Input birth slot exceeds original material")?;
        self.lease
            .0
            .budget
            .validate_owned_material_binding(scope, ledger, lease_bytes)?;
        let slots = registry
            .0
            .slots
            .lock()
            .map_err(|_| anyhow::anyhow!("Input query registry poisoned"))?;
        let slot = slots
            .get(&self.lease.0.id)
            .context("Input birth original slot absent")?;
        ensure!(
            Weak::ptr_eq(&slot.lease, &Arc::downgrade(&self.lease.0))
                && Arc::ptr_eq(&slot._slot_budget, &self.lease.0._slot_budget),
            "Input birth original slot differs"
        );
        slot._slot_budget
            .validate_owned_material_binding(scope, ledger, slot_bytes)?;
        Ok(())
    }
    pub(in crate::runtime) fn material_estimate(&self) -> (u64, usize) {
        self.lease.material_estimate()
    }
}

#[cfg(test)]
mod held_material_tests {
    use super::*;
    use crate::runtime::tests::support::LifecycleHarness;

    #[tokio::test]
    async fn input_held_validation_preserves_actual_incomplete_without_a_new_guard() -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let _pause = runtime.pause_resource_observer_for_test().await;
        let held = runtime.inner.mutation_owner.reserve().await?;
        let root = held.capture_ready_snapshot()?.unwrap();
        let scope = root.tasks.as_ref().unwrap().scope();
        let ledger = &runtime.inner.input_evidence_budget;
        // A real registration is only a read role. It does not create Source
        // completeness or install a synthetic Ready publication.
        let lease = runtime.inner.mutation_owner.input_queries()?.register(
            scope,
            root.token.owner_epoch,
            InputQuerySpec {
                birth_after: Some(0),
                ..Default::default()
            },
            ledger,
            (64 * 1024 * 1024, 65_536),
        )?;
        let session = InputBirthSession { cursor: 0, lease };
        drop(held);
        let before = ledger.report()?.observed_bytes;
        let strong = Arc::strong_count(&root);
        runtime.runtime_db().connection()?.execute_batch(
            "ALTER TABLE messages RENAME TO held_input_forbidden_messages;
             ALTER TABLE agent_states RENAME TO held_input_forbidden_states;",
        )?;
        let error = session
            .validate_held_material(runtime, &root)
            .err()
            .context("read role turned an incomplete Input into a complete baseline")?;
        assert!(
            matches!(
                error.downcast_ref::<InputQueryError>(),
                Some(InputQueryError::Incomplete)
            ),
            "{error:#}"
        );
        assert_eq!(Arc::strong_count(&root), strong);
        assert_eq!(ledger.report()?.observed_bytes, before);
        Ok(())
    }
}
/// Selected original records and metadata only; the lease remains live across
/// actual asynchronous use without retaining a Root or Runtime.
pub(crate) struct CapturedInputQuery {
    pub(crate) input: super::resident_input_evidence::CapturedInputEvidence,
    pub(crate) query: InputQueryCoverage,
    _lease: Arc<LeaseInner>,
    _owned: Arc<BudgetViewGuard>,
    material: (u64, usize),
}
impl CapturedInputQuery {
    /// Borrow the original selected ledger without any Runtime or Root lookup.
    pub(in crate::runtime) fn actual_material_ledger(
        &self,
        token: &super::mutation_owner::PublicationToken,
    ) -> Result<&InputEvidenceBudgetLedger> {
        let scope = &self.input.scope;
        ensure!(
            self._lease.scope == *scope
                && self._lease.epoch == token.owner_epoch
                && scope.identity_incarnation == token.identity_incarnation,
            "captured Input report scope or owner differs"
        );
        let ledger = self
            .input
            .budget
            .as_ref()
            .context("captured Input has no actual ledger")?;
        self._owned
            .validate_owned_material_binding(scope, ledger, 0)?;
        self._lease
            .budget
            .validate_owned_material_binding(scope, ledger, 0)?;
        self._lease
            ._slot_budget
            .validate_owned_material_binding(scope, ledger, 0)?;
        for view in &self.input.budget_views {
            view.validate_owned_material_binding(scope, ledger, 0)?;
        }
        self.query.validate_selection(&self._lease.selection)?;
        Ok(ledger)
    }
    /// Own retained lease, query metadata and selected Input; no caller prior.
    pub(crate) fn material_estimate(&self) -> (u64, usize) {
        self.material
    }
}
impl super::RuntimeHandle {
    pub(in crate::runtime) fn register_input_birth_session_from_snapshot(
        &self,
        reservation: &super::mutation_owner::MutationReservation,
        root: &Arc<super::mutation_owner::PostureObservation>,
    ) -> Result<InputBirthSession> {
        self.register_input_birth_session_from_snapshot_with_prior(
            reservation,
            root,
            (usize::MAX, usize::MAX),
            (0, 0),
        )
    }
    pub(in crate::runtime) fn register_input_birth_session_from_snapshot_with_prior(
        &self,
        reservation: &super::mutation_owner::MutationReservation,
        root: &Arc<super::mutation_owner::PostureObservation>,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<InputBirthSession> {
        reservation.validate_owner(&self.inner.mutation_owner)?;
        let current = reservation
            .capture_ready_snapshot()?
            .context("Input query root unpublished")?;
        ensure!(
            Arc::ptr_eq(&current, root),
            "Input query registration publication changed"
        );
        let input = root
            .input_evidence
            .as_ref()
            .ok_or(InputQueryError::Incomplete)?;
        ensure!(
            root.state.id == self.inner.mutation_owner.agent_id()
                && input.scope().agent_id == root.state.id
                && input.scope().identity_incarnation == root.token.identity_incarnation
                && root.token.identity_incarnation == self.inner.identity_incarnation
                && root.token.owner_epoch == reservation.owner_epoch()
                && input.shares_query_ledger(&self.inner.input_evidence_budget),
            "Input query held publication scope or ledger differs"
        );
        let cursor = input
            .query_coverage()
            .birth_cursor
            .ok_or(InputQueryError::Incomplete)?;
        let remaining = super::run_birth_sessions::remaining_material(
            &self.inner.input_evidence_budget,
            limits,
            prior,
        )?;
        let lease = self.inner.mutation_owner.input_queries()?.register(
            input.scope(),
            root.token.owner_epoch,
            InputQuerySpec {
                required_keys: &[],
                birth_after: Some(cursor),
            },
            &self.inner.input_evidence_budget,
            remaining,
        )?;
        Ok(InputBirthSession { cursor, lease })
    }
    pub(in crate::runtime) fn capture_input_birth_session_from_snapshot(
        &self,
        session: &InputBirthSession,
        root: &Arc<super::mutation_owner::PostureObservation>,
    ) -> Result<CapturedInputQuery> {
        self.capture_input_birth_session_from_snapshot_with_prior(session, root, (0, 0))
    }
    /// The coherent multi-domain caller debits its already-held material before
    /// this Input selection; the original Input lease is always added here.
    pub(in crate::runtime) fn capture_input_birth_session_from_snapshot_with_prior(
        &self,
        session: &InputBirthSession,
        root: &Arc<super::mutation_owner::PostureObservation>,
        prior: (u64, usize),
    ) -> Result<CapturedInputQuery> {
        self.capture_input_birth_session_from_snapshot_with_limits_and_prior(
            session,
            root,
            (usize::MAX, usize::MAX),
            prior,
        )
    }
    pub(in crate::runtime) fn capture_input_birth_session_from_snapshot_with_limits_and_prior(
        &self,
        session: &InputBirthSession,
        root: &Arc<super::mutation_owner::PostureObservation>,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<CapturedInputQuery> {
        session.validate_held_material(self, root)?;
        let input = root
            .input_evidence
            .as_ref()
            .ok_or(InputQueryError::Incomplete)?;
        let (input, query, owned, material) =
            input.capture_query_selection_with_material(&session.lease, limits, prior)?;
        Ok(CapturedInputQuery {
            input,
            query,
            _lease: session.lease.0.clone(),
            _owned: owned,
            material,
        })
    }
}

#[cfg(test)]
pub(crate) fn source_roles_for_fixture(
    owner: &super::mutation_owner::MutationOwner,
    scope: &InputEvidenceScope,
    spec: InputQuerySpec<'_>,
    ledger: &InputEvidenceBudgetLedger,
    limits: (usize, usize),
) -> Result<(
    super::mutation_owner::MutationReservation,
    InputQueryLease,
    InputQuerySnapshot,
)> {
    let reservation = owner.try_reserve()?;
    let lease =
        owner
            .input_queries()?
            .register(scope, reservation.owner_epoch(), spec, ledger, limits)?;
    let roles = reservation
        .input_query_roles(scope, ledger, limits)?
        .context("actual fixture role missing")?;
    Ok((reservation, lease, roles))
}

#[cfg(test)]
pub(crate) fn source_capture_for_fixture(
    input: &super::resident_input_evidence::ResidentInputEvidence,
    lease: &InputQueryLease,
) -> Result<(
    super::resident_input_evidence::CapturedInputEvidence,
    InputQueryCoverage,
    Arc<BudgetViewGuard>,
)> {
    input.capture_query_selection(lease, (0, 0))
}

#[cfg(test)]
pub(crate) fn source_capture_with_limits_for_fixture(
    input: &super::resident_input_evidence::ResidentInputEvidence,
    lease: &InputQueryLease,
    limits: (usize, usize),
    prior: (u64, usize),
) -> Result<(
    super::resident_input_evidence::CapturedInputEvidence,
    InputQueryCoverage,
    Arc<BudgetViewGuard>,
    (u64, usize),
)> {
    input.capture_query_selection_with_material(lease, limits, prior)
}
#[cfg(test)]
mod unified_capture_boundary_tests {
    use super::*;
    use crate::runtime::{
        run_birth_sessions::{RunBirthBaseline, RunBirthSessions},
        tests::support::LifecycleHarness,
    };

    #[tokio::test]
    async fn unified_capture_keeps_incomplete_input_before_any_selected_allocation() -> Result<()> {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let _pause = runtime.pause_resource_observer_for_test().await;
        let held = runtime.inner.mutation_owner.reserve().await?;
        let root = held
            .capture_ready_snapshot()?
            .context("actual Root missing")?;
        let tasks = runtime.register_task_birth_session_from_snapshot(&held, &root)?;
        let events = runtime.register_current_event_birth_session_from_snapshot(&held, &root)?;
        let ledger = &runtime.inner.input_evidence_budget;
        let scope = root.tasks.as_ref().unwrap().scope();
        // A real read role preserves the unprepared Input frontier. It creates
        // no Ready domain, body, synthetic publication or successful baseline.
        let lease = runtime.inner.mutation_owner.input_queries()?.register(
            scope,
            root.token.owner_epoch,
            InputQuerySpec {
                birth_after: Some(0),
                ..Default::default()
            },
            ledger,
            (64 * 1024 * 1024, 65536),
        )?;
        let sessions = RunBirthSessions {
            baseline: RunBirthBaseline {
                turn_index: root.state.turn_index,
                total_input_tokens: root.state.total_input_tokens,
                total_output_tokens: root.state.total_output_tokens,
                total_model_rounds: root.state.total_model_rounds,
                task_cursor: tasks.cursor(),
                input_cursor: 0,
                event_cursor: events.cursor,
            },
            tasks,
            inputs: InputBirthSession { cursor: 0, lease },
            events,
        };
        drop(held);
        let baseline = ledger.report()?.observed_bytes;
        let strong = Arc::strong_count(&root);
        runtime.runtime_db().connection()?.execute_batch(
            "ALTER TABLE agent_states RENAME TO unified_forbidden_states;
             ALTER TABLE tasks RENAME TO unified_forbidden_tasks;
             ALTER TABLE messages RENAME TO unified_forbidden_messages;
             ALTER TABLE audit_events RENAME TO unified_forbidden_events;",
        )?;
        for limits in [(1, 1), (64 * 1024 * 1024, 65536)] {
            let error = runtime
                .capture_run_birth_sessions_from_snapshot(&sessions, &root, limits)
                .err()
                .context("unified read upgraded an incomplete Input frontier")?;
            assert!(
                matches!(
                    error.downcast_ref::<InputQueryError>(),
                    Some(InputQueryError::Incomplete)
                ),
                "{error:#}"
            );
            assert_eq!(ledger.report()?.observed_bytes, baseline);
            assert_eq!(Arc::strong_count(&root), strong);
        }
        let error = runtime
            .capture_run_birth_sessions(&sessions)
            .err()
            .context("unified current read upgraded the Input frontier")?;
        assert!(
            matches!(
                error.downcast_ref::<InputQueryError>(),
                Some(InputQueryError::Incomplete)
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        assert_eq!(Arc::strong_count(&root), strong);
        Ok(())
    }
}

#[cfg(test)]
#[path = "input_query_original_base_tests.rs"]
mod original_base_tests;

#[cfg(test)]
#[path = "input_acquisition_source_tests.rs"]
mod acquisition_source_tests;
