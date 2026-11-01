//! Bounded original producer observations; never an ordinary projection fallback.
use super::*;
use crate::runtime_db::ExternalTriggerRepository;
use crate::types::ExternalTriggerRecord;

#[cfg(test)]
#[path = "trigger_observation/body_budget_tests.rs"]
mod body_budget_tests;

pub(crate) struct ExternalTriggerWriterObservation {
    scope: InputEvidenceScope,
    record: Option<ExternalTriggerRecord>,
    bytes: usize,
    records: usize,
    ledger: InputEvidenceBudgetLedger,
    guards: Vec<Arc<BudgetViewGuard>>,
    phase: crate::diagnostics::resource_io::ResourceReadMaterial,
}
impl std::fmt::Debug for ExternalTriggerWriterObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalTriggerWriterObservation")
            .field("scope", &self.scope)
            .field("record", &self.record)
            .field("bytes", &self.bytes)
            .field("records", &self.records)
            .finish_non_exhaustive()
    }
}
impl ExternalTriggerWriterObservation {
    pub(crate) fn with_command_phase<T>(&self, build: impl FnOnce() -> Result<T>) -> Result<T> {
        self.phase.finish()?;
        let _resource_io = self.phase.enter();
        let result = build()?;
        self.phase.finish()?;
        Ok(result)
    }
    pub(crate) fn transaction_material(
        &self,
    ) -> Result<crate::diagnostics::resource_io::ResourceReadMaterial> {
        self.phase.transaction()
    }
    pub(crate) fn record(&self) -> Option<&ExternalTriggerRecord> {
        self.record.as_ref()
    }
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }
    pub(crate) fn material(&self, ledger: &InputEvidenceBudgetLedger) -> Result<(usize, usize)> {
        ensure!(
            self.ledger.same_ledger(ledger),
            "Trigger observation belongs to another ledger"
        );
        Ok((self.bytes, self.records))
    }
    pub(crate) fn budget_views(&self) -> Vec<Arc<BudgetViewGuard>> {
        self.guards.clone()
    }
}

