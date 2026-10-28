//! Immutable observations and an explicit, synchronous refresh seam.
//! This core has no Runtime/root, Work, Host, writer or storage backreference.
use super::{
    input_evidence_budget::{serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger},
    mutation_owner::{MutationOwnerError, MutationReservation},
    resident_input_evidence::InputEvidenceScope,
};
use crate::{
    diagnostics::resource_io::{self, ResourceReadMaterial},
    types::{LoadedAgentMemory, LoadedAgentsMd, SkillsRuntimeView},
};
use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    pub host_generation: u64,
    pub active_skills_selector: [u8; 32],
    pub work_revision: u64,
    pub plans: Vec<ContextPlanSource>,
}

impl ContextResourceSource {
    fn same_instruction_sources(&self, other: &Self) -> bool {
        self.scope == other.scope
            && self.agent_home == other.agent_home
            && self.user_home == other.user_home
            && self.workspace_id == other.workspace_id
            && self.workspace_anchor == other.workspace_anchor
            && self.execution_root_id == other.execution_root_id
            && self.execution_root == other.execution_root
            && self.include_user_skills == other.include_user_skills
            && self.config_generation == other.config_generation
            && self.host_generation == other.host_generation
            && self.active_skills_selector == other.active_skills_selector
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ContextPlanSource {
    pub work_item_id: String,
    pub recorded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceInvalidation {
    KnownWrite,
    WorkspaceChange,
    ConfigurationChange,
    ExplicitRefresh,
    #[cfg(test)]
    ExternalEdit,
    #[cfg(test)]
    WatchOverflow,
    #[cfg(test)]
    WatchRestart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ResourceFreshness {
    #[default]
    Unobserved,
    Ready,
    Dirty(ResourceInvalidation),
    RefreshFailed,
    Closed,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResourceObservationError {
    #[error("context resource source differs from its observation")]
    SourceChanged,
    #[error("context resource inventory lock is temporarily unavailable")]
    InventoryContended,
    #[error("context resource inventory snapshot changed before publication")]
    InventorySnapshotChanged,
    #[error("context resources are not ready: {0:?}")]
    NotReady(ResourceFreshness),
    #[error("context resource refresh was superseded")]
    ObsoleteRefresh,
    #[error("context resource generation exhausted")]
    GenerationExhausted,
}

#[derive(Debug, Default)]
pub(crate) struct ContextResourceBody {
    pub guidance: LoadedAgentsMd,
    pub memory: LoadedAgentMemory,
    pub skills: SkillsRuntimeView,
    /// None means observed absence, never an omitted read failure.
    pub notes: Option<String>,
    pub plans: std::collections::BTreeMap<String, crate::work_item_plan::PlanArtifactQuery>,
    /// The real immutable Host catalogue and its allocation guard travel together.
    pub _host_catalogue: Option<Arc<crate::skills::SkillsRegistry>>,
}

impl ContextResourceBody {
    #[cfg(test)]
    pub(crate) fn admit_plan_notes_projection(&self) -> Result<()> {
        resource_io::admit_cloned_value(&(&self.notes, &self.plans))?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn admit_skills_projection(&self) -> Result<()> {
        // Catalog Arc clones keep the original observation's allocation.
        resource_io::admit_cloned_value(&(
            &self.skills.discovered_roots,
            &self.skills.active_skills,
            &self.skills.agent_templates_catalog,
        ))?;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn admit_guidance_memory_projection(&self) -> Result<()> {
        // Public serialization omits these private bodies.
        resource_io::admit_cloned_value(&(
            &self.guidance,
            &self.memory,
            self.guidance
                .user_global_source
                .as_ref()
                .map(|s| &s.content),
            self.guidance.agent_source.as_ref().map(|s| &s.content),
            self.guidance.workspace_source.as_ref().map(|s| &s.content),
            self.memory.operator_source.as_ref().map(|s| &s.content),
            self.memory.self_source.as_ref().map(|s| &s.content),
        ))?;
        Ok(())
    }
    fn digest(&self) -> Result<[u8; 32]> {
        struct HashWriter(Sha256);
        impl Write for HashWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        // Fits the fixed digest scratch admitted before the reader. Batch JSON's
        // small writes without retaining a second serialized resource body.
        let mut sink = io::BufWriter::with_capacity(1024, HashWriter(Sha256::new()));
        // Public serialization deliberately skips guidance/memory contents.
        serde_json::to_writer(
            &mut sink,
            &(
                &self.guidance,
                self.guidance
                    .user_global_source
                    .as_ref()
                    .map(|s| &s.content),
                self.guidance.agent_source.as_ref().map(|s| &s.content),
                self.guidance.workspace_source.as_ref().map(|s| &s.content),
                &self.memory,
                self.memory.operator_source.as_ref().map(|s| &s.content),
                self.memory.self_source.as_ref().map(|s| &s.content),
                &self.skills,
                &self.notes,
                &self.plans,
            ),
        )?;
        let sink = sink.into_inner().map_err(|error| error.into_error())?;
        Ok(sink.0.finalize().into())
    }

    /// Include spare capacity and private records, not just public Serialize.
    fn retained_material(&self) -> Result<(u64, usize)> {
        let mut owned = RetainedMaterial {
            bytes: std::mem::size_of::<Self>() + 1024,
            records: 16,
        };
        for source in [
            &self.guidance.user_global_source,
            &self.guidance.agent_source,
            &self.guidance.workspace_source,
        ]
        .into_iter()
        .flatten()
        {
            owned.add(source.path.capacity(), 1)?;
            owned.string(&source.content)?;
        }
        for source in [&self.memory.operator_source, &self.memory.self_source]
            .into_iter()
            .flatten()
        {
            owned.add(source.path.capacity(), 1)?;
            owned.string(&source.content)?;
        }
        if let Some(notes) = &self.notes {
            owned.string(notes)?;
        }
        for (id, plan) in &self.plans {
            owned.string(id)?;
            owned.add(
                usize::try_from(serialized_bytes(plan)?)?.saturating_mul(4) + 512,
                16,
            )?;
        }
        let skills = &self.skills;
        owned.vector(&skills.discovered_roots)?;
        for root in &skills.discovered_roots {
            owned.add(root.path.capacity(), 1)?;
        }
        for (index, catalog) in [&skills.discoverable_skills, &skills.attached_skills]
            .into_iter()
            .enumerate()
        {
            if index == 1 && Arc::ptr_eq(&skills.discoverable_skills, &skills.attached_skills) {
                continue;
            }
            owned.add(128, 1)?;
            owned.vector(catalog.as_ref())?;
            for entry in catalog.iter() {
                for text in [
                    &entry.skill_id,
                    &entry.root_id,
                    &entry.skill_dir,
                    &entry.name,
                    &entry.description,
                ] {
                    owned.string(text)?;
                }
                owned.add(entry.path.capacity(), 1)?;
            }
        }
        owned.vector(&skills.active_skills)?;
        for active in &skills.active_skills {
            for text in [&active.skill_id, &active.name, &active.agent_id] {
                owned.string(text)?;
            }
            owned.add(active.path.capacity(), 1)?;
        }
        owned.vector(&skills.agent_templates_catalog)?;
        for template in &skills.agent_templates_catalog {
            for text in [
                &template.catalog_id,
                &template.template,
                &template.template_id,
                &template.name,
                &template.description,
            ] {
                owned.string(text)?;
            }
            for text in [
                &template.schema_version,
                &template.source_id,
                &template.resolved_ref,
                &template.resolved_revision,
                &template.source_url,
            ]
            .into_iter()
            .flatten()
            {
                owned.string(text)?;
            }
            if let Some(path) = &template.path {
                owned.add(path.capacity(), 1)?;
            }
            owned.vector(&template.included_skills)?;
            for text in &template.included_skills {
                owned.string(text)?;
            }
        }
        Ok((u64::try_from(owned.bytes)?, owned.records))
    }
}

struct RetainedMaterial {
    bytes: usize,
    records: usize,
}
impl RetainedMaterial {
    fn add(&mut self, bytes: usize, records: usize) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("resource bytes overflow")?;
        self.records = self
            .records
            .checked_add(records)
            .context("resource records overflow")?;
        Ok(())
    }
    fn string(&mut self, value: &String) -> Result<()> {
        self.add(value.capacity(), 1)
    }
    fn vector<T>(&mut self, value: &Vec<T>) -> Result<()> {
        self.add(
            value
                .capacity()
                .checked_mul(std::mem::size_of::<T>())
                .context("resource vector overflow")?,
            value.capacity(),
        )
    }
}

#[derive(Debug)]
struct PreparedSource {
    key: ContextResourceSource,
    usage: (u64, usize),
    _material: Arc<BudgetViewGuard>,
}
impl PreparedSource {
    #[cfg(test)]
    fn new(
        source: &ContextResourceSource,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Arc<Self>> {
        Self::after(source, ledger, (0, 0))
    }
    fn after(
        source: &ContextResourceSource,
        ledger: &InputEvidenceBudgetLedger,
        prior: (u64, usize),
    ) -> Result<Arc<Self>> {
        // Measure borrowed identity before reserving, cloning or allocating Arc.
        let bytes = serialized_bytes(source)?
            .checked_add(1024)
            .context("resource source bytes overflow")?;
        let material = ResourceReadMaterial::after(ledger, &source.scope, prior)?;
        let _io = material.enter();
        resource_io::charge_owned_metadata(bytes, 32)?;
        let measured = material.finish()?;
        let usage = (
            prior
                .0
                .checked_add(measured.0)
                .context("resource phase bytes overflow")?,
            prior
                .1
                .checked_add(measured.1)
                .context("resource phase records overflow")?,
        );
        let guard = material.guard()?;
        Ok(Arc::new(Self {
            key: source.clone(),
            usage,
            _material: guard,
        }))
    }
}

#[derive(Debug)]
struct SealedResourceBody {
    body: ContextResourceBody,
    digest: [u8; 32],
    material: Arc<BudgetViewGuard>,
}

impl SealedResourceBody {
    fn new(body: ContextResourceBody, material: &ResourceReadMaterial) -> Result<Self> {
        let (bytes, records) = body.retained_material()?;
        let admitted = material.finish()?;
        // Sealing is not retroactive permission for an unmetered reader.
        anyhow::ensure!(
            bytes <= admitted.0 && records <= admitted.1,
            crate::runtime_error::RuntimeError::new(
                crate::runtime_error::RuntimeErrorDomain::Io,
                "context_resource_exhausted",
                "ResourceExhausted: unreserved context resource ownership",
            )
            .with_retryable(true)
        );
        let material = material.seal_retained(bytes, records)?;
        let digest = body.digest()?;
        Ok(Self {
            body,
            digest,
            material,
        })
    }
}

#[derive(Debug)]
pub(crate) struct ResourceObservation {
    source: Arc<PreparedSource>,
    epoch: u64,
    generation: u64,
    digest: [u8; 32],
    body: ContextResourceBody,
    _material: Arc<BudgetViewGuard>,
}
impl ResourceObservation {
    #[cfg(test)]
    pub(crate) fn source(&self) -> &ContextResourceSource {
        &self.source.key
    }
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
    pub(crate) fn body(&self) -> &ContextResourceBody {
        &self.body
    }
}

/// Summary cannot consume plans or memory under an instruction-only binding.
#[derive(Debug)]
pub(crate) struct InstructionResourceRead(Arc<ResourceObservation>);
impl InstructionResourceRead {
    pub(crate) fn guidance(&self) -> &LoadedAgentsMd {
        &self.0.body.guidance
    }
    pub(crate) fn skills(&self) -> &SkillsRuntimeView {
        &self.0.body.skills
    }
    #[cfg(test)]
    pub(super) fn observation_weak(&self) -> std::sync::Weak<ResourceObservation> {
        Arc::downgrade(&self.0)
    }
}

#[derive(Default)]
struct ObservationState {
    source: Option<Arc<PreparedSource>>,
    generation: u64,
    epoch: u64,
    nonce: u64,
    exhausted: bool,
    freshness: ResourceFreshness,
    value: Option<Arc<ResourceObservation>>,
}

/// Only minted by begin_refresh. No Clone/Serde or external ticket constructor.
struct ResourceRefreshTicket {
    source: Arc<PreparedSource>,
    generation: u64,
    nonce: u64,
}

pub(crate) struct ContextResourceRegistry {
    ledger: InputEvidenceBudgetLedger,
    state: Mutex<ObservationState>,
}

impl ContextResourceRegistry {
    pub(crate) fn new(ledger: InputEvidenceBudgetLedger) -> Self {
        Self {
            ledger,
            state: Mutex::new(ObservationState::default()),
        }
    }
    fn lock(&self) -> Result<MutexGuard<'_, ObservationState>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("resource registry poisoned"))
    }
    pub(crate) fn freshness(&self) -> Result<ResourceFreshness> {
        Ok(self.lock()?.freshness)
    }
    pub(crate) fn invalidate(&self, reason: ResourceInvalidation) -> Result<()> {
        let mut state = self.lock()?;
        if state.freshness == ResourceFreshness::Closed {
            return Err(ResourceObservationError::NotReady(ResourceFreshness::Closed).into());
        }
        // Mark Dirty before fallible advancement: MAX must not revive a ticket.
        state.freshness = ResourceFreshness::Dirty(reason);
        state.generation = advance(&mut state, |s| s.generation)?;
        Ok(())
    }
    pub(crate) fn close(&self) -> Result<()> {
        let mut state = self.lock()?;
        state.freshness = ResourceFreshness::Closed;
        state.exhausted = true;
        state.value = None;
        state.source = None;
        Ok(())
    }
    fn begin_refresh(&self, source: Arc<PreparedSource>) -> Result<ResourceRefreshTicket> {
        let mut state = self.lock()?;
        if state.exhausted {
            return Err(ResourceObservationError::GenerationExhausted.into());
        }
        if state.freshness == ResourceFreshness::Closed {
            return Err(ResourceObservationError::NotReady(ResourceFreshness::Closed).into());
        }
        // A candidate is not the published source until validated installation.
        let source = state
            .source
            .as_ref()
            .filter(|old| old.key == source.key)
            .cloned()
            .unwrap_or(source);
        state.nonce = advance(&mut state, |s| s.nonce)?;
        Ok(ResourceRefreshTicket {
            source,
            generation: state.generation,
            nonce: state.nonce,
        })
    }
    fn matches(state: &ObservationState, ticket: &ResourceRefreshTicket) -> bool {
        !state.exhausted
            && state.freshness != ResourceFreshness::Closed
            && state.generation == ticket.generation
            && state.nonce == ticket.nonce
    }
    fn fail(&self, ticket: ResourceRefreshTicket) -> Result<()> {
        let mut state = self.lock()?;
        if Self::matches(&state, &ticket)
            && state
                .source
                .as_ref()
                .is_none_or(|source| Arc::ptr_eq(source, &ticket.source))
        {
            // An unrelated candidate failure cannot invalidate a published view.
            state.freshness = ResourceFreshness::RefreshFailed;
        }
        Ok(())
    }
    fn install(
        &self,
        ticket: ResourceRefreshTicket,
        candidate: SealedResourceBody,
    ) -> Result<Arc<ResourceObservation>> {
        let SealedResourceBody {
            body,
            digest,
            material: guard,
        } = candidate;
        let mut state = self.lock()?;
        if !Self::matches(&state, &ticket) {
            return Err(ResourceObservationError::ObsoleteRefresh.into());
        }
        if let Some(old) = state.value.as_ref().filter(|old| {
            Arc::ptr_eq(&old.source, &ticket.source)
                && old.digest == digest
                && old.generation == ticket.generation
        }) {
            let old = old.clone();
            state.freshness = ResourceFreshness::Ready;
            return Ok(old);
        }
        let generation = if state
            .source
            .as_ref()
            .is_some_and(|source| Arc::ptr_eq(source, &ticket.source))
        {
            state.generation
        } else {
            advance(&mut state, |s| s.generation)?
        };
        let epoch = advance(&mut state, |s| s.epoch)?;
        let value = Arc::new(ResourceObservation {
            source: ticket.source.clone(),
            epoch,
            generation,
            digest,
            body,
            _material: guard,
        });
        state.source = Some(ticket.source);
        state.generation = generation;
        state.epoch = epoch;
        state.value = Some(value.clone());
        state.freshness = ResourceFreshness::Ready;
        Ok(value)
    }
    /// No reader, capability check, filesystem access or body copy on capture.
    pub(crate) fn capture(
        &self,
        source: &ContextResourceSource,
    ) -> Result<Arc<ResourceObservation>> {
        self.capture_matching(|old| old == source)
    }
    pub(crate) fn capture_instructions(
        &self,
        source: &ContextResourceSource,
    ) -> Result<InstructionResourceRead> {
        self.capture_matching(|old| old.same_instruction_sources(source))
            .map(InstructionResourceRead)
    }
    fn capture_matching(
        &self,
        matches: impl FnOnce(&ContextResourceSource) -> bool,
    ) -> Result<Arc<ResourceObservation>> {
        let state = self.lock()?;
        if state.exhausted {
            return Err(ResourceObservationError::GenerationExhausted.into());
        }
        if state.source.as_ref().is_none_or(|old| !matches(&old.key)) {
            return Err(ResourceObservationError::SourceChanged.into());
        }
        if state.freshness != ResourceFreshness::Ready {
            return Err(ResourceObservationError::NotReady(state.freshness).into());
        }
        state
            .value
            .clone()
            .context("ready resource observation missing")
    }
    /// Explicit core seam, not a production Context/Work/Host acquisition path.
    /// `read` uses the existing metered synchronous loaders. Owner acquisition
    /// happens only after I/O ends; `validate` checks current root/source/identity
    /// under that original owner reservation, which lives through installation.
    #[cfg(test)]
    pub(crate) fn refresh(
        &self,
        source: &ContextResourceSource,
        read: impl FnOnce() -> Result<ContextResourceBody>,
        try_owner: impl FnOnce() -> Result<MutationReservation>,
        validate: impl FnOnce(&MutationReservation, &ContextResourceSource) -> Result<()>,
    ) -> Result<Arc<ResourceObservation>> {
        self.refresh_fenced_after(source, (0, 0), read, try_owner, validate)
    }

    /// Validation returns a narrow guard covering registry installation.
    /// Lock order: original owner -> Host readiness fence -> registry.
    pub(crate) fn refresh_fenced_after<F>(
        &self,
        source: &ContextResourceSource,
        prior: (u64, usize),
        read: impl FnOnce() -> Result<ContextResourceBody>,
        try_owner: impl FnOnce() -> Result<MutationReservation>,
        validate: impl FnOnce(&MutationReservation, &ContextResourceSource) -> Result<F>,
    ) -> Result<Arc<ResourceObservation>> {
        let prepared = PreparedSource::after(source, &self.ledger, prior)?;
        let ticket = self.begin_refresh(prepared)?;
        let material =
            match ResourceReadMaterial::after(&self.ledger, &source.scope, ticket.source.usage) {
                Ok(material) => material,
                Err(error) => {
                    self.fail(ticket)?;
                    return Err(error);
                }
            };
        let body = (|| {
            let _io = material.enter();
            // Admit fixed body/digest/Arc/container headers before the reader.
            resource_io::charge_owned_metadata(8192, 128)?;
            let body = read();
            // A swallowed optional-reader quota failure remains a typed error.
            material.finish()?;
            body
        })();
        let candidate = match body.and_then(|body| SealedResourceBody::new(body, &material)) {
            Ok(candidate) => candidate,
            Err(error) => {
                self.fail(ticket)?;
                return Err(error);
            }
        };
        let owner = match try_owner() {
            Ok(owner) => owner,
            Err(error) => {
                if !matches!(
                    error.downcast_ref::<MutationOwnerError>(),
                    Some(MutationOwnerError::Busy)
                ) && !matches!(
                    error.downcast_ref::<ResourceObservationError>(),
                    Some(ResourceObservationError::InventoryContended)
                ) {
                    self.fail(ticket)?;
                }
                return Err(error);
            }
        };
        let validation: Result<F> = (|| {
            anyhow::ensure!(
                owner.agent_id() == source.scope.agent_id
                    && owner.identity_incarnation() == source.scope.identity_incarnation,
                ResourceObservationError::SourceChanged
            );
            let _io = material.enter();
            let fence = validate(&owner, &ticket.source.key)?;
            material.finish()?;
            Ok(fence)
        })();
        let fence = match validation {
            Ok(fence) => fence,
            Err(error) => {
                if !matches!(
                    error.downcast_ref::<MutationOwnerError>(),
                    Some(MutationOwnerError::Busy)
                ) && !matches!(
                    error.downcast_ref::<ResourceObservationError>(),
                    Some(ResourceObservationError::InventoryContended)
                ) {
                    self.fail(ticket)?;
                }
                return Err(error);
            }
        };
        let result = self.install(ticket, candidate);
        drop(fence);
        result
    }
}

fn advance(
    state: &mut ObservationState,
    value: impl FnOnce(&ObservationState) -> u64,
) -> Result<u64> {
    if !state.exhausted {
        if let Some(next) = value(state).checked_add(1) {
            return Ok(next);
        }
    }
    state.exhausted = true;
    if !matches!(state.freshness, ResourceFreshness::Dirty(_)) {
        state.freshness = ResourceFreshness::Dirty(ResourceInvalidation::ExplicitRefresh);
    }
    Err(ResourceObservationError::GenerationExhausted.into())
}

/// Successful optional absence is only NotFound; all other failures propagate.
#[cfg(test)]
pub(crate) fn read_optional_text(path: &std::path::Path) -> Result<Option<String>> {
    match crate::diagnostics::io_probe::read_to_string(path) {
        Ok(body) => Ok(Some(body)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
#[path = "resource_observation_tests.rs"]
mod tests;
