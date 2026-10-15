//! Resident-first Work mutation source; the cold by-ID boundary is explicit.
use super::input_evidence_budget::{
    serialized_bytes, BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate,
};
use super::resident_work::{
    CanonicalWorkRecord, CapturedWork, WorkAvailability, WorkCaptureRequest, WorkFamily, WorkKey,
    WorkReadError,
};
use super::*;
use crate::runtime_db::work_snapshot::{
    self, WorkDomainLoadRequest, WorkItemSourceRead, WorkSnapshotError,
};
use crate::types::WorkItemRecord;

pub(in crate::runtime) struct WorkItemSourceMaterial {
    _selected: Option<CapturedWork>,
    _guards: Vec<Arc<BudgetViewGuard>>,
}
impl std::fmt::Debug for WorkItemSourceMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkItemSourceMaterial")
            .finish_non_exhaustive()
    }
}
impl RuntimeHandle {
    /// Synchronous because Wait callers already hold this original owner.
    /// A resident hit reads no SQL. Missing selected metadata permits only this
    /// explicit by-ID source read; Absent/Pruned never fall back to raw history.
    pub(super) fn validate_owned_work_item(
        &self,
        agent_id: &str,
        id: &str,
    ) -> Result<(WorkItemRecord, Arc<WorkItemSourceMaterial>)> {
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("Work source publication missing")?;
        let limits = self.inner.input_evidence_budget.limits();
        let record_limit = usize::try_from(limits.record_limit.min(65_536))?;
        let byte_limit = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("Work source byte limit overflow")?
            .min(64 * 1024 * 1024);
        let address_bytes = serialized_bytes(&(self.instance_agent_id(), agent_id, id, self.instance_incarnation()))?
            .checked_add(1024)
            .and_then(|n| n.checked_mul(8))
            .context("Work source address overflow")?;
        anyhow::ensure!(
            address_bytes <= byte_limit && 16 <= record_limit,
            WorkSnapshotError::ResourceExhausted("Work source address")
        );
        // Actual Host metadata accounting precedes the first owned scope/key.
        let address = self.inner.input_evidence_budget.register_host_owned_bytes(
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: address_bytes,
            },
        )?;
        let scope = super::resident_input_evidence::InputEvidenceScope {
            agent_id: self.instance_agent_id().to_owned(),
            identity_incarnation: self.instance_incarnation(),
        };
        let key = WorkKey::new(WorkFamily::Work, id);
        if let Some(work) = root.work.as_ref() {
            anyhow::ensure!(
                work.scope() == &scope && root.state.id == scope.agent_id,
                "Work source root scope differs"
            );
            match work.coverage().observations.get(&key) {
                Some(WorkAvailability::Absent) => return Err(work_source_missing(id)),
                Some(WorkAvailability::Pruned) => {
                    return Err(WorkReadError::NeedsPreparation(key).into())
                }
                Some(WorkAvailability::Present) => {
                    let actual = work
                        .borrowed_records()
                        .find(|record| record.key == key)
                        .ok_or_else(|| WorkReadError::NeedsPreparation(key.clone()))?;
                    let CanonicalWorkRecord::Work(record) = &actual.record else {
                        anyhow::bail!("qualified Work source family differs")
                    };
                    if record.agent_id != agent_id {
                        return Err(work_source_foreign(agent_id, id));
                    }
                    let (body, nodes) = actual.record.resident_material_estimate()?;
                    let copy = body
                        .checked_mul(8)
                        .context("Work source DTO bytes overflow")?;
                    let count = nodes
                        .checked_mul(8)
                        .context("Work source DTO count overflow")?;
                    let prior = (
                        usize::try_from(address_bytes)?
                            .checked_add(copy)
                            .context("Work source bytes overflow")?,
                        16usize
                            .checked_add(count)
                            .context("Work source count overflow")?,
                    );
                    anyhow::ensure!(
                        prior.0 <= usize::try_from(byte_limit)? && prior.1 <= record_limit,
                        WorkSnapshotError::ResourceExhausted("Work source DTO")
                    );
                    let owned = self.inner.input_evidence_budget.register_owned_bytes(
                        &scope,
                        InputMemoryCategory::Pins,
                        OwnedBytesEstimate {
                            payload_bytes: 0,
                            metadata_estimate_bytes: u64::try_from(copy)?,
                        },
                    )?;
                    let mut request = WorkCaptureRequest::current();
                    request.include_live = false;
                    request.nonopen_limit = 0;
                    request.mandatory_roots.insert(key.clone());
                    let (selected, _usage) = work.capture_with_prior(
                        &request,
                        &self.inner.input_evidence_budget,
                        prior,
                        (usize::try_from(byte_limit)?, record_limit),
                    )?;
                    let record = record.clone();
                    return Ok((
                        record,
                        Arc::new(WorkItemSourceMaterial {
                            _selected: Some(selected),
                            _guards: vec![address, owned],
                        }),
                    ));
                }
                None => {}
            }
        }
        let mut capture = WorkCaptureRequest::current();
        capture.include_live = false;
        capture.nonopen_limit = 0;
        let request = WorkDomainLoadRequest {
            capture,
            record_limit: record_limit.saturating_sub(16),
            byte_limit: usize::try_from(byte_limit - address_bytes)?,
        };
        let source = self.inner.runtime_db.transaction(|tx| {
            work_snapshot::read_owned_work_item_source_tx(
                tx,
                &scope,
                id,
                &request,
                &self.inner.input_evidence_budget,
            )
        })?;
        match source {
            WorkItemSourceRead::Missing => Err(work_source_missing(id)),
            WorkItemSourceRead::Foreign => Err(work_source_foreign(agent_id, id)),
            WorkItemSourceRead::Present(record, mut guards) => {
                if record.agent_id != agent_id {
                    return Err(work_source_foreign(agent_id, id));
                }
                guards.push(address);
                Ok((
                    record,
                    Arc::new(WorkItemSourceMaterial {
                        _selected: None,
                        _guards: guards,
                    }),
                ))
            }
        }
    }
}
fn work_source_missing(id: &str) -> anyhow::Error {
    RuntimeError::not_found("work_item_not_found", format!("unknown work item {id}"))
        .with_safe_context("work_item_id", id)
        .into()
}
fn work_source_foreign(agent: &str, id: &str) -> anyhow::Error {
    RuntimeError::policy(
        "work_item_access_denied",
        format!("work item {id} belongs to another agent"),
    )
    .with_safe_context("work_item_id", id)
    .with_safe_context("agent_id", agent)
    .into()
}
#[cfg(test)]
#[path = "work_owned_preparation_tests.rs"]
mod tests;
