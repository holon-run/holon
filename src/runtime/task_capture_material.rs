//! First-allocation admission for the original regular Task capture algorithm.
use super::*;
use crate::runtime::input_evidence_budget::{borrowed_json_material, OwnedCompletionBudget};

#[derive(Serialize)]
struct BorrowedTaskKey<'a> {
    family: TaskFamily,
    id: &'a str,
}

#[derive(Debug)]
pub(crate) struct TaskCaptureMaterial {
    own: (u64, usize),
    additional: (u64, usize),
    #[cfg(test)]
    peak: (u64, usize),
    _guards: Vec<Arc<BudgetViewGuard>>,
}
impl TaskCaptureMaterial {
    /// Standalone selected material, including any shared physical body loan.
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        Ok(self.own)
    }
    /// New capture allocations and unshared bodies; excludes caller prior.
    pub(crate) fn additional_material_estimate(&self) -> Result<(u64, usize)> {
        Ok(self.additional)
    }
    pub(crate) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            !self._guards.is_empty(),
            "Task capture retained guards missing"
        );
        for guard in &self._guards {
            guard.validate_owned_material_binding(scope, ledger, 0)?;
        }
        Ok(())
    }
    #[cfg(test)]
    fn phase_peak(&self) -> (u64, usize) {
        self.peak
    }
}

