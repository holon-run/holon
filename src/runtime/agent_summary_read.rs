//! Scheduling observations follow the original summary DTO until serialization.
use super::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger};
use super::resident_scheduling::CapturedScheduling;
use super::scheduling_output::{project_selected_scheduling_records, SchedulingReadMaterial};
use super::*;
use std::ops::Deref;

/// The JSON representation is the original `AgentSummary`.
pub struct OwnedAgentSummaryRead {
    summary: AgentSummary,
    material: AgentSummaryReadMaterial,
}
pub(crate) struct AgentSummaryReadMaterial {
    scope: super::resident_input_evidence::InputEvidenceScope,
    ledger: InputEvidenceBudgetLedger,
    _scheduling: SchedulingReadMaterial,
    _owned: Vec<Arc<BudgetViewGuard>>,
    _resources: Option<Arc<super::resource_observation::CapturedResourceObservations>>,
    _skills: Option<crate::host::AgentSkillsReadMaterial>,
    _model: Option<Arc<super::selected_model_read::SelectedModelReadMaterial>>,
    _children: Option<Box<crate::host::ChildAgentSummariesReadMaterial>>,
}
impl OwnedAgentSummaryRead {
    #[cfg(test)]
    pub(crate) fn children_material_for_test(
        &self,
    ) -> Option<&crate::host::ChildAgentSummariesReadMaterial> {
        self.material._children.as_deref()
    }
    pub(crate) fn new(
        summary: AgentSummary,
        scheduling: SchedulingReadMaterial,
        owned: Vec<Arc<BudgetViewGuard>>,
        scope: super::resident_input_evidence::InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Self {
        Self {
            summary,
            material: AgentSummaryReadMaterial {
                scope,
                ledger: ledger.clone(),
                _scheduling: scheduling,
                _owned: owned,
                _resources: None,
                _skills: None,
                _model: None,
                _children: None,
            },
        }
    }

    pub(crate) fn with_resource_observations(
        mut self,
        resources: Arc<super::resource_observation::CapturedResourceObservations>,
    ) -> Self {
        self.material._resources = Some(resources);
        self
    }

    pub(crate) fn with_skills_material(
        mut self,
        material: crate::host::AgentSkillsReadMaterial,
    ) -> Self {
        self.material._skills = Some(material);
        self
    }

    pub(super) fn with_selected_model_material(
        mut self,
        material: Arc<super::selected_model_read::SelectedModelReadMaterial>,
    ) -> Self {
        self.material._model = Some(material);
        self
    }

    pub(super) fn with_children_material(
        mut self,
        material: Option<crate::host::ChildAgentSummariesReadMaterial>,
    ) -> Self {
        self.material._children = material.map(Box::new);
        self
    }

    // The caller keeps `material` through the synchronous DTO serialization.
    pub(crate) fn into_parts(self) -> (AgentSummary, AgentSummaryReadMaterial) {
        (self.summary, self.material)
    }
}
/// One concrete encoded summary owns its original material through the last
/// HTTP Bytes clone. It retains no publication or Runtime/database handle.
pub(crate) struct AgentSummaryJson {
    bytes: Vec<u8>,
    _read: OwnedAgentSummaryRead,
    _encoded: Arc<BudgetViewGuard>,
}
impl AsRef<[u8]> for AgentSummaryJson {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum AgentSummaryWireError {
    #[error("ResourceExhausted: agent summary source and JSON output material")]
    ResourceExhausted,
    #[error("agent summary wire material belongs to another scope or ledger")]
    ForeignMaterial,
}
struct SummaryJsonPlan {
    encoded_len: usize,
    own: (u64, usize),
    total: (u64, usize),
}
impl OwnedAgentSummaryRead {
    fn json_wire_plan(&self) -> Result<SummaryJsonPlan> {
        use super::input_evidence_budget::serialized_bytes;
        let source = &self.material;
        anyhow::ensure!(
            self.summary.agent.id == source.scope.agent_id
                && self.summary.identity.agent_id == source.scope.agent_id
                && self.summary.identity.incarnation == source.scope.identity_incarnation,
            AgentSummaryWireError::ForeignMaterial
        );
        source
            ._scheduling
            .validate_material_binding(&source.scope, &source.ledger)
            .context(AgentSummaryWireError::ForeignMaterial)?;
        for guard in &source._owned {
            guard
                .validate_owned_material_binding(&source.scope, &source.ledger, 0)
                .context(AgentSummaryWireError::ForeignMaterial)?;
        }
        let scheduling = source._scheduling.material_estimate()?;
        let prior = if let Some(skills) = source._skills.as_ref() {
            // Cold skills already carry Scheduling as their actual prior.
            let actual = skills
                .validated_material_estimate(&source.scope, &source.ledger)
                .context(AgentSummaryWireError::ForeignMaterial)?;
            anyhow::ensure!(
                actual.0 >= scheduling.0 && actual.1 >= scheduling.1,
                AgentSummaryWireError::ForeignMaterial
            );
            actual
        } else {
            scheduling
        };
        if let Some(model) = source._model.as_ref() {
            model
                .validate_accounting(&source.scope, &source.ledger)
                .context(AgentSummaryWireError::ForeignMaterial)?;
            let actual = model.material_estimate();
            anyhow::ensure!(
                actual.0 <= prior.0 && actual.1 <= prior.1,
                AgentSummaryWireError::ForeignMaterial
            );
        }
        if let Some(resources) = source._resources.as_ref() {
            resources
                .validate_material_binding(&source.scope, &source.ledger)
                .context(AgentSummaryWireError::ForeignMaterial)?;
            let actual = resources.material_estimate();
            // Loaded capture already borrowed resources into Scheduling's
            // cumulative phase. Do not register/debit that allocation twice.
            anyhow::ensure!(
                actual.0 <= prior.0 && actual.1 <= prior.1,
                AgentSummaryWireError::ForeignMaterial
            );
        }
        let prior = if let Some(children) = source._children.as_ref() {
            children.validate_accounting(&source.ledger)?;
            let actual = children.material_estimate()?;
            (
                prior
                    .0
                    .checked_add(actual.0)
                    .context(AgentSummaryWireError::ResourceExhausted)?,
                prior
                    .1
                    .checked_add(actual.1)
                    .context(AgentSummaryWireError::ResourceExhausted)?,
            )
        } else {
            prior
        };
        // The new allocation is an encoded byte buffer, not another JSON tree.
        // Stream the original DTO before reserving its exact capacity.
        let encoded = serialized_bytes(&self.summary)?;
        let exhausted = || AgentSummaryWireError::ResourceExhausted;
        let own = (encoded.checked_add(4096).context(exhausted())?, 16usize);
        let total = (
            prior.0.checked_add(own.0).context(exhausted())?,
            prior.1.checked_add(own.1).context(exhausted())?,
        );
        Ok(SummaryJsonPlan {
            encoded_len: usize::try_from(encoded)?,
            own,
            total,
        })
    }
    pub(crate) fn into_json_wire(self) -> Result<AgentSummaryJson> {
        self.into_json_wire_with_limits((usize::MAX, usize::MAX))
    }
    pub(crate) fn into_json_wire_with_limits(
        self,
        limits: (usize, usize),
    ) -> Result<AgentSummaryJson> {
        use super::input_evidence_budget::{InputMemoryCategory, OwnedBytesEstimate};
        let plan = self.json_wire_plan()?;
        let configured = self.material.ledger.limits();
        let bytes = configured
            .history_per_agent_bytes
            .checked_add(configured.pending_per_agent_bytes)
            .context(AgentSummaryWireError::ResourceExhausted)?
            .min(64 * 1024 * 1024)
            .min(u64::try_from(limits.0)?);
        let records = usize::try_from(configured.record_limit.min(65_536))?.min(limits.1);
        anyhow::ensure!(
            plan.total.0 <= bytes && plan.total.1 <= records,
            AgentSummaryWireError::ResourceExhausted
        );
        let encoded = self.material.ledger.register_owned_bytes(
            &self.material.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(plan.encoded_len)?,
                metadata_estimate_bytes: plan
                    .own
                    .0
                    .checked_sub(u64::try_from(plan.encoded_len)?)
                    .context(AgentSummaryWireError::ResourceExhausted)?,
            },
        )?;
        // This writer cannot grow beyond the admitted byte buffer even if a
        // future serializer accidentally becomes non-deterministic.
        struct ExactJson {
            bytes: Vec<u8>,
            limit: usize,
        }
        impl std::io::Write for ExactJson {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self
                    .bytes
                    .len()
                    .checked_add(bytes.len())
                    .is_none_or(|n| n > self.limit)
                {
                    return Err(std::io::Error::other(
                        "summary JSON exceeded its admitted length",
                    ));
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = ExactJson {
            bytes: Vec::with_capacity(plan.encoded_len),
            limit: plan.encoded_len,
        };
        serde_json::to_writer(&mut writer, &self.summary)?;
        anyhow::ensure!(
            writer.bytes.len() == plan.encoded_len,
            "summary JSON changed its admitted length"
        );
        Ok(AgentSummaryJson {
            bytes: writer.bytes,
            _read: self,
            _encoded: encoded,
        })
    }
}

impl Deref for OwnedAgentSummaryRead {
    type Target = AgentSummary;
    fn deref(&self) -> &Self::Target {
        &self.summary
    }
}
impl std::fmt::Debug for OwnedAgentSummaryRead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.summary, formatter)
    }
}
impl serde::Serialize for OwnedAgentSummaryRead {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&self.summary, serializer)
    }
}

