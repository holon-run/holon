//! Original physical file outcome plus actual Work ACK; no ResourceReady grant.
use super::*;
use crate::runtime::{
    input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
    resident_input_evidence::InputEvidenceScope,
    resident_work::{CanonicalWorkRecord, QualifiedWorkRecord},
    state_mutation::RuntimeMutation,
};
use crate::types::WorkItemRecord;
use crate::work_item_plan::PlanArtifactQuery;

/// Original prepared output only; this proof grants no accepted Input lookup.
#[derive(Debug)]
struct PreparedWorkReportProof {
    target_id: String,
    target_revision: u64,
    record_digest: [u8; 32],
}
#[derive(Debug)]
pub(crate) struct PreparedWorkReportRead<'a> {
    record: &'a WorkItemRecord,
    legacy: Option<&'a super::work_plans::PreparedLegacyReport>,
}
impl PreparedWorkReportRead<'_> {
    pub(crate) fn legacy_summary(
        &self,
    ) -> Option<(&str, &str, Option<u64>, chrono::DateTime<chrono::Utc>)> {
        self.legacy.map(|value| {
            (
                value.id.as_str(),
                value.text.as_str(),
                value.turn_index,
                value.created_at,
            )
        })
    }
    pub(crate) fn result_summary(&self) -> Option<&str> {
        (self.record.state == crate::types::WorkItemState::Completed).then_some(())?;
        self.record
            .result_summary
            .as_deref()
            .filter(|text| !text.is_empty())
    }
}
fn prepared_report_digest(record: &impl serde::Serialize) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    struct Sink(Sha256);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink(Sha256::new());
    serde_json::to_writer(&mut sink, record)?;
    Ok(sink.0.finalize().into())
}

