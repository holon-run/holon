//! Agent skill views retain their actual observation through wire serialization.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
    },
    resident_input_evidence::InputEvidenceScope,
    resource_observation::{CapturedResourceObservations, ContextResourceBody},
};
use std::ops::Deref;

pub(crate) struct OwnedAgentSkillsRead {
    view: crate::types::SkillsRuntimeView,
    material: AgentSkillsReadMaterial,
}

pub(crate) struct AgentSkillsReadMaterial {
    _resources: Option<Arc<CapturedResourceObservations>>,
    _owned: Vec<Arc<BudgetViewGuard>>,
    scope: InputEvidenceScope,
    bytes: u64,
    records: usize,
}

impl AgentSkillsReadMaterial {
    pub(crate) fn validated_material_estimate(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(u64, usize)> {
        anyhow::ensure!(self.scope == *scope, "skills wire scope differs");
        if let Some(resources) = self._resources.as_ref() {
            resources.validate_material_binding(scope, ledger)?;
        }
        for guard in &self._owned {
            guard.validate_owned_material_binding(scope, ledger, 0)?;
        }
        Ok((self.bytes, self.records))
    }
}

impl OwnedAgentSkillsRead {
    pub(crate) fn from_observation(
        resources: Arc<CapturedResourceObservations>,
        active: &[crate::types::ActiveSkillRecord],
        include_user: bool,
    ) -> Self {
        let (bytes, records) = resources.material_estimate();
        let scope = resources.observation.source.scope.clone();
        let view = resources.skills_with_visibility(active, include_user);
        Self {
            view,
            material: AgentSkillsReadMaterial {
                _resources: Some(resources),
                _owned: Vec::new(),
                scope,
                bytes,
                records,
            },
        }
    }

