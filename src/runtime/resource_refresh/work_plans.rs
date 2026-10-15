//! Original Work/file preparation, distinct from pure observed metadata use.
use super::*;
use crate::runtime::input_evidence_budget::BudgetViewGuard;
use crate::runtime::resident_work::{
    CanonicalWorkRecord, QualifiedWorkRecord, WorkFamily, WorkKey,
};
use std::io::Write;

pub(in crate::runtime) fn borrowed_resource_record_count(
    value: &impl serde::Serialize,
) -> Result<usize> {
    #[derive(Default)]
    struct Counter {
        nodes: usize,
        string: bool,
        escape: bool,
        primitive: bool,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            for byte in bytes {
                if self.string {
                    if self.escape {
                        self.escape = false;
                    } else if *byte == b'\\' {
                        self.escape = true;
                    } else if *byte == b'"' {
                        self.string = false;
                    }
                    continue;
                }
                let start = match *byte {
                    b'"' => {
                        self.string = true;
                        self.primitive = false;
                        true
                    }
                    b'{' | b'[' => {
                        self.primitive = false;
                        true
                    }
                    b'}' | b']' | b',' | b':' => {
                        self.primitive = false;
                        false
                    }
                    b' ' | b'\n' | b'\r' | b'\t' => false,
                    _ if !self.primitive => {
                        self.primitive = true;
                        true
                    }
                    _ => false,
                };
                if start {
                    self.nodes = self.nodes.checked_add(1).ok_or_else(|| {
                        std::io::Error::other("resource Work native count overflow")
                    })?;
                }
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.nodes)
}

#[derive(Debug, thiserror::Error)]
#[error("Work plan requires its original named preparation")]
pub(in crate::runtime) struct WorkPlanFilePreparationRequired;

#[derive(Debug, thiserror::Error)]
#[error("original Work plan publication changed; replan the original command")]
pub(in crate::runtime) struct WorkPlanReplanRequired;

/// Scalar observation only, minted while the original owner holds the read.
/// No Root/Runtime/body reference and no Ready or mutation authority.
#[derive(Debug, Clone, Copy)]
pub(in crate::runtime) struct WorkResourceReadStamp {
    token: crate::runtime::mutation_owner::PublicationToken,
    config_generation: u64,
    host_write_generation: Option<u64>,
    host_available: bool,
}
#[derive(Clone, Copy)]
pub(in crate::runtime) struct WorkPlanReadPrior<'a> {
    state: &'a crate::types::AgentState,
    work: &'a crate::runtime::resident_work::CapturedWork,
    stamp: &'a WorkResourceReadStamp,
    material: (u64, usize),
}
impl<'a> WorkPlanReadPrior<'a> {
    pub(in crate::runtime) fn new(
        state: &'a crate::types::AgentState,
        work: &'a crate::runtime::resident_work::CapturedWork,
        stamp: &'a WorkResourceReadStamp,
        material: (u64, usize),
    ) -> Self {
        Self {
            state,
            work,
            stamp,
            material,
        }
    }
}
/// Finite same-publication Legacy fields for the original prepared report.
/// The original qualified body remains borrowed; its evidence Value is not cloned.
#[derive(Debug)]
pub(super) struct PreparedLegacyReport {
    pub(super) id: String,
    pub(super) text: String,
    pub(super) turn_index: Option<u64>,
    pub(super) created_at: chrono::DateTime<chrono::Utc>,
}
/// Only the selected qualified Work and selected observed plan survive. No
/// Root, complete Resource body, registry or Runtime is retained.
#[derive(Debug)]
pub(in crate::runtime) struct ObservedWorkPlanRead {
    target: Arc<QualifiedWorkRecord>,
    plan: Arc<ObservedPlan>,
    ledger: crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    source_material: (u64, usize),
    prepared_legacy: Option<PreparedLegacyReport>,
    legacy_unavailable: bool,
    legacy_material: (u64, usize),
    _legacy_material: Option<Arc<BudgetViewGuard>>,
    _work_pins: Arc<BudgetViewGuard>,
    _source_material: Arc<BudgetViewGuard>,
    _plan_material: Arc<BudgetViewGuard>,
    _shared_refresh_material: Arc<BudgetViewGuard>,
}
impl ObservedWorkPlanRead {
    pub(super) fn prepared_legacy_report(&self) -> Result<Option<&PreparedLegacyReport>> {
        anyhow::ensure!(
            !self.legacy_unavailable,
            ResourceObservationError::SourceChanged
        );
        Ok(self.prepared_legacy.as_ref())
    }
    pub(super) fn borrowed_result_plan(&self) -> &ObservedPlan {
        self.plan.as_ref()
    }
    pub(super) fn result_plan_material(&self) -> &Arc<BudgetViewGuard> {
        &self._plan_material
    }

