//! Only the original completed Task readback may replace its dead decoder suffix.
use super::*;
use crate::runtime::input_evidence_budget::borrowed_json_material;

pub(in crate::runtime_db) struct ReleasedTaskReadbackMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    bytes: usize,
    records: usize,
}
impl<'a> ReleasedTaskReadbackMaterial<'a> {
    // Only sibling closed decoders construct this after actual DTO/scratch exit.
    pub(super) fn completed_decoder(scope: &'a InputEvidenceScope, ledger: &'a InputEvidenceBudgetLedger, bytes: usize, records: usize) -> Self {
        Self { scope, ledger, bytes, records }
    }
    pub(in crate::runtime_db) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        ensure!(
            self.scope == scope && self.ledger.same_ledger(ledger),
            "Task readback release differs from original Memory scope/ledger"
        );
        Ok((self.bytes, self.records))
    }
}

fn retained_material(facts: &writes::CommittedTaskFacts) -> Result<(usize, usize, usize)> {
    let (metadata, nodes) = borrowed_json_material(&(
        &facts.delta.scope,
        &facts.delta.removals,
        &facts.delta.coverage,
        &facts._input_roots,
        &facts._work_roots,
    ))?;
    let mut nodes = nodes
        .checked_add(16)
        .context("Task retained metadata nodes overflow")?;
    let mut owned = usize::try_from(metadata)?
        .checked_add(1024)
        .and_then(|n| {
            nodes
                .checked_mul(256)
                .and_then(|floor| n.checked_add(floor))
        })
        .context("Task retained metadata bytes overflow")?;
    let bookkeeping_scopes = facts
        .delta
        .records
        .len()
        .checked_add(facts.delta.stop_records.len())
        .and_then(|n| n.checked_add(3))
        .context("Task retained ledger scope slots overflow")?;
    owned = owned
        .checked_add(
            facts
                .delta
                .scope
                .agent_id
                .len()
                .checked_mul(bookkeeping_scopes)
                .context("Task retained ledger scope bytes overflow")?,
        )
        .context("Task retained ledger bookkeeping overflow")?;
    let mut shared = 0usize;
    // This Vec comes from the validated ResidentTaskDomain's unique body map.
    // Borrow actual data, never trust a hand-written resident_bytes as its floor.
    for record in &facts.delta.records {
        let (encoded, count) = borrowed_json_material(&(&record.locator, &record.fact))?;
        let count = count
            .checked_add(3)
            .context("Task retained body slots overflow")?;
        let actual = usize::try_from(encoded)?
            .max(record.resident_bytes)
            .checked_add(
                count
                    .checked_mul(256)
                    .context("Task retained body native bytes overflow")?,
            )
            .context("Task retained body bytes overflow")?;
        shared = shared
            .checked_add(record.resident_bytes)
            .context("Task retained shared bytes overflow")?;
        owned = owned
            .checked_add(
                actual
                    .checked_sub(record.resident_bytes)
                    .context("Task retained body floor underflow")?,
            )
            .context("Task retained owned bytes overflow")?;
        nodes = nodes
            .checked_add(count)
            .context("Task retained body nodes overflow")?;
    }
    for record in &facts.delta.stop_records {
        let (encoded, count) = borrowed_json_material(record.as_ref())?;
        let count = count
            .checked_add(3)
            .context("Stop retained body slots overflow")?;
        let observed = usize::try_from(record.resident_bytes()?)?;
        let actual = usize::try_from(encoded)?
            .max(observed)
            .checked_add(
                count
                    .checked_mul(256)
                    .context("Stop retained native bytes overflow")?,
            )
            .context("Stop retained body bytes overflow")?;
        shared = shared
            .checked_add(observed)
            .context("Stop retained shared bytes overflow")?;
        owned = owned
            .checked_add(
                actual
                    .checked_sub(observed)
                    .context("Stop retained body floor underflow")?,
            )
            .context("Stop retained owned bytes overflow")?;
        nodes = nodes
            .checked_add(count)
            .context("Stop retained nodes overflow")?;
    }
    Ok((owned, shared, nodes))
}

impl TaskReadMaterial<'_> {
    /// The closure is the same synchronous once-finalizer. Its internal raw,
    /// StoredTask maps and temporary snapshot have exited before it returns.
    /// The proof/selector prefix and the actual returned facts remain held.
    pub(in crate::runtime_db::task_snapshot) fn with_original_final_readback(
        &mut self,
        readback: impl FnOnce(&mut Self) -> Result<writes::CommittedTaskFacts>,
    ) -> Result<writes::CommittedTaskFacts> {
        let scope = self.scope;
        let request = self.request;
        let ledger = self
            .budget
            .context("Task final readback requires actual ledger")?;
        let anchor = self.guards.first().map(Arc::downgrade);
        let mark = (self.bytes, self.records, self.guards.len());
        let facts = readback(self)?;
        ensure!(
            self.scope == scope
                && std::ptr::eq(self.request, request)
                && self.budget.is_some_and(|actual| actual.same_ledger(ledger))
                && facts.delta.scope == *scope
                && anchor
                    .as_ref()
                    .is_none_or(|anchor| self.guards.first().is_some_and(|actual| {
                        std::sync::Weak::ptr_eq(anchor, &Arc::downgrade(actual))
                    })),
            "Task retained facts changed original phase scope/request/ledger"
        );
        ensure!(
            self.guards.len() >= mark.2,
            "Task final readback retired its original prefix"
        );
        ensure!(
            self.guards[mark.2..]
                .iter()
                .all(|guard| Arc::strong_count(guard) == 1),
            "Task final decoder guard has another live holder"
        );
        for guard in &self.guards[mark.2..] {
            guard.validate_owned_material_binding(self.scope, ledger, 0)?;
        }
        let (owned, shared, nodes) = retained_material(&facts)?;
        let retained = owned
            .checked_add(shared)
            .context("Task retained total bytes overflow")?;
        let suffix = (
            self.bytes
                .checked_sub(mark.0)
                .context("Task readback bytes underflow")?,
            self.records
                .checked_sub(mark.1)
                .context("Task readback nodes underflow")?,
        );
        ensure!(
            retained <= suffix.0 && nodes <= suffix.1,
            TaskSnapshotError::ResourceExhausted("Task retained facts exceed admitted decoder")
        );
        let next = (
            mark.0
                .checked_add(retained)
                .context("Task retained phase bytes overflow")?,
            mark.1
                .checked_add(nodes)
                .context("Task retained phase nodes overflow")?,
        );
        bound(next.1, next.0, Some(self.request))?;
        // Same actual Arcs share their original allocation IDs. Only metadata,
        // native trees and bookkeeping receive an independent owned estimate.
        let views = [
            ledger.register_task_view(
                self.scope,
                InputMemoryCategory::Candidate,
                facts.delta.records.iter(),
            )?,
            ledger.register_task_stop_view(
                self.scope,
                InputMemoryCategory::Candidate,
                facts.delta.stop_records.iter(),
            )?,
            ledger.register_owned_bytes(
                self.scope,
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: u64::try_from(owned)?,
                },
            )?,
        ];
        if let Some(memory) = self.memory_budget.as_deref_mut() {
            memory.release_task_readback_material(ReleasedTaskReadbackMaterial {
                scope: self.scope,
                ledger,
                bytes: suffix.0 - retained,
                records: suffix.1 - nodes,
            })?;
        }
        self.guards.truncate(mark.2);
        self.guards.extend(views);
        self.bytes = next.0;
        self.records = next.1;
        Ok(facts)
    }
}
