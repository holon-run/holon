//! Allocation admission for the existing pure Input selection algorithm.
use super::*;
use crate::runtime::input_evidence_budget::{borrowed_json_material, OwnedCompletionBudget};

#[derive(Debug, thiserror::Error)]
pub(crate) enum InputCaptureError {
    #[error("ResourceExhausted: Input capture material")]
    ResourceExhausted,
    #[error("Input capture belongs to another scope or ledger")]
    ForeignMaterial,
}

#[derive(Serialize)]
struct BorrowedInputKey<'a> {
    family: InputEvidenceFamily,
    id: &'a str,
}

/// Standalone material excludes caller prior/scratch; the additional getter
/// subtracts only physically shared bodies whose original caller remains held.
/// These guards share the captured view; neither owner retains a publication.
#[derive(Debug)]
pub(crate) struct InputCaptureMaterial {
    own: (u64, usize),
    loaned: (u64, usize),
    #[cfg(test)]
    peak: (u64, usize),
    _guards: Vec<Arc<BudgetViewGuard>>,
}
impl InputCaptureMaterial {
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        Ok(self.own)
    }
    pub(crate) fn additional_material_estimate(&self) -> Result<(u64, usize)> {
        Ok((
            self.own
                .0
                .checked_sub(self.loaned.0)
                .context("Input loan exceeds own bytes")?,
            self.own
                .1
                .checked_sub(self.loaned.1)
                .context("Input loan exceeds own nodes")?,
        ))
    }
    pub(crate) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(!self._guards.is_empty(), InputCaptureError::ForeignMaterial);
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

