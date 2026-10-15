//! Actual resident allocation admission; it grants no Work/body membership.
use super::*;
use crate::runtime::input_evidence_budget::OwnedCompletionBudget;
use anyhow::Context;

/// Only the actual Host-address registration below can provide this borrowed
/// origin. It has no record, lookup, mutation or business authority.
pub(crate) struct OriginalWorkHostAddress<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    guard: &'a Arc<BudgetViewGuard>,
}
/// Retains the same actual guard after the original observer admits this new
/// scope/keeper metadata. Private fields prevent DTO/boolean reconstruction.
pub(crate) struct OriginalWorkHostAddressLoan {
    scope: InputEvidenceScope,
    ledger: InputEvidenceBudgetLedger,
    guard: Arc<BudgetViewGuard>,
}
impl OriginalWorkHostAddress<'_> {
    pub(crate) fn validate_binding(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        guard: &Arc<BudgetViewGuard>,
    ) -> Result<()> {
        ensure!(
            self.scope == scope
                && self.ledger.same_ledger(ledger)
                && Arc::ptr_eq(self.guard, guard),
            "original Work Host-address origin differs"
        );
        guard.validate_host_owned_material_binding(ledger, 0)?;
        Ok(())
    }
    pub(crate) fn retained_material_estimate(&self) -> Result<(u64, usize)> {
        let (bytes, nodes) =
            crate::runtime::input_evidence_budget::borrowed_json_material(self.scope)?;
        let nodes = nodes
            .checked_add(8)
            .context("Work Host-address keeper nodes overflow")?;
        Ok((
            bytes
                .checked_add(
                    u64::try_from(nodes)?
                        .checked_mul(256)
                        .context("Work Host-address keeper bytes overflow")?,
                )
                .and_then(|value| {
                    value.checked_add(std::mem::size_of::<OriginalWorkHostAddressLoan>() as u64)
                })
                .context("Work Host-address keeper bytes overflow")?,
            nodes,
        ))
    }
    pub(crate) fn retain(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        guard: &Arc<BudgetViewGuard>,
    ) -> Result<OriginalWorkHostAddressLoan> {
        self.validate_binding(scope, ledger, guard)?;
        Ok(OriginalWorkHostAddressLoan {
            scope: scope.clone(),
            ledger: ledger.clone(),
            guard: guard.clone(),
        })
    }
}
impl OriginalWorkHostAddressLoan {
    pub(crate) fn validate_binding(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == *scope && self.ledger.same_ledger(ledger),
            "retained Work Host-address origin differs"
        );
        self.guard.validate_host_owned_material_binding(ledger, 0)?;
        Ok(())
    }
    pub(crate) fn matches_guard(&self, guard: &Arc<BudgetViewGuard>) -> bool {
        Arc::ptr_eq(&self.guard, guard)
    }
    #[cfg(test)]
    pub(crate) fn guard_for_test(&self) -> &Arc<BudgetViewGuard> {
        &self.guard
    }
}
impl std::fmt::Debug for OriginalWorkHostAddressLoan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OriginalWorkHostAddressLoan")
            .finish_non_exhaustive()
    }
}
pub(crate) type WorkMaterialObserver<'a> = dyn FnMut(&Arc<BudgetViewGuard>, usize, usize, Option<&OriginalWorkHostAddress<'_>>) -> Result<()>
    + 'a;

