//! Selected Work reads. Preparation and canonical writes stay at named owners.
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};

use super::{
    resident_work::{
        CanonicalWorkRecord, CapturedWork, WorkCaptureRequest, WorkFamily, WorkKey, WorkReadError,
    },
    state_mutation::RuntimeMutation,
    RuntimeHandle,
};
use crate::types::{
    AgentState, WaitConditionRecord, WaitConditionStatus, WorkItemRecord, WorkItemState,
};

/// Recheck readers require the complete live set, not optional history bodies.
pub(super) fn live_work_request() -> WorkCaptureRequest {
    let mut request = WorkCaptureRequest::current();
    request.nonopen_limit = 0;
    request
}

/// The same published root's State and selected records, never a RuntimeAgent
/// guard, storage handle or complete resident map. Deliberately not Clone.
pub(crate) struct WorkReadSnapshot {
    pub state: Arc<AgentState>,
    pub work: CapturedWork,
}
impl RuntimeMutation {
    pub(super) fn capture_work_read(
        &self,
        request: &WorkCaptureRequest,
    ) -> Result<WorkReadSnapshot> {
        let (_, state, work) = self.capture_work_with_state(request)?;
        ensure!(
            state.id == work.scope.agent_id,
            "Work read State scope differs"
        );
        Ok(WorkReadSnapshot { state, work })
    }
}
impl RuntimeHandle {
    /// Ordinary readers never prepare a missing key or expand a window here.
    /// Planning callers already holding the owner use capture_work_read instead.
    pub(crate) async fn capture_work_read(
        &self,
        request: &WorkCaptureRequest,
    ) -> Result<WorkReadSnapshot> {
        let mutation = self.begin_mutation().await?;
        mutation.capture_work_read(request)
    }
}
impl CapturedWork {
    pub(crate) fn work_item(&self, id: &str) -> Result<Option<&WorkItemRecord>> {
        self.lookup(&WorkKey::new(WorkFamily::Work, id))?
            .map(|record| match &record.record {
                CanonicalWorkRecord::Work(work) => Ok(work),
                _ => anyhow::bail!("captured Work key has a different family"),
            })
            .transpose()
    }
    pub(crate) fn wait_condition(&self, id: &str) -> Result<Option<&WaitConditionRecord>> {
        self.lookup(&WorkKey::new(WorkFamily::Wait, id))?
            .map(|record| match &record.record {
                CanonicalWorkRecord::Wait(wait) => Ok(wait),
                _ => anyhow::bail!("captured Wait key has a different family"),
            })
            .transpose()
    }
    pub(crate) fn live_work_items(&self) -> Result<Vec<&WorkItemRecord>> {
        ensure_live(self)?;
        self.live_work_ids
            .iter()
            .map(|id| {
                let work = self.work_item(id)?.ok_or_else(|| {
                    WorkReadError::NeedsPreparation(WorkKey::new(WorkFamily::Work, id))
                })?;
                ensure!(
                    matches!(work.state, WorkItemState::Open | WorkItemState::Completing),
                    "captured live Work has a non-live state"
                );
                Ok(work)
            })
            .collect()
    }
    /// Original raw-unresolved predicate/order, including Triggered. This is
    /// not the Open-owner Active-only context work-queue filter.
    pub(crate) fn unresolved_wait_conditions(&self) -> Result<Vec<&WaitConditionRecord>> {
        ensure_live(self)?;
        let mut waits = self
            .unresolved_wait_ids
            .iter()
            .map(|id| {
                let wait = self.wait_condition(id)?.ok_or_else(|| {
                    WorkReadError::NeedsPreparation(WorkKey::new(WorkFamily::Wait, id))
                })?;
                ensure!(
                    matches!(
                        wait.status,
                        WaitConditionStatus::Active | WaitConditionStatus::Triggered
                    ),
                    "captured unresolved Wait has a terminal state"
                );
                Ok(wait)
            })
            .collect::<Result<Vec<_>>>()?;
        waits.sort_by(|a, b| {
            (
                std::cmp::Reverse(a.updated_at),
                std::cmp::Reverse(a.created_at),
                &a.id,
            )
                .cmp(&(
                    std::cmp::Reverse(b.updated_at),
                    std::cmp::Reverse(b.created_at),
                    &b.id,
                ))
        });
        Ok(waits)
    }
    /// Original Active-only/Open-owner context predicate and canonical order.
    /// A missing preparation is never interpreted as an absent owner.
    /// Preserve the original unique Active Timer wait / Open owner predicate.
    pub(in crate::runtime) fn timer_wait_work_item_id(
        &self,
        timer_id: &str,
    ) -> Result<Option<&str>> {
        ensure_live(self)?;
        let mut selected = None;
        let mut count = 0usize;
        for id in &self.unresolved_wait_ids {
            let wait = self
                .records
                .iter()
                .find_map(|record| match &record.record {
                    CanonicalWorkRecord::Wait(wait) if &wait.id == id => Some(wait),
                    _ => None,
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("complete Work Timer route lacks selected Wait body")
                })?;
            if wait.status != WaitConditionStatus::Active
                || wait.kind != crate::types::WaitConditionKind::Timer
                || wait.subject_ref.as_deref() != Some(timer_id)
            {
                continue;
            }
            if let Some(owner) = wait.work_item_id.as_deref() {
                let is_open = self.records.iter().any(|record| {
                    matches!(&record.record, CanonicalWorkRecord::Work(work)
                        if work.id == owner && work.state == WorkItemState::Open)
                });
                if !is_open {
                    continue;
                }
            }
            count += 1;
            selected = wait.work_item_id.as_deref();
        }
        Ok((count == 1).then_some(selected).flatten())
    }