pub(crate) struct WorkItemMutationMaterial {
    scope: InputEvidenceScope,
    ledger: InputEvidenceBudgetLedger,
    query: PlanArtifactQuery,
    _file: Arc<BudgetViewGuard>,
    _original_read: Option<Arc<ObservedWorkPlanRead>>,
    _view: Arc<BudgetViewGuard>,
    _target: Option<Arc<QualifiedWorkRecord>>,
    _pins: Option<Arc<BudgetViewGuard>>,
    cost: (u64, usize),
    prepared_report: Option<PreparedWorkReportProof>,
    _prepared_report_material: Option<Arc<BudgetViewGuard>>,
    expected_report_record: [u8; 32],
    expects_completed_report: bool,
    pending_report: std::sync::Mutex<Option<Arc<OriginalWorkReportContinuation>>>,
    #[cfg(test)]
    lose_report_ack: std::sync::atomic::AtomicBool,
    acknowledged_report:
        std::sync::OnceLock<Arc<crate::runtime::work_report_sources::OriginalWorkReportRead>>,
}
pub(crate) type OwnedCreatedWorkItemRead = OwnedWorkItemMutationRead;
pub(crate) struct OwnedWorkItemMutationRead {
    record: WorkItemRecord,
    material: Arc<WorkItemMutationMaterial>,
}
impl OwnedWorkItemMutationRead {
    pub(crate) fn material(&self) -> &Arc<WorkItemMutationMaterial> {
        &self.material
    }
    pub(crate) fn into_parts(self) -> (WorkItemRecord, Arc<WorkItemMutationMaterial>) {
        (self.record, self.material)
    }
}
impl std::fmt::Debug for WorkItemMutationMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkItemMutationMaterial")
            .field("scope", &self.scope)
            .field("cost", &self.cost)
            .finish_non_exhaustive()
    }
}
impl WorkItemMutationMaterial {
    pub(crate) fn prepared_report<'a>(
        &'a self,
        record: &'a WorkItemRecord,
    ) -> Result<Option<PreparedWorkReportRead<'a>>> {
        let Some(proof) = &self.prepared_report else {
            return Ok(None);
        };
        anyhow::ensure!(
            record.agent_id == self.scope.agent_id
                && record.id == proof.target_id
                && record.revision == proof.target_revision
                && prepared_report_digest(record)? == proof.record_digest,
            "prepared Work report differs from its original frozen record"
        );
        let legacy = if record.state == crate::types::WorkItemState::Completed {
            self._original_read
                .as_ref()
                .map(|read| read.prepared_legacy_report())
                .transpose()?
                .flatten()
        } else {
            None
        };
        Ok(Some(PreparedWorkReportRead { record, legacy }))
    }
    fn freeze_prepared_report(
        &mut self,
        record: &WorkItemRecord,
        metadata: Option<&Arc<crate::runtime::work_reads::WorkQueryMetadataMaterial>>,
    ) -> Result<()> {
        anyhow::ensure!(
            self.prepared_report.is_none() && record.agent_id == self.scope.agent_id,
            "prepared Work report source differs"
        );
        let proof_bytes = u64::try_from(record.id.len())?
            .checked_mul(8)
            .and_then(|n| n.checked_add(8192))
            .context("prepared Work report proof bytes overflow")?;
        let cost = (
            self.cost
                .0
                .checked_add(proof_bytes)
                .context("prepared report total bytes overflow")?,
            self.cost
                .1
                .checked_add(32)
                .context("prepared report total records overflow")?,
        );
        let extra = metadata
            .map(|value| value.validate_accounting(&self.scope, &self.ledger))
            .transpose()?
            .unwrap_or((0, 0));
        let limits = self.ledger.limits();
        let limit_bytes = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("prepared report limit overflow")?
            .min(64 * 1024 * 1024);
        anyhow::ensure!(
            cost.0
                .checked_add(u64::try_from(extra.0)?)
                .is_some_and(|bytes| bytes <= limit_bytes)
                && cost.1.checked_add(extra.1).is_some_and(|nodes| nodes
                    <= usize::try_from(limits.record_limit)
                        .unwrap_or(usize::MAX)
                        .min(65_536)),
            ResourceObservationError::ResourceExhausted("prepared Work report source")
        );
        let guard = self.ledger.register_owned_bytes(
            &self.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: proof_bytes,
            },
        )?;
        // The extra ID/proof is admitted separately. The original result's
        // eight-copy view guard covers every report text/DTO copy before SQL.
        self.prepared_report = Some(PreparedWorkReportProof {
            target_id: record.id.clone(),
            target_revision: record.revision,
            record_digest: prepared_report_digest(record)?,
        });
        self._prepared_report_material = Some(guard);
        self.cost = cost;
        Ok(())
    }

    pub(in crate::runtime) fn shares_work_allocation(
        &self,
        record: &Arc<QualifiedWorkRecord>,
    ) -> bool {
        if self
            .acknowledged_report
            .get()
            .is_some_and(|read| Arc::ptr_eq(&read.target, record))
        {
            return true;
        }
        self._original_read
            .as_ref()
            .is_some_and(|read| read.shares_work_allocation(record))
    }
    pub(crate) fn plan_query(&self) -> PlanArtifactQuery {
        self.query.clone()
    }
    // Same actual allocation loan for the SQL Work material actor; no register.
    pub(in crate::runtime) fn material_estimate(&self) -> (u64, usize) {
        if let Some(read) = self.acknowledged_report.get() {
            return read.total_cost;
        }
        self.pending_report
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .map_or(self.cost, |pending| pending.material_estimate())
    }
    pub(in crate::runtime) fn validate_material_binding(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        anyhow::ensure!(
            &self.scope == scope && self.ledger.same_ledger(ledger),
            "created Work result material binding differs"
        );
        Ok(())
    }
}
impl RuntimeHandle {
    pub(in crate::runtime) fn prepare_created_work_item_result_material(
        &self,
        record: &WorkItemRecord,
        file: Arc<BudgetViewGuard>,
        file_cost: (u64, usize),
    ) -> Result<WorkItemMutationMaterial> {
        self.prepare_created_work_item_result_with_limits(
            record,
            file,
            file_cost,
            self.resource_material_limits()?,
        )
    }
    fn prepare_created_work_item_result_with_limits(
        &self,
        record: &WorkItemRecord,
        file: Arc<BudgetViewGuard>,
        file_cost: (u64, usize),
        limits: (usize, u64),
    ) -> Result<WorkItemMutationMaterial> {
        let scope = InputEvidenceScope {
            agent_id: self.instance_agent_id().to_owned(),
            identity_incarnation: self.inner.identity_incarnation,
        };
        anyhow::ensure!(
            record.agent_id == scope.agent_id && record.plan_artifact.is_some(),
            "created Work file outcome differs"
        );
        let raw_nodes = borrowed_resource_record_count(record)?;
        let nodes = raw_nodes
            .checked_mul(8)
            .context("created Work view count overflow")?;
        let view = serialized_bytes(record)?
            .checked_add(
                u64::try_from(raw_nodes)?
                    .checked_mul(256)
                    .context("created Work view native overflow")?,
            )
            .and_then(|bytes| bytes.checked_mul(8))
            .context("created Work view bytes overflow")?;
        let cost = (
            file_cost
                .0
                .checked_add(view)
                .context("created Work total bytes overflow")?,
            file_cost
                .1
                .checked_add(nodes)
                .context("created Work total count overflow")?,
        );
        let (records, bytes) = self.resource_material_limits()?;
        anyhow::ensure!(
            cost.0 <= bytes.min(limits.1) && cost.1 <= records.min(limits.0),
            ResourceObservationError::ResourceExhausted("created Work file and result")
        );
        let owned = self.inner.input_evidence_budget.register_owned_bytes(
            &scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: view,
            },
        )?;
        Ok(WorkItemMutationMaterial {
            scope,
            ledger: self.inner.input_evidence_budget.clone(),
            query: PlanArtifactQuery::Available(record.plan_artifact.as_ref().unwrap().clone()),
            _file: file,
            _original_read: None,
            _view: owned,
            _target: None,
            _pins: None,
            cost,
            prepared_report: None,
            _prepared_report_material: None,
            expected_report_record: prepared_report_digest(record)?,
            expects_completed_report: record.state == crate::types::WorkItemState::Completed,
            pending_report: std::sync::Mutex::new(None),
            #[cfg(test)]
            lose_report_ack: std::sync::atomic::AtomicBool::new(false),
            acknowledged_report: Default::default(),
        })
    }
    pub(in crate::runtime) fn prepare_observed_work_item_result_material(
        &self,
        record: &WorkItemRecord,
        read: Arc<ObservedWorkPlanRead>,
        written: bool,
    ) -> Result<WorkItemMutationMaterial> {
        self.prepare_observed_work_item_result_material_with_metadata(record, read, written, None)
    }
    fn prepare_observed_work_item_result_material_with_metadata(
        &self,
        record: &WorkItemRecord,
        read: Arc<ObservedWorkPlanRead>,
        written: bool,
        metadata: Option<&Arc<crate::runtime::work_reads::WorkQueryMetadataMaterial>>,
    ) -> Result<WorkItemMutationMaterial> {
        let scope = read.scope();
        read.validate_material_binding(scope, &self.inner.input_evidence_budget)?;
        anyhow::ensure!(
            scope.agent_id == self.instance_agent_id()
                && scope.identity_incarnation == self.inner.identity_incarnation
                && record.agent_id == scope.agent_id
                && record.id == read.target_id(),
            "original Work result owner/target differs"
        );
        let plan = read.borrowed_result_plan();
        if written {
            anyhow::ensure!(
                matches!(plan,ObservedPlan::Present(artifact) if record.plan_artifact.as_ref()==Some(artifact)),
                "written Work result differs from original plan outcome"
            );
        }
        let nodes = borrowed_resource_record_count(record)?;
        let view = serialized_bytes(record)?
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("updated Work result native bytes overflow")?,
            )
            .and_then(|bytes| bytes.checked_mul(8))
            .context("updated Work result bytes overflow")?;
        let records = nodes
            .checked_mul(8)
            .context("updated Work result records overflow")?;
        let prior = read.material_estimate()?;
        let cost = (
            prior
                .0
                .checked_add(view)
                .context("updated Work result combined bytes overflow")?,
            prior
                .1
                .checked_add(records)
                .context("updated Work result combined records overflow")?,
        );
        let (limit_records, limit_bytes) = self.resource_material_limits()?;
        let extra = metadata
            .map(|material| material.validate_accounting(scope, &self.inner.input_evidence_budget))
            .transpose()?
            .unwrap_or((0, 0));
        let combined = (
            cost.0
                .checked_add(u64::try_from(extra.0)?)
                .context("Wait result bytes overflow")?,
            cost.1
                .checked_add(extra.1)
                .context("Wait result records overflow")?,
        );
        anyhow::ensure!(
            combined.0 <= limit_bytes.min(64 * 1024 * 1024)
                && combined.1 <= limit_records.min(65_536),
            ResourceObservationError::ResourceExhausted("observed Work and result")
        );
        let view = self.inner.input_evidence_budget.register_owned_bytes(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: view,
            },
        )?;
        let query = match plan {
            ObservedPlan::Absent => PlanArtifactQuery::NotRecorded,
            ObservedPlan::RecordedMissing => PlanArtifactQuery::Missing,
            ObservedPlan::Present(artifact) => PlanArtifactQuery::Available(artifact.clone()),
            ObservedPlan::RefreshFailed(error) => PlanArtifactQuery::Unreadable(error.clone()),
        };
        Ok(WorkItemMutationMaterial {
            scope: scope.clone(),
            ledger: self.inner.input_evidence_budget.clone(),
            query,
            _file: read.result_plan_material().clone(),
            _view: view,
            _original_read: Some(read),
            _target: None,
            _pins: None,
            cost,
            prepared_report: None,
            _prepared_report_material: None,
            expected_report_record: prepared_report_digest(record)?,
            expects_completed_report: record.state == crate::types::WorkItemState::Completed,
            pending_report: std::sync::Mutex::new(None),
            #[cfg(test)]
            lose_report_ack: std::sync::atomic::AtomicBool::new(false),
            acknowledged_report: Default::default(),
        })
    }
    pub(in crate::runtime) fn prepare_planned_work_item_result_read(
        &self,
        record: &WorkItemRecord,
        read: Arc<ObservedWorkPlanRead>,
    ) -> Result<OwnedWorkItemMutationRead> {
        let mut material = self.prepare_observed_work_item_result_material(record, read, true)?;
        material.freeze_prepared_report(record, None)?;
        let material = Arc::new(material);
        Ok(OwnedWorkItemMutationRead {
            record: record.clone(),
            material,
        })
    }
    pub(in crate::runtime) fn prepare_planned_work_item_result_read_with_metadata(
        &self,
        record: &WorkItemRecord,
        read: Arc<ObservedWorkPlanRead>,
        metadata: Option<&Arc<crate::runtime::work_reads::WorkQueryMetadataMaterial>>,
    ) -> Result<OwnedWorkItemMutationRead> {
        let mut material = self.prepare_observed_work_item_result_material_with_metadata(
            record, read, true, metadata,
        )?;
        material.freeze_prepared_report(record, metadata)?;
        let material = Arc::new(material);
        Ok(OwnedWorkItemMutationRead {
            record: record.clone(),
            material,
        })
    }
    pub(crate) fn prepare_completion_result_read(
        &self,
        prepared: &mut crate::runtime::PreparedWorkItemCompletion,
    ) -> Result<OwnedWorkItemMutationRead> {
        let read = self.prepare_planned_work_item_result_read(
            &prepared.record,
            prepared
                ._resource_plan
                .clone()
                .context("completion original Work plan absent")?,
        )?;
        prepared._result_view_material = Some(read.material.clone());
        Ok(read)
    }
    pub(in crate::runtime) fn result_read_material<'a>(
        &self,
        read: &'a OwnedWorkItemMutationRead,
    ) -> &'a Arc<WorkItemMutationMaterial> {
        &read.material
    }
    pub(in crate::runtime) fn validate_result_read_ack(
        &self,
        mutation: &RuntimeMutation,
        read: &OwnedWorkItemMutationRead,
    ) -> Result<()> {
        read.material.validate_material_binding(
            &mutation.input_evidence_scope(),
            &self.inner.input_evidence_budget,
        )?;
        let root = mutation.capture_published_root()?;
        let target = root
            .work
            .as_ref()
            .context("Work result ACK domain absent")?
            .qualified_context_plan_source(&read.record.id)
            .context("Work result ACK target absent")?;
        anyhow::ensure!(
            matches!(&target.record,CanonicalWorkRecord::Work(actual) if actual==&read.record),
            "Work result differs from its complete actual ACK"
        );
        Ok(())
    }
    pub(crate) async fn capture_unchanged_work_item_result_read(
        &self,
        record: &WorkItemRecord,
    ) -> Result<OwnedWorkItemMutationRead> {
        let observed = self.capture_observed_work_plan(&record.id, record.revision)?;
        let material = self.prepare_observed_work_item_result_material(record, observed, false)?;
        let mutation = self.begin_mutation().await?;
        let material = if record.state == crate::types::WorkItemState::Completed {
            let root = mutation.capture_published_root()?;
            let work = root
                .work
                .as_ref()
                .context("unchanged report Work domain absent")?;
            let target = work
                .qualified_context_plan_source(&record.id)
                .context("unchanged report target absent")?;
            anyhow::ensure!(
                matches!(&target.record,CanonicalWorkRecord::Work(actual) if actual==record),
                "unchanged report canonical target differs"
            );
            let inputs = root
                .input_evidence
                .as_deref()
                .context("unchanged report Input closure not prepared")?;
            let material = Arc::new(material);
            let mut used = material.material_estimate();
            let (count, bytes) = self.resource_material_limits()?;
            let limits = (bytes.min(64 * 1024 * 1024), count.min(65_536));
            let observation = (
                &target.scope,
                &target.key,
                target.revision,
                &target.content_digest,
                root.token.owner_epoch,
                root.token.generation,
                "unchanged_current_work_report",
            );
            let handoff = material.prepare_original_report_handoff(
                target,
                &work.coverage().legacy_delivery_heads,
                inputs,
                &observation,
                |bytes, records| {
                    let bytes = usize::try_from(bytes)?
                        .checked_add(std::mem::size_of::<BudgetViewGuard>())
                        .and_then(|n| {
                            n.checked_add(2 * std::mem::size_of::<Arc<BudgetViewGuard>>())
                        })
                        .context("unchanged report guard bytes overflow")?;
                    let records = records
                        .checked_add(1)
                        .context("unchanged report guard count overflow")?;
                    let next = (
                        used.0
                            .checked_add(u64::try_from(bytes)?)
                            .context("unchanged report bytes overflow")?,
                        used.1
                            .checked_add(records)
                            .context("unchanged report count overflow")?,
                    );
                    anyhow::ensure!(
                        next.0 <= limits.0 && next.1 <= limits.1,
                        ResourceObservationError::ResourceExhausted("unchanged Work report")
                    );
                    let guard = self.inner.input_evidence_budget.register_owned_bytes(
                        &target.scope,
                        InputMemoryCategory::Pins,
                        OwnedBytesEstimate {
                            payload_bytes: 0,
                            metadata_estimate_bytes: u64::try_from(bytes)?,
                        },
                    )?;
                    used = next;
                    Ok((guard, (bytes, records)))
                },
            )?;
            handoff.publish();
            drop(handoff);
            Arc::try_unwrap(material)
                .map_err(|_| anyhow::anyhow!("unchanged report result material still borrowed"))?
        } else {
            material
        };
        self.confirm_work_item_mutation_result(&mutation, record, material)
    }
    pub(in crate::runtime) fn confirm_created_work_item_result(
        &self,
        mutation: &RuntimeMutation,
        record: &WorkItemRecord,
        material: WorkItemMutationMaterial,
    ) -> Result<OwnedCreatedWorkItemRead> {
        self.confirm_work_item_mutation_result(mutation, record, material)
    }
    pub(in crate::runtime) fn confirm_work_item_mutation_result(
        &self,
        mutation: &RuntimeMutation,
        record: &WorkItemRecord,
        mut material: WorkItemMutationMaterial,
    ) -> Result<OwnedWorkItemMutationRead> {
        material.validate_material_binding(
            &mutation.input_evidence_scope(),
            &self.inner.input_evidence_budget,
        )?;
        let cost = material.material_estimate();
        let (records, bytes) = self.resource_material_limits()?;
        anyhow::ensure!(
            cost.0 <= bytes && cost.1 <= records,
            "created Work result admitted material changed"
        );
        // This runs synchronously while the actual writer still holds its owner,
        // after complete SQL ACK/publication and before any other owner can act.
        let root = mutation.capture_published_root()?;
        let target = root
            .work
            .as_ref()
            .context("created Work ACK domain absent")?
            .qualified_context_plan_source(&record.id)
            .context("created Work ACK body absent")?;
        anyhow::ensure!(
            matches!(&target.record,CanonicalWorkRecord::Work(actual) if actual==record),
            "created Work result differs from its actual ACK"
        );
        let pins = self.inner.input_evidence_budget.register_work_view(
            &material.scope,
            InputMemoryCategory::Pins,
            std::iter::once(target),
        )?;
        material._target = Some(target.clone());
        material._pins = Some(pins);
        Ok(OwnedWorkItemMutationRead {
            record: record.clone(),
            material: Arc::new(material),
        })
    }
}