pub(crate) fn active_trigger_summaries(
    records: Vec<ExternalTriggerRecord>,
) -> Vec<ExternalTriggerSummary> {
    records
        .into_iter()
        .filter(|record| record.status == ExternalTriggerStatus::Active)
        .map(|record| ExternalTriggerSummary {
            external_trigger_id: record.external_trigger_id,
            target_agent_id: record.target_agent_id,
            scope: record.scope,
            delivery_mode: record.delivery_mode,
            status: record.status,
            delivery_count: record.delivery_count,
            created_at: record.created_at,
            revoked_at: record.revoked_at,
            last_delivered_at: record.last_delivered_at,
        })
        .collect()
}
pub(super) struct SummaryProjectionRead {
    pub(super) read: super::state_mutation::PreparedSchedulerRead,
    pub(super) resources: Arc<super::resource_observation::CapturedResourceObservations>,
    pub(super) owned: Arc<BudgetViewGuard>,
    pub(super) trigger_prior: (usize, usize),
    pub(super) identity: crate::types::AgentIdentityView,
    pub(super) model: crate::types::AgentModelState,
    pub(super) model_material: Arc<super::selected_model_read::SelectedModelReadMaterial>,
}

impl RuntimeHandle {
    // First loaded gate is an actual published header, with no SQL or State DTO copy.
    pub(crate) async fn capture_active_identity_read(
        &self,
    ) -> Result<(Arc<crate::types::AgentIdentityRecord>, Arc<BudgetViewGuard>)> {
        let owner = self.inner.mutation_owner.reserve().await?;
        let root = owner
            .capture_ready_snapshot()?
            .context("identity has no published State")?;
        let identity = root.active_identity()?;
        anyhow::ensure!(
            identity.agent_id == self.inner.mutation_owner.agent_id()
                && identity.incarnation == self.inner.identity_incarnation,
            "loaded identity differs from its actual owner"
        );
        let guard = root
            ._identity_budget
            .as_ref()
            .context("actual identity material requires preparation")?;
        Ok((identity.clone(), guard.clone()))
    }
    #[inline(never)]
    pub(crate) fn capture_agent_skills_read(
        &self,
    ) -> std::pin::Pin<
        Box<
            impl std::future::Future<Output = Result<crate::host::OwnedAgentSkillsRead>> + Send + '_,
        >,
    > {
        Box::pin(async move {
            let owner = self.inner.mutation_owner.reserve().await?;
            let root = owner
                .capture_ready_snapshot()?
                .ok_or(super::current_read_facts::CurrentReadError::RetiredScope)?;
            let identity = root.active_identity()?;
            anyhow::ensure!(
                identity.agent_id == owner.agent_id()
                    && identity.incarnation == owner.identity_incarnation(),
                "skills publication differs from its actual owner"
            );
            let _identity_guard = root
                ._identity_budget
                .as_ref()
                .context("actual skills identity material requires preparation")?;
            let resources = Arc::new(self.capture_context_resources_for_snapshot(
                &root.state,
                root.work.as_deref(),
                None,
            )?);
            Ok(crate::host::OwnedAgentSkillsRead::from_observation(
                resources,
                &root.state.active_skills,
                self.inner.resource_default_skill_visibility,
            ))
        })
    }