struct Material<'a> {
    bytes: usize,
    records: usize,
    byte_limit: usize,
    record_limit: usize,
    owned: Option<OwnedCompletionBudget>,
    observer: Option<&'a mut WorkMaterialObserver<'a>>,
}
impl<'a> Material<'a> {
    fn new(
        domain: &ResidentWorkDomain,
        ledger: Option<&InputEvidenceBudgetLedger>,
        category: InputMemoryCategory,
        observer: Option<&'a mut WorkMaterialObserver<'a>>,
    ) -> Result<Self> {
        Self::new_with_prior(
            domain,
            ledger,
            category,
            observer,
            (0, 0),
            (domain.byte_limit, domain.record_limit),
        )
    }
    fn new_with_prior(
        domain: &ResidentWorkDomain,
        ledger: Option<&InputEvidenceBudgetLedger>,
        category: InputMemoryCategory,
        observer: Option<&'a mut WorkMaterialObserver<'a>>,
        prior: (usize, usize),
        limits: (usize, usize),
    ) -> Result<Self> {
        let scope_bytes = usize::try_from(serialized_bytes(&domain.scope)?)?
            .checked_mul(8)
            .context("Work resident address overflow")?;
        let address_bytes = scope_bytes
            .checked_add(512)
            .context("Work address overflow")?;
        let base = scope_bytes
            .checked_add(4096)
            .context("Work scratch overflow")?;
        let mut material = Self {
            bytes: prior.0,
            records: prior.1,
            byte_limit: domain.byte_limit.min(64 * 1024 * 1024).min(limits.0),
            record_limit: domain.record_limit.min(65_536).min(limits.1),
            owned: None,
            observer,
        };
        // The first actual Host metadata view covers scoped ledger-key creation.
        // It grants no namespace; both this transient address and the retained
        // scoped view debit the same original phase before its String copy.
        let next = material.bounds(address_bytes, 1)?;
        let address_estimate = OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: u64::try_from(address_bytes)?,
        };
        let address = ledger
            .map(|ledger| ledger.register_host_owned_bytes(category, address_estimate))
            .transpose()?;
        if let (Some(observer), Some(address)) = (&mut material.observer, &address) {
            let origin = OriginalWorkHostAddress {
                scope: &domain.scope,
                ledger: ledger.context("Work address has no actual ledger")?,
                guard: address,
            };
            observer(address, address_bytes, 1, Some(&origin))?;
        }
        (material.bytes, material.records) = next;
        material.owned = ledger
            .map(|ledger| {
                ledger.reserve_owned_completion(
                    &domain.scope,
                    category,
                    OwnedBytesEstimate::default(),
                )
            })
            .transpose()?;
        material.charge(base, 1)?;
        // The retained scoped estimate now covers its own address. SQL observers
        // keep the first view through their original read phase, conservatively.
        drop(address);
        Ok(material)
    }
    fn bounds(&self, bytes: usize, records: usize) -> Result<(usize, usize)> {
        let bytes = self
            .bytes
            .checked_add(bytes)
            .context("Work resident material overflow")?;
        let records = self
            .records
            .checked_add(records)
            .context("Work resident count overflow")?;
        ensure!(
            bytes <= self.byte_limit && records <= self.record_limit,
            WorkReadError::ResourceExhausted("resident temporary/selected material")
        );
        Ok((bytes, records))
    }
    fn charge(&mut self, bytes: usize, records: usize) -> Result<()> {
        let next = self.bounds(bytes, records)?;
        if let Some(owned) = &self.owned {
            // Actual view growth precedes the copy; the original SQL/Memory
            // observer debits this already registered allocation, never twice.
            owned.observe_material(0, u64::try_from(bytes)?);
            if let Some(observer) = &mut self.observer {
                observer(&owned.guard(), bytes, records, None)?;
            }
        } else {
            ensure!(
                self.observer.is_none(),
                "Work observer has no actual ledger"
            );
        }
        (self.bytes, self.records) = next;
        Ok(())
    }
    fn shared(&mut self, bytes: usize, records: usize) -> Result<()> {
        // Existing immutable body allocation is bounded once, not re-owned.
        (self.bytes, self.records) = self.bounds(bytes, records)?;
        Ok(())
    }
    fn encoded(&mut self, value: &impl Serialize, records: usize, copies: usize) -> Result<()> {
        let bytes = usize::try_from(serialized_bytes(value)?)?
            .checked_add(
                records
                    .checked_mul(128)
                    .context("Work native metadata overflow")?,
            )
            .and_then(|n| n.checked_mul(copies))
            .context("Work metadata copies overflow")?;
        self.charge(
            bytes,
            records
                .checked_mul(copies)
                .context("Work metadata count overflow")?,
        )
    }
    fn guard(&self) -> Option<Arc<BudgetViewGuard>> {
        self.owned.as_ref().map(OwnedCompletionBudget::guard)
    }
}

