//! Immutable external observations. Freshness never grants mutation authority.
use super::input_evidence_budget::{
    BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
};
use super::resident_input_evidence::InputEvidenceScope;
use crate::types::{LoadedAgentMemory, LoadedAgentsMd, SkillsRuntimeView};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextResourceSource {
    pub scope: InputEvidenceScope,
    pub agent_home: PathBuf,
    pub user_home: Option<PathBuf>,
    pub workspace_id: Option<String>,
    pub workspace_anchor: Option<PathBuf>,
    pub execution_root_id: Option<String>,
    pub execution_root: Option<PathBuf>,
    pub include_user_skills: bool,
    pub config_generation: u64,
    pub host_write_generation: Option<u64>,
    pub work_revision: Option<u64>,
    pub plans: Vec<ContextPlanSource>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextPlanSource {
    pub work_item_id: String,
    pub recorded: bool,
}
#[derive(Debug, serde::Serialize)]
pub(crate) enum ObservedPlan {
    Absent,
    RecordedMissing,
    Present(crate::types::WorkItemPlanArtifact),
    RefreshFailed(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceInvalidation {
    KnownWrite,
    WorkspaceChange,
    ConfigurationChange,
    ExplicitRefresh,
    ExternalEdit,
    WatchOverflow,
    WatchRestart,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResourceFreshness {
    Unobserved,
    Ready,
    Dirty(ResourceInvalidation),
    RefreshFailed(String),
    Closed,
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum ResourceObservationError {
    #[error("context resource source differs from its observation")]
    SourceChanged,
    #[error("context resources are not ready: {0:?}")]
    NotReady(ResourceFreshness),
    #[error("context resource refresh arrived after invalidation or source replacement")]
    ObsoleteRefresh,
    #[error("context resource generation exhausted")]
    GenerationExhausted,
    #[error("ResourceExhausted: context resource {0}")]
    ResourceExhausted(&'static str),
}

#[derive(Debug)]
pub(crate) struct ContextResourceBody {
    pub guidance: LoadedAgentsMd,
    pub memory: LoadedAgentMemory,
    /// Catalog and roots were observed; active records are selected from State.
    pub skills: SkillsRuntimeView,
    pub notes: Option<String>,
    /// Optional failure is distinct from a successful observation of absence.
    pub notes_refresh_error: Option<String>,
    pub source_versions: std::collections::BTreeMap<
        PathBuf,
        crate::diagnostics::resource_io::ObservedResourceVersion,
    >,
}
impl ContextResourceBody {
    pub(crate) fn retained_material(&self) -> Result<(u64, usize)> {
        struct CountWriter(u64);
        impl Write for CountWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self
                    .0
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| std::io::Error::other("resource retained bytes overflow"))?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let guidance = &self.guidance;
        let mut sink = CountWriter(0);
        serde_json::to_writer(
            &mut sink,
            &(
                guidance,
                guidance
                    .user_global_source
                    .as_ref()
                    .map(|s| s.content.as_str()),
                guidance.agent_source.as_ref().map(|s| s.content.as_str()),
                guidance
                    .workspace_source
                    .as_ref()
                    .map(|s| s.content.as_str()),
                &self.memory,
                self.memory
                    .operator_source
                    .as_ref()
                    .map(|s| s.content.as_str()),
                self.memory.self_source.as_ref().map(|s| s.content.as_str()),
                &self.skills,
                &self.notes,
                &self.notes_refresh_error,
                &self.source_versions,
            ),
        )?;
        let catalog = self
            .skills
            .discoverable_skills
            .len()
            .checked_add(self.skills.attached_skills.len())
            .context("resource catalog count overflow")?;
        let mut nodes = 128usize
            .checked_add(
                catalog
                    .checked_mul(12)
                    .context("resource catalog nodes overflow")?,
            )
            .context("resource nodes overflow")?;
        nodes = nodes
            .checked_add(
                self.skills
                    .discovered_roots
                    .len()
                    .checked_mul(4)
                    .context("resource root nodes overflow")?,
            )
            .context("resource nodes overflow")?;
        for template in &self.skills.agent_templates_catalog {
            nodes = nodes
                .checked_add(24)
                .and_then(|n| {
                    template
                        .included_skills
                        .len()
                        .checked_mul(4)
                        .and_then(|skills| n.checked_add(skills))
                })
                .context("resource template nodes overflow")?;
        }
        nodes = nodes
            .checked_add(
                self.skills
                    .active_skills
                    .len()
                    .checked_mul(24)
                    .context("resource active nodes overflow")?,
            )
            .context("resource nodes overflow")?;
        nodes = nodes
            .checked_add(
                self.source_versions
                    .len()
                    .checked_mul(8)
                    .context("resource source-version nodes overflow")?,
            )
            .context("resource nodes overflow")?;
        let bytes = sink
            .0
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("resource retained metadata overflow")?,
            )
            .and_then(|n| n.checked_mul(8))
            .context("resource retained material overflow")?;
        Ok((
            bytes,
            nodes
                .checked_mul(8)
                .context("resource retained records overflow")?,
        ))
    }
    pub(crate) fn digest(&self) -> Result<String> {
        struct HashWriter(Sha256);
        impl Write for HashWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let guidance = &self.guidance;
        // LoadedAgentsMd intentionally skips content in its public Serialize.
        let mut sink = HashWriter(Sha256::new());
        serde_json::to_writer(
            &mut sink,
            &(
                guidance,
                guidance
                    .user_global_source
                    .as_ref()
                    .map(|s| s.content.as_str()),
                guidance.agent_source.as_ref().map(|s| s.content.as_str()),
                guidance
                    .workspace_source
                    .as_ref()
                    .map(|s| s.content.as_str()),
                &self.memory,
                self.memory
                    .operator_source
                    .as_ref()
                    .map(|s| s.content.as_str()),
                self.memory.self_source.as_ref().map(|s| s.content.as_str()),
                &self.skills,
                &self.notes,
                &self.notes_refresh_error,
                &self.source_versions,
            ),
        )?;
        Ok(format!("{:x}", sink.0.finalize()))
    }
}
/// The registry owns this complete observation. Captures never retain it.
#[derive(Debug)]
pub(crate) struct ResourceObservation {
    pub core: Arc<ResourceCoreObservation>,
    plans: std::collections::BTreeMap<String, Arc<ObservedPlan>>,
}
impl std::ops::Deref for ResourceObservation {
    type Target = ResourceCoreObservation;
    fn deref(&self) -> &Self::Target {
        &self.core
    }
}
/// Core resources contain no Work body, plan body map, Runtime or root link.
#[derive(Debug)]
pub(crate) struct ResourceCoreObservation {
    pub source: Arc<ContextResourceSource>,
    pub epoch: u64,
    pub invalidation_generation: u64,
    pub digest: String,
    pub body: Arc<ContextResourceBody>,
    pub material_bytes: u64,
    pub material_records: usize,
    // One conservative refresh allocation guard is shared across all pieces;
    // retaining core can overcount dropped plans, but retains no plan body.
    _material: Arc<BudgetViewGuard>,
    _source_material: Arc<BudgetViewGuard>,
}
/// Includes the plan map's slots and strings before its allocation or clone.
pub(super) fn plan_material(id: &str, plan: &ObservedPlan) -> Result<(u64, usize)> {
    let bytes = super::input_evidence_budget::serialized_bytes(&(id, plan))?
        .checked_add(32 * 256)
        .and_then(|n| n.checked_mul(8))
        .context("resource plan material overflow")?;
    Ok((bytes, 32 * 8))
}
pub(crate) fn observation_digest(
    body: &ContextResourceBody,
    plans: &std::collections::BTreeMap<String, ObservedPlan>,
) -> Result<String> {
    struct Sink(Sha256);
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink(Sha256::new());
    serde_json::to_writer(&mut sink, &(body.digest()?, plans))?;
    Ok(format!("{:x}", sink.0.finalize()))
}
pub(crate) fn observation_material(
    body: &ContextResourceBody,
    plans: &std::collections::BTreeMap<String, ObservedPlan>,
) -> Result<(u64, usize)> {
    let (mut bytes, mut records) = body.retained_material()?;
    for (id, plan) in plans {
        let (b, r) = plan_material(id, plan)?;
        bytes = bytes
            .checked_add(b)
            .context("resource observation bytes overflow")?;
        records = records
            .checked_add(r)
            .context("resource observation records overflow")?;
    }
    Ok((bytes, records))
}
#[derive(Debug, Clone)]
pub(crate) struct CapturedResourceObservations {
    pub observation: Arc<ResourceCoreObservation>,
    plans: std::collections::BTreeMap<String, Arc<ObservedPlan>>,
    // Covers owned guidance/memory/catalog/notes/render copies. Arc clones of
    // this carrier share one allocation responsibility, without a registry ref.
    _owned: Arc<BudgetViewGuard>,
    _active_material: Option<Arc<BudgetViewGuard>>,
    phase_bytes: u64,
    phase_records: usize,
}
impl CapturedResourceObservations {
    pub(super) fn retain_active_material(
        &mut self,
        guard: Arc<BudgetViewGuard>,
        bytes: u64,
        records: usize,
    ) {
        self._active_material = Some(guard);
        self.phase_bytes += bytes;
        self.phase_records += records;
    }
    /// Already observed by this carrier's actual guards; outer joint material
    /// debits the same cost without a second registration or body copy.
    pub(crate) fn material_estimate(&self) -> (u64, usize) {
        (self.phase_bytes, self.phase_records)
    }
    pub(crate) fn validate_material_binding(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        anyhow::ensure!(
            self.observation.source.scope == *scope,
            "resource wire scope differs"
        );
        self._owned
            .validate_owned_material_binding(scope, ledger, 0)?;
        anyhow::ensure!(
            self.observation._material.matches_resource_ledger(ledger)
                && self
                    .observation
                    ._source_material
                    .matches_resource_ledger(ledger),
            "resource wire ledger differs"
        );
        self.observation
            ._material
            .validate_resource_observation(scope, self.observation.material_bytes)?;
        if let Some(active) = self._active_material.as_ref() {
            active.validate_owned_material_binding(scope, ledger, 0)?;
        }
        Ok(())
    }
    pub(crate) fn guidance(&self) -> LoadedAgentsMd {
        self.observation.body.guidance.clone()
    }
    pub(crate) fn memory(&self) -> LoadedAgentMemory {
        self.observation.body.memory.clone()
    }
    pub(crate) fn skills(&self, active: &[crate::types::ActiveSkillRecord]) -> SkillsRuntimeView {
        let mut view = self.observation.body.skills.clone();
        view.active_skills =
            crate::skills::active_skills_from_observed_catalog(&view.discoverable_skills, active);
        view
    }
    pub(crate) fn skills_with_visibility(
        &self,
        active: &[crate::types::ActiveSkillRecord],
        include_user: bool,
    ) -> SkillsRuntimeView {
        let mut view = self.skills(active);
        if !include_user {
            view.discovered_roots
                .retain(|root| root.scope != crate::types::SkillScope::UserGlobal);
            view.discoverable_skills
                .retain(|skill| skill.scope != crate::types::SkillScope::UserGlobal);
            view.attached_skills
                .retain(|skill| skill.scope != crate::types::SkillScope::UserGlobal);
            view.active_skills
                .retain(|skill| skill.scope != crate::types::SkillScope::UserGlobal);
        }
        view
    }
}
impl crate::context::ContextResources for CapturedResourceObservations {
    fn plan(
        &self,
        item: &crate::types::WorkItemRecord,
    ) -> Option<crate::types::WorkItemPlanArtifact> {
        if item.agent_id != self.observation.source.scope.agent_id {
            return None;
        }
        match self.plans.get(&item.id).map(Arc::as_ref) {
            Some(ObservedPlan::Present(plan)) => Some(plan.clone()),
            Some(
                ObservedPlan::Absent
                | ObservedPlan::RecordedMissing
                | ObservedPlan::RefreshFailed(_),
            )
            | None => None,
        }
    }
    fn notes(&self) -> Option<String> {
        self.observation.body.notes.clone()
    }
}

#[derive(Default)]
pub(crate) struct ContextResourceRegistry {
    state: Mutex<ObservationState>,
}
struct ObservationState {
    source: Option<Arc<ContextResourceSource>>,
    source_material: Option<Arc<BudgetViewGuard>>,
    generation: u64,
    active_writes: usize,
    exhausted: bool,
    #[cfg(test)]
    command_exits: u64,
    epoch: u64,
    refresh_nonce: u64,
    freshness: ResourceFreshness,
    value: Option<Arc<ResourceObservation>>,
}
impl Default for ObservationState {
    fn default() -> Self {
        Self {
            source: None,
            source_material: None,
            generation: 0,
            active_writes: 0,
            exhausted: false,
            #[cfg(test)]
            command_exits: 0,
            epoch: 0,
            refresh_nonce: 0,
            freshness: ResourceFreshness::Unobserved,
            value: None,
        }
    }
}
fn advance_generation(state: &mut ObservationState) -> Result<()> {
    match state.generation.checked_add(1) {
        Some(next) if !state.exhausted => {
            state.generation = next;
            Ok(())
        }
        _ => {
            state.exhausted = true;
            Err(ResourceObservationError::GenerationExhausted.into())
        }
    }
}
/// Arbitrary Commands keep their background semantics. Their original process
/// observes start/actual exit; changes while running are latest-observed, not
/// an atomic filesystem transaction. This hook has no Runtime/root backref.
pub(crate) struct ResourceProcessChanges {
    registry: std::sync::Weak<ContextResourceRegistry>,
    wake: Arc<tokio::sync::Notify>,
    host: Option<crate::host::HostResourceProcessChanges>,
}
impl ResourceProcessChanges {
    pub(super) fn start(
        registry: &Arc<ContextResourceRegistry>,
        wake: Arc<tokio::sync::Notify>,
        host: Option<crate::host::HostResourceProcessChanges>,
    ) -> Result<Self> {
        registry.invalidate(ResourceInvalidation::KnownWrite)?;
        wake.notify_one();
        Ok(Self {
            registry: Arc::downgrade(registry),
            wake,
            host,
        })
    }
    pub(in crate::runtime) fn physical_exit(self) {
        if let Some(host) = self.host {
            host.physical_exit();
        }
        if let Some(registry) = self.registry.upgrade() {
            #[cfg(test)]
            {
                let mut state = registry
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                state.command_exits += 1;
            }
            if let Err(error) = registry.invalidate(ResourceInvalidation::KnownWrite) {
                tracing::warn!(%error,"original Command resource exit invalidation failed");
            }
        }
        self.wake.notify_one();
    }
}
/// Readiness only: the original authorized physical kernel owns this lease.
/// It must outlive detached blocking/owned I/O, never merely the caller future.
pub(crate) struct ResourceWriteLease {
    pub(super) host_write: Option<crate::host::HostResourceWriteLease>,
    local_active: bool,
    registry: Arc<ContextResourceRegistry>,
    wake: Arc<tokio::sync::Notify>,
}
impl Drop for ResourceWriteLease {
    fn drop(&mut self) {
        if !self.local_active {
            return;
        }
        let mut state = self
            .registry
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.active_writes = state
            .active_writes
            .checked_sub(1)
            .expect("original resource write lease");
        if state.freshness != ResourceFreshness::Closed {
            state.freshness = ResourceFreshness::Dirty(ResourceInvalidation::KnownWrite);
            if let Some(next) = state.generation.checked_add(1) {
                state.generation = next;
            }
            // Exhaustion remains Dirty and never admits a new refresh/capture.
            else {
                state.exhausted = true;
            }
        }
        drop(state);
        self.wake.notify_one();
    }
}
/// Only minted by begin_refresh; no external constructor, Clone or Serde.
pub(crate) struct ResourceRefreshTicket {
    source: Arc<ContextResourceSource>,
    generation: u64,
    nonce: u64,
    source_material: Arc<BudgetViewGuard>,
}
impl ContextResourceRegistry {
    #[cfg(test)]
    pub(super) fn set_max_generation_for_test(&self) -> Result<()> {
        self.lock()?.generation = u64::MAX;
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn actual_command_exits_for_test(&self) -> Result<u64> {
        Ok(self.lock()?.command_exits)
    }
    pub(super) fn retain_host_kernel(
        self: &Arc<Self>,
        wake: Arc<tokio::sync::Notify>,
        host_write: crate::host::HostResourceWriteLease,
    ) -> ResourceWriteLease {
        ResourceWriteLease {
            host_write: Some(host_write),
            local_active: false,
            registry: self.clone(),
            wake,
        }
    }
    pub(super) fn begin_write(
        self: &Arc<Self>,
        wake: Arc<tokio::sync::Notify>,
    ) -> Result<Option<ResourceWriteLease>> {
        let mut state = self.lock()?;
        if state.freshness == ResourceFreshness::Closed {
            return Ok(None);
        }
        state.freshness = ResourceFreshness::Dirty(ResourceInvalidation::KnownWrite);
        advance_generation(&mut state)?;
        let active = state
            .active_writes
            .checked_add(1)
            .context("resource active writer count exhausted")?;
        state.active_writes = active;
        Ok(Some(ResourceWriteLease {
            host_write: None,
            local_active: true,
            registry: self.clone(),
            wake,
        }))
    }
    pub(super) fn for_transient(
        source: ContextResourceSource,
        source_material: Arc<BudgetViewGuard>,
        generation: u64,
    ) -> Self {
        Self {
            state: Mutex::new(ObservationState {
                source: Some(Arc::new(source)),
                source_material: Some(source_material),
                generation,
                ..ObservationState::default()
            }),
        }
    }
    pub(super) fn generation_stamp(&self) -> Result<u64> {
        let state = self.lock()?;
        anyhow::ensure!(
            !state.exhausted,
            ResourceObservationError::GenerationExhausted
        );
        anyhow::ensure!(
            state.freshness != ResourceFreshness::Closed,
            ResourceObservationError::NotReady(ResourceFreshness::Closed)
        );
        anyhow::ensure!(
            state.active_writes == 0,
            ResourceObservationError::NotReady(ResourceFreshness::Dirty(
                ResourceInvalidation::KnownWrite
            ))
        );
        Ok(state.generation)
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ObservationState>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("resource observation lock poisoned"))
    }
    pub(crate) fn freshness(&self) -> Result<ResourceFreshness> {
        Ok(self.lock()?.freshness.clone())
    }
    pub(super) fn freshness_for_host_generation(
        &self,
        expected: Option<u64>,
    ) -> Result<ResourceFreshness> {
        let state = self.lock()?;
        if state.freshness == ResourceFreshness::Ready
            && state
                .value
                .as_ref()
                .map(|value| value.source.host_write_generation)
                != Some(expected)
        {
            return Ok(ResourceFreshness::Dirty(ResourceInvalidation::KnownWrite));
        }
        Ok(state.freshness.clone())
    }

    pub(crate) fn invalidate(&self, cause: ResourceInvalidation) -> Result<()> {
        let mut state = self.lock()?;
        if state.freshness == ResourceFreshness::Closed {
            return Ok(());
        }
        state.freshness = ResourceFreshness::Dirty(cause);
        advance_generation(&mut state)?;
        Ok(())
    }
    pub(crate) fn begin_refresh(
        &self,
        source: ContextResourceSource,
        source_material: Arc<BudgetViewGuard>,
    ) -> Result<ResourceRefreshTicket> {
        anyhow::ensure!(
            source.scope.identity_incarnation > 0,
            "resource source has no incarnation"
        );
        let mut state = self.lock()?;
        anyhow::ensure!(
            !state.exhausted,
            ResourceObservationError::GenerationExhausted
        );
        if state.freshness == ResourceFreshness::Closed {
            return Err(ResourceObservationError::NotReady(ResourceFreshness::Closed).into());
        }
        anyhow::ensure!(
            state.active_writes == 0,
            ResourceObservationError::NotReady(ResourceFreshness::Dirty(
                ResourceInvalidation::KnownWrite
            ))
        );
        if state.source.as_deref() != Some(&source) {
            state.freshness = ResourceFreshness::Dirty(ResourceInvalidation::WorkspaceChange);
            advance_generation(&mut state)?;
            state.source = Some(Arc::new(source));
            state.source_material = Some(source_material);
            state.freshness = ResourceFreshness::Dirty(ResourceInvalidation::WorkspaceChange);
        }
        state.refresh_nonce = match state.refresh_nonce.checked_add(1) {
            Some(next) => next,
            None => {
                state.freshness = ResourceFreshness::Dirty(ResourceInvalidation::ExplicitRefresh);
                state.exhausted = true;
                return Err(ResourceObservationError::GenerationExhausted.into());
            }
        };
        Ok(ResourceRefreshTicket {
            source: state
                .source
                .as_ref()
                .context("resource source absent")?
                .clone(),
            generation: state.generation,
            nonce: state.refresh_nonce,
            source_material: state
                .source_material
                .as_ref()
                .context("resource source material absent")?
                .clone(),
        })
    }
    pub(crate) fn observe_refresh_change(
        &self,
        ticket: &mut ResourceRefreshTicket,
        digest: &str,
    ) -> Result<()> {
        let mut state = self.lock()?;
        if state.freshness == ResourceFreshness::Closed
            || state.exhausted
            || state.active_writes != 0
            || ticket.generation != state.generation
            || ticket.nonce != state.refresh_nonce
            || state.source.as_deref() != Some(ticket.source.as_ref())
        {
            return Err(ResourceObservationError::ObsoleteRefresh.into());
        }
        if state.value.as_ref().is_some_and(|old| old.digest != digest) {
            state.freshness = ResourceFreshness::Dirty(ResourceInvalidation::ExternalEdit);
            advance_generation(&mut state)?;
            ticket.generation = state.generation;
        }
        Ok(())
    }
    pub(crate) fn install(
        &self,
        ticket: ResourceRefreshTicket,
        current: &ContextResourceSource,
        body: ContextResourceBody,
        plans: std::collections::BTreeMap<String, ObservedPlan>,
        material_bytes: u64,
        material_records: usize,
        guard: Arc<BudgetViewGuard>,
    ) -> Result<Arc<ResourceObservation>> {
        let digest = observation_digest(&body, &plans)?;
        let (actual_bytes, actual_records) = observation_material(&body, &plans)?;
        anyhow::ensure!(
            plans
                .keys()
                .map(String::as_str)
                .eq(current.plans.iter().map(|p| p.work_item_id.as_str())),
            "resource plan source differs from observation"
        );
        let (core_bytes, core_records) = body.retained_material()?;
        anyhow::ensure!(
            material_bytes >= actual_bytes && material_records >= actual_records,
            "resource retained estimate does not cover original body"
        );
        guard.validate_resource_observation(&current.scope, material_bytes)?;
        let mut state = self.lock()?;
        if state.freshness == ResourceFreshness::Closed
            || state.exhausted
            || state.active_writes != 0
            || ticket.generation != state.generation
            || ticket.nonce != state.refresh_nonce
            || ticket.source.as_ref() != current
            || state.source.as_deref() != Some(current)
        {
            return Err(ResourceObservationError::ObsoleteRefresh.into());
        }
        // A successful unchanged poll retains the existing immutable allocation.
        if let Some(old) = state.value.as_ref().filter(|old| {
            old.source.as_ref() == current
                && old.digest == digest
                && old.invalidation_generation == ticket.generation
        }) {
            let old = old.clone();
            state.freshness = ResourceFreshness::Ready;
            return Ok(old);
        }
        state.epoch = match state.epoch.checked_add(1) {
            Some(next) => next,
            None => {
                state.exhausted = true;
                state.freshness = ResourceFreshness::Dirty(ResourceInvalidation::ExplicitRefresh);
                return Err(ResourceObservationError::GenerationExhausted.into());
            }
        };
        let core = Arc::new(ResourceCoreObservation {
            source: ticket.source,
            epoch: state.epoch,
            invalidation_generation: state.generation,
            digest,
            body: Arc::new(body),
            material_bytes: core_bytes,
            material_records: core_records,
            _material: guard,
            _source_material: ticket.source_material,
        });
        let value = Arc::new(ResourceObservation {
            core,
            plans: plans
                .into_iter()
                .map(|(id, plan)| (id, Arc::new(plan)))
                .collect(),
        });
        state.value = Some(value.clone());
        state.freshness = ResourceFreshness::Ready;
        Ok(value)
    }
    /// Rebind only a published metadata revision, never a file-source change.
    pub(super) fn rebind_work_source(
        &self,
        before_work_revision: Option<u64>,
        source: &ContextResourceSource,
        source_material: Arc<BudgetViewGuard>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<bool> {
        anyhow::ensure!(
            source_material.matches_resource_ledger(ledger),
            "Work source header belongs to another ledger"
        );
        let mut state = self.lock()?;
        if state.freshness != ResourceFreshness::Ready
            || state.exhausted
            || state.active_writes != 0
        {
            return Ok(false);
        }
        let Some(old) = state.value.as_ref().cloned() else {
            return Ok(false);
        };
        anyhow::ensure!(
            old._material.matches_resource_ledger(ledger),
            "Work resource body belongs to another ledger"
        );
        let previous = old.source.as_ref();
        if previous.work_revision != before_work_revision
            || state.source.as_deref() != Some(previous)
            || previous.scope != source.scope
            || previous.agent_home != source.agent_home
            || previous.user_home != source.user_home
            || previous.workspace_id != source.workspace_id
            || previous.workspace_anchor != source.workspace_anchor
            || previous.execution_root_id != source.execution_root_id
            || previous.execution_root != source.execution_root
            || previous.include_user_skills != source.include_user_skills
            || previous.config_generation != source.config_generation
            || previous.host_write_generation != source.host_write_generation
            || previous.plans != source.plans
        {
            return Ok(false);
        }
        let epoch = match state.epoch.checked_add(1) {
            Some(epoch) => epoch,
            None => {
                state.exhausted = true;
                state.freshness = ResourceFreshness::Dirty(ResourceInvalidation::ExplicitRefresh);
                return Err(ResourceObservationError::GenerationExhausted.into());
            }
        };
        let source = Arc::new(source.clone());
        let core = Arc::new(ResourceCoreObservation {
            source: source.clone(),
            epoch,
            invalidation_generation: old.invalidation_generation,
            digest: old.digest.clone(),
            body: old.body.clone(),
            material_bytes: old.material_bytes,
            material_records: old.material_records,
            _material: old._material.clone(),
            _source_material: source_material.clone(),
        });
        let value = Arc::new(ResourceObservation {
            core,
            plans: old.plans.clone(),
        });
        state.epoch = epoch;
        state.source = Some(source);
        state.source_material = Some(source_material);
        state.value = Some(value);
        Ok(true)
    }
    pub(super) fn work_query_source_version(
        &self,
        source: &ContextResourceSource,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(u64, u64)> {
        let state = self.lock()?;
        anyhow::ensure!(
            state.freshness == ResourceFreshness::Ready
                && !state.exhausted
                && state.active_writes == 0,
            ResourceObservationError::NotReady(state.freshness.clone())
        );
        let value = state
            .value
            .as_ref()
            .context("resource observation absent")?;
        anyhow::ensure!(
            value.source.as_ref() == source,
            ResourceObservationError::SourceChanged
        );
        anyhow::ensure!(
            value._material.matches_resource_ledger(ledger),
            "resource query source ledger differs"
        );
        Ok((value.epoch, value.invalidation_generation))
    }
    /// Only the exact selected plan is retained; no core/body/map or Root escapes.
    pub(super) fn capture_work_plan(
        &self,
        source: &ContextResourceSource,
        id: &str,
        ledger: &InputEvidenceBudgetLedger,
        byte_limit: u64,
        record_limit: usize,
    ) -> Result<(
        Arc<ObservedPlan>,
        Arc<BudgetViewGuard>,
        Arc<BudgetViewGuard>,
    )> {
        self.capture_work_plan_inner(source, id, ledger, byte_limit, record_limit, None)
    }
    pub(super) fn capture_work_plan_at_version(
        &self,
        source: &ContextResourceSource,
        id: &str,
        ledger: &InputEvidenceBudgetLedger,
        byte_limit: u64,
        record_limit: usize,
        version: (u64, u64),
    ) -> Result<(
        Arc<ObservedPlan>,
        Arc<BudgetViewGuard>,
        Arc<BudgetViewGuard>,
    )> {
        self.capture_work_plan_inner(source, id, ledger, byte_limit, record_limit, Some(version))
    }
    fn capture_work_plan_inner(
        &self,
        source: &ContextResourceSource,
        id: &str,
        ledger: &InputEvidenceBudgetLedger,
        byte_limit: u64,
        record_limit: usize,
        version: Option<(u64, u64)>,
    ) -> Result<(
        Arc<ObservedPlan>,
        Arc<BudgetViewGuard>,
        Arc<BudgetViewGuard>,
    )> {
        let state = self.lock()?;
        anyhow::ensure!(
            state.freshness == ResourceFreshness::Ready
                && !state.exhausted
                && state.active_writes == 0,
            ResourceObservationError::NotReady(state.freshness.clone())
        );
        let value = state
            .value
            .as_ref()
            .context("resource observation absent")?;
        anyhow::ensure!(
            value.source.as_ref() == source,
            ResourceObservationError::SourceChanged
        );
        anyhow::ensure!(
            value._material.matches_resource_ledger(ledger),
            "resource plan belongs to another ledger"
        );
        anyhow::ensure!(
            version.is_none_or(|version| version == (value.epoch, value.invalidation_generation)),
            ResourceObservationError::ObsoleteRefresh
        );
        let plan = value
            .plans
            .get(id)
            .context("qualified Work plan source is not observed")?;
        let (bytes, records) = plan_material(id, plan)?;
        anyhow::ensure!(
            bytes <= byte_limit && records <= record_limit,
            ResourceObservationError::ResourceExhausted("selected Work plan")
        );
        let material = ledger.register_owned_bytes(
            &source.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        Ok((plan.clone(), material, value._material.clone()))
    }
    pub(crate) fn fail(&self, ticket: ResourceRefreshTicket, error: String) -> Result<()> {
        let mut state = self.lock()?;
        if ticket.nonce == state.refresh_nonce
            && ticket.generation == state.generation
            && state.source.as_deref() == Some(ticket.source.as_ref())
            && state.freshness != ResourceFreshness::Closed
        {
            state.freshness = ResourceFreshness::RefreshFailed(error.chars().take(256).collect());
        }
        Ok(())
    }
    pub(crate) fn capture(
        &self,
        source: &ContextResourceSource,
        ledger: &InputEvidenceBudgetLedger,
        records: usize,
        bytes: u64,
        selected_work: Option<&super::resident_work::CapturedWork>,
    ) -> Result<CapturedResourceObservations> {
        let value = {
            let state = self.lock()?;
            anyhow::ensure!(
                !state.exhausted,
                ResourceObservationError::GenerationExhausted
            );
            if state.source.as_deref() != Some(source) {
                return Err(ResourceObservationError::SourceChanged.into());
            }
            if state.freshness != ResourceFreshness::Ready {
                return Err(ResourceObservationError::NotReady(state.freshness.clone()).into());
            }
            state
                .value
                .as_ref()
                .context("ready resource observation missing")?
                .clone()
        };
        anyhow::ensure!(
            value._material.matches_resource_ledger(ledger),
            "resource capture belongs to another ledger"
        );
        if let Some(work) = selected_work {
            anyhow::ensure!(
                work.scope == source.scope
                    && (value.source.work_revision.is_none()
                        || value.source.work_revision == Some(work.revision)),
                ResourceObservationError::SourceChanged
            );
            for qualified in &work.records {
                if let super::resident_work::CanonicalWorkRecord::Work(item) = &qualified.record {
                    anyhow::ensure!(
                        value
                            .source
                            .plans
                            .iter()
                            .any(|plan| plan.work_item_id == item.id
                                && plan.recorded == item.plan_artifact.is_some()),
                        ResourceObservationError::SourceChanged
                    );
                }
            }
        }
        let selected = || {
            selected_work
                .into_iter()
                .flat_map(|work| work.records.iter())
                .filter_map(|record| match &record.record {
                    super::resident_work::CanonicalWorkRecord::Work(item) => value
                        .plans
                        .get(&item.id)
                        .map(|plan| (item.id.as_str(), plan)),
                    _ => None,
                })
        };
        let (mut phase_bytes, mut phase_records) = (value.material_bytes, value.material_records);
        for (id, plan) in selected() {
            let (b, r) = plan_material(id, plan)?;
            phase_bytes = phase_bytes
                .checked_add(b)
                .context("selected resource bytes overflow")?;
            phase_records = phase_records
                .checked_add(r)
                .context("selected resource records overflow")?;
        }
        anyhow::ensure!(
            phase_bytes <= bytes && phase_records <= records,
            ResourceObservationError::ResourceExhausted("capture copies")
        );
        let owned = ledger.register_owned_bytes(
            &source.scope,
            InputMemoryCategory::Pins,
            OwnedBytesEstimate {
                payload_bytes: phase_bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let plans = selected()
            .map(|(id, plan)| (id.to_owned(), plan.clone()))
            .collect();
        Ok(CapturedResourceObservations {
            observation: value.core.clone(),
            plans,
            _owned: owned,
            _active_material: None,
            phase_bytes,
            phase_records,
        })
    }

    #[cfg(test)]
    pub(super) fn plan_weak_for_test(
        &self,
        id: &str,
    ) -> Result<Option<std::sync::Weak<ObservedPlan>>> {
        Ok(self
            .lock()?
            .value
            .as_ref()
            .and_then(|value| value.plans.get(id))
            .map(Arc::downgrade))
    }
    pub(crate) fn close(&self) -> Result<()> {
        let mut state = self.lock()?;
        state.freshness = ResourceFreshness::Closed;
        state.value = None;
        state.source = None;
        state.source_material = None;
        advance_generation(&mut state)?;
        Ok(())
    }
}