    #[inline(never)]
    pub(super) fn capture_summary_projection(
        &self,
    ) -> std::pin::Pin<
        Box<impl std::future::Future<Output = Result<SummaryProjectionRead>> + Send + '_>,
    > {
        Box::pin(async move {
            let owner = self.inner.mutation_owner.reserve().await?;
            owner.validate_owner(&self.inner.mutation_owner)?;
            let root = owner
                .capture_ready_snapshot()?
                .ok_or(super::current_read_facts::CurrentReadError::RetiredScope)?;
            let identity = root.active_identity()?;
            let scope = root
                .work
                .as_ref()
                .ok_or(super::resident_work::WorkReadError::IncompleteLive)?
                .scope();
            let identity_guard = root
                ._identity_budget
                .as_ref()
                .context("summary identity has no original material")?;
            identity_guard.validate_owned_material_binding(
                scope,
                &self.inner.input_evidence_budget,
                0,
            )?;
            // This actual publication allocation is still held during every
            // Scheduler selector/body copy; borrow it before the first one.
            let identity_material = (identity_guard.owned_material_bytes()?, 24 * 8);
            let read = self.capture_scheduler_read_from_snapshot_with_material(
                &root,
                &super::resident_work::WorkCaptureRequest::current(),
                self.now(),
                (usize::MAX, usize::MAX),
                identity_material,
                None,
            )?;
            anyhow::ensure!(
                Arc::ptr_eq(&read.state, &root.state),
                "summary resource State differs from its held publication"
            );
            // The immutable identity body shares its original guard. Charge the
            // distinct display/wire copies before cloning their String fields.
            let identity_bytes = super::input_evidence_budget::serialized_bytes(identity.as_ref())?
                .checked_add(512)
                .context("summary identity material overflow")?;
            let identity_output = identity_bytes
                .checked_mul(4)
                .and_then(|n| n.checked_add(2048))
                .context("summary identity output overflow")?;
            let mut retained = read.material_estimate()?;
            retained.0 = retained
                .0
                .checked_add(identity_material.0)
                .context("summary identity retained bytes overflow")?;
            retained.1 = retained
                .1
                .checked_add(identity_material.1)
                .context("summary identity retained records overflow")?;
            let mut output = read.summary_output_material_estimate()?;
            output.0 = output
                .0
                .checked_add(identity_output)
                .context("summary identity output bytes overflow")?;
            output.1 = output
                .1
                .checked_add(24 * 8)
                .context("summary identity output records overflow")?;
            let limits = self.inner.input_evidence_budget.limits();
            let bytes = limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("summary byte limit overflow")?
                .min(64 * 1024 * 1024);
            let records = usize::try_from(limits.record_limit.min(65_536))?;
            let prior_bytes = retained
                .0
                .checked_add(output.0)
                .context("summary retained bytes overflow")?;
            let prior_records = retained
                .1
                .checked_add(output.1)
                .context("summary retained count overflow")?;
            anyhow::ensure!(
                prior_bytes <= bytes && prior_records <= records,
                super::resident_scheduling::SchedulingReadError::ResourceExhausted(
                    "summary State/Wait and retained input"
                )
            );
            let owned = self.inner.input_evidence_budget.register_owned_bytes(
                &read.work.scope,
                super::input_evidence_budget::InputMemoryCategory::Pins,
                super::input_evidence_budget::OwnedBytesEstimate {
                    payload_bytes: output.0,
                    metadata_estimate_bytes: 0,
                },
            )?;
            let resources = self.capture_context_resources_for_snapshot_with_limits(
                &root.state,
                root.work.as_deref(),
                None,
                records - prior_records,
                bytes - prior_bytes,
            )?;
            let resource_material = resources.material_estimate();
            let model_prior = (
                prior_bytes
                    .checked_add(resource_material.0)
                    .context("summary model prior bytes overflow")?,
                prior_records
                    .checked_add(resource_material.1)
                    .context("summary model prior nodes overflow")?,
            );
            let config = self.inner.config_snapshot.load();
            let (model, model_material) = super::selected_model_read::capture_selected_model_state(
                &config.model_catalog,
                &config.base_context_config,
                &read.state,
                &read.work.scope,
                &self.inner.input_evidence_budget,
                &read._owned_budget,
                (usize::MAX, usize::MAX),
                model_prior,
            )?;
            let model_cost = model_material.material_estimate();
            drop(config);
            // The Trigger projector counts its own retained Scheduling once.
            let selected = read.scheduling.selected_material_estimate()?;
            let trigger_prior = (
                usize::try_from(
                    prior_bytes
                        .checked_add(resource_material.0)
                        .and_then(|n| n.checked_add(model_cost.0))
                        .and_then(|n| n.checked_sub(selected.0))
                        .context("summary Trigger byte debit overflow")?,
                )?,
                prior_records
                    .checked_add(resource_material.1)
                    .and_then(|n| n.checked_add(model_cost.1))
                    .and_then(|n| n.checked_sub(selected.1))
                    .context("summary Trigger count debit overflow")?,
            );
            let identity_view = crate::types::AgentIdentityView::from_record(
                identity.as_ref(),
                &self.inner.default_agent_id,
            );
            Ok(SummaryProjectionRead {
                read,
                resources: Arc::new(resources),
                owned,
                trigger_prior,
                identity: identity_view,
                model,
                model_material: Arc::new(model_material),
            })
        })
    }

    pub(super) fn summary_triggers_from_capture(
        &self,
        selected: CapturedScheduling,
        prior: (usize, usize),
    ) -> Result<(Vec<ExternalTriggerSummary>, SchedulingReadMaterial)> {
        let ledger: &InputEvidenceBudgetLedger = &self.inner.input_evidence_budget;
        let limits = ledger.limits();
        let (timers, triggers, material) = project_selected_scheduling_records(
            selected,
            0,
            ledger,
            usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("summary Scheduling byte limit overflow")?,
            )?,
            usize::try_from(limits.record_limit)?,
            prior,
            Vec::new(),
        )?;
        debug_assert!(timers.is_empty());
        // The existing output allowance covers four full record copies;
        // this scalar subset moves strings from one of those copies.
        Ok((active_trigger_summaries(triggers), material))
    }
}

#[cfg(test)]
#[path = "agent_summary_read_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "current_read_ordinary_tests.rs"]
mod ordinary_tests;

#[cfg(test)]
#[path = "agent_summary_wire_tests.rs"]
mod wire_tests;
