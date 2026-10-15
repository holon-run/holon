//! Complete native Event selections from one immutable publication.
use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CurrentEventQueryCoverage {
    pub birth_cursor: Option<u64>,
    pub birth_after: Option<u64>,
    pub born_event_seqs: BTreeMap<String, u64>,
}
impl CurrentEventQueryCoverage {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.birth_after
                .is_none_or(|after| self.birth_cursor.is_some_and(|cursor| after <= cursor)),
            CurrentReadError::NeedsPreparation
        );
        ensure!(
            self.born_event_seqs
                .values()
                .all(|seq| self.birth_after.is_some_and(|after| *seq > after)
                    && self.birth_cursor.is_some_and(|cursor| *seq <= cursor)),
            CurrentReadError::CanonicalChanged
        );
        Ok(())
    }
    pub(super) fn require_after(&self, after: u64) -> Result<u64> {
        self.validate()?;
        let cursor = self
            .birth_cursor
            .ok_or(CurrentReadError::NeedsPreparation)?;
        ensure!(
            after <= cursor
                && (after == cursor || self.birth_after.is_some_and(|cut| cut <= after)),
            CurrentReadError::NeedsPreparation
        );
        Ok(cursor)
    }
}

/// Only selected canonical Arcs and their actual guards, with no Root back edge.
pub(crate) struct CapturedCurrentEvents {
    pub scope: InputEvidenceScope,
    pub revision: u64,
    pub birth_cursor: u64,
    /// Original event sequence, oldest first.
    pub events: Vec<Arc<QualifiedCurrentReadFact>>,
    material: (usize, usize),
    _views: Vec<Arc<BudgetViewGuard>>,
}
impl CapturedCurrentEvents {
    pub(crate) fn material_estimate(&self) -> Result<(u64, usize)> {
        Ok((u64::try_from(self.material.0)?, self.material.1))
    }
}
impl CapturedCurrentEvents {
    /// Borrow the actual selected guards; never turn IDs into body authority.
    pub(in crate::runtime) fn validate_scheduler_body_loan(
        &self,
        current: &ResidentCurrentReadFacts,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            self.scope == current.scope && current.uses_ledger(ledger) && !self._views.is_empty(),
            "scheduler Event loan scope or ledger differs"
        );
        for guard in &self._views {
            guard.validate_owned_material_binding(&self.scope, ledger, 0)?;
        }
        for record in &self.events {
            ensure!(
                matches!(&record.fact, CurrentReadFact::Event(_))
                    && current
                        .records
                        .get(&record.locator.key)
                        .is_some_and(|actual| Arc::ptr_eq(actual, record)),
                "scheduler Event loan is not this publication allocation"
            );
        }
        Ok(())
    }
}
impl ResidentCurrentReadFacts {
    pub(crate) fn capture_event_births(
        &self,
        after: u64,
        request: &CurrentReadRequest,
    ) -> Result<CapturedCurrentEvents> {
        request.validate()?;
        ensure!(
            self.coverage.queue_complete,
            CurrentReadError::NeedsPreparation
        );
        let cursor = self.coverage.event_query.require_after(after)?;
        let selected = self
            .coverage
            .event_query
            .born_event_seqs
            .iter()
            .filter(|(_, seq)| **seq > after);
        // This reservation precedes index slots, sorting, scope and Arc copies.
        let count = selected.clone().count();
        let metadata_bytes = usize::try_from(serialized_bytes(&self.scope)?)?
            .checked_add(
                count
                    .checked_mul(128)
                    .context("Event query slots overflow")?,
            )
            .and_then(|n| n.checked_add(2048))
            .context("Event query metadata overflow")?;
        let metadata_records = count
            .checked_mul(4)
            .and_then(|n| n.checked_add(32))
            .context("Event query metadata nodes overflow")?;
        let mut material = (metadata_bytes, metadata_records);
        for (id, seq) in selected.clone() {
            let observation = self
                .coverage
                .observations
                .iter()
                .find(|o| o.key.family == CurrentReadFamily::Event && &o.key.id == id)
                .ok_or(CurrentReadError::NeedsPreparation)?;
            ensure!(
                observation.availability != CurrentReadAvailability::Pruned,
                CurrentReadError::Pruned(observation.key.clone())
            );
            ensure!(
                observation.availability == CurrentReadAvailability::Present,
                CurrentReadError::NeedsPreparation
            );
            let record = self
                .records
                .get(&observation.key)
                .ok_or(CurrentReadError::NeedsPreparation)?;
            ensure!(
                matches!(&record.fact, CurrentReadFact::Event(event) if event.event_seq==*seq),
                CurrentReadError::CanonicalChanged
            );
            material.0 = material
                .0
                .checked_add(record.resident_bytes)
                .context("Event query bytes overflow")?;
            material.1 = material
                .1
                .checked_add(record.native_records)
                .context("Event query nodes overflow")?;
        }
        request.check_material(material.0, material.1)?;
        let metadata = self.ledger.register_owned_bytes(
            &self.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(metadata_bytes)?,
            },
        )?;
        let mut ordered = selected
            .map(|(id, seq)| {
                let key = &self
                    .coverage
                    .observations
                    .iter()
                    .find(|o| o.key.family == CurrentReadFamily::Event && &o.key.id == id)
                    .expect("Event query validated observation")
                    .key;
                (key, *seq)
            })
            .collect::<Vec<_>>();
        ordered.sort_unstable_by_key(|(_, seq)| *seq);
        let pins = self.ledger.register_current_read_view(
            &self.scope,
            InputMemoryCategory::Pins,
            ordered.iter().map(|(key, _)| &self.records[*key]),
        )?;
        let events = ordered
            .into_iter()
            .map(|(key, _)| self.records[key].clone())
            .collect();
        Ok(CapturedCurrentEvents {
            scope: self.scope.clone(),
            revision: self.coverage.revision,
            birth_cursor: cursor,
            events,
            material,
            _views: vec![metadata, pins],
        })
    }
}