#[cfg(test)]
#[path = "work_results_tests.rs"]
mod tests;

pub(crate) struct OriginalWorkReportHandoff {
    result: Arc<WorkItemMutationMaterial>,
    report: Arc<crate::runtime::work_report_sources::OriginalWorkReportRead>,
    continuation: Option<Arc<OriginalWorkReportContinuation>>,
}
impl std::fmt::Debug for OriginalWorkReportHandoff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginalWorkReportHandoff")
            .finish_non_exhaustive()
    }
}
impl OriginalWorkReportHandoff {
    /// The SQL actor validated the exact original record/attempt and once slot
    /// before COMMIT. Publishing this retained Arc performs no fallible work.
    pub(crate) fn publish(&self) {
        if let Some(continuation) = &self.continuation {
            continuation.publish_acknowledged();
        } else {
            let _ = self.result.acknowledged_report.set(self.report.clone());
        }
    }
}
impl WorkItemMutationMaterial {
    #[cfg(test)]
    pub(in crate::runtime) fn take_lost_report_ack_for_test(&self) -> bool {
        self.lose_report_ack
            .swap(false, std::sync::atomic::Ordering::AcqRel)
    }
    pub(crate) fn expects_completed_report(&self) -> bool {
        self.expects_completed_report
    }
    pub(crate) fn report_target_id(&self) -> Option<&str> {
        self.prepared_report
            .as_ref()
            .map(|proof| proof.target_id.as_str())
            .or_else(|| self._original_read.as_ref().map(|read| read.target_id()))
    }
    pub(crate) fn acknowledged_report(
        &self,
        work: &WorkItemRecord,
    ) -> Result<Option<Option<&crate::runtime::work_report_sources::OriginalWorkReportProjection>>>
    {
        let Some(report) = self.acknowledged_report.get() else {
            return Ok(None);
        };
        report.validate_record(work)?;
        Ok(Some(report.projection.as_ref()))
    }
    pub(crate) fn prepare_original_report_handoff(
        self: &Arc<Self>,
        target: &Arc<QualifiedWorkRecord>,
        heads: &std::collections::BTreeMap<
            String,
            crate::runtime::resident_work::WorkDeliveryHeads,
        >,
        inputs: &crate::runtime::resident_input_evidence::ResidentInputEvidence,
        attempt: &impl serde::Serialize,
        mut observe: impl FnMut(u64, usize) -> Result<(Arc<BudgetViewGuard>, (usize, usize))>,
    ) -> Result<OriginalWorkReportHandoff> {
        use crate::runtime::work_report_sources::{
            CapturedWorkReportSources, OriginalWorkReportRead,
        };
        use sha2::{Digest, Sha256};
        let CanonicalWorkRecord::Work(work) = &target.record else {
            anyhow::bail!("original report target family differs")
        };
        self.validate_material_binding(&target.scope, &self.ledger)?;
        anyhow::ensure!(
            work.state == crate::types::WorkItemState::Completed
                && prepared_report_digest(work)? == self.expected_report_record
                && self.acknowledged_report.get().is_none(),
            "original successful Work report record/once differs"
        );
        let continuation = self
            .pending_report
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(continuation) = &continuation {
            anyhow::ensure!(
                prepared_report_digest(attempt)? == continuation.descriptor_digest,
                "report source changed original attempt descriptor"
            );
            if let Some(report) = continuation.borrowed_source() {
                report.validate_record(work)?;
                report._sources.validate_original_sources(inputs)?;
                anyhow::ensure!(
                    report.head_digest == prepared_report_digest(&heads.get(&work.id))?,
                    "retried original report head changed"
                );
                return Ok(OriginalWorkReportHandoff {
                    result: self.clone(),
                    report,
                    continuation: Some(continuation.clone()),
                });
            }
        }
        let base_cost = continuation
            .as_ref()
            .map_or(self.cost, |value| value.total_cost);
        let limits = self.ledger.limits();
        let limit_bytes = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("original report limit overflow")?
            .min(64 * 1024 * 1024);
        let limit_records = usize::try_from(limits.record_limit)?.min(65_536);
        let mut retained = (0u64, 0usize);
        let mut guards = Vec::new();
        let mut admit = |cost: (usize, usize), owned: bool| -> Result<()> {
            let expected = if owned {
                (
                    cost.0
                        .checked_add(std::mem::size_of::<BudgetViewGuard>())
                        .and_then(|n| {
                            n.checked_add(2 * std::mem::size_of::<Arc<BudgetViewGuard>>())
                        })
                        .context("original report guard bytes overflow")?,
                    cost.1
                        .checked_add(1)
                        .context("original report guard count overflow")?,
                )
            } else {
                cost
            };
            let next = (
                retained
                    .0
                    .checked_add(u64::try_from(expected.0)?)
                    .context("original report bytes overflow")?,
                retained
                    .1
                    .checked_add(expected.1)
                    .context("original report count overflow")?,
            );
            anyhow::ensure!(
                base_cost
                    .0
                    .checked_add(next.0)
                    .is_some_and(|n| n <= limit_bytes)
                    && base_cost
                        .1
                        .checked_add(next.1)
                        .is_some_and(|n| n <= limit_records),
                ResourceObservationError::ResourceExhausted("original successful Work report")
            );
            if owned {
                let (guard, actual) = observe(u64::try_from(cost.0)?, cost.1)?;
                anyhow::ensure!(
                    actual == expected,
                    "original report accounting callback differs from actual Input guard"
                );
                guard.validate_owned_material_binding(
                    &self.scope,
                    &self.ledger,
                    u64::try_from(actual.0)?,
                )?;
                guards.push(guard);
            }
            retained = next;
            Ok(())
        };
        let sources = CapturedWorkReportSources::capture_records(
            &target.scope,
            std::iter::once(target),
            heads,
            Some(inputs),
            &self.ledger,
            &mut admit,
        )?;
        if !sources.has_brief_report(work)? {
            if let Some(summary) = work.result_summary.as_ref().filter(|text| !text.is_empty()) {
                let bytes = summary
                    .len()
                    .checked_add(4096)
                    .and_then(|n| n.checked_mul(8))
                    .context("original report summary copy overflow")?;
                admit((bytes, 128), true)?;
            }
        }
        if !self.shares_work_allocation(target) {
            admit(target.record.resident_material_estimate()?, false)?;
        }
        drop(admit);
        let total = (
            base_cost
                .0
                .checked_add(retained.0)
                .context("original report total bytes overflow")?,
            base_cost
                .1
                .checked_add(retained.1)
                .context("original report total nodes overflow")?,
        );
        let head = &heads
            .get(&work.id)
            .context("original Work report head not prepared")?
            .latest_any;
        let projection = sources.render_original_report(work, head)?;
        struct Sink(Sha256);
        impl std::io::Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut sink = Sink(Sha256::new());
        serde_json::to_writer(&mut sink, attempt)?;
        let pins = self.ledger.register_work_view(
            &target.scope,
            InputMemoryCategory::Pins,
            std::iter::once(target),
        )?;
        if let Some(continuation) = &continuation {
            guards.push(continuation._material.clone());
        }
        // The result can be dropped while its original witness still owns the
        // Source. Keep only that Source's actual remaining allocation floor.
        let target_floor = if self.shares_work_allocation(target) {
            target.record.resident_material_estimate()?
        } else {
            (0, 0)
        };
        let retained_cost = (
            total
                .0
                .checked_sub(self.cost.0)
                .and_then(|n| n.checked_add(u64::try_from(target_floor.0).ok()?))
                .context("retained report bytes overflow")?,
            total
                .1
                .checked_sub(self.cost.1)
                .and_then(|n| n.checked_add(target_floor.1))
                .context("retained report count overflow")?,
        );
        let report = Arc::new(OriginalWorkReportRead {
            target: target.clone(),
            projection,
            _sources: sources,
            _work_pin: pins,
            _guards: guards,
            total_cost: total,
            retained_cost,
            attempt: sink.0.finalize().into(),
            head_digest: prepared_report_digest(&heads.get(&work.id))?,
        });
        if let Some(continuation) = &continuation {
            continuation.retain_source(report.clone())?;
        }
        Ok(OriginalWorkReportHandoff {
            result: self.clone(),
            report,
            continuation,
        })
    }
}