pub(super) struct TaskCapturePhase<'a> {
    scope: Option<&'a InputEvidenceScope>,
    ledger: Option<&'a InputEvidenceBudgetLedger>,
    loan: Option<&'a CapturedTasks>,
    limits: (u64, usize),
    prior: (u64, usize),
    used: (u64, usize),
    peak: (u64, usize),
    owned_bytes: u64,
    bodies: BTreeSet<usize>,
    loaned: (u64, usize),
    owned: Option<OwnedCompletionBudget>,
}
impl<'a> TaskCapturePhase<'a> {
    pub(super) fn unobserved() -> Self {
        Self {
            scope: None,
            ledger: None,
            loan: None,
            limits: (u64::MAX, usize::MAX),
            prior: (0, 0),
            used: (0, 0),
            peak: (0, 0),
            owned_bytes: 0,
            bodies: BTreeSet::new(),
            loaned: (0, 0),
            owned: None,
        }
    }
    pub(super) fn is_observed(&self) -> bool {
        self.ledger.is_some()
    }
    pub(super) fn new(
        domain: &'a ResidentTaskDomain,
        ledger: &'a InputEvidenceBudgetLedger,
        limits: (usize, usize),
        prior: (u64, usize),
        loan: Option<&'a CapturedTasks>,
    ) -> Result<Self> {
        ensure!(
            domain.scope.identity_incarnation > 0
                && !domain.scope.agent_id.is_empty()
                && domain
                    .budget
                    .as_ref()
                    .is_some_and(|actual| actual.same_ledger(ledger))
                && !domain.budget_views.is_empty(),
            "Task capture material belongs to another scope or ledger"
        );
        for guard in &domain.budget_views {
            guard.validate_owned_material_binding(&domain.scope, ledger, 0)?;
        }
        if let Some(loan) = loan {
            ensure!(
                loan.scope == domain.scope && !loan._budget_views.is_empty(),
                "Task capture body loan belongs to another scope"
            );
            for guard in &loan._budget_views {
                guard.validate_owned_material_binding(&domain.scope, ledger, 0)?;
            }
            for record in &loan.records {
                ensure!(
                    record.locator.scope == domain.scope
                        && record.fact.agent_id() == domain.scope.agent_id,
                    "Task capture body loan contains another scope"
                );
            }
            for record in &loan.stop_records {
                ensure!(
                    record.intent.task.scope == domain.scope,
                    "Task capture Stop loan contains another scope"
                );
            }
            let body = loan_body_material(loan)?;
            ensure!(
                prior.0 >= body.0 && prior.1 >= body.1,
                "Task capture caller prior omits actual borrowed bodies"
            );
        }
        let configured = ledger.limits();
        let mut phase = Self {
            scope: Some(&domain.scope),
            ledger: Some(ledger),
            loan,
            limits: (
                u64::try_from(limits.0.min(64 * 1024 * 1024))?.min(
                    configured
                        .history_per_agent_bytes
                        .checked_add(configured.pending_per_agent_bytes)
                        .context("Task capture actual limits overflow")?,
                ),
                limits
                    .1
                    .min(65536)
                    .min(usize::try_from(configured.record_limit)?),
            ),
            prior,
            used: (0, 0),
            peak: (0, 0),
            owned_bytes: 0,
            bodies: BTreeSet::new(),
            loaned: (0, 0),
            owned: None,
        };
        // Capture, controller, guard, scope and fixed coverage header slots.
        phase.charge(8192 + 64 * 256, 64)?;
        phase.json(&domain.scope, 2)?;
        phase.owned = Some(ledger.reserve_owned_completion(
            &domain.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: phase.owned_bytes,
            },
        )?);
        Ok(phase)
    }
    fn admit(&self, extra: (u64, usize)) -> Result<()> {
        let bytes = self
            .prior
            .0
            .checked_add(self.used.0)
            .and_then(|n| n.checked_add(extra.0))
            .context(TaskReadError::ResourceExhausted("Task capture material"))?;
        let nodes = self
            .prior
            .1
            .checked_add(self.used.1)
            .and_then(|n| n.checked_add(extra.1))
            .context(TaskReadError::ResourceExhausted("Task capture material"))?;
        ensure!(
            bytes <= self.limits.0 && nodes <= self.limits.1,
            TaskReadError::ResourceExhausted("Task capture material")
        );
        Ok(())
    }
    fn charge(&mut self, bytes: u64, nodes: usize) -> Result<()> {
        if !self.is_observed() {
            return Ok(());
        }
        self.admit((bytes, nodes))?;
        self.used.0 += bytes;
        self.used.1 += nodes;
        self.owned_bytes = self
            .owned_bytes
            .checked_add(bytes)
            .context(TaskReadError::ResourceExhausted("Task capture material"))?;
        self.peak = (self.peak.0.max(self.used.0), self.peak.1.max(self.used.1));
        if let Some(owned) = &self.owned {
            owned.observe_material(0, bytes);
        }
        Ok(())
    }
    pub(super) fn json(&mut self, value: &impl Serialize, copies: usize) -> Result<()> {
        if !self.is_observed() {
            return Ok(());
        }
        let (bytes, nodes) = json_cost(value, copies)?;
        self.charge(bytes, nodes)
    }
    pub(super) fn key(&mut self, family: TaskFamily, id: &str, copies: usize) -> Result<()> {
        self.json(&BorrowedTaskKey { family, id }, copies)
    }
    pub(super) fn slots(&mut self, count: usize, copies: usize) -> Result<()> {
        if !self.is_observed() {
            return Ok(());
        }
        let count = count
            .checked_mul(copies)
            .context("Task capture slots overflow")?;
        self.charge(
            u64::try_from(count)?
                .checked_mul(256)
                .context("Task capture slots overflow")?,
            count,
        )
    }
    /// Only this synchronous temporary key can retire its private reservation.
    /// It cannot export a key or guard; both exit before the phase is restored.
    pub(super) fn with_temporary_key(
        &mut self,
        family: TaskFamily,
        id: &str,
        select: impl FnOnce(&TaskKey) -> bool,
    ) -> Result<bool> {
        if !self.is_observed() {
            return Ok(select(&TaskKey::new(family, id)));
        }
        let cost = json_cost(&(self.scope.unwrap(), BorrowedTaskKey { family, id }), 1)?;
        self.admit(cost)?;
        self.peak = (
            self.peak.0.max(self.used.0 + cost.0),
            self.peak.1.max(self.used.1 + cost.1),
        );
        let guard = self.ledger.unwrap().register_owned_bytes(
            self.scope.unwrap(),
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: cost.0,
            },
        )?;
        let key = TaskKey::new(family, id);
        let selected = select(&key);
        drop(key);
        drop(guard);
        Ok(selected)
    }
    pub(super) fn retain_task_heads<T>(
        &mut self,
        map: &mut BTreeMap<String, T>,
        selected: &BTreeSet<TaskKey>,
    ) -> Result<()> {
        self.retain_heads(map, selected, TaskFamily::Task)
    }
    pub(super) fn retain_result_heads<T>(
        &mut self,
        map: &mut BTreeMap<String, T>,
        selected: &BTreeSet<TaskKey>,
    ) -> Result<()> {
        self.retain_heads(map, selected, TaskFamily::ResultSettlement)
    }
    fn retain_heads<T>(
        &mut self,
        map: &mut BTreeMap<String, T>,
        selected: &BTreeSet<TaskKey>,
        family: TaskFamily,
    ) -> Result<()> {
        let mut error = None;
        map.retain(|id, _| {
            if error.is_some() {
                return true;
            }
            match self.with_temporary_key(family, id, |key| selected.contains(key)) {
                Ok(keep) => keep,
                Err(e) => {
                    error = Some(e);
                    true
                }
            }
        });
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    pub(super) fn body(&mut self, record: &Arc<QualifiedTaskFact>) -> Result<()> {
        if !self.is_observed() {
            return Ok(());
        }
        ensure!(
            record.locator.scope == *self.scope.unwrap(),
            "Task capture selected body scope differs"
        );
        let address = Arc::as_ptr(record) as usize;
        if self.bodies.contains(&address) {
            return Ok(());
        }
        let cost = task_body_material(record)?;
        let shared = self.loan.is_some_and(|loan| {
            loan.records
                .binary_search_by(|body| body.locator.key.cmp(&record.locator.key))
                .ok()
                .is_some_and(|index| Arc::ptr_eq(&loan.records[index], record))
        });
        self.observe_body(address, cost, shared)
    }
    pub(super) fn stop_body(&mut self, record: &Arc<TaskStopRecord>) -> Result<()> {
        if !self.is_observed() {
            return Ok(());
        }
        ensure!(
            record.intent.task.scope == *self.scope.unwrap(),
            "Task capture selected Stop scope differs"
        );
        let address = Arc::as_ptr(record) as usize;
        if self.bodies.contains(&address) {
            return Ok(());
        }
        let cost = stop_body_material(record)?;
        let shared = self.loan.is_some_and(|loan| {
            loan.stop_records
                .binary_search_by_key(&record.effect_seq, |body| body.effect_seq)
                .ok()
                .is_some_and(|index| Arc::ptr_eq(&loan.stop_records[index], record))
        });
        self.observe_body(address, cost, shared)
    }
    fn observe_body(&mut self, address: usize, cost: (u64, usize), shared: bool) -> Result<()> {
        self.slots(1, 1)?; // Pointer selector node precedes insertion.
        if shared {
            self.loaned.0 = self
                .loaned
                .0
                .checked_add(cost.0)
                .context("Task body loan overflow")?;
            self.loaned.1 = self
                .loaned
                .1
                .checked_add(cost.1)
                .context("Task body loan overflow")?;
        } else {
            self.admit(cost)?;
            self.used.0 += cost.0;
            self.used.1 += cost.1;
            self.peak = (self.peak.0.max(self.used.0), self.peak.1.max(self.used.1));
        }
        // Bodies already have actual domain guards; never re-register owned bytes.
        self.bodies.insert(address);
        Ok(())
    }
    pub(super) fn finish(
        mut self,
        mut captured: CapturedTasks,
    ) -> Result<(CapturedTasks, TaskCaptureMaterial)> {
        match self.finish_inner(&mut captured) {
            Ok(material) => Ok((captured, material)),
            Err(error) => {
                // No failure can drop the reservation before its actual DTOs.
                drop(captured);
                Err(error)
            }
        }
    }
    fn finish_inner(&mut self, captured: &mut CapturedTasks) -> Result<TaskCaptureMaterial> {
        let ledger = self.ledger.context("Task capture actual ledger missing")?;
        let scope = self.scope.context("Task capture actual scope missing")?;
        ensure!(
            captured.scope == *scope,
            "Task capture retained scope differs"
        );
        let original = self
            .owned
            .as_ref()
            .context("Task capture original reservation missing")?;
        ensure!(
            original.matches_scope(scope) && original.matches_ledger(ledger),
            "Task capture reservation belongs to another phase"
        );
        let guard = original.guard();
        ensure!(
            Arc::strong_count(&guard) == 2,
            "Task capture scratch guard has another live holder"
        );
        guard.validate_owned_material_binding(scope, ledger, self.owned_bytes)?;
        drop(guard);
        // The original capture function returned: only this pointer selector is
        // still scratch, and it exits before retained observation is installed.
        self.bodies.clear();
        let metadata = retained_metadata(captured)?;
        let bodies = loan_body_material(captured)?;
        let own = (
            metadata
                .0
                .checked_add(bodies.0)
                .context("Task retained bytes overflow")?,
            metadata
                .1
                .checked_add(bodies.1)
                .context("Task retained nodes overflow")?,
        );
        let additional = (
            own.0
                .checked_sub(self.loaned.0)
                .context("Task loan bytes exceed retained bodies")?,
            own.1
                .checked_sub(self.loaned.1)
                .context("Task loan nodes exceed retained bodies")?,
        );
        ensure!(
            metadata.0 <= self.owned_bytes
                && additional.0 <= self.used.0
                && additional.1 <= self.used.1,
            "Task retained material exceeded its original admitted allocation"
        );
        let retained = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: metadata.0,
            },
        )?;
        captured._budget_views.push(retained);
        let guards = captured._budget_views.clone();
        Ok(TaskCaptureMaterial {
            own,
            additional,
            #[cfg(test)]
            peak: self.peak,
            _guards: guards,
        })
    }
}
fn json_cost(value: &impl Serialize, copies: usize) -> Result<(u64, usize)> {
    let (bytes, nodes) = borrowed_json_material(value)?;
    let nodes = nodes
        .checked_add(1)
        .and_then(|n| n.checked_mul(copies))
        .context("Task capture native count overflow")?;
    let bytes = bytes
        .checked_mul(u64::try_from(copies)?)
        .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
        .context("Task capture byte count overflow")?;
    Ok((bytes, nodes))
}
fn task_body_material(record: &QualifiedTaskFact) -> Result<(u64, usize)> {
    let (_, nodes) = borrowed_json_material(&record.fact)?;
    Ok((
        u64::try_from(record.resident_bytes)?
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("Task body native bytes overflow")?,
            )
            .context("Task body bytes overflow")?,
        nodes,
    ))
}
fn stop_body_material(record: &TaskStopRecord) -> Result<(u64, usize)> {
    let (_, nodes) = borrowed_json_material(record)?;
    Ok((
        record
            .resident_bytes()?
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("Stop body native bytes overflow")?,
            )
            .context("Stop body bytes overflow")?,
        nodes,
    ))
}
fn loan_body_material(captured: &CapturedTasks) -> Result<(u64, usize)> {
    let mut total = (0u64, 0usize);
    // Captured vectors originate from unique canonical keys/sequences. No new
    // dedup set is allocated merely to validate a caller's existing loan.
    for record in &captured.records {
        let cost = task_body_material(record)?;
        total.0 = total
            .0
            .checked_add(cost.0)
            .context("Task loan body bytes overflow")?;
        total.1 = total
            .1
            .checked_add(cost.1)
            .context("Task loan body nodes overflow")?;
    }
    for record in &captured.stop_records {
        let cost = stop_body_material(record)?;
        total.0 = total
            .0
            .checked_add(cost.0)
            .context("Stop loan body bytes overflow")?;
        total.1 = total
            .1
            .checked_add(cost.1)
            .context("Stop loan body nodes overflow")?;
    }
    Ok(total)
}
fn retained_metadata(captured: &CapturedTasks) -> Result<(u64, usize)> {
    let mut cost = json_cost(&(&captured.scope, &captured.coverage), 1)?;
    // The actual two pin views and retained metadata guard each own a scope.
    let scopes = json_cost(&captured.scope, 3)?;
    cost.0 = cost
        .0
        .checked_add(scopes.0)
        .context("Task retained scopes overflow")?;
    cost.1 = cost
        .1
        .checked_add(scopes.1)
        .context("Task retained scopes overflow")?;
    let slots = captured
        .records
        .len()
        .checked_add(captured.stop_records.len())
        .and_then(|n| n.checked_mul(3))
        .and_then(|n| n.checked_add(12))
        .context("Task retained slots overflow")?;
    cost.0 = cost
        .0
        .checked_add(
            u64::try_from(slots)?
                .checked_mul(256)
                .context("Task retained slots overflow")?,
        )
        .and_then(|n| n.checked_add(std::mem::size_of::<CapturedTasks>() as u64))
        .context("Task retained slots overflow")?;
    cost.1 = cost
        .1
        .checked_add(slots)
        .context("Task retained slots overflow")?;
    Ok(cost)
}

#[cfg(test)]
#[path = "task_capture_material_tests.rs"]
mod tests;