fn remaining(
    request: &SchedulingDomainLoadRequest,
    material: &Material<'_, '_>,
) -> Result<SchedulingDomainLoadRequest> {
    Ok(SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest::current(),
        byte_limit: request
            .byte_limit
            .min(MAX_BYTES)
            .checked_sub(material.bytes)
            .filter(|n| *n > 0)
            .ok_or(SchedulingSnapshotError::ResourceExhausted(
                "Trigger observation bytes",
            ))?,
        record_limit: request
            .record_limit
            .min(MAX_RECORDS)
            .checked_sub(material.records)
            .filter(|n| *n > 0)
            .ok_or(SchedulingSnapshotError::ResourceExhausted(
                "Trigger observation records",
            ))?,
    })
}
fn observe_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    id: Option<&str>,
    original_cleanup: bool,
    request: &SchedulingDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<ExternalTriggerWriterObservation> {
    let phase =
        crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new(ledger, scope)?;
    let _resource_io = phase.enter();
    let mut material = Material::new(scope, request, ledger);
    loader::scope_facts(tx, scope, &mut material, original_cleanup)?
        .context(SchedulingSnapshotError::RetiredScope)?;
    let id = if let Some(id) = id {
        material.charge(
            id.len()
                .checked_add(64)
                .context("Trigger selector overflow")?,
            1,
        )?;
        Some(id.to_owned())
    } else {
        let size: Option<usize> = tx
            .query_row(
                "SELECT length(CAST(external_trigger_id AS BLOB))+64 FROM external_triggers
             WHERE target_agent_id=?1 AND status='active'
             ORDER BY created_at DESC,external_trigger_id ASC LIMIT 1",
                [&scope.agent_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(size) = size {
            material.charge(size, 1)?;
            Some(tx.query_row(
                "SELECT external_trigger_id FROM external_triggers
                 WHERE target_agent_id=?1 AND status='active'
                 ORDER BY created_at DESC,external_trigger_id ASC LIMIT 1",
                [&scope.agent_id],
                |row| row.get::<_, String>(0),
            )?)
        } else {
            None
        }
    };
    let Some(id) = id else {
        crate::diagnostics::resource_io::admit_cloned_value(scope)?;
        return Ok(ExternalTriggerWriterObservation {
            scope: scope.clone(),
            record: None,
            bytes: material.bytes,
            records: material.records,
            ledger: ledger.clone(),
            guards: material.guards,
            phase,
        });
    };
    let key = SchedulingKey::new(SchedulingFamily::ExternalTrigger, id);
    crate::diagnostics::resource_io::admit_cloned_value(&key)?;
    let keys = BTreeSet::from([key.clone()]);
    let allowance = remaining(request, &material)?;
    let mut proof = prepare(tx, scope, &keys, &allowance, ledger, original_cleanup, None)?;
    proof.absorb_material(material.bytes, material.records, request)?;
    let record = match proof.fact_before(&key) {
        Some(CanonicalSchedulingFact::ExternalTrigger(record)) => {
            crate::diagnostics::resource_io::admit_cloned_value(record)?;
            Some(record.clone())
        }
        None => None,
        _ => anyhow::bail!("Trigger observation has another family"),
    };
    let mut guards = material.guards;
    guards.append(&mut proof.guards);
    crate::diagnostics::resource_io::admit_cloned_value(scope)?;
    Ok(ExternalTriggerWriterObservation {
        scope: scope.clone(),
        record,
        bytes: proof.material_bytes,
        records: proof.material_records,
        ledger: ledger.clone(),
        guards,
        phase,
    })
}
impl ExternalTriggerRepository<'_> {
    pub(crate) fn observe_default_for_writer(
        &self,
        scope: &InputEvidenceScope,
        request: &SchedulingDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<ExternalTriggerWriterObservation> {
        self.db
            .transaction(|tx| observe_tx(tx, scope, None, false, request, ledger))
    }
    pub(crate) fn observe_active_for_admission(
        &self,
        scope: &InputEvidenceScope,
        id: &str,
        request: &SchedulingDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<ExternalTriggerWriterObservation> {
        self.db
            .transaction(|tx| observe_tx(tx, scope, Some(id), false, request, ledger))
    }
    /// Original accounting/revocation only; it grants no new Active ingress.
    pub(crate) fn observe_existing_for_cleanup(
        &self,
        scope: &InputEvidenceScope,
        id: &str,
        request: &SchedulingDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<ExternalTriggerWriterObservation> {
        self.db
            .transaction(|tx| observe_tx(tx, scope, Some(id), true, request, ledger))
    }
}
impl SchedulingWriteProof<'_, '_> {
    /// Read only a closed reserved target. The actual stored body supplies ACK.
    pub(crate) fn read_external_trigger_ack_tx(
        &mut self,
        tx: &Transaction<'_>,
        id: &str,
        request: &SchedulingDomainLoadRequest,
    ) -> Result<ExternalTriggerRecord> {
        ensure!(
            std::ptr::eq(tx, self.tx),
            "Trigger ACK crossed its original TX"
        );
        let key = SchedulingKey::new(SchedulingFamily::ExternalTrigger, id);
        ensure!(
            self.before.contains_key(&key),
            "Trigger ACK escaped its finite targets"
        );
        let mut material = Material::new(&self.scope, request, &self.ledger);
        material.bytes = self.material_bytes;
        material.records = self.material_records;
        let actual = stored::read_stored(tx, &key, &self.scope.agent_id, Some(&mut material))?
            .context("Trigger business ACK has no stored target")?;
        self.material_bytes = material.bytes;
        self.material_records = material.records;
        self.guards.extend(material.guards);
        let CanonicalSchedulingFact::ExternalTrigger(record) = actual.fact else {
            anyhow::bail!("Trigger ACK has another family");
        };
        Ok(record)
    }
}