    /// Borrowed cumulative allowance for the original SQL actor. Guards already
    /// account for these allocations; this estimate must not register them again.
    pub(in crate::runtime) fn material_estimate(&self) -> Result<(u64, usize)> {
        let (work_bytes, work_records) = self.target.record.resident_material_estimate()?;
        let plan = crate::runtime::resource_observation::plan_material(
            &self.target().id,
            self.plan.as_ref(),
        )?;
        let bytes = u64::try_from(work_bytes)?
            .checked_add(self.source_material.0)
            .and_then(|n| n.checked_add(plan.0))
            .and_then(|n| n.checked_add(self.legacy_material.0))
            .context("Work plan cumulative bytes overflow")?;
        let records = work_records
            .checked_add(self.source_material.1)
            .and_then(|n| n.checked_add(plan.1))
            .and_then(|n| n.checked_add(self.legacy_material.1))
            .context("Work plan cumulative records overflow")?;
        Ok((bytes, records))
    }
    pub(in crate::runtime) fn scope(
        &self,
    ) -> &crate::runtime::resident_input_evidence::InputEvidenceScope {
        &self.target.scope
    }
    pub(in crate::runtime) fn validate_material_binding(
        &self,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<()> {
        anyhow::ensure!(
            self.scope() == scope && self.ledger.same_ledger(ledger),
            "observed Work plan material belongs to another original scope or ledger"
        );
        Ok(())
    }
    pub(in crate::runtime) fn shares_work_allocation(
        &self,
        record: &Arc<QualifiedWorkRecord>,
    ) -> bool {
        Arc::ptr_eq(&self.target, record)
    }
    pub(super) fn target_id(&self) -> &str {
        &self.target().id
    }
    fn target(&self) -> &crate::types::WorkItemRecord {
        let CanonicalWorkRecord::Work(target) = &self.target.record else {
            unreachable!("private qualified Work plan source")
        };
        target
    }
    pub(in crate::runtime) fn requires_first_file(&self) -> bool {
        self.target().plan_artifact.is_none() && matches!(self.plan.as_ref(), ObservedPlan::Absent)
    }
    pub(in crate::runtime) fn apply(
        &self,
        record: &mut crate::types::WorkItemRecord,
    ) -> Result<bool> {
        let target = self.target();
        anyhow::ensure!(
            target.id == record.id
                && target.agent_id == record.agent_id
                && (target.revision == record.revision
                    || target.revision.checked_add(1) == Some(record.revision)),
            "observed Work plan belongs to another target"
        );
        let plan = match self.plan.as_ref() {
            ObservedPlan::Present(plan) => plan,
            ObservedPlan::Absent if target.plan_artifact.is_none() => {
                return Err(WorkPlanFilePreparationRequired.into());
            }
            ObservedPlan::Absent | ObservedPlan::RecordedMissing => {
                anyhow::bail!("missing plan artifact for recorded Work item")
            }
            ObservedPlan::RefreshFailed(_) => {
                anyhow::bail!("plan artifact observation refresh failed")
            }
        };
        anyhow::ensure!(
            plan.owner_agent_id == record.agent_id
                && plan.relative_path == crate::work_item_plan::plan_relative_path(&record.id),
            "observed Work plan address differs from its qualified source"
        );
        let changed = record.plan_artifact.as_ref() != Some(plan);
        record.plan_artifact = Some(plan.clone());
        Ok(changed)
    }
}
impl RuntimeHandle {
    pub(in crate::runtime) fn capture_work_resource_read_stamp(
        &self,
        root: &crate::runtime::resident_state::AgentSnapshot,
        state: &crate::types::AgentState,
        work: &crate::runtime::resident_work::CapturedWork,
    ) -> Result<WorkResourceReadStamp> {
        anyhow::ensure!(
            root.state.as_ref() == state
                && root
                    .work
                    .as_ref()
                    .is_some_and(|domain| domain.scope() == &work.scope
                        && domain.coverage().revision == work.revision),
            WorkPlanReplanRequired
        );
        let host = self
            .inner
            .host_bridge
            .as_ref()
            .map(RuntimeHostBridge::context_resource_generation)
            .transpose();
        let (host_write_generation, host_available) = match host {
            Ok(value) => (value, true),
            Err(error)
                if matches!(
                    error.downcast_ref::<ResourceObservationError>(),
                    Some(
                        ResourceObservationError::NotReady(_)
                            | ResourceObservationError::GenerationExhausted
                    )
                ) =>
            {
                (None, false)
            }
            Err(error) => return Err(error),
        };
        Ok(WorkResourceReadStamp {
            token: root.token,
            config_generation: self
                .inner
                .resource_config_generation
                .load(Ordering::Acquire),
            host_write_generation,
            host_available,
        })
    }
    fn validate_work_plan_read_prior(
        &self,
        root: &crate::runtime::resident_state::AgentSnapshot,
        prior: WorkPlanReadPrior<'_>,
    ) -> Result<()> {
        anyhow::ensure!(
            root.token == prior.stamp.token
                && root.state.as_ref() == prior.state
                && root
                    .work
                    .as_ref()
                    .is_some_and(|domain| domain.scope() == &prior.work.scope
                        && domain.coverage().revision == prior.work.revision)
                && self
                    .inner
                    .resource_config_generation
                    .load(Ordering::Acquire)
                    == prior.stamp.config_generation
                && prior.stamp.host_available,
            WorkPlanReplanRequired
        );
        let current = self
            .inner
            .host_bridge
            .as_ref()
            .map(RuntimeHostBridge::context_resource_generation)
            .transpose()?;
        anyhow::ensure!(
            current == prior.stamp.host_write_generation,
            WorkPlanReplanRequired
        );
        Ok(())
    }
    fn with_original_work_plan_fence<T>(
        &self,
        source: &ContextResourceSource,
        prior: Option<WorkPlanReadPrior<'_>>,
        read: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        if let Some(prior) = prior {
            anyhow::ensure!(
                source.config_generation == prior.stamp.config_generation
                    && source.host_write_generation == prior.stamp.host_write_generation,
                WorkPlanReplanRequired
            );
        }
        let expected_host = prior
            .map(|prior| prior.stamp.host_write_generation)
            .unwrap_or(source.host_write_generation);
        let read = || {
            // Verify only the original expected token. No successor facts become
            // inputs to this candidate, even after a long borrowed size pass.
            if let Some(prior) = prior {
                let current = self
                    .inner
                    .mutation_owner
                    .capture()?
                    .context(WorkPlanReplanRequired)?;
                anyhow::ensure!(current.token == prior.stamp.token, WorkPlanReplanRequired);
            }
            let captured = read()?;
            anyhow::ensure!(
                self.inner
                    .resource_config_generation
                    .load(Ordering::Acquire)
                    == source.config_generation,
                WorkPlanReplanRequired
            );
            Ok(captured)
        };
        match (&self.inner.host_bridge, expected_host) {
            (Some(bridge), Some(generation)) => {
                bridge.with_context_resource_generation(generation, read)
            }
            (None, None) => read(),
            _ => Err(WorkPlanReplanRequired.into()),
        }
    }
    /// Pure published-root acquisition. The caller may hold the original owner;
    /// this function neither prepares domains nor reads filesystem/SQL.
    #[cfg(test)]
    pub(in crate::runtime) fn capture_observed_work_plan(
        &self,
        id: &str,
        expected_revision: u64,
    ) -> Result<Arc<ObservedWorkPlanRead>> {
        self.capture_observed_work_plan_with_prior(id, expected_revision, None)
    }
    pub(in crate::runtime) fn capture_observed_work_plan_with_prior(
        &self,
        id: &str,
        expected_revision: u64,
        prior: Option<WorkPlanReadPrior<'_>>,
    ) -> Result<Arc<ObservedWorkPlanRead>> {
        self.capture_observed_work_plan_with_limits(
            id,
            expected_revision,
            prior,
            self.resource_material_limits()?,
        )
    }
    fn capture_observed_work_plan_with_limits(
        &self,
        id: &str,
        expected_revision: u64,
        prior: Option<WorkPlanReadPrior<'_>>,
        limits: (usize, u64),
    ) -> Result<Arc<ObservedWorkPlanRead>> {
        self.capture_observed_work_plan_after_validation(
            id,
            expected_revision,
            prior,
            limits,
            || Ok(()),
        )
    }
    fn capture_observed_work_plan_after_validation(
        &self,
        id: &str,
        expected_revision: u64,
        prior: Option<WorkPlanReadPrior<'_>>,
        limits: (usize, u64),
        after_validation: impl FnOnce() -> Result<()>,
    ) -> Result<Arc<ObservedWorkPlanRead>> {
        let configured = self.resource_material_limits()?;
        anyhow::ensure!(
            limits.0 > 0 && limits.1 > 0 && limits.0 <= configured.0 && limits.1 <= configured.1,
            ResourceObservationError::ResourceExhausted("Work plan request")
        );
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context(WorkPlanFilePreparationRequired)?;
        if let Some(prior) = prior {
            self.validate_work_plan_read_prior(&root, prior)?;
        }
        after_validation()?;
        let work = root
            .work
            .as_ref()
            .context(WorkPlanFilePreparationRequired)?;
        let qualified = work
            .qualified_context_plan_source(id)
            .context(WorkPlanFilePreparationRequired)?;
        let CanonicalWorkRecord::Work(target) = &qualified.record else {
            unreachable!("qualified Work-only getter")
        };
        anyhow::ensure!(
            target.revision == expected_revision,
            "Work item changed before observed plan acquisition"
        );
        let (records, bytes) = limits;
        let (mut work_bytes, mut work_records) = qualified.record.resident_material_estimate()?;
        let (prior_bytes, prior_records) = if let Some(prior) = prior {
            let captured = prior.work;
            let material = prior.material;
            anyhow::ensure!(
                captured.scope == qualified.scope,
                "Work plan prior belongs to another original scope"
            );
            let minimum = captured.selected_material_estimate()?;
            anyhow::ensure!(
                material.0 >= minimum.0 && material.1 >= minimum.1,
                "Work plan prior does not cover its original selected allocation"
            );
            // Both carriers retain one allocation when their immutable Arc is equal.
            if captured
                .records
                .iter()
                .any(|record| Arc::ptr_eq(record, qualified))
            {
                work_bytes = 0;
                work_records = 0;
            }
            material
        } else {
            (0, 0)
        };
        let source_remaining_bytes = bytes
            .checked_sub(prior_bytes)
            .and_then(|n| {
                u64::try_from(work_bytes)
                    .ok()
                    .and_then(|used| n.checked_sub(used))
            })
            .ok_or(ResourceObservationError::ResourceExhausted(
                "combined Work plan bytes",
            ))?;
        let source_remaining_records = records
            .checked_sub(prior_records)
            .and_then(|n| n.checked_sub(work_records))
            .ok_or(ResourceObservationError::ResourceExhausted(
                "combined Work plan records",
            ))?;
        // Admit the source's borrowed shape against the same remaining phase
        // before allocating/cloning its descriptor or registering its guard.
        let source = self.context_resource_source_with_material(
            &root.state,
            Some(work),
            (source_remaining_records, source_remaining_bytes),
        )?;
        if let Some(prior) = prior {
            anyhow::ensure!(
                source.source.config_generation == prior.stamp.config_generation
                    && source.source.host_write_generation == prior.stamp.host_write_generation,
                WorkPlanReplanRequired
            );
        }
        anyhow::ensure!(
            qualified.scope == source.source.scope,
            "Work plan source scope differs from its publication"
        );
        let (legacy, legacy_cost, legacy_guard, legacy_unavailable) = self
            .freeze_original_work_legacy_report(
                &root,
                qualified,
                (
                    remaining_work_records(records, prior_records, source.records, work_records)?,
                    remaining_work_bytes(bytes, prior_bytes, source.bytes, work_bytes)?,
                ),
            )?;
        let remaining_bytes = bytes
            .checked_sub(prior_bytes)
            .and_then(|n| n.checked_sub(source.bytes))
            .and_then(|n| n.checked_sub(legacy_cost.0))
            .and_then(|n| {
                u64::try_from(work_bytes)
                    .ok()
                    .and_then(|used| n.checked_sub(used))
            })
            .ok_or(ResourceObservationError::ResourceExhausted(
                "combined Work plan bytes",
            ))?;
        let remaining_records = records
            .checked_sub(prior_records)
            .and_then(|n| n.checked_sub(source.records))
            .and_then(|n| n.checked_sub(legacy_cost.1))
            .and_then(|n| n.checked_sub(work_records))
            .ok_or(ResourceObservationError::ResourceExhausted(
                "combined Work plan records",
            ))?;
        let observed = self.with_original_work_plan_fence(&source.source, prior, || {
            self.inner.context_resources.capture_work_plan(
                &source.source,
                id,
                &self.inner.input_evidence_budget,
                remaining_bytes,
                remaining_records,
            )
        });
        if observed.as_ref().err().is_some_and(|error| {
            matches!(
                error.downcast_ref::<ResourceObservationError>(),
                Some(ResourceObservationError::SourceChanged)
            )
        }) {
            self.invalidate_context_resources(ResourceInvalidation::WorkspaceChange)?;
        }
        let (plan, material, shared) = observed?;
        let work_pins = self.inner.input_evidence_budget.register_work_view(
            &source.source.scope,
            InputMemoryCategory::Pins,
            std::iter::once(qualified),
        )?;
        let selected = Arc::new(ObservedWorkPlanRead {
            target: qualified.clone(),
            plan,
            ledger: self.inner.input_evidence_budget.clone(),
            source_material: (source.bytes, source.records),
            prepared_legacy: legacy,
            legacy_unavailable,
            legacy_material: legacy_cost,
            _legacy_material: legacy_guard,
            _work_pins: work_pins,
            _source_material: source._material,
            _plan_material: material,
            _shared_refresh_material: shared,
        });
        selected.validate_material_binding(&qualified.scope, &self.inner.input_evidence_budget)?;
        // Reuse this same borrowed contract in the later original SQL actor.
        // Pointer equality alone permits subtracting an already-held Work body.
        let mut held = selected.material_estimate()?;
        if let Some(prior) = prior {
            let captured = prior.work;
            let material = prior.material;
            if captured
                .records
                .iter()
                .any(|record| selected.shares_work_allocation(record))
            {
                let (body_bytes, body_records) = qualified.record.resident_material_estimate()?;
                held.0 = held
                    .0
                    .checked_sub(u64::try_from(body_bytes)?)
                    .context("Work plan shared body bytes mismatch")?;
                held.1 = held
                    .1
                    .checked_sub(body_records)
                    .context("Work plan shared body count mismatch")?;
            }
            held.0 = held
                .0
                .checked_add(material.0)
                .context("Work plan phase bytes overflow")?;
            held.1 = held
                .1
                .checked_add(material.1)
                .context("Work plan phase records overflow")?;
        }
        anyhow::ensure!(
            held.0 <= bytes && held.1 <= records,
            ResourceObservationError::ResourceExhausted("retained Work plan phase")
        );
        Ok(selected)
    }
    /// Existing Work mutation preflight. Only a genuine qualified legacy Work
    /// with observed absence enters its original file-creation workflow.
    #[inline(never)]
    pub(in crate::runtime) fn prepare_observed_work_plan<'a>(
        &'a self,
        id: &'a str,
        expected_revision: u64,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<Arc<ObservedWorkPlanRead>>> + Send + 'a>,
    > {
        self.prepare_observed_work_plan_with_prior(id, expected_revision, None)
    }
    #[inline(never)]
    pub(in crate::runtime) fn prepare_observed_work_plan_with_prior<'a>(
        &'a self,
        id: &'a str,
        expected_revision: u64,
        prior: Option<WorkPlanReadPrior<'a>>,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<Arc<ObservedWorkPlanRead>>> + Send + 'a>,
    > {
        Box::pin(self.prepare_observed_work_plan_inner(id, expected_revision, prior))
    }
    async fn prepare_observed_work_plan_inner(
        &self,
        id: &str,
        expected_revision: u64,
        prior: Option<WorkPlanReadPrior<'_>>,
    ) -> Result<Arc<ObservedWorkPlanRead>> {
        self.start_context_resource_observer()?;
        for _ in 0..6 {
            match self.capture_observed_work_plan_with_prior(id, expected_revision, prior) {
                Ok(read) if !read.requires_first_file() => return Ok(read),
                Ok(read) => {
                    let _original_admission = self.execution_admission_lease()?;
                    // The private carrier holds the exact qualified Work/body
                    // while the original owner and Root have already dropped.
                    let (_, _file_material) = self.create_work_plan_file(read.target(), None)?;
                    self.refresh_context_resources().await?;
                }
                Err(error) if error.downcast_ref::<WorkPlanFilePreparationRequired>().is_some() => {
                    let mut owner = self.begin_mutation().await?;
                    let scope = owner.input_evidence_scope();
                    let bytes = u64::try_from(id.len())?.checked_add(1024)
                        .and_then(|n| n.checked_mul(8)).context("Work plan selector bytes overflow")?;
                    let (records, limit) = self.resource_material_limits()?;
                    anyhow::ensure!(bytes <= limit && records >= 64,
                        ResourceObservationError::ResourceExhausted("Work plan selector"));
                    let _selector = self.inner.input_evidence_budget.register_owned_bytes(
                        &scope, InputMemoryCategory::Candidate,
                        OwnedBytesEstimate { payload_bytes: 0, metadata_estimate_bytes: bytes },
                    )?;
                    let mut request = crate::runtime::work_reads::live_work_request();
                    request.mandatory_roots.insert(WorkKey::new(WorkFamily::Work, id));
                    owner.prepare_owner_work_request(&request).await?;
                    drop(owner);
                    self.inner.context_resource_observer.notify.notify_one();
                }
                Err(error) if matches!(error.downcast_ref::<ResourceObservationError>(),
                    Some(ResourceObservationError::SourceChanged | ResourceObservationError::ObsoleteRefresh
                        | ResourceObservationError::NotReady(crate::runtime::resource_observation::ResourceFreshness::Dirty(_)
                            | crate::runtime::resource_observation::ResourceFreshness::Unobserved))) => {
                    self.inner.context_resource_observer.notify.notify_one();
                    self.wait_for_context_resources().await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(anyhow!(
            "original Work plan preparation could not capture a stable source"
        ))
    }
}

#[cfg(test)]
#[path = "work_plans_tests.rs"]
mod tests;

fn remaining_work_records(total: usize, prior: usize, source: usize, work: usize) -> Result<usize> {
    total
        .checked_sub(prior)
        .and_then(|n| n.checked_sub(source))
        .and_then(|n| n.checked_sub(work))
        .context(ResourceObservationError::ResourceExhausted(
            "prepared Legacy Work material",
        ))
}
fn remaining_work_bytes(total: u64, prior: u64, source: u64, work: usize) -> Result<u64> {
    total
        .checked_sub(prior)
        .and_then(|n| n.checked_sub(source))
        .and_then(|n| n.checked_sub(work as u64))
        .context(ResourceObservationError::ResourceExhausted(
            "prepared Legacy Work material",
        ))
}
impl RuntimeHandle {
    fn freeze_original_work_legacy_report(
        &self,
        root: &crate::runtime::resident_state::AgentSnapshot,
        target: &Arc<QualifiedWorkRecord>,
        limits: (usize, u64),
    ) -> Result<(
        Option<PreparedLegacyReport>,
        (u64, usize),
        Option<Arc<BudgetViewGuard>>,
        bool,
    )> {
        use crate::runtime::{
            resident_input_evidence::{
                InputEvidenceFamily, LegacyDeliverySourceAvailability, PreparedInputEvidence,
            },
            resident_work::WorkDeliveryHead,
        };
        let CanonicalWorkRecord::Work(work) = &target.record else {
            unreachable!()
        };
        let Some(head) = root
            .work
            .as_ref()
            .and_then(|domain| domain.coverage().legacy_delivery_heads.get(&work.id))
            .map(|heads| &heads.latest_any)
        else {
            return Ok((None, (0, 0), None, true));
        };
        let WorkDeliveryHead::Present {
            record_id,
            content_digest,
            source_revision,
            availability: LegacyDeliverySourceAvailability::Present,
        } = head
        else {
            return Ok((
                None,
                (0, 0),
                None,
                !matches!(head, WorkDeliveryHead::Absent),
            ));
        };
        let Some(inputs) = root.input_evidence.as_ref() else {
            return Ok((None, (0, 0), None, true));
        };
        anyhow::ensure!(
            inputs.belongs_to(&target.scope)
                && inputs.shares_query_ledger(&self.inner.input_evidence_budget),
            "prepared Legacy report Input scope/ledger differs"
        );
        let Some(record) =
            inputs.borrowed_work_report_source(InputEvidenceFamily::LegacyDelivery, record_id)
        else {
            return Ok((None, (0, 0), None, true));
        };
        let PreparedInputEvidence::LegacyDelivery(body) = &record.record else {
            anyhow::bail!("prepared Legacy report family differs")
        };
        anyhow::ensure!(
            record.locator.scope == target.scope
                && record.locator.content_digest == *content_digest
                && (*source_revision == 0 || record.locator.revision == *source_revision)
                && body.id == *record_id
                && body.work_item_id == work.id
                && body.agent_id == work.agent_id,
            "prepared Legacy report differs from original qualified Work head"
        );
        let bytes = u64::try_from(
            body.id
                .len()
                .checked_add(body.text.len())
                .context("prepared Legacy text overflow")?,
        )?
        .checked_mul(8)
        .and_then(|n| n.checked_add(8192))
        .context("prepared Legacy fields bytes overflow")?;
        let count = 64;
        anyhow::ensure!(
            bytes <= limits.1 && count <= limits.0,
            ResourceObservationError::ResourceExhausted("prepared Legacy report fields")
        );
        let guard = self.inner.input_evidence_budget.register_owned_bytes(
            &target.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        Ok((
            Some(PreparedLegacyReport {
                id: body.id.clone(),
                text: body.text.clone(),
                turn_index: body.source_turn_index,
                created_at: body.created_at,
            }),
            (bytes, count),
            Some(guard),
            false,
        ))
    }
}