/// One original attempt, held by its owner witness through ambiguous COMMIT.
/// This is accounting/terminal-result responsibility, never new write authority.
pub(crate) struct OriginalWorkReportContinuation {
    original: crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt,
    descriptor_digest: [u8; 32],
    confirmed: std::sync::atomic::AtomicBool,
    result: std::sync::Weak<WorkItemMutationMaterial>,
    source:
        std::sync::Mutex<Option<Arc<crate::runtime::work_report_sources::OriginalWorkReportRead>>>,
    total_cost: (u64, usize),
    own_cost: (u64, usize),
    _material: Arc<BudgetViewGuard>,
}
impl std::fmt::Debug for OriginalWorkReportContinuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginalWorkReportContinuation")
            .field("cost", &self.total_cost)
            .finish_non_exhaustive()
    }
}
impl OriginalWorkReportContinuation {
    pub(crate) fn original_material_prior(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        anyhow::ensure!(
            self.original.prepared().scopes() == std::slice::from_ref(scope),
            "report recovery material scope differs from original attempt"
        );
        self._material.validate_owned_material_binding(
            scope,
            ledger,
            self._material.owned_material_bytes()?,
        )?;
        if let Some(source) = self.borrowed_source() {
            anyhow::ensure!(
                &source.target.scope == scope,
                "report recovery Source scope differs"
            );
            for guard in &source._guards {
                guard.validate_owned_material_binding(
                    scope,
                    ledger,
                    guard.owned_material_bytes()?,
                )?;
            }
            anyhow::ensure!(
                source._work_pin.matches_resource_ledger(ledger),
                "report recovery Work ledger differs"
            );
        }
        let cost = self.result.upgrade().map_or_else(
            || {
                self.borrowed_source()
                    .map_or(self.own_cost, |source| source.retained_cost)
            },
            |result| result.material_estimate(),
        );
        Ok((usize::try_from(cost.0)?, cost.1))
    }
    fn borrowed_source(
        &self,
    ) -> Option<Arc<crate::runtime::work_report_sources::OriginalWorkReportRead>> {
        self.source
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    fn material_estimate(&self) -> (u64, usize) {
        self.source
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map_or(self.total_cost, |value| value.total_cost)
    }
    fn retain_source(
        &self,
        source: Arc<crate::runtime::work_report_sources::OriginalWorkReportRead>,
    ) -> Result<()> {
        let mut saved = self.source.lock().unwrap_or_else(|p| p.into_inner());
        anyhow::ensure!(
            saved.is_none(),
            "original report attempt already has frozen source"
        );
        *saved = Some(source);
        Ok(())
    }
    fn detach(&self) {
        if let Some(result) = self.result.upgrade() {
            let mut pending = result
                .pending_report
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if pending
                .as_ref()
                .is_some_and(|value| std::ptr::eq(value.as_ref(), self))
            {
                *pending = None;
            }
        }
    }
    fn publish_acknowledged(&self) {
        let source = self.source.lock().unwrap_or_else(|p| p.into_inner()).take();
        if source.is_some() {
            self.confirmed
                .store(true, std::sync::atomic::Ordering::Release);
        }
        if let (Some(result), Some(source)) = (self.result.upgrade(), source) {
            let _ = result.acknowledged_report.set(source);
        }
        self.detach();
    }
    pub(crate) fn confirm_receipt(
        &self,
        attempt: &crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt,
        receipt: &crate::runtime_db::input_mutation_receipts::InputMutationReceipt,
    ) -> Result<()> {
        anyhow::ensure!(
            self.original.same_attempt(attempt) && receipt.prepared == *self.original.prepared(),
            "report receipt differs from original actual attempt"
        );
        anyhow::ensure!(
            self.confirmed.load(std::sync::atomic::Ordering::Acquire)
                || self
                    .source
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .is_some(),
            "original report receipt has no retained frozen Source"
        );
        self.publish_acknowledged();
        Ok(())
    }
    pub(crate) fn definitely_not_committed(
        &self,
        attempt: &crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt,
    ) -> Result<()> {
        anyhow::ensure!(
            self.original.same_attempt(attempt)
                && !self.confirmed.load(std::sync::atomic::Ordering::Acquire),
            "report absence differs from original unconfirmed actual attempt"
        );
        self.source.lock().unwrap_or_else(|p| p.into_inner()).take();
        self.detach();
        Ok(())
    }
}
impl WorkItemMutationMaterial {
    pub(crate) fn retain_original_report_attempt(
        self: &Arc<Self>,
        attempt: &crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt,
        limits: (usize, usize),
        prior: (usize, usize),
    ) -> Result<Option<Arc<OriginalWorkReportContinuation>>> {
        anyhow::ensure!(
            self.expects_completed_report
                && attempt.prepared().scopes() == std::slice::from_ref(&self.scope),
            "report attempt is outside its original result scope"
        );
        if let Some(source) = self.acknowledged_report.get() {
            self.validate_material_binding(&self.scope, &self.ledger)?;
            let CanonicalWorkRecord::Work(work) = &source.target.record else {
                anyhow::bail!("acknowledged report target family differs")
            };
            anyhow::ensure!(
                source.target.scope == self.scope
                    && work.state == crate::types::WorkItemState::Completed
                    && prepared_report_digest(work)? == self.expected_report_record
                    && prepared_report_digest(attempt.prepared())? == source.attempt
                    && self
                        .pending_report
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .is_none(),
                "acknowledged report replay differs from its full original descriptor/record"
            );
            for guard in &source._guards {
                guard.validate_owned_material_binding(
                    &self.scope,
                    &self.ledger,
                    guard.owned_material_bytes()?,
                )?;
            }
            anyhow::ensure!(
                source._work_pin.matches_resource_ledger(&self.ledger),
                "acknowledged report replay Work ledger differs"
            );
            // The original whole receipt remains the only replay authority.
            // Do not create a new token that could confirm old pending Source.
            return Ok(None);
        }
        let nodes = super::borrowed_resource_record_count(attempt.prepared())?
            .checked_add(64)
            .and_then(|n| n.checked_mul(8))
            .context("report attempt metadata count overflow")?;
        let bytes = serialized_bytes(attempt.prepared())?
            .checked_add(4096)
            .and_then(|n| n.checked_mul(8))
            .and_then(|n| n.checked_add((nodes as u64).checked_mul(256)?))
            .context("report attempt metadata bytes overflow")?;
        let total_cost = (
            self.cost
                .0
                .checked_add(bytes)
                .context("report attempt cost overflow")?,
            self.cost
                .1
                .checked_add(nodes)
                .context("report attempt count overflow")?,
        );
        let byte_limit = u64::try_from(limits.1.min(64 * 1024 * 1024))?;
        anyhow::ensure!(
            u64::try_from(prior.0)? >= self.cost.0
                && prior.1 >= self.cost.1
                && u64::try_from(prior.0)?
                    .checked_add(bytes)
                    .is_some_and(|n| n <= byte_limit)
                && prior
                    .1
                    .checked_add(nodes)
                    .is_some_and(|n| n <= limits.0.min(65_536)),
            ResourceObservationError::ResourceExhausted("original report attempt metadata")
        );
        let mut pending = self
            .pending_report
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        anyhow::ensure!(pending.is_none(), "original report attempt remains pending");
        let guard = self.ledger.register_owned_bytes(
            &self.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        let continuation = Arc::new(OriginalWorkReportContinuation {
            original: attempt.clone(),
            descriptor_digest: prepared_report_digest(attempt.prepared())?,
            confirmed: std::sync::atomic::AtomicBool::new(false),
            result: Arc::downgrade(self),
            source: std::sync::Mutex::new(None),
            total_cost,
            own_cost: (bytes, nodes),
            _material: guard,
        });
        *pending = Some(continuation.clone());
        Ok(Some(continuation))
    }
}