/// The original algorithm retains its allocation reservation until its scratch
/// has exited. Final capture metadata replaces that reservation on return.
pub(super) struct CapturePhase<'a> {
    scope: Option<&'a InputEvidenceScope>,
    ledger: Option<&'a InputEvidenceBudgetLedger>,
    loan: Option<&'a CapturedInputEvidence>,
    loaned: (u64, usize),
    limits: (u64, usize),
    prior: (u64, usize),
    used: (u64, usize),
    peak: (u64, usize),
    owned_bytes: u64,
    bodies: BTreeSet<usize>,
    owned: Option<OwnedCompletionBudget>,
}
pub(super) struct CaptureCheckpoint {
    used: (u64, usize),
    owned_bytes: u64,
    parent: Option<OwnedCompletionBudget>,
}
impl<'a> CapturePhase<'a> {
    pub(super) fn unobserved() -> Self {
        Self {
            scope: None,
            ledger: None,
            loan: None,
            loaned: (0, 0),
            limits: (u64::MAX, usize::MAX),
            prior: (0, 0),
            used: (0, 0),
            peak: (0, 0),
            owned_bytes: 0,
            bodies: BTreeSet::new(),
            owned: None,
        }
    }
    pub(super) fn new(
        domain: &'a ResidentInputEvidence,
        ledger: &'a InputEvidenceBudgetLedger,
        limits: (usize, usize),
        prior: (u64, usize),
    ) -> Result<Self> {
        Self::new_with_body_loan(domain, ledger, limits, prior, None)
    }
    pub(super) fn new_with_body_loan(
        domain: &'a ResidentInputEvidence,
        ledger: &'a InputEvidenceBudgetLedger,
        limits: (usize, usize),
        prior: (u64, usize),
        loan: Option<&'a CapturedInputEvidence>,
    ) -> Result<Self> {
        ensure!(
            domain.shares_query_ledger(ledger),
            InputCaptureError::ForeignMaterial
        );
        ensure!(
            domain.scope.identity_incarnation > 0 && !domain.scope.agent_id.is_empty(),
            InputCaptureError::ForeignMaterial
        );
        ensure!(
            !domain.budget_views.is_empty(),
            InputCaptureError::ForeignMaterial
        );
        for guard in &domain.budget_views {
            guard
                .validate_owned_material_binding(&domain.scope, ledger, 0)
                .context(InputCaptureError::ForeignMaterial)?;
        }
        if let Some(loan) = loan {
            ensure!(
                loan.scope == domain.scope
                    && loan
                        .budget
                        .as_ref()
                        .is_some_and(|actual| actual.same_ledger(ledger))
                    && !loan.budget_views.is_empty(),
                InputCaptureError::ForeignMaterial
            );
            for guard in &loan.budget_views {
                guard.validate_owned_material_binding(&domain.scope, ledger, 0)?;
            }
            let mut bodies = (0u64, 0usize);
            for record in &loan.records {
                ensure!(
                    record.locator.scope == domain.scope,
                    InputCaptureError::ForeignMaterial
                );
                let (bytes, nodes) = record.resident_material_estimate()?;
                bodies.0 = bodies
                    .0
                    .checked_add(u64::try_from(bytes)?)
                    .context(InputCaptureError::ResourceExhausted)?;
                bodies.1 = bodies
                    .1
                    .checked_add(nodes)
                    .context(InputCaptureError::ResourceExhausted)?;
            }
            ensure!(
                prior.0 >= bodies.0 && prior.1 >= bodies.1,
                "Input caller prior omits actual loan bodies"
            );
        }
        let configured = ledger.limits();
        let mut phase = Self {
            scope: Some(&domain.scope),
            ledger: Some(ledger),
            loan,
            loaned: (0, 0),
            limits: (
                u64::try_from(limits.0.min(64 * 1024 * 1024))?.min(
                    configured
                        .history_per_agent_bytes
                        .checked_add(configured.pending_per_agent_bytes)
                        .context(InputCaptureError::ResourceExhausted)?,
                ),
                limits
                    .1
                    .min(65_536)
                    .min(usize::try_from(configured.record_limit)?),
            ),
            prior,
            used: (0, 0),
            peak: (0, 0),
            owned_bytes: 0,
            bodies: BTreeSet::new(),
            owned: None,
        };
        phase.admit(0, 0)?;
        // Fixed reservation/guard and material carrier slots precede even the
        // first requested-message selector allocation.
        phase.charge(1024, 4)?;
        phase.json(&domain.scope, 1)?;
        phase.owned = Some(ledger.reserve_owned_completion(
            &domain.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: phase.used.0,
            },
        )?);
        Ok(phase)
    }
    fn admit(&self, bytes: u64, nodes: usize) -> Result<()> {
        let bytes = self
            .prior
            .0
            .checked_add(self.used.0)
            .and_then(|n| n.checked_add(bytes))
            .context(InputCaptureError::ResourceExhausted)?;
        let nodes = self
            .prior
            .1
            .checked_add(self.used.1)
            .and_then(|n| n.checked_add(nodes))
            .context(InputCaptureError::ResourceExhausted)?;
        ensure!(
            bytes <= self.limits.0 && nodes <= self.limits.1,
            InputCaptureError::ResourceExhausted
        );
        Ok(())
    }
    pub(super) fn charge(&mut self, bytes: u64, nodes: usize) -> Result<()> {
        if self.ledger.is_none() {
            return Ok(());
        }
        self.admit(bytes, nodes)?;
        self.used.0 = self
            .used
            .0
            .checked_add(bytes)
            .context(InputCaptureError::ResourceExhausted)?;
        self.owned_bytes = self
            .owned_bytes
            .checked_add(bytes)
            .context(InputCaptureError::ResourceExhausted)?;
        self.peak = (self.peak.0.max(self.used.0), self.peak.1);
        self.used.1 = self
            .used
            .1
            .checked_add(nodes)
            .context(InputCaptureError::ResourceExhausted)?;
        self.peak.1 = self.peak.1.max(self.used.1);
        if let Some(owned) = &self.owned {
            owned.observe_material(0, bytes);
        }
        Ok(())
    }
    pub(super) fn json(&mut self, value: &impl Serialize, copies: usize) -> Result<()> {
        if self.ledger.is_none() {
            return Ok(());
        }
        let (bytes, nodes) = borrowed_json_material(value)?;
        let nodes = nodes
            .checked_add(1)
            .and_then(|n| n.checked_mul(copies))
            .context(InputCaptureError::ResourceExhausted)?;
        let bytes = bytes
            .checked_mul(u64::try_from(copies)?)
            .and_then(|n| n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
            .context(InputCaptureError::ResourceExhausted)?;
        self.charge(bytes, nodes)
    }
    pub(super) fn key(
        &mut self,
        family: InputEvidenceFamily,
        id: &str,
        copies: usize,
    ) -> Result<()> {
        self.json(&BorrowedInputKey { family, id }, copies)
    }
    pub(super) fn continuity(&mut self, message: &MessageEnvelope) -> Result<()> {
        if super::super::turn::TurnModelSelection::message_has_provider_recovery_provenance(message)
        {
            // The original parser owns the model/recovery DTO, not body text.
            self.json(
                &(message
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("provider_recovery")),),
                2,
            )?;
        }
        Ok(())
    }
    pub(super) fn completion_sources(
        &mut self,
        head: Option<&QualifiedInputEvidence>,
        source: &QualifiedInputEvidence,
    ) -> Result<()> {
        if let Some(head) = head {
            if let PreparedInputEvidence::Brief(brief) = &head.record {
                if let Some(id) = brief.content_source.transcript_entry_id() {
                    self.key(InputEvidenceFamily::Transcript, id, 2)?;
                }
            }
        }
        if let PreparedInputEvidence::Transcript(entry) = &source.record {
            if let Some(id) = entry.related_message_id.as_deref() {
                self.key(InputEvidenceFamily::Message, id, 2)?;
            }
        }
        Ok(())
    }
    pub(super) fn body(&mut self, record: &Arc<QualifiedInputEvidence>) -> Result<()> {
        if self.ledger.is_none() {
            return Ok(());
        }
        let address = Arc::as_ptr(record) as usize;
        if self.bodies.contains(&address) {
            return Ok(());
        }
        let (bytes, nodes) = record.resident_material_estimate()?;
        self.charge(256, 1)?;
        // A selected physical body is already allocated/guarded by the domain.
        // Only its phase allowance is borrowed; no second owned registration.
        let shared = self.loan.is_some_and(|loan| {
            loan.records
                .binary_search_by(|body| body.locator.key.cmp(&record.locator.key))
                .ok()
                .is_some_and(|index| Arc::ptr_eq(&loan.records[index], record))
        });
        if shared {
            self.loaned.0 = self
                .loaned
                .0
                .checked_add(u64::try_from(bytes)?)
                .context(InputCaptureError::ResourceExhausted)?;
            self.loaned.1 = self
                .loaned
                .1
                .checked_add(nodes)
                .context(InputCaptureError::ResourceExhausted)?;
        } else {
            self.admit(u64::try_from(bytes)?, nodes)?;
            self.used.0 += u64::try_from(bytes)?;
            self.used.1 += nodes;
            self.peak = (self.peak.0.max(self.used.0), self.peak.1.max(self.used.1));
        }
        self.bodies.insert(address);
        Ok(())
    }
    pub(super) fn checkpoint(&mut self) -> Result<CaptureCheckpoint> {
        let used = self.used;
        let owned_bytes = self.owned_bytes;
        let child = if let Some(ledger) = self.ledger {
            let (bytes, nodes) = Self::selector_cost(self.scope.unwrap())?;
            self.admit(bytes, nodes)?;
            self.used = (self.used.0 + bytes, self.used.1 + nodes);
            self.owned_bytes += bytes;
            self.peak = (self.peak.0.max(self.used.0), self.peak.1.max(self.used.1));
            Some(ledger.reserve_owned_completion(
                self.scope.unwrap(),
                InputMemoryCategory::Pins,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: bytes,
                },
            )?)
        } else {
            None
        };
        let parent = std::mem::replace(&mut self.owned, child);
        Ok(CaptureCheckpoint {
            used,
            owned_bytes,
            parent,
        })
    }
    pub(super) fn collapse(
        &mut self,
        checkpoint: CaptureCheckpoint,
        retained: (u64, usize),
    ) -> Result<()> {
        if self.ledger.is_none() {
            return Ok(());
        }
        let next = (
            checkpoint
                .used
                .0
                .checked_add(retained.0)
                .context(InputCaptureError::ResourceExhausted)?,
            checkpoint
                .used
                .1
                .checked_add(retained.1)
                .context(InputCaptureError::ResourceExhausted)?,
        );
        ensure!(
            next.0 <= self.used.0 && next.1 <= self.used.1,
            "Input scratch observation did not cover its retained selector"
        );
        self.used = next;
        self.owned_bytes = checkpoint
            .owned_bytes
            .checked_add(retained.0)
            .context(InputCaptureError::ResourceExhausted)?;
        // The function's locals have exited. Its already-admitted returned
        // selector transfers into the parent's existing guard; no second
        // registration of that allocation or caller loan is made.
        if let Some(parent) = &checkpoint.parent {
            parent.observe_material(0, retained.0);
        }
        self.owned = checkpoint.parent;
        Ok(())
    }
    pub(super) fn selector_cost(value: &impl Serialize) -> Result<(u64, usize)> {
        let (bytes, nodes) = borrowed_json_material(value)?;
        let bytes = bytes
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context(InputCaptureError::ResourceExhausted)?,
            )
            .context(InputCaptureError::ResourceExhausted)?;
        Ok((bytes, nodes))
    }
    pub(super) fn retained_metadata(captured: &CapturedInputEvidence) -> Result<(u64, usize)> {
        let (bytes, nodes) = borrowed_json_material(&(
            &captured.scope,
            &captured.windows,
            &captured.unavailable,
            &captured.turn_transcript_keys,
            &captured.message_turn_keys,
            &captured.recovery_source_absences,
            &captured.execution,
        ))?;
        #[cfg(test)]
        let (bytes, nodes) = {
            let test = borrowed_json_material(&(
                &captured.selected_completion_brief_ids,
                &captured.completion_source_observations,
            ))?;
            (
                bytes
                    .checked_add(test.0)
                    .context(InputCaptureError::ResourceExhausted)?,
                nodes
                    .checked_add(test.1)
                    .context(InputCaptureError::ResourceExhausted)?,
            )
        };
        let scope = borrowed_json_material(&captured.scope)?;
        let bytes = bytes
            .checked_add(
                scope
                    .0
                    .checked_mul(2)
                    .context(InputCaptureError::ResourceExhausted)?,
            )
            .context(InputCaptureError::ResourceExhausted)?;
        let nodes = nodes
            .checked_add(
                scope
                    .1
                    .checked_mul(2)
                    .context(InputCaptureError::ResourceExhausted)?,
            )
            .context(InputCaptureError::ResourceExhausted)?;
        let nodes = nodes
            .checked_add(
                captured
                    .records
                    .len()
                    .checked_mul(3)
                    .context(InputCaptureError::ResourceExhausted)?,
            )
            .and_then(|n| n.checked_add(8))
            .context(InputCaptureError::ResourceExhausted)?;
        let bytes = bytes
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context(InputCaptureError::ResourceExhausted)?,
            )
            .and_then(|n| n.checked_add(std::mem::size_of::<CapturedInputEvidence>() as u64))
            .context(InputCaptureError::ResourceExhausted)?;
        Ok((bytes, nodes))
    }
    pub(super) fn finish_captured(
        mut self,
        mut captured: CapturedInputEvidence,
    ) -> Result<(CapturedInputEvidence, InputCaptureMaterial)> {
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
        captured: &mut CapturedInputEvidence,
    ) -> Result<InputCaptureMaterial> {
        let actual_ledger = self.ledger.context("Input actual ledger absent")?;
        let actual_scope = self.scope.context("Input actual scope absent")?;
        let original = self
            .owned
            .as_ref()
            .context("Input original capture guard absent")?;
        ensure!(
            original.matches_scope(actual_scope) && original.matches_ledger(actual_ledger),
            "Input capture reservation belongs to another phase"
        );
        let original_guard = original.guard();
        ensure!(
            Arc::strong_count(&original_guard) == 2,
            "Input capture scratch has another live holder"
        );
        original_guard.validate_owned_material_binding(
            actual_scope,
            actual_ledger,
            self.owned_bytes,
        )?;
        drop(original_guard);
        self.bodies.clear();
        self.json(&captured.scope, 2)?;
        let pins = captured
            .records
            .len()
            .checked_mul(3)
            .context(InputCaptureError::ResourceExhausted)?;
        self.charge(
            u64::try_from(pins)?
                .checked_mul(256)
                .context(InputCaptureError::ResourceExhausted)?,
            pins,
        )?;
        let metadata = Self::retained_metadata(captured)?;
        let mut own = metadata;
        for record in &captured.records {
            let (bytes, nodes) = record.resident_material_estimate()?;
            own.0 = own
                .0
                .checked_add(u64::try_from(bytes)?)
                .context(InputCaptureError::ResourceExhausted)?;
            own.1 = own
                .1
                .checked_add(nodes)
                .context(InputCaptureError::ResourceExhausted)?;
        }
        let additional = (
            own.0
                .checked_sub(self.loaned.0)
                .context("Input loan bytes exceed own material")?,
            own.1
                .checked_sub(self.loaned.1)
                .context("Input loan nodes exceed own material")?,
        );
        ensure!(
            additional.0 <= self.used.0 && additional.1 <= self.used.1,
            "Input retained capture exceeded its admitted allocations"
        );
        let ledger = self.ledger.context("Input capture actual ledger absent")?;
        let scope = self.scope.context("Input capture actual scope absent")?;
        // All copies were admitted before allocation. Transfer the same
        // retained allocation's observation, then release the scratch view.
        let metadata_guard = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: metadata.0,
            },
        )?;
        let pins = ledger.register_view(scope, InputMemoryCategory::Pins, &captured.records)?;
        let guards = vec![metadata_guard, pins];
        captured.budget_views.extend(guards.iter().cloned());
        Ok(InputCaptureMaterial {
            own,
            loaned: self.loaned,
            #[cfg(test)]
            peak: self.peak,
            _guards: guards,
        })
    }
}

#[cfg(test)]
#[path = "input_capture_material_tests.rs"]
mod tests;