    pub(crate) fn context_active_wait_conditions(&self) -> Result<Vec<&WaitConditionRecord>> {
        let mut selected = Vec::new();
        for wait in self.unresolved_wait_conditions()? {
            if wait.status != WaitConditionStatus::Active {
                continue;
            }
            let live_owner = match wait.work_item_id.as_deref() {
                Some(id) => self
                    .work_item(id)?
                    .is_some_and(|work| work.state == WorkItemState::Open),
                None => true,
            };
            if live_owner {
                selected.push(wait);
            }
        }
        Ok(selected)
    }
    /// Work repository SQL compares its normalized millisecond columns. The
    /// returned typed Work remains unchanged, including its original payload.
    pub(crate) fn blocked_work_rechecks(&self) -> Result<Vec<&WorkItemRecord>> {
        let mut works = self
            .live_work_items()?
            .into_iter()
            .filter(|work| {
                work.state == WorkItemState::Open
                    && work.blocked_by.is_some()
                    && work.recheck_at.is_some_and(|at| {
                        work.recheck_consumed_at.is_none_or(|consumed| {
                            consumed.timestamp_millis() < at.timestamp_millis()
                        })
                    })
            })
            .collect::<Vec<_>>();
        works.sort_by(|a, b| {
            (a.recheck_at.unwrap().timestamp_millis(), &a.id)
                .cmp(&(b.recheck_at.unwrap().timestamp_millis(), &b.id))
        });
        Ok(works)
    }
    pub(crate) fn next_work_recheck_at(&self) -> Result<Option<DateTime<Utc>>> {
        self.blocked_work_rechecks()?
            .first()
            .map(|work| {
                DateTime::from_timestamp_millis(work.recheck_at.unwrap().timestamp_millis())
                    .ok_or_else(|| anyhow::anyhow!("Work recheck timestamp is invalid"))
            })
            .transpose()
    }
    pub(crate) fn due_work_rechecks(&self, now: DateTime<Utc>) -> Result<Vec<&WorkItemRecord>> {
        Ok(self
            .blocked_work_rechecks()?
            .into_iter()
            .filter(|work| work.recheck_at.unwrap().timestamp_millis() <= now.timestamp_millis())
            .collect())
    }
    pub(crate) fn next_agent_wait_recheck_at(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(self
            .unresolved_wait_conditions()?
            .into_iter()
            .filter(|wait| {
                wait.work_item_id.is_none() && wait.status == WaitConditionStatus::Active
            })
            .filter_map(WaitConditionRecord::recheck_at)
            .min())
    }
    pub(crate) fn due_agent_wait_recheck(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<&WaitConditionRecord>> {
        Ok(self
            .unresolved_wait_conditions()?
            .into_iter()
            .filter(|wait| {
                wait.work_item_id.is_none()
                    && wait.status == WaitConditionStatus::Active
                    && wait.recheck_at().is_some_and(|at| at <= now)
            })
            .min_by_key(|wait| wait.recheck_at()))
    }
}
fn ensure_live(work: &CapturedWork) -> Result<()> {
    if !work.live_complete {
        return Err(WorkReadError::IncompleteLive.into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "work_reads_tests.rs"]
mod tests;

/// The ordinary list query uses the original full-live/128-history policy.
/// Trigger facts remain an explicit D6 adapter observed under this same owner.
pub(crate) struct WorkQuerySnapshot {
    pub state: Arc<AgentState>,
    pub items: Vec<crate::work_item_scheduling::WorkItemSchedulingProjection>,
    _owned: Arc<super::input_evidence_budget::BudgetViewGuard>,
    _scheduling: super::resident_scheduling::CapturedScheduling,
    _trigger_material: Arc<super::input_evidence_budget::BudgetViewGuard>,
    allowance: (usize, usize),
    _result_prior: Option<Arc<super::WorkItemMutationMaterial>>,
    _metadata_prior: Option<Arc<WorkQueryMetadataMaterial>>,
    pub(in crate::runtime) work: CapturedWork,
    report_sources: Option<super::work_report_sources::CapturedWorkReportSources>,
    pub(in crate::runtime) resource_source:
        Option<Arc<super::resource_refresh::WorkQueryPlanSource>>,
}
impl WorkQuerySnapshot {
    #[cfg(test)]
    pub(in crate::runtime) fn report_sources_for_test(
        &self,
    ) -> Option<&super::work_report_sources::CapturedWorkReportSources> {
        self.report_sources.as_ref()
    }

    /// The original selected/read/derived allowance, with actual guards retained.
    pub(in crate::runtime) fn material_estimate(&self) -> Result<(u64, usize)> {
        Ok((u64::try_from(self.allowance.0)?, self.allowance.1))
    }
    /// Selected owning closure includes qualified bodies outside display heads.
    /// Absence here is not a grant; Get keeps its explicit by-ID recall cause.
    pub(crate) fn work_report_brief_text<'a>(
        &'a self,
        record: &WorkItemRecord,
    ) -> Result<Option<(&'a crate::types::BriefRecord, String)>> {
        self.validate_report_work(record)?;
        self.report_sources
            .as_ref()
            .context("Work report sources were not captured")?
            .brief_text(record)
    }
    pub(crate) fn work_report_delivery_summary<'a>(
        &'a self,
        record: &WorkItemRecord,
        latest_nonempty: bool,
    ) -> Result<Option<&'a crate::types::DeliverySummaryRecord>> {
        self.validate_report_work(record)?;
        let heads = self.work.delivery_heads(&record.id)?;
        let head = if latest_nonempty {
            &heads.latest_nonempty
        } else {
            &heads.latest_any
        };
        self.report_sources
            .as_ref()
            .context("Work report sources were not captured")?
            .legacy_summary(&record.id, head)
    }
    fn validate_report_work(&self, record: &WorkItemRecord) -> Result<()> {
        ensure!(
            self.selected_work_item(&record.id) == Some(record),
            "Work report record differs from original selected publication"
        );
        Ok(())
    }
    pub(crate) fn selected_work_item(&self, id: &str) -> Option<&WorkItemRecord> {
        self.work
            .records
            .iter()
            .find_map(|qualified| match &qualified.record {
                CanonicalWorkRecord::Work(record) if record.id == id => Some(record),
                _ => None,
            })
    }
}
/// Only actual owned selection/disclosure material. No body or Ready authority.
#[derive(Debug)]
pub(crate) struct WorkQueryMetadataMaterial {
    scope: super::resident_input_evidence::InputEvidenceScope,
    ledger: super::input_evidence_budget::InputEvidenceBudgetLedger,
    owned: Arc<super::input_evidence_budget::BudgetViewGuard>,
    cost: (usize, usize),
    observed_bytes: u64,
    _previous: Option<Arc<WorkQueryMetadataMaterial>>,
}
impl WorkQueryMetadataMaterial {
    pub(in crate::runtime) fn validate_accounting(
        &self,
        scope: &super::resident_input_evidence::InputEvidenceScope,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        ensure!(
            &self.scope == scope && self.ledger.same_ledger(ledger),
            "Work query metadata material binding differs"
        );
        self.owned
            .validate_owned_material_binding(scope, ledger, self.observed_bytes)?;
        if let Some(previous) = &self._previous {
            previous.validate_accounting(scope, ledger)?;
        }
        Ok(self.cost)
    }
}
/// One read allowance; existing immutable allocations are only debited, while
/// each new selected/derived container is observed before its first copy.
struct WorkQueryMaterial {
    used: (usize, usize),
    limits: (usize, usize),
    owned: super::input_evidence_budget::OwnedCompletionBudget,
}
impl WorkQueryMaterial {
    fn new(
        scope: &super::resident_input_evidence::InputEvidenceScope,
        ledger: &super::input_evidence_budget::InputEvidenceBudgetLedger,
        limits: (usize, usize),
        prior: (usize, usize),
    ) -> Result<Self> {
        use super::input_evidence_budget::{
            serialized_bytes, InputMemoryCategory, OwnedBytesEstimate,
        };
        let limits = (limits.0.min(64 * 1024 * 1024), limits.1.min(65_536));
        let address = usize::try_from(serialized_bytes(scope)?)?
            .checked_mul(8)
            .and_then(|n| n.checked_add(1024))
            .context("Work query address overflow")?;
        let base = address
            .checked_add(4096)
            .context("Work query scratch overflow")?;
        let used = (
            prior
                .0
                .checked_add(address)
                .and_then(|n| n.checked_add(base))
                .context("Work query prior bytes overflow")?,
            prior
                .1
                .checked_add(16)
                .context("Work query prior records overflow")?,
        );
        ensure!(
            used.0 <= limits.0 && used.1 <= limits.1,
            WorkReadError::ResourceExhausted("Work query held material")
        );
        let address_guard = ledger.register_host_owned_bytes(
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(address)?,
            },
        )?;
        let owned = ledger.reserve_owned_completion(
            scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate::default(),
        )?;
        owned.observe_material(0, u64::try_from(base)?);
        drop(address_guard);
        Ok(Self {
            used,
            limits,
            owned,
        })
    }
    fn bounds(&self, next: (usize, usize)) -> Result<(usize, usize)> {
        let used = (
            self.used
                .0
                .checked_add(next.0)
                .context("Work query bytes overflow")?,
            self.used
                .1
                .checked_add(next.1)
                .context("Work query count overflow")?,
        );
        ensure!(
            used.0 <= self.limits.0 && used.1 <= self.limits.1,
            WorkReadError::ResourceExhausted("Work query combined material")
        );
        Ok(used)
    }
    fn shared(&mut self, next: (usize, usize)) -> Result<()> {
        self.used = self.bounds(next)?;
        Ok(())
    }
    fn copy(&mut self, next: (usize, usize)) -> Result<()> {
        let used = self.bounds(next)?;
        self.owned.observe_material(0, u64::try_from(next.0)?);
        self.used = used;
        Ok(())
    }
    fn typed(value: &impl serde::Serialize, copies: usize) -> Result<(usize, usize)> {
        let nodes = super::resource_refresh::borrowed_resource_record_count(value)?;
        let bytes = usize::try_from(super::input_evidence_budget::serialized_bytes(value)?)?
            .checked_add(
                nodes
                    .checked_mul(256)
                    .context("Work query typed nodes overflow")?,
            )
            .and_then(|n| n.checked_mul(copies))
            .context("Work query typed bytes overflow")?;
        Ok((
            bytes,
            nodes
                .checked_mul(copies)
                .context("Work query typed count overflow")?,
        ))
    }
    fn guard(&self) -> Arc<super::input_evidence_budget::BudgetViewGuard> {
        self.owned.guard()
    }
}
impl RuntimeHandle {
    pub(crate) async fn capture_work_query_snapshot(&self) -> Result<WorkQuerySnapshot> {
        self.capture_work_query_snapshot_inner(false, None).await
    }
    pub(crate) async fn capture_work_query_snapshot_with_resources(
        &self,
    ) -> Result<WorkQuerySnapshot> {
        self.capture_work_query_snapshot_inner(true, None).await
    }
    pub(crate) async fn capture_work_query_snapshot_for_mutation_result(
        &self,
        material: &Arc<super::WorkItemMutationMaterial>,
    ) -> Result<WorkQuerySnapshot> {
        self.capture_work_query_snapshot_inner(false, Some(material))
            .await
    }
    /// Account the complete original snapshot before the first owner-selection
    /// clone. After it drops only these actual metadata allocations remain.
    pub(crate) fn retain_wait_selection_material(
        &self,
        snapshot: &WorkQuerySnapshot,
        borrowed: &impl serde::Serialize,
    ) -> Result<Arc<WorkQueryMetadataMaterial>> {
        let (records, bytes) = self.resource_material_limits()?;
        self.retain_wait_selection_with_limits(
            snapshot,
            borrowed,
            (usize::try_from(bytes)?, records),
        )
    }
    fn retain_wait_selection_with_limits(
        &self,
        snapshot: &WorkQuerySnapshot,
        borrowed: &impl serde::Serialize,
        limits: (usize, usize),
    ) -> Result<Arc<WorkQueryMetadataMaterial>> {
        let scope = &snapshot.work.scope;
        let ledger = &self.inner.input_evidence_budget;
        snapshot
            ._owned
            .validate_owned_material_binding(scope, ledger, 0)?;
        ensure!(
            scope.agent_id == self.instance_agent_id()
                && scope.identity_incarnation == self.inner.identity_incarnation,
            "Wait selection source scope differs"
        );
        let mut cost = WorkQueryMaterial::typed(borrowed, 8)?;
        let address = WorkQueryMaterial::typed(scope, 8)?;
        cost.0 = cost
            .0
            .checked_add(address.0)
            .context("Wait address bytes overflow")?;
        cost.1 = cost
            .1
            .checked_add(address.1)
            .context("Wait address records overflow")?;
        // Fixed Wait/summary/disclosure containers, generated IDs and scalar
        // fields; variable reason/resource/owner strings are borrowed above.
        cost.0 = cost
            .0
            .checked_add(64 * 1024)
            .context("Wait metadata bytes overflow")?;
        cost.1 = cost
            .1
            .checked_add(1024)
            .context("Wait metadata records overflow")?;
        let phase = WorkQueryMaterial::new(scope, ledger, limits, snapshot.allowance)?;
        phase.bounds(cost)?;
        // The returned carrier owns only its metadata: do not retain the
        // original snapshot's shared bodies or charge them again in SQL.
        let owned = ledger.register_owned_bytes(
            scope,
            super::input_evidence_budget::InputMemoryCategory::Pins,
            super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: u64::try_from(cost.0)?,
            },
        )?;
        Ok(Arc::new(WorkQueryMetadataMaterial {
            scope: scope.clone(),
            ledger: ledger.clone(),
            owned,
            cost,
            observed_bytes: u64::try_from(cost.0)?,
            _previous: None,
        }))
    }
    pub(in crate::runtime) fn retain_wait_registration_material(
        &self,
        original: &Arc<WorkQueryMetadataMaterial>,
        registration: &super::waiting::WaitForRegistration,
        plan: Option<&Arc<super::resource_refresh::ObservedWorkPlanRead>>,
    ) -> Result<Arc<WorkQueryMetadataMaterial>> {
        let (records, bytes) = self.resource_material_limits()?;
        self.retain_wait_registration_with_limits(
            original,
            registration,
            plan,
            (usize::try_from(bytes)?, records),
        )
    }
    fn retain_wait_registration_with_limits(
        &self,
        original: &Arc<WorkQueryMetadataMaterial>,
        registration: &super::waiting::WaitForRegistration,
        plan: Option<&Arc<super::resource_refresh::ObservedWorkPlanRead>>,
        limits: (usize, usize),
    ) -> Result<Arc<WorkQueryMetadataMaterial>> {
        let scope = &original.scope;
        let ledger = &self.inner.input_evidence_budget;
        let prior = original.validate_accounting(scope, ledger)?;
        ensure!(
            scope.agent_id == self.instance_agent_id()
                && scope.identity_incarnation == self.inner.identity_incarnation,
            "Wait returned registration material owner differs"
        );
        let plan = plan
            .map(|read| -> Result<(usize, usize)> {
                read.validate_material_binding(scope, ledger)?;
                let cost = read.material_estimate()?;
                Ok((usize::try_from(cost.0)?, cost.1))
            })
            .transpose()?
            .unwrap_or((0, 0));
        // Borrow the actual finite DTO, including every cancelled ID and the
        // pending condition, before registration/outcome/result/Value copies.
        let added = WorkQueryMaterial::typed(registration, 8)?;
        let cost = (
            prior
                .0
                .checked_add(added.0)
                .context("Wait DTO bytes overflow")?,
            prior
                .1
                .checked_add(added.1)
                .context("Wait DTO records overflow")?,
        );
        let configured = self.resource_material_limits()?;
        let limits = (
            limits
                .0
                .min(usize::try_from(configured.1)?)
                .min(64 * 1024 * 1024),
            limits.1.min(configured.0).min(65_536),
        );
        ensure!(
            cost.0.checked_add(plan.0).is_some_and(|n| n <= limits.0)
                && cost.1.checked_add(plan.1).is_some_and(|n| n <= limits.1),
            WorkReadError::ResourceExhausted("Wait actual registration and held material")
        );
        let observed_bytes = u64::try_from(added.0)?;
        let owned = ledger.register_owned_bytes(
            scope,
            super::input_evidence_budget::InputMemoryCategory::Pins,
            super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: observed_bytes,
            },
        )?;
        Ok(Arc::new(WorkQueryMetadataMaterial {
            scope: scope.clone(),
            ledger: ledger.clone(),
            owned,
            cost,
            observed_bytes,
            _previous: Some(original.clone()),
        }))
    }
    pub(crate) async fn capture_work_query_snapshot_for_wait(
        &self,
        result: Option<&Arc<super::WorkItemMutationMaterial>>,
        metadata: &Arc<WorkQueryMetadataMaterial>,
    ) -> Result<WorkQuerySnapshot> {
        let mutation = self.begin_mutation().await?;
        let (_, records, bytes) = mutation.context_input_evidence_policy()?;
        self.capture_work_query_snapshot_with_metadata_from_owner(
            &mutation,
            false,
            result,
            Some(metadata),
            (bytes, records),
        )
    }
    async fn capture_work_query_snapshot_inner(
        &self,
        include_resources: bool,
        result: Option<&Arc<super::WorkItemMutationMaterial>>,
    ) -> Result<WorkQuerySnapshot> {
        // This original named acquisition boundary is unchanged. C4 replaces
        // its identity/OCC adapter; the synchronous selected read below is pure.
        let mutation = self.begin_mutation().await?;
        let (_, records, bytes) = mutation.context_input_evidence_policy()?;
        self.capture_work_query_snapshot_from_owner(
            &mutation,
            include_resources,
            result,
            (bytes, records),
        )
    }
    #[cfg(test)]
    pub(in crate::runtime) fn capture_work_report_query_with_limits_for_test(
        &self,
        mutation: &RuntimeMutation,
        limits: (usize, usize),
    ) -> Result<WorkQuerySnapshot> {
        self.capture_work_query_snapshot_from_owner(mutation, true, None, limits)
    }
    fn capture_work_query_snapshot_from_owner(
        &self,
        mutation: &RuntimeMutation,
        include_resources: bool,
        result: Option<&Arc<super::WorkItemMutationMaterial>>,
        limits: (usize, usize),
    ) -> Result<WorkQuerySnapshot> {
        self.capture_work_query_snapshot_with_metadata_from_owner(
            mutation,
            include_resources,
            result,
            None,
            limits,
        )
    }
    fn capture_work_query_snapshot_with_metadata_from_owner(
        &self,
        mutation: &RuntimeMutation,
        include_resources: bool,
        result: Option<&Arc<super::WorkItemMutationMaterial>>,
        metadata: Option<&Arc<WorkQueryMetadataMaterial>>,
        limits: (usize, usize),
    ) -> Result<WorkQuerySnapshot> {
        let root = mutation.capture_published_root()?;
        let work = root.work.as_ref().ok_or(WorkReadError::IncompleteLive)?;
        let scheduling = root
            .scheduling
            .as_ref()
            .context("Work query Scheduling domain missing")?;
        ensure!(
            root.state.id == work.scope().agent_id && scheduling.scope() == work.scope(),
            "Work query same-root scopes differ"
        );
        let ledger = &self.inner.input_evidence_budget;
        let mut prior = if let Some(result) = result {
            result.validate_material_binding(work.scope(), ledger)?;
            let cost = result.material_estimate();
            (usize::try_from(cost.0)?, cost.1)
        } else {
            (0, 0)
        };
        if let Some(metadata) = metadata {
            let cost = metadata.validate_accounting(work.scope(), ledger)?;
            prior.0 = prior
                .0
                .checked_add(cost.0)
                .context("Wait query prior bytes overflow")?;
            prior.1 = prior
                .1
                .checked_add(cost.1)
                .context("Wait query prior records overflow")?;
        }
        let mut phase = WorkQueryMaterial::new(work.scope(), ledger, limits, prior)?;
        phase.shared(WorkQueryMaterial::typed(root.state.as_ref(), 1)?)?;
        phase.copy(WorkQueryMaterial::typed(root.state.as_ref(), 4)?)?;
        // State-derived selector Strings are within the already admitted State
        // copy material; the selected capture separately admits its own keys.
        let request = super::context_capture::context_work_capture_request(&root.state);
        let (selected, used) = if let Some(result) = result {
            work.capture_with_result_prior(&request, ledger, phase.used, phase.limits, result)?
        } else {
            work.capture_with_prior(&request, ledger, phase.used, phase.limits)?
        };
        phase.used = used;
        let (scheduling, used) =
            scheduling.capture_work_query_triggers_with_prior(ledger, phase.used, phase.limits)?;
        phase.used = used;
        // The original Trigger adapter adds its own selected estimate. Subtract
        // that exact component, leaving every other held allocation debited.
        let selected_scheduling = scheduling.selected_material_estimate()?;
        let other = (
            phase
                .used
                .0
                .checked_sub(usize::try_from(selected_scheduling.0)?)
                .context("Work query Scheduling byte estimate exceeds admitted cost")?,
            phase
                .used
                .1
                .checked_sub(selected_scheduling.1)
                .context("Work query Scheduling count exceeds admitted cost")?,
        );
        let triggers = scheduling.clone_trigger_records(
            ledger,
            phase
                .limits
                .1
                .checked_sub(other.1)
                .context("Work query Trigger remaining records exhausted")?,
            phase
                .limits
                .0
                .checked_sub(other.0)
                .context("Work query Trigger remaining bytes exhausted")?,
        )?;
        phase.shared(triggers.cost)?;
        // Borrowed canonical Work/Wait/Frame bodies bound every map/Vec/summary
        // and original queue category copy before work_queue owns any of them.
        for record in &selected.records {
            let (bytes, nodes) = record.record.resident_material_estimate()?;
            let bytes = bytes
                .checked_add(4096)
                .and_then(|n| n.checked_mul(8))
                .context("Work query projection bytes overflow")?;
            let nodes = nodes
                .checked_add(8)
                .and_then(|n| n.checked_mul(8))
                .context("Work query projection count overflow")?;
            phase.copy((bytes, nodes))?;
        }
        let trigger_material = triggers.material;
        let work_queue = selected.work_queue(&root.state, triggers.records)?;
        let items = work_queue.items;
        let owned = phase.guard();
        let mut snapshot = WorkQuerySnapshot {
            state: root.state.clone(),
            items,
            _owned: owned,
            _scheduling: scheduling,
            _trigger_material: trigger_material,
            allowance: phase.used,
            _result_prior: result.cloned(),
            _metadata_prior: metadata.cloned(),
            work: selected,
            report_sources: None,
            resource_source: None,
        };
        if include_resources {
            snapshot.report_sources = Some(
                super::work_report_sources::CapturedWorkReportSources::capture(
                    &snapshot.work,
                    root.input_evidence.as_deref(),
                    ledger,
                    |cost, owned| {
                        if owned {
                            phase.copy(cost)
                        } else {
                            phase.shared(cost)
                        }
                    },
                )?,
            );
            snapshot.allowance = phase.used;
            snapshot.resource_source = Some(self.capture_work_query_plan_source(
                &root.state,
                work,
                &snapshot.work,
                snapshot.material_estimate()?,
                snapshot._owned.clone(),
            )?);
        }
        Ok(snapshot)
    }
}
#[cfg(test)]
#[path = "work_query_prior_tests.rs"]
mod query_prior_tests;

#[cfg(test)]
#[path = "wait_selection_material_tests.rs"]
mod wait_selection_material_tests;