pub(super) fn coverage_nodes(coverage: &WorkCoverage, records: usize) -> Result<usize> {
    let mut count = records
        .checked_add(1)
        .context("Work coverage nodes overflow")?;
    for n in [
        coverage.live_work_ids.len(),
        coverage.unresolved_wait_ids.len(),
        coverage.active_frame_ids.len(),
        coverage.mandatory_roots.len(),
        coverage.observations.len(),
        coverage.nonopen.keys.len(),
        coverage.latest_results.len(),
        coverage
            .latest_results
            .values()
            .filter(|v| v.is_some())
            .count(),
    ] {
        count = count
            .checked_add(n)
            .context("Work coverage count overflow")?;
    }
    count = count
        .checked_add(coverage.legacy_delivery_heads.len())
        .context("Work Delivery map count overflow")?;
    for heads in coverage.legacy_delivery_heads.values() {
        count = count
            .checked_add(heads.metadata_records()?)
            .context("Work Delivery source count overflow")?;
    }
    for relation in [
        &coverage.turn_wait_keys,
        &coverage.task_wait_keys,
        &coverage.trigger_wait_keys,
    ] {
        count = count
            .checked_add(relation.len())
            .context("Work relations overflow")?;
        for members in relation.values() {
            count = count
                .checked_add(members.len())
                .context("Work relation members overflow")?;
        }
    }
    Ok(count)
}
pub(super) fn coverage_preflight(
    domain: &ResidentWorkDomain,
    coverage: &WorkCoverage,
    record_count: usize,
    category: InputMemoryCategory,
) -> Result<Option<Arc<BudgetViewGuard>>> {
    let mut material = Material::new(domain, domain.budget.as_ref(), category, None)?;
    material.encoded(
        &(&domain.scope, coverage),
        coverage_nodes(coverage, record_count)?,
        8,
    )?;
    // Removed keys may be absent from final coverage but the old record map
    // is still cloned before removal; account those actual String copies too.
    for key in domain.records.keys() {
        material.encoded(key, 1, 2)?;
    }
    Ok(material.guard())
}

pub(super) fn candidate_preflight(
    domain: &ResidentWorkDomain,
) -> Result<Option<Arc<BudgetViewGuard>>> {
    let Some(ledger) = domain.budget.as_ref() else {
        return Ok(None);
    };
    let mut material = Material::new(domain, Some(ledger), InputMemoryCategory::Candidate, None)?;
    // The body view shares existing Arc allocations, but its ledger membership,
    // unique-address tree and guard slots are distinct owned containers.
    let nodes = domain
        .records
        .len()
        .checked_add(1)
        .context("Work candidate nodes overflow")?;
    material.charge(
        nodes
            .checked_mul(4 * 256)
            .context("Work candidate metadata overflow")?,
        nodes
            .checked_mul(4)
            .context("Work candidate count overflow")?,
    )?;
    Ok(material.guard())
}

