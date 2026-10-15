//! Actual material admission for the existing full Scheduling capture.
use super::*;
use crate::runtime::input_evidence_budget::{borrowed_json_material, OwnedCompletionBudget};

#[derive(Serialize)]
struct BorrowedSchedulingKey<'a> {
    family: SchedulingFamily,
    id: &'a str,
}

#[derive(Debug)]
pub(crate) struct SchedulingCaptureMaterial {
    own: (u64, usize),
    #[cfg(test)]
    peak: (u64, usize),
    _guards: Vec<Arc<BudgetViewGuard>>,
}
impl SchedulingCaptureMaterial {
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        Ok(self.own)
    }
    pub(crate) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            !self._guards.is_empty(),
            "Scheduling capture retained guards missing"
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
pub(super) struct SchedulingCapturePhase<'a> {
    scope: Option<&'a InputEvidenceScope>,
    ledger: Option<&'a InputEvidenceBudgetLedger>,
    limits: (u64, usize),
    prior: (u64, usize),
    used: (u64, usize),
    peak: (u64, usize),
    owned_bytes: u64,
    bodies: BTreeSet<usize>,
    owned: Option<OwnedCompletionBudget>,
}
impl<'a> SchedulingCapturePhase<'a> {
    pub(super) fn unobserved() -> Self {
        Self {
            scope: None,
            ledger: None,
            limits: (u64::MAX, usize::MAX),
            prior: (0, 0),
            used: (0, 0),
            peak: (0, 0),
            owned_bytes: 0,
            bodies: BTreeSet::new(),
            owned: None,
        }
    }
    pub(super) fn is_observed(&self) -> bool {
        self.ledger.is_some()
    }
    pub(super) fn new(
        domain: &'a ResidentSchedulingDomain,
        ledger: &'a InputEvidenceBudgetLedger,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<Self> {
        ensure!(
            domain.scope.identity_incarnation > 0
                && !domain.scope.agent_id.is_empty()
                && domain
                    .budget
                    .as_ref()
                    .is_some_and(|actual| actual.same_ledger(ledger))
                && !domain.views.is_empty(),
            "Scheduling capture material belongs to another scope or ledger"
        );
        for guard in &domain.views {
            guard.validate_owned_material_binding(&domain.scope, ledger, 0)?;
        }
        let configured = ledger.limits();
        let mut phase = Self {
            scope: Some(&domain.scope),
            ledger: Some(ledger),
            limits: (
                u64::try_from(limits.0.min(64 * 1024 * 1024))?.min(
                    configured
                        .history_per_agent_bytes
                        .checked_add(configured.pending_per_agent_bytes)
                        .context("Scheduling capture limits overflow")?,
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
            owned: None,
        };
        phase.charge(8192 + 32 * 256, 32)?;
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
    fn admit(&self, cost: (u64, usize)) -> Result<()> {
        let bytes = self
            .prior
            .0
            .checked_add(self.used.0)
            .and_then(|n| n.checked_add(cost.0))
            .context(SchedulingReadError::ResourceExhausted(
                "Scheduling capture material",
            ))?;
        let nodes = self
            .prior
            .1
            .checked_add(self.used.1)
            .and_then(|n| n.checked_add(cost.1))
            .context(SchedulingReadError::ResourceExhausted(
                "Scheduling capture material",
            ))?;
        ensure!(
            bytes <= self.limits.0 && nodes <= self.limits.1,
            SchedulingReadError::ResourceExhausted("Scheduling capture material")
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
            .context("Scheduling capture owned bytes overflow")?;
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
        let cost = json_cost(value, copies)?;
        self.charge(cost.0, cost.1)
    }
    pub(super) fn key(&mut self, key: &SchedulingKey, copies: usize) -> Result<()> {
        self.json(key, copies)
    }
    pub(super) fn key_parts(
        &mut self,
        family: SchedulingFamily,
        id: &str,
        copies: usize,
    ) -> Result<()> {
        self.json(&BorrowedSchedulingKey { family, id }, copies)
    }
    pub(super) fn slots(&mut self, count: usize, copies: usize) -> Result<()> {
        if !self.is_observed() {
            return Ok(());
        }
        let nodes = count
            .checked_mul(copies)
            .context("Scheduling capture slots overflow")?;
        self.charge(
            u64::try_from(nodes)?
                .checked_mul(256)
                .context("Scheduling capture slots overflow")?,
            nodes,
        )
    }
    pub(super) fn body(&mut self, record: &Arc<QualifiedSchedulingFact>) -> Result<()> {
        if !self.is_observed() {
            return Ok(());
        }
        ensure!(
            record.locator.scope == *self.scope.unwrap(),
            "Scheduling selected body scope differs"
        );
        let address = Arc::as_ptr(record) as usize;
        if self.bodies.contains(&address) {
            return Ok(());
        }
        let body = body_material(record)?;
        self.slots(1, 1)?;
        self.admit(body)?;
        self.used.0 += body.0;
        self.used.1 += body.1;
        self.peak = (self.peak.0.max(self.used.0), self.peak.1.max(self.used.1));
        // The actual domain already guards the physical body. Only this
        // invocation's allowance is borrowed, without a second owned charge.
        self.bodies.insert(address);
        Ok(())
    }
    pub(super) fn finish(
        mut self,
        mut captured: CapturedScheduling,
    ) -> Result<(CapturedScheduling, SchedulingCaptureMaterial)> {
        match self.finish_inner(&mut captured) {
            Ok(material) => Ok((captured, material)),
            Err(error) => {
                drop(captured);
                Err(error)
            }
        }
    }
    fn finish_inner(
        &mut self,
        captured: &mut CapturedScheduling,
    ) -> Result<SchedulingCaptureMaterial> {
        let ledger = self.ledger.context("Scheduling actual ledger missing")?;
        let scope = self.scope.context("Scheduling actual scope missing")?;
        ensure!(
            captured.scope == *scope,
            "Scheduling retained scope differs"
        );
        let original = self
            .owned
            .as_ref()
            .context("Scheduling capture reservation missing")?;
        ensure!(
            original.matches_scope(scope) && original.matches_ledger(ledger),
            "Scheduling capture reservation belongs to another phase"
        );
        let guard = original.guard();
        ensure!(
            Arc::strong_count(&guard) == 2,
            "Scheduling scratch guard has another live holder"
        );
        guard.validate_owned_material_binding(scope, ledger, self.owned_bytes)?;
        drop(guard);
        // Original keys/initial closure/selection locals have exited; this
        // pointer selector is the last scratch allocation still in the phase.
        self.bodies.clear();
        let metadata = retained_metadata(captured)?;
        let mut own = metadata;
        for record in &captured.records {
            let body = body_material(record)?;
            own.0 = own
                .0
                .checked_add(body.0)
                .context("Scheduling retained bytes overflow")?;
            own.1 = own
                .1
                .checked_add(body.1)
                .context("Scheduling retained nodes overflow")?;
        }
        ensure!(
            metadata.0 <= self.owned_bytes && own.0 <= self.used.0 && own.1 <= self.used.1,
            "Scheduling retained capture exceeded its admitted material"
        );
        let retained = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: metadata.0,
            },
        )?;
        captured._views.push(retained);
        Ok(SchedulingCaptureMaterial {
            own,
            #[cfg(test)]
            peak: self.peak,
            _guards: captured._views.clone(),
        })
    }
}
fn json_cost(value: &impl Serialize, copies: usize) -> Result<(u64, usize)> {
    let (bytes, nodes) = borrowed_json_material(value)?;
    let nodes = nodes
        .checked_add(1)
        .and_then(|n| n.checked_mul(copies))
        .context("Scheduling native count overflow")?;
    let bytes = bytes
        .checked_mul(u64::try_from(copies)?)
        .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
        .context("Scheduling native bytes overflow")?;
    Ok((bytes, nodes))
}
fn body_material(record: &QualifiedSchedulingFact) -> Result<(u64, usize)> {
    let (_, nodes) = borrowed_json_material(&record.fact)?;
    let bytes = u64::try_from(record.resident_bytes)?
        .checked_add(
            u64::try_from(nodes)?
                .checked_mul(256)
                .context("Scheduling body native bytes overflow")?,
        )
        .context("Scheduling body bytes overflow")?;
    Ok((bytes, nodes))
}
fn retained_metadata(captured: &CapturedScheduling) -> Result<(u64, usize)> {
    let mut cost = json_cost(&(&captured.scope, &captured.coverage), 1)?;
    let scopes = json_cost(&captured.scope, 2)?;
    let slots = captured
        .records
        .len()
        .checked_mul(3)
        .and_then(|n| n.checked_add(10))
        .context("Scheduling retained slots overflow")?;
    cost.0 = cost
        .0
        .checked_add(scopes.0)
        .and_then(|n| n.checked_add(u64::try_from(slots).ok()?.checked_mul(256)?))
        .and_then(|n| n.checked_add(std::mem::size_of::<CapturedScheduling>() as u64))
        .context("Scheduling retained bytes overflow")?;
    cost.1 = cost
        .1
        .checked_add(scopes.1)
        .and_then(|n| n.checked_add(slots))
        .context("Scheduling retained nodes overflow")?;
    Ok(cost)
}

#[cfg(test)]
#[path = "scheduling_capture_material_tests.rs"]
mod tests;
