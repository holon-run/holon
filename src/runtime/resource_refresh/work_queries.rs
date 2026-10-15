//! Same-publication ordinary Work plan reads and explicit by-ID inspection.
use super::*;
use crate::runtime::{
    input_evidence_budget::BudgetViewGuard,
    resident_work::{CanonicalWorkRecord, CapturedWork, QualifiedWorkRecord, ResidentWorkDomain},
};
use crate::types::{AgentState, WorkItemRecord};
use crate::work_item_plan::PlanArtifactQuery;

pub(in crate::runtime) struct WorkQueryPlanSource {
    source: ResourceSourceRead,
    ledger: crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    usage: StdMutex<(u64, usize)>,
    limits: (u64, usize),
    _original_metadata: Arc<BudgetViewGuard>,
    observation_version: Option<(u64, u64)>,
}
/// Private same-owner read material, retained until actual synchronous wire.
/// Historical inspection keeps its explicit cause and never publishes Ready.
pub(crate) struct WorkItemPlanQueryRead {
    query: PlanArtifactQuery,
    _target: Option<Arc<QualifiedWorkRecord>>,
    _work_pins: Option<Arc<BudgetViewGuard>>,
    _source: Arc<WorkQueryPlanSource>,
    _plan: Option<Arc<ObservedPlan>>,
    _material: Arc<BudgetViewGuard>,
    _view_material: Arc<BudgetViewGuard>,
    _shared: Option<Arc<BudgetViewGuard>>,
    _cold_cause: Option<&'static str>,
}
impl WorkItemPlanQueryRead {
    pub(crate) fn into_parts(self) -> (PlanArtifactQuery, Self) {
        // Eight conservative owned copies were admitted before this clone.
        (self.query.clone(), self)
    }
}
impl WorkQueryPlanSource {
    fn lock_usage(&self) -> Result<std::sync::MutexGuard<'_, (u64, usize)>> {
        self.usage
            .lock()
            .map_err(|_| anyhow!("Work query resource allowance poisoned"))
    }
    fn remaining(&self, used: &(u64, usize)) -> Result<(u64, usize)> {
        Ok((
            self.limits.0.checked_sub(used.0).ok_or(
                ResourceObservationError::ResourceExhausted("Work plan query bytes"),
            )?,
            self.limits.1.checked_sub(used.1).ok_or(
                ResourceObservationError::ResourceExhausted("Work plan query records"),
            )?,
        ))
    }
}
fn view_material(record: &WorkItemRecord) -> Result<(u64, usize)> {
    let nodes = borrowed_resource_record_count(record)?;
    Ok((
        serialized_bytes(record)?
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("Work view nodes overflow")?,
            )
            .and_then(|n| n.checked_mul(8))
            .context("Work view bytes overflow")?,
        nodes.checked_mul(8).context("Work view count overflow")?,
    ))
}
fn add(used: &mut (u64, usize), next: (u64, usize)) -> Result<()> {
    used.0 = used
        .0
        .checked_add(next.0)
        .context("Work query bytes overflow")?;
    used.1 = used
        .1
        .checked_add(next.1)
        .context("Work query count overflow")?;
    Ok(())
}
impl RuntimeHandle {
    pub(in crate::runtime) fn capture_work_query_plan_source(
        &self,
        state: &AgentState,
        work: &ResidentWorkDomain,
        captured: &CapturedWork,
        prior: (u64, usize),
        original_metadata: Arc<BudgetViewGuard>,
    ) -> Result<Arc<WorkQueryPlanSource>> {
        anyhow::ensure!(
            captured.scope == *work.scope() && captured.revision == work.coverage().revision,
            "Work query resource source publication differs"
        );
        let (records, bytes) = self.resource_material_limits()?;
        let remaining = (
            records
                .checked_sub(prior.1)
                .ok_or(ResourceObservationError::ResourceExhausted(
                    "Work query prior records",
                ))?,
            bytes
                .checked_sub(prior.0)
                .ok_or(ResourceObservationError::ResourceExhausted(
                    "Work query prior bytes",
                ))?,
        );
        let source = self.context_resource_source_with_material(state, Some(work), remaining)?;
        let mut usage = prior;
        add(&mut usage, (source.bytes, source.records))?;
        // Freeze the original observation version while the caller still holds
        // its Work publication. Unavailable remains a debt for ordinary reads;
        // only Get's explicit historical inspection uses that separate cause.
        let observation_version =
            match self.with_context_resource_host_fence(&source.source, || {
                let version = self
                    .inner
                    .context_resources
                    .work_query_source_version(&source.source, &self.inner.input_evidence_budget)?;
                anyhow::ensure!(
                    self.inner
                        .resource_config_generation
                        .load(Ordering::Acquire)
                        == source.source.config_generation,
                    ResourceObservationError::SourceChanged
                );
                Ok(version)
            }) {
                Ok(version) => Some(version),
                Err(error) if error.downcast_ref::<ResourceObservationError>().is_some() => None,
                Err(error) => return Err(error),
            };
        Ok(Arc::new(WorkQueryPlanSource {
            source,
            ledger: self.inner.input_evidence_budget.clone(),
            usage: StdMutex::new(usage),
            limits: (bytes, records),
            _original_metadata: original_metadata,
            observation_version,
        }))
    }
    pub(crate) fn capture_work_item_plan_query(
        &self,
        snapshot: &super::super::work_reads::WorkQuerySnapshot,
        record: &WorkItemRecord,
    ) -> Result<WorkItemPlanQueryRead> {
        let source = snapshot
            .resource_source
            .as_ref()
            .context("Work query lacks its resource source")?;
        anyhow::ensure!(
            source.ledger.same_ledger(&self.inner.input_evidence_budget)
                && source.source.source.scope == snapshot.work.scope,
            "Work query resource ledger/scope differs"
        );
        let key = crate::runtime::resident_work::WorkKey::new(
            crate::runtime::resident_work::WorkFamily::Work,
            &record.id,
        );
        let target = snapshot
            .work
            .lookup(&key)?
            .context("ordinary Work query target is not selected")?;
        let CanonicalWorkRecord::Work(actual) = &target.record else {
            anyhow::bail!("Work query selected family differs")
        };
        anyhow::ensure!(
            actual == record,
            "ordinary Work query target differs from its captured publication"
        );
        let view = view_material(record)?;
        let mut used = source.lock_usage()?;
        let mut with_view = *used;
        add(&mut with_view, view)?;
        let remaining = source.remaining(&with_view)?;
        let version = source
            .observation_version
            .ok_or(ResourceObservationError::SourceChanged)?;
        let (plan, material, shared) =
            self.with_context_resource_host_fence(&source.source.source, || {
                let selected = self.inner.context_resources.capture_work_plan_at_version(
                    &source.source.source,
                    &record.id,
                    &source.ledger,
                    remaining.0,
                    remaining.1,
                    version,
                )?;
                anyhow::ensure!(
                    self.inner
                        .resource_config_generation
                        .load(Ordering::Acquire)
                        == source.source.source.config_generation,
                    ResourceObservationError::SourceChanged
                );
                Ok(selected)
            })?;
        let selected =
            crate::runtime::resource_observation::plan_material(&record.id, plan.as_ref())?;
        let view_guard = source.ledger.register_owned_bytes(
            &snapshot.work.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: view.0,
            },
        )?;
        let work_pins = source.ledger.register_work_view(
            &snapshot.work.scope,
            InputMemoryCategory::Pins,
            std::iter::once(target),
        )?;
        let query = match plan.as_ref() {
            ObservedPlan::Absent => PlanArtifactQuery::NotRecorded,
            ObservedPlan::RecordedMissing => PlanArtifactQuery::Missing,
            ObservedPlan::Present(artifact) => PlanArtifactQuery::Available(artifact.clone()),
            ObservedPlan::RefreshFailed(error) => PlanArtifactQuery::Unreadable(error.clone()),
        };
        add(&mut with_view, selected)?;
        *used = with_view;
        Ok(WorkItemPlanQueryRead {
            query,
            _target: Some(target.clone()),
            _work_pins: Some(work_pins),
            _source: source.clone(),
            _plan: Some(plan),
            _material: material,
            _view_material: view_guard,
            _shared: Some(shared),
            _cold_cause: None,
        })
    }
    pub(crate) fn reserve_work_item_query_slots(
        &self,
        snapshot: &super::super::work_reads::WorkQuerySnapshot,
        count: usize,
    ) -> Result<Arc<BudgetViewGuard>> {
        let source = snapshot
            .resource_source
            .as_ref()
            .context("Work query resource source absent")?;
        anyhow::ensure!(
            source.ledger.same_ledger(&self.inner.input_evidence_budget),
            "Work query slot ledger differs"
        );
        // Per-view native slots and the three original output Vec allocations.
        let per = std::mem::size_of::<WorkItemPlanQueryRead>()
            .checked_add(
                std::mem::size_of::<crate::tool::tools::work_item_query::WorkItemView>()
                    .checked_mul(3)
                    .context("Work query view slots overflow")?,
            )
            .context("Work query slots overflow")?;
        let bytes = u64::try_from(
            count
                .checked_mul(per)
                .and_then(|n| n.checked_add(1024))
                .and_then(|n| n.checked_mul(8))
                .context("Work query slot bytes overflow")?,
        )?;
        let records = count
            .checked_add(1)
            .and_then(|n| n.checked_mul(8))
            .context("Work query slot count overflow")?;
        let mut used = source.lock_usage()?;
        let mut next = *used;
        add(&mut next, (bytes, records))?;
        source.remaining(&next)?;
        let guard = source.ledger.register_owned_bytes(
            &snapshot.work.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        *used = next;
        Ok(guard)
    }
    /// Explicit original by-ID read. Its original row material is admitted
    /// before raw String or JSON decoding, using this query's same allowance.
    pub(crate) fn inspect_historical_work_item(
        &self,
        snapshot: &super::super::work_reads::WorkQuerySnapshot,
        id: &str,
    ) -> Result<Option<crate::runtime_db::work_item_recall::ExplicitWorkItemRecall>> {
        let source = snapshot
            .resource_source
            .as_ref()
            .context("Work recall source absent")?;
        anyhow::ensure!(
            source.ledger.same_ledger(&self.inner.input_evidence_budget)
                && source.source.source.scope == snapshot.work.scope,
            "Work recall original source differs"
        );
        anyhow::ensure!(
            !snapshot
                .work
                .records
                .iter()
                .any(|value| matches!(&value.record,
            CanonicalWorkRecord::Work(work) if work.id==id)),
            "selected Work cannot use explicit row recall"
        );
        let mut used = source.lock_usage()?;
        let remaining = source.remaining(&used)?;
        let read = self
            .inner
            .runtime_db
            .inspect_work_item_for_explicit_recall(
                &snapshot.work.scope,
                &source.ledger,
                id,
                remaining.0,
                remaining.1,
            )?;
        if let Some(read) = &read {
            add(&mut used, read.material_estimate())?;
        }
        Ok(read)
    }
    /// Original Get/by-ID fallback only. Keep the caller's own AgentHome even
    /// for a foreign record; this grants no foreign Home/State or Work alias.
    pub(crate) fn inspect_historical_work_item_plan(
        &self,
        snapshot: &super::super::work_reads::WorkQuerySnapshot,
        record: &WorkItemRecord,
    ) -> Result<WorkItemPlanQueryRead> {
        let _timing = crate::diagnostics::attribution::RESOURCE_REFRESH.start();
        let source = snapshot
            .resource_source
            .as_ref()
            .context("Work recall resource source absent")?;
        anyhow::ensure!(
            source.ledger.same_ledger(&self.inner.input_evidence_budget)
                && source.source.source.scope == snapshot.work.scope,
            "Work recall original ledger/scope differs"
        );
        anyhow::ensure!(
            !snapshot
                .work
                .records
                .iter()
                .any(|value| matches!(&value.record,
            CanonicalWorkRecord::Work(work) if work.id==record.id)),
            "explicit Work plan recall cannot replace an ordinary selected observation"
        );
        let generation = self
            .inner
            .host_bridge
            .as_ref()
            .map(RuntimeHostBridge::context_resource_generation)
            .transpose()?;
        let view = view_material(record)?;
        let mut used = source.lock_usage()?;
        let mut with_view = *used;
        add(&mut with_view, view)?;
        let remaining = source.remaining(&with_view)?;
        let completion = source.ledger.reserve_owned_completion(
            &snapshot.work.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate::default(),
        )?;
        let io = ResourceReadBudget::enter(completion, remaining.0, remaining.1).map_err(|_| {
            ResourceObservationError::ResourceExhausted("historical Work allowance")
        })?;
        io.charge_metadata(
            u64::try_from(record.id.len())?
                .checked_add(u64::try_from(
                    self.inner.storage.data_dir().as_os_str().len(),
                )?)
                .and_then(|n| n.checked_add(8192))
                .context("historical Work plan address overflow")?,
            32,
        )
        .map_err(|_| ResourceObservationError::ResourceExhausted("historical Work address"))?;
        let query = crate::work_item_plan::query_plan_artifact(self.agent_home().as_path(), record);
        let observed = io
            .finish()
            .map_err(|_| ResourceObservationError::ResourceExhausted("historical Work plan"))?;
        let material = io.completion_guard()?;
        let view_guard = source.ledger.register_owned_bytes(
            &snapshot.work.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: view.0,
            },
        )?;
        add(&mut with_view, observed)?;
        let read = WorkItemPlanQueryRead {
            query,
            _target: None,
            _work_pins: None,
            _source: source.clone(),
            _plan: None,
            _material: material,
            _view_material: view_guard,
            _shared: None,
            _cold_cause: Some("explicit_historical_work_plan"),
        };
        // The synchronous I/O candidate is discarded if a real Host writer
        // began or completed during inspection. Never install ResourceReady.
        if let Some(generation) = generation {
            self.inner
                .host_bridge
                .as_ref()
                .context("original Host disappeared")?
                .with_context_resource_generation(generation, || Ok(()))?;
        }
        *used = with_view;
        Ok(read)
    }
}

#[cfg(test)]
#[path = "work_queries_tests.rs"]
mod tests;