impl ResidentWorkDomain {
    pub(crate) fn capture_with_material<'a>(
        &self,
        request: &WorkCaptureRequest,
        ledger: &InputEvidenceBudgetLedger,
        observer: &'a mut WorkMaterialObserver<'a>,
    ) -> Result<CapturedWork> {
        if let Some(resident) = &self.budget {
            ensure!(resident.same_ledger(ledger), "Work capture ledger differs");
        }
        self.capture_accounted(request, Some(ledger), Some(observer))
    }
    pub(super) fn capture_accounted<'a>(
        &self,
        request: &WorkCaptureRequest,
        ledger: Option<&InputEvidenceBudgetLedger>,
        observer: Option<&'a mut WorkMaterialObserver<'a>>,
    ) -> Result<CapturedWork> {
        self.capture_accounted_with_prior(
            request,
            ledger,
            observer,
            (0, 0),
            (self.byte_limit, self.record_limit),
            None,
        )
        .map(|(captured, _)| captured)
    }
    /// Debit already-held allocations before any selected metadata is cloned.
    /// The prior grants no lookup or body membership and is never re-registered.
    pub(in crate::runtime) fn capture_with_prior(
        &self,
        request: &WorkCaptureRequest,
        ledger: &InputEvidenceBudgetLedger,
        prior: (usize, usize),
        limits: (usize, usize),
    ) -> Result<(CapturedWork, (usize, usize))> {
        if let Some(resident) = &self.budget {
            ensure!(resident.same_ledger(ledger), "Work prior ledger differs");
        }
        self.capture_accounted_with_prior(request, Some(ledger), None, prior, limits, None)
    }
    pub(in crate::runtime) fn capture_with_result_prior(
        &self,
        request: &WorkCaptureRequest,
        ledger: &InputEvidenceBudgetLedger,
        prior: (usize, usize),
        limits: (usize, usize),
        result: &crate::runtime::WorkItemMutationMaterial,
    ) -> Result<(CapturedWork, (usize, usize))> {
        result.validate_material_binding(&self.scope, ledger)?;
        let cost = result.material_estimate();
        ensure!(
            prior.0 >= usize::try_from(cost.0)? && prior.1 >= cost.1,
            "Work Result prior was not debited"
        );
        if let Some(resident) = &self.budget {
            ensure!(resident.same_ledger(ledger), "Work Result ledger differs");
        }
        self.capture_accounted_with_prior(request, Some(ledger), None, prior, limits, Some(result))
    }
    fn capture_accounted_with_prior<'a>(
        &self,
        request: &WorkCaptureRequest,
        ledger: Option<&InputEvidenceBudgetLedger>,
        observer: Option<&'a mut WorkMaterialObserver<'a>>,
        prior: (usize, usize),
        limits: (usize, usize),
        shared_result: Option<&crate::runtime::WorkItemMutationMaterial>,
    ) -> Result<(CapturedWork, (usize, usize))> {
        if request.include_live && !self.coverage.live_complete {
            return Err(WorkReadError::IncompleteLive.into());
        }
        if request.nonopen_limit > self.coverage.nonopen.limit {
            return Err(WorkReadError::WindowExpansion {
                requested: request.nonopen_limit,
                prepared: self.coverage.nonopen.limit,
            }
            .into());
        }
        let mut material = Material::new_with_prior(
            self,
            ledger,
            InputMemoryCategory::Pins,
            observer,
            prior,
            limits,
        )?;
        // Borrowed tuple sets have no String copies. Every BTree node is
        // admitted before insertion; owning edges expose at most two borrows.
        let mut pending = BTreeSet::new();
        fn add<'k>(
            set: &mut BTreeSet<(WorkFamily, &'k str)>,
            key: (WorkFamily, &'k str),
            material: &mut Material<'_>,
        ) -> Result<()> {
            if !set.contains(&key) {
                material.charge(128, 1)?;
                set.insert(key);
            }
            Ok(())
        }
        for key in self
            .coverage
            .mandatory_roots
            .iter()
            .chain(&request.mandatory_roots)
        {
            add(&mut pending, (key.family, &key.id), &mut material)?;
        }
        if request.include_live {
            for (family, ids) in [
                (WorkFamily::Work, &self.coverage.live_work_ids),
                (WorkFamily::Wait, &self.coverage.unresolved_wait_ids),
                (WorkFamily::Frame, &self.coverage.active_frame_ids),
            ] {
                for id in ids {
                    add(&mut pending, (family, id), &mut material)?;
                }
            }
        }
        for key in self
            .coverage
            .nonopen
            .keys
            .iter()
            .take(request.nonopen_limit)
        {
            add(&mut pending, (key.family, &key.id), &mut material)?;
        }
        for id in &request.latest_result_work_ids {
            add(&mut pending, (WorkFamily::Work, id), &mut material)?;
        }
        // Preserve original relation-error priority before closure validation.
        for (kind, ids, relation) in [
            (0, &request.required_turn_ids, &self.coverage.turn_wait_keys),
            (1, &request.required_task_ids, &self.coverage.task_wait_keys),
            (
                2,
                &request.required_trigger_message_ids,
                &self.coverage.trigger_wait_keys,
            ),
        ] {
            for id in ids {
                material.encoded(id, 1, 8)?;
                let members = relation.get(id).ok_or_else(|| match kind {
                    0 => WorkReadError::TurnWaitsNotPrepared(id.clone()),
                    1 => WorkReadError::TaskWaitsNotPrepared(id.clone()),
                    _ => WorkReadError::TriggerWaitsNotPrepared(id.clone()),
                })?;
                material.encoded(&(id, members), members.len() + 1, 8)?;
                for member in members {
                    add(&mut pending, (WorkFamily::Wait, member), &mut material)?;
                }
            }
        }
        let mut selected = BTreeSet::new();
        while let Some((family, id)) = pending.pop_first() {
            if selected.contains(&(family, id)) {
                continue;
            }
            material.charge(128, 1)?;
            selected.insert((family, id));
            if selected.len() > self.record_limit {
                return Err(WorkReadError::ResourceExhausted("required record count").into());
            }
            material.encoded(&id, 1, 4)?;
            let key = WorkKey::new(family, id);
            match self.coverage.observations.get(&key) {
                Some(WorkAvailability::Present) => {
                    let record = self
                        .records
                        .get(&key)
                        .ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?;
                    for edge in record.record.borrowed_references().into_iter().flatten() {
                        add(&mut pending, edge, &mut material)?;
                    }
                }
                Some(WorkAvailability::Absent | WorkAvailability::Pruned) => {}
                None => return Err(WorkReadError::NeedsPreparation(key).into()),
            }
        }
        // Every old capture container/temporary String copy is now covered by
        // selected-only borrowed metadata. Unselected history is not charged.
        material.encoded(&self.scope, 1, 8)?;
        if request.include_live {
            for ids in [
                &self.coverage.live_work_ids,
                &self.coverage.unresolved_wait_ids,
                &self.coverage.active_frame_ids,
            ] {
                material.encoded(ids, ids.len(), 8)?;
            }
        }
        for key in self
            .coverage
            .nonopen
            .keys
            .iter()
            .take(request.nonopen_limit)
        {
            material.encoded(key, 1, 8)?;
        }
        for (family, id) in &selected {
            material.encoded(&(*family, *id), 4, 8)?;
            let key = WorkKey::new(*family, *id);
            if *family == WorkFamily::Work {
                let head = self
                    .coverage
                    .latest_results
                    .get(*id)
                    .ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?;
                material.encoded(&(*id, head), 2 + usize::from(head.is_some()), 8)?;
                if let Some(delivery) = self.coverage.legacy_delivery_heads.get(*id) {
                    material.encoded(
                        &(*id, delivery),
                        delivery
                            .metadata_records()?
                            .checked_add(1)
                            .context("Work selected Delivery metadata overflow")?,
                        8,
                    )?;
                }
            }
            if let Some(record) = self.records.get(&key) {
                let (bytes, nodes) = record.record.resident_material_estimate()?;
                if !shared_result.is_some_and(|prior| prior.shares_work_allocation(record)) {
                    material.shared(bytes, nodes)?;
                }
            }
        }
        let physical = ledger
            .map(|ledger| {
                ledger.register_work_view(
                    &self.scope,
                    InputMemoryCategory::Pins,
                    selected
                        .iter()
                        .filter_map(|(family, id)| self.records.get(&WorkKey::new(*family, *id))),
                )
            })
            .transpose()?;
        let owned = material.guard();
        // The original projection/qualification predicates remain unchanged.
        let mut captured = self.capture_prepaid(request)?;
        captured.budget = ledger.cloned();
        if let Some(physical) = physical {
            captured.budget_views.push(physical);
        }
        if let Some(owned) = owned {
            captured.budget_views.push(owned);
        }
        Ok((captured, (material.bytes, material.records)))
    }
}