    pub(crate) fn into_parts(self) -> (crate::types::SkillsRuntimeView, AgentSkillsReadMaterial) {
        (self.view, self.material)
    }
}

impl Deref for OwnedAgentSkillsRead {
    type Target = crate::types::SkillsRuntimeView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl RuntimeHost {
    /// Cold inspection uses its caller-held identity and State. It creates no
    /// Runtime or Ready resource domain and never rereads a different State.
    pub(super) fn cold_agent_skills_from_state(
        &self,
        identity: &AgentIdentityRecord,
        state: &AgentState,
        prior: (u64, usize),
        mut guards: Vec<Arc<BudgetViewGuard>>,
    ) -> Result<OwnedAgentSkillsRead> {
        let generation = self.context_resource_generation()?;
        anyhow::ensure!(
            identity.agent_id == state.id && identity.status == AgentRegistryStatus::Active,
            "cold skill observation identity differs from State"
        );
        let scope = InputEvidenceScope {
            agent_id: identity.agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        let ledger = &self.inner.input_evidence_budget;
        let limits = ledger.limits();
        let byte_limit = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("cold skill byte limit overflow")?
            .min(64 * 1024 * 1024);
        let record_limit = usize::try_from(limits.record_limit.min(65_536))?;
        let remaining_bytes = byte_limit
            .checked_sub(prior.0)
            .context("ResourceExhausted: cold skill prior bytes")?;
        let remaining_records = record_limit
            .checked_sub(prior.1)
            .context("ResourceExhausted: cold skill prior records")?;
        let reservation = ledger.reserve_owned_completion(
            &scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate::default(),
        )?;
        let io = crate::diagnostics::resource_io::ResourceReadBudget::enter(
            reservation,
            remaining_bytes,
            remaining_records,
        )?;
        let config = self.config();
        // Source paths and active records are admitted before constructing the
        // registrations or their owned copies in the original catalog kernel.
        let source_bytes = serialized_bytes(&(
            &state.id,
            &state.active_skills,
            &state.active_workspace_entry,
            &config.user_home_dir,
            config.data_dir.as_path(),
        ))?;
        let candidates = crate::skills::COMPAT_SKILL_ROOT_SUFFIXES
            .len()
            .checked_add(
                crate::skills::SKILL_ROOT_SUFFIXES
                    .len()
                    .checked_mul(2)
                    .context("cold skill root count overflow")?,
            )
            .context("cold skill root count overflow")?;
        io.charge_metadata(
            source_bytes
                .checked_add(4096)
                .and_then(|n| n.checked_mul(u64::try_from(candidates).ok()?))
                .context("cold skill source bytes overflow")?,
            candidates
                .checked_mul(32)
                .and_then(|n| n.checked_add(state.active_skills.len().checked_mul(24)?))
                .context("cold skill source records overflow")?,
        )?;
        let agent_home = self.agent_data_dir(&state.id);
        let identity_view = AgentIdentityView::from_record(identity, &config.default_agent_id);
        let roots = effective_skill_root_registrations(
            skill_visibility(&identity_view),
            config.user_home_dir.as_deref(),
            &state.id,
            &agent_home,
            state
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.execution_root.as_path()),
        );
        // A per-inspection registry preserves root IDs and precedence without
        // holding the shared async registry lock across this synchronous phase.
        let mut registry = crate::skills::SkillsRegistry::new();
        registry.replace_roots(roots.clone())?;
        let mut view = skills_runtime_view_from_catalog(
            registry.catalog_for_roots(&roots, None),
            &roots,
            &state.active_skills,
        );
        view.agent_templates_catalog =
            discover_agent_templates_catalog(config.user_home_dir.as_deref(), &agent_home);
        io.finish()?;
        let body = ContextResourceBody {
            guidance: Default::default(),
            memory: Default::default(),
            skills: view,
            notes: None,
            notes_refresh_error: None,
            source_versions: io.observed_versions()?,
        };
        let (retained_bytes, retained_records) = body.retained_material()?;
        drop((registry, roots, agent_home, identity_view));
        let guard = io.seal_retained(retained_bytes, retained_records)?;
        guards.push(guard);
        self.with_context_resource_generation(generation, || {
            Ok(OwnedAgentSkillsRead {
                view: body.skills,
                material: AgentSkillsReadMaterial {
                    _resources: None,
                    _owned: guards,
                    scope,
                    bytes: prior
                        .0
                        .checked_add(retained_bytes)
                        .context("cold skill material overflow")?,
                    records: prior
                        .1
                        .checked_add(retained_records)
                        .context("cold skill material overflow")?,
                },
            })
        })
    }

    /// The original Host drain owns this read through caller cancellation.
    pub(crate) async fn agent_skill_detail_read(
        &self,
        mut material: AgentSkillsReadMaterial,
        skill: crate::types::SkillCatalogEntry,
    ) -> Result<(
        crate::types::SkillCatalogEntry,
        String,
        AgentSkillsReadMaterial,
        Arc<BudgetViewGuard>,
    )> {
        let host = self.clone();
        self.run_owned_external_mutation(async move {
            let reservations = host
                .reserve_mutation_scopes(&[(
                    material.scope.agent_id.clone(),
                    material.scope.identity_incarnation,
                )])
                .await?;
            drop(reservations[0].capture_ready_snapshot()?);
            tokio::task::spawn_blocking(move || {
                let _reservations = reservations;
                let limits = host.inner.input_evidence_budget.limits();
                let request = crate::runtime_db::scheduling_snapshot::SchedulingDomainLoadRequest {
                    capture: crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(
                    ),
                    byte_limit: usize::try_from(
                        limits
                            .history_per_agent_bytes
                            .checked_add(limits.pending_per_agent_bytes)
                            .context("skill identity limit overflow")?
                            .min(64 * 1024 * 1024)
                            .checked_sub(material.bytes)
                            .context("ResourceExhausted: detail identity bytes")?,
                    )?,
                    record_limit: usize::try_from(limits.record_limit.min(65_536))?
                        .checked_sub(material.records)
                        .context("ResourceExhausted: detail identity records")?,
                };
                let (_, usage, guards) = host.runtime_db().transaction(|tx| {
                    crate::runtime_db::scheduling_snapshot::read_cold_resource_identity_tx(
                        tx,
                        &material.scope,
                        &request,
                        &host.inner.input_evidence_budget,
                    )
                })?;
                material.bytes = material
                    .bytes
                    .checked_add(u64::try_from(usage.0)?)
                    .context("skill identity material overflow")?;
                material.records = material
                    .records
                    .checked_add(usage.1)
                    .context("skill identity material overflow")?;
                material._owned.extend(guards);
                let (content, content_material) =
                    host.read_agent_skill_content(&material, &skill.path)?;
                Ok((skill, content, material, content_material))
            })
            .await?
        })
        .await
    }

    fn read_agent_skill_content(
        &self,
        material: &AgentSkillsReadMaterial,
        path: &Path,
    ) -> Result<(String, Arc<BudgetViewGuard>)> {
        let generation = self.context_resource_generation()?;
        let limits = self.inner.input_evidence_budget.limits();
        let bytes = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("skill content limit overflow")?
            .min(64 * 1024 * 1024)
            .checked_sub(material.bytes)
            .context("ResourceExhausted: skill detail combined bytes")?;
        let records = usize::try_from(limits.record_limit.min(65_536))?
            .checked_sub(material.records)
            .context("ResourceExhausted: skill detail combined records")?;
        let reservation = self.inner.input_evidence_budget.reserve_owned_completion(
            &material.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate::default(),
        )?;
        let io = crate::diagnostics::resource_io::ResourceReadBudget::enter(
            reservation,
            bytes,
            records,
        )?;
        let content = crate::diagnostics::io_probe::read_to_string(path)?;
        let guard = io.completion_guard()?;
        self.with_context_resource_generation(generation, || Ok((content, guard)))
    }
}

pub(crate) struct OwnedGlobalSkillsRead {
    catalog: Vec<crate::types::SkillCatalogEntry>,
    material: GlobalSkillsReadMaterial,
}
pub(crate) struct GlobalSkillsReadMaterial {
    _owned: Arc<BudgetViewGuard>,
    bytes: u64,
    records: usize,
    generation: u64,
}
impl OwnedGlobalSkillsRead {
    pub(crate) fn into_parts(
        self,
    ) -> (
        Vec<crate::types::SkillCatalogEntry>,
        GlobalSkillsReadMaterial,
    ) {
        (self.catalog, self.material)
    }
}
impl RuntimeHost {
    fn global_skill_material_limits(&self) -> Result<(u64, usize)> {
        let limits = self.inner.input_evidence_budget.limits();
        Ok((
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("global skill byte limit overflow")?
                .min(64 * 1024 * 1024),
            usize::try_from(limits.record_limit.min(65_536))?,
        ))
    }
    /// Explicit global inspection is owned by the original Host drain, without
    /// creating an Agent identity, Runtime, mutation scope, or Ready domain.
    pub(crate) async fn global_skills_catalog_read(
        &self,
        scope: Option<crate::types::SkillScope>,
    ) -> Result<OwnedGlobalSkillsRead> {
        let host = self.clone();
        self.run_owned_external_mutation(async move {
            tokio::task::spawn_blocking(move || host.read_global_skills_catalog(scope)).await?
        })
        .await
    }
    fn read_global_skills_catalog(
        &self,
        scope: Option<crate::types::SkillScope>,
    ) -> Result<OwnedGlobalSkillsRead> {
        let generation = self.context_resource_generation()?;
        let config = self.config();
        let (bytes, records) = self.global_skill_material_limits()?;
        let reservation = self
            .inner
            .input_evidence_budget
            .reserve_host_owned_completion(
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate::default(),
            )?;
        let io = crate::diagnostics::resource_io::ResourceReadBudget::enter(
            reservation,
            bytes,
            records,
        )?;
        io.charge_metadata(
            serialized_bytes(&config.user_home_dir)?
                .checked_add(4096)
                .context("global skill source bytes overflow")?,
            32,
        )?;
        let user_home = config
            .user_home_dir
            .clone()
            .or_else(|| crate::agent_template::user_home_dir().ok());
        let root_count = crate::skills::COMPAT_SKILL_ROOT_SUFFIXES.len();
        io.charge_metadata(
            serialized_bytes(&user_home)?
                .checked_add(1024)
                .and_then(|n| n.checked_mul(u64::try_from(root_count).ok()?))
                .context("global skill roots bytes overflow")?,
            root_count
                .checked_mul(32)
                .context("global skill root count overflow")?,
        )?;
        let roots: Vec<_> = crate::skills::existing_skill_roots(
            user_home.as_deref(),
            &crate::skills::COMPAT_SKILL_ROOT_SUFFIXES,
        )
        .into_iter()
        .map(|root| {
            crate::skills::skill_root_registration(
                crate::types::SkillRootSourceKind::UserGlobal,
                None,
                root,
            )
        })
        .collect();
        let mut registry = crate::skills::SkillsRegistry::new();
        registry.replace_roots(roots.clone())?;
        let catalog = registry.catalog_for_roots(&roots, scope);
        io.finish()?;
        let body = ContextResourceBody {
            guidance: Default::default(),
            memory: Default::default(),
            skills: crate::types::SkillsRuntimeView {
                discoverable_skills: catalog,
                ..Default::default()
            },
            notes: None,
            notes_refresh_error: None,
            source_versions: io.observed_versions()?,
        };
        let (retained_bytes, retained_records) = body.retained_material()?;
        drop((registry, roots, user_home));
        let guard = io.seal_retained(retained_bytes, retained_records)?;
        self.with_context_resource_generation(generation, || {
            anyhow::ensure!(
                Arc::ptr_eq(&config, &self.config()),
                "global skill config source changed"
            );
            Ok(OwnedGlobalSkillsRead {
                catalog: body.skills.discoverable_skills,
                material: GlobalSkillsReadMaterial {
                    _owned: guard,
                    bytes: retained_bytes,
                    records: retained_records,
                    generation,
                },
            })
        })
    }
    pub(crate) async fn global_skill_detail_read(
        &self,
        material: GlobalSkillsReadMaterial,
        skill: crate::types::SkillCatalogEntry,
    ) -> Result<(
        crate::types::SkillCatalogEntry,
        String,
        GlobalSkillsReadMaterial,
        Arc<BudgetViewGuard>,
    )> {
        let host = self.clone();
        self.run_owned_external_mutation(async move {
            tokio::task::spawn_blocking(move || {
                let (content, guard) = host.read_global_skill_content(&material, &skill.path)?;
                Ok((skill, content, material, guard))
            })
            .await?
        })
        .await
    }
    fn read_global_skill_content(
        &self,
        material: &GlobalSkillsReadMaterial,
        path: &Path,
    ) -> Result<(String, Arc<BudgetViewGuard>)> {
        self.with_context_resource_generation(material.generation, || Ok(()))?;
        let (bytes, records) = self.global_skill_material_limits()?;
        let bytes = bytes
            .checked_sub(material.bytes)
            .context("ResourceExhausted: global detail bytes")?;
        let records = records
            .checked_sub(material.records)
            .context("ResourceExhausted: global detail records")?;
        let reservation = self
            .inner
            .input_evidence_budget
            .reserve_host_owned_completion(
                InputMemoryCategory::Pins,
                OwnedBytesEstimate::default(),
            )?;
        let io = crate::diagnostics::resource_io::ResourceReadBudget::enter(
            reservation,
            bytes,
            records,
        )?;
        let content = crate::diagnostics::io_probe::read_to_string(path)?;
        let guard = io.completion_guard()?;
        self.with_context_resource_generation(material.generation, || Ok((content, guard)))
    }
}

#[cfg(test)]
#[path = "agent_skills_read_tests.rs"]
mod tests;
