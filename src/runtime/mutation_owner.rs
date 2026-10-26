//! Process-local serialization for the state migration. Published posture
//! observations are deliberately not an authority for legacy state readers.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

use arc_swap::ArcSwapOption;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use super::{
    mutation_witness::{MutationWitness, MutationWitnessKind},
    resident_input_evidence::{InputEvidenceScope, ResidentInputEvidence},
    resident_state::ResidentEpisodes,
};

use crate::types::{AgentIdentityRecord, AgentRegistryStatus, AgentState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryKey {
    pub agent_id: String,
    pub identity_incarnation: u64,
    pub operation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PublicationToken {
    pub identity_incarnation: u64,
    pub owner_epoch: u64,
    pub generation: u64,
}

pub(crate) type PostureObservation = super::resident_state::AgentSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryPhase {
    Committing,
    NeedsReconcile,
}

/// Exact acknowledged identity facts, including an explicitly absent state row.
/// A different identity incarnation describes retirement of this slot; it must
/// never carry an AgentState belonging to that successor incarnation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IdentityMutationFacts {
    pub identity: Option<Arc<AgentIdentityRecord>>,
    pub state: Option<Arc<AgentState>>,
}

#[derive(Debug, Clone)]
pub(crate) enum KnownCommit {
    State {
        state: Arc<AgentState>,
        // Explicit prepared domain; None means unprepared, not ready empty.
        episodes: Option<Arc<ResidentEpisodes>>,
        input_evidence: Option<Arc<ResidentInputEvidence>>,
        work: Option<Arc<super::resident_work::ResidentWorkDomain>>,
        tasks: Option<Arc<super::resident_task::ResidentTaskDomain>>,
        scheduling: Option<Arc<super::resident_scheduling::ResidentSchedulingDomain>>,
        runtime: Option<Arc<super::state_mutation::RuntimeMutationFacts>>,
    },
    Metadata(Arc<IdentityMutationFacts>),
}

impl KnownCommit {
    pub(crate) fn state(&self) -> Option<&Arc<AgentState>> {
        match self {
            Self::State { state, .. } => Some(state),
            Self::Metadata(facts) => facts.state.as_ref(),
        }
    }

    fn same_facts(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::State {
                    state: a,
                    episodes: ae,
                    input_evidence: ai,
                    work: aw,
                    tasks: at,
                    scheduling: asg,
                    runtime: ar,
                },
                Self::State {
                    state: b,
                    episodes: be,
                    input_evidence: bi,
                    work: bw,
                    tasks: bt,
                    scheduling: bsg,
                    runtime: br,
                },
            ) => {
                a == b
                    && match (ae, be) {
                        (None, None) => true,
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        _ => false,
                    }
                    && match (ar, br) {
                        (None, None) => true,
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        _ => false,
                    }
                    && match (aw, bw) {
                        (None, None) => true,
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        _ => false,
                    }
                    && match (at, bt) {
                        (None, None) => true,
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        _ => false,
                    }
                    && match (asg, bsg) {
                        (None, None) => true,
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        _ => false,
                    }
                    && match (ai, bi) {
                        (None, None) => true,
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        _ => false,
                    }
            }
            (Self::Metadata(a), Self::Metadata(b)) => a == b,
            _ => false,
        }
    }
}

#[derive(Debug)]
struct RecoveryMember {
    key: RecoveryKey,
    slot: Weak<MutationOwnerSlot>,
}

#[derive(Debug)]
struct RecoveryGroup {
    // Host registries retain the slots. Weak links avoid retaining bodies through
    // a slot -> group -> slot cycle. Independent standalone namespaces do not
    // acquire a cross-runtime recovery authority by constructing this group.
    members: Vec<RecoveryMember>,
}

/// Only sealed DB evidence may select a narrower positive Input recovery route.
#[derive(Debug)]
pub(in crate::runtime) enum OriginalInputRecoveryEvidence {
    Sparse(Arc<crate::runtime_db::task_stop::TaskStopSparseRecoveryEvidence>),
    Command(Arc<crate::runtime_db::command_task_terminal::OriginalCommandTaskSparseRecovery>),
    TaskStatus(Arc<super::input_recovery::TaskStatusMetadataKnown>),
    ChildTerminal(Arc<super::input_recovery::ChildTerminalKnown>),
    ChildBinding(Arc<super::input_recovery::ChildMonitorBindingKnown>),
    Readonly(Vec<Arc<crate::runtime_db::task_stop::InitialTaskStopReadonlyRecovery>>),
    Partial(Arc<super::input_recovery::TaskStopPartialKnown>),
}

#[derive(Debug, Clone)]
pub(crate) struct RecoveryRecord {
    pub key: RecoveryKey,
    pub phase: RecoveryPhase,
    // Compatibility views only; `known` distinguishes absent rows from unknown.
    pub committed_state: Option<Arc<AgentState>>,
    pub(super) committed_runtime: Option<Arc<super::state_mutation::RuntimeMutationFacts>>,
    pub known: Option<KnownCommit>,
    pub witness: Option<MutationWitness>,
    // A successor claim clears its root. Preserve the original cold boundary
    // so recovery cannot reinterpret a formerly published State as absent.
    original_root_absent: bool,
    _original_input_evidence: Option<Arc<OriginalInputRecoveryEvidence>>,
    group: Arc<RecoveryGroup>,
}

impl RecoveryRecord {
    fn set_known(&mut self, known: KnownCommit) {
        self.committed_state = known.state().cloned();
        self.committed_runtime = match &known {
            KnownCommit::State { runtime, .. } => runtime.clone(),
            KnownCommit::Metadata(_) => None,
        };
        self.known = Some(known);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WitnessUnknownResolution {
    Known,
    NotCommitted,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MutationOwnerError {
    #[error("agent mutation owner is busy")]
    Busy,
    #[error("agent mutation owner has been replaced")]
    Replaced,
    #[error("agent mutation requires reconciliation: {0:?}")]
    NeedsReconcile(RecoveryKey),
    #[error("agent mutation publication ticket does not match")]
    TicketMismatch,
}

#[derive(Debug, Default)]
struct Publication {
    epoch: u64,
    generation: u64,
    recovery: Option<RecoveryRecord>,
    lifetime: Weak<OwnerLifetime>,
}

#[derive(Debug, Default)]
struct MutationOwnerSlot {
    reservation: Arc<AsyncMutex<()>>,
    publication: Mutex<Publication>,
    // Load/store only after validating epoch and fence under publication.
    root: ArcSwapOption<PostureObservation>,
}

#[derive(Debug)]
struct OwnerLifetime {
    epoch: u64,
    slot: Weak<MutationOwnerSlot>,
}

impl Drop for OwnerLifetime {
    fn drop(&mut self) {
        let Some(slot) = self.slot.upgrade() else {
            return;
        };
        let observation = {
            let publication = slot.publication.lock().expect("publication poisoned");
            if publication.epoch == self.epoch {
                // Keep unresolved committed facts for the next owner to recover.
                slot.root.swap(None)
            } else {
                None
            }
        };
        drop(observation);
    }
}

#[derive(Debug, Default)]
pub(crate) struct MutationOwnerRegistry {
    slots: Mutex<BTreeMap<(String, u64), Arc<MutationOwnerSlot>>>,
}

impl MutationOwnerRegistry {
    pub(crate) fn claim(&self, agent_id: &str, incarnation: u64) -> anyhow::Result<MutationOwner> {
        self.claim_reserved(agent_id, incarnation)
            .map(|(owner, _reservation)| owner)
    }

    pub(crate) fn claim_reserved(
        &self,
        agent_id: &str,
        incarnation: u64,
    ) -> anyhow::Result<(MutationOwner, MutationReservation)> {
        let slot = self
            .slots
            .lock()
            .expect("mutation registry poisoned")
            .entry((agent_id.to_owned(), incarnation))
            .or_default()
            .clone();
        // Runtime construction currently happens under the host registry lock.
        // Never wait there for a producer that may need the host to finish.
        let guard = slot
            .reservation
            .clone()
            .try_lock_owned()
            .map_err(|_| MutationOwnerError::Busy)?;
        let (epoch, lifetime, old_root) = {
            let mut publication = slot.publication.lock().expect("publication poisoned");
            publication.epoch = publication
                .epoch
                .checked_add(1)
                .expect("owner epoch exhausted");
            publication.generation = 0;
            let old_root = slot.root.swap(None);
            if let Some(recovery) = publication.recovery.as_mut() {
                recovery.phase = RecoveryPhase::NeedsReconcile;
            }
            // Recovery responsibility survives ownership transfer. Claiming a
            // successor must not silently acknowledge an interrupted operation.
            let lifetime = Arc::new(OwnerLifetime {
                epoch: publication.epoch,
                slot: Arc::downgrade(&slot),
            });
            publication.lifetime = Arc::downgrade(&lifetime);
            (publication.epoch, lifetime, old_root)
        };
        drop(old_root);
        let owner = MutationOwner {
            agent_id: agent_id.to_owned(),
            incarnation,
            epoch,
            _lifetime: lifetime,
            slot,
        };
        let reservation = MutationReservation {
            owner: owner.clone(),
            _guard: guard,
            ticket: None,
        };
        Ok((owner, reservation))
    }

    pub(crate) fn current_owner(
        &self,
        agent_id: &str,
        incarnation: u64,
    ) -> anyhow::Result<MutationOwner> {
        let slot = self
            .slots
            .lock()
            .expect("mutation registry poisoned")
            .entry((agent_id.to_owned(), incarnation))
            .or_default()
            .clone();
        let (epoch, lifetime) = {
            let mut publication = slot.publication.lock().expect("publication poisoned");
            if publication.epoch == 0 {
                publication.epoch = 1;
            }
            let lifetime = publication.lifetime.upgrade().unwrap_or_else(|| {
                Arc::new(OwnerLifetime {
                    epoch: publication.epoch,
                    slot: Arc::downgrade(&slot),
                })
            });
            publication.lifetime = Arc::downgrade(&lifetime);
            (publication.epoch, lifetime)
        };
        Ok(MutationOwner {
            agent_id: agent_id.to_owned(),
            incarnation,
            epoch,
            slot,
            _lifetime: lifetime,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MutationOwner {
    agent_id: String,
    incarnation: u64,
    epoch: u64,
    slot: Arc<MutationOwnerSlot>,
    // Runtime clones and reservations keep this epoch's observation resident.
    _lifetime: Arc<OwnerLifetime>,
}

impl MutationOwner {
    pub(crate) fn owner_epoch(&self) -> u64 {
        self.epoch
    }

    // A standalone runtime owns its own slot; it does not coordinate independent
    // handles opened against the same database without a Host.
    pub(crate) fn standalone(agent_id: &str, incarnation: u64) -> Self {
        MutationOwnerRegistry::default()
            .claim(agent_id, incarnation)
            .expect("fresh mutation owner slot")
    }

    pub(crate) fn validate_bootstrap(&self) -> Result<(), MutationOwnerError> {
        let publication = self.slot.publication.lock().expect("publication poisoned");
        self.validate_publication(&publication)?;
        if let Some(recovery) = &publication.recovery {
            if recovery.known.is_none() || recovery.group.members.len() != 1 {
                return Err(MutationOwnerError::NeedsReconcile(recovery.key.clone()));
            }
        }
        Ok(())
    }

    pub(crate) fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub(crate) fn try_reserve(&self) -> Result<MutationReservation, MutationOwnerError> {
        let guard = self
            .slot
            .reservation
            .clone()
            .try_lock_owned()
            .map_err(|_| MutationOwnerError::Busy)?;
        self.validate_epoch()?;
        Ok(MutationReservation {
            owner: self.clone(),
            _guard: guard,
            ticket: None,
        })
    }

    pub(crate) fn recovery_owners(&self) -> Result<Vec<Self>, MutationOwnerError> {
        let recovery = {
            let publication = self.slot.publication.lock().expect("publication poisoned");
            self.validate_publication(&publication)?;
            publication.recovery.clone()
        };
        let Some(recovery) = recovery else {
            return Ok(vec![self.clone()]);
        };
        recovery
            .group
            .members
            .iter()
            .map(|member| {
                let slot = member
                    .slot
                    .upgrade()
                    .ok_or_else(|| MutationOwnerError::NeedsReconcile(recovery.key.clone()))?;
                let (epoch, lifetime) = {
                    let mut publication = slot.publication.lock().expect("publication poisoned");
                    let lifetime = publication.lifetime.upgrade().unwrap_or_else(|| {
                        Arc::new(OwnerLifetime {
                            epoch: publication.epoch,
                            slot: Arc::downgrade(&slot),
                        })
                    });
                    publication.lifetime = Arc::downgrade(&lifetime);
                    (publication.epoch, lifetime)
                };
                Ok(Self {
                    agent_id: member.key.agent_id.clone(),
                    incarnation: member.key.identity_incarnation,
                    epoch,
                    slot,
                    _lifetime: lifetime,
                })
            })
            .collect()
    }

    pub(crate) async fn reserve_all(
        mut owners: Vec<Self>,
    ) -> Result<Vec<MutationReservation>, MutationOwnerError> {
        loop {
            let mut expanded = owners.clone();
            for owner in &owners {
                expanded.extend(owner.recovery_owners()?);
            }
            expanded
                .sort_by(|a, b| (&a.agent_id, a.incarnation).cmp(&(&b.agent_id, b.incarnation)));
            expanded.dedup_by(|a, b| a.agent_id == b.agent_id && a.incarnation == b.incarnation);
            let mut reservations = Vec::with_capacity(expanded.len());
            for owner in &expanded {
                reservations.push(owner.reserve().await?);
            }
            // A group can be created while this waiter is queued. Drop every
            // permit before acquiring an additional member in sorted order.
            let mut complete = true;
            for reservation in &reservations {
                for member in reservation.owner.recovery_owners()? {
                    if !expanded
                        .iter()
                        .any(|owner| Arc::ptr_eq(&owner.slot, &member.slot))
                    {
                        expanded.push(member);
                        complete = false;
                    }
                }
            }
            if complete {
                return Ok(reservations);
            }
            drop(reservations);
            owners = expanded;
        }
    }

    pub(crate) async fn reserve(&self) -> Result<MutationReservation, MutationOwnerError> {
        let guard = self.slot.reservation.clone().lock_owned().await;
        self.validate_epoch()?;
        Ok(MutationReservation {
            owner: self.clone(),
            _guard: guard,
            ticket: None,
        })
    }

    fn validate_epoch(&self) -> Result<(), MutationOwnerError> {
        if self
            .slot
            .publication
            .lock()
            .expect("publication poisoned")
            .epoch
            != self.epoch
        {
            return Err(MutationOwnerError::Replaced);
        }
        Ok(())
    }

    pub(crate) fn capture(&self) -> Result<Option<Arc<PostureObservation>>, MutationOwnerError> {
        let publication = self.slot.publication.lock().expect("publication poisoned");
        self.validate_publication(&publication)?;
        if let Some(recovery) = &publication.recovery {
            return Err(MutationOwnerError::NeedsReconcile(recovery.key.clone()));
        }
        Ok(self.slot.root.load_full())
    }

    fn validate_publication(&self, publication: &Publication) -> Result<(), MutationOwnerError> {
        if publication.epoch != self.epoch {
            return Err(MutationOwnerError::Replaced);
        }
        Ok(())
    }
}

pub(crate) struct MutationReservation {
    owner: MutationOwner,
    _guard: OwnedMutexGuard<()>,
    ticket: Option<RecoveryKey>,
}

impl MutationReservation {
    pub(crate) fn owner_epoch(&self) -> u64 {
        self.owner.epoch
    }
    pub(crate) fn agent_id(&self) -> &str {
        &self.owner.agent_id
    }
    pub(crate) fn identity_incarnation(&self) -> u64 {
        self.owner.incarnation
    }

    /// A preparation actor may only derive a new domain from a Ready root under
    /// this original reservation. Pending or replaced scopes remain fenced.
    pub(crate) fn capture_ready_snapshot(
        &self,
    ) -> Result<Option<Arc<PostureObservation>>, MutationOwnerError> {
        self.owner.capture()
    }

    /// A State-only command preserves this domain; call before begin_all fences
    /// capture. Unprepared remains explicit and is never promoted here.
    pub(crate) fn prepared_episodes_for_state_only_commit(
        &self,
    ) -> Result<Option<Arc<super::resident_state::ResidentEpisodes>>, MutationOwnerError> {
        Ok(self.owner.capture()?.and_then(|root| root.episodes.clone()))
    }

    pub(crate) fn prepared_inputs_for_state_only_commit(
        &self,
    ) -> Result<Option<Arc<ResidentInputEvidence>>, MutationOwnerError> {
        Ok(self
            .owner
            .capture()?
            .and_then(|root| root.input_evidence.clone()))
    }

    pub(crate) fn prepared_work_for_state_only_commit(
        &self,
    ) -> Result<Option<Arc<super::resident_work::ResidentWorkDomain>>, MutationOwnerError> {
        Ok(self.owner.capture()?.and_then(|root| root.work.clone()))
    }

    pub(crate) fn prepared_tasks_for_state_only_commit(
        &self,
    ) -> Result<Option<Arc<super::resident_task::ResidentTaskDomain>>, MutationOwnerError> {
        Ok(self.owner.capture()?.and_then(|root| root.tasks.clone()))
    }

    pub(crate) fn prepared_scheduling_for_state_only_commit(
        &self,
    ) -> Result<Option<Arc<super::resident_scheduling::ResidentSchedulingDomain>>, MutationOwnerError>
    {
        Ok(self
            .owner
            .capture()?
            .and_then(|root| root.scheduling.clone()))
    }

    pub(crate) fn validate_owner(&self, owner: &MutationOwner) -> Result<(), MutationOwnerError> {
        if self.owner.epoch != owner.epoch || !Arc::ptr_eq(&self.owner.slot, &owner.slot) {
            return Err(MutationOwnerError::Replaced);
        }
        self.owner.validate_epoch()
    }

    fn validate_order(reservations: &[Self]) -> Result<(), MutationOwnerError> {
        for pair in reservations.windows(2) {
            if (&pair[0].owner.agent_id, pair[0].owner.incarnation)
                >= (&pair[1].owner.agent_id, pair[1].owner.incarnation)
            {
                return Err(MutationOwnerError::TicketMismatch);
            }
        }
        Ok(())
    }

    fn validate_complete_groups(
        reservations: &[Self],
        publications: &[std::sync::MutexGuard<'_, Publication>],
    ) -> Result<(), MutationOwnerError> {
        for (reservation, publication) in reservations.iter().zip(publications) {
            reservation.owner.validate_publication(publication)?;
            if let Some(recovery) = &publication.recovery {
                for member in &recovery.group.members {
                    let Some(slot) = member.slot.upgrade() else {
                        return Err(MutationOwnerError::NeedsReconcile(recovery.key.clone()));
                    };
                    let Some(index) = reservations
                        .iter()
                        .position(|candidate| Arc::ptr_eq(&candidate.owner.slot, &slot))
                    else {
                        return Err(MutationOwnerError::NeedsReconcile(recovery.key.clone()));
                    };
                    if publications[index].recovery.as_ref().is_none_or(|other| {
                        other.key != member.key || !Arc::ptr_eq(&other.group, &recovery.group)
                    }) {
                        return Err(MutationOwnerError::TicketMismatch);
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_known(&self, known: &KnownCommit) -> Result<(), MutationOwnerError> {
        if known
            .state()
            .is_some_and(|state| state.id != self.owner.agent_id)
        {
            return Err(MutationOwnerError::TicketMismatch);
        }
        if let KnownCommit::State {
            episodes: Some(episodes),
            ..
        } = known
        {
            if !episodes.belongs_to(&self.owner.agent_id, self.owner.incarnation) {
                return Err(MutationOwnerError::TicketMismatch);
            }
        }
        if let KnownCommit::State {
            input_evidence: Some(inputs),
            ..
        } = known
        {
            if !inputs.belongs_to(&InputEvidenceScope {
                agent_id: self.owner.agent_id.clone(),
                identity_incarnation: self.owner.incarnation,
            }) {
                return Err(MutationOwnerError::TicketMismatch);
            }
        }
        if let KnownCommit::State {
            work: Some(work), ..
        } = known
        {
            if work.scope()
                != &(InputEvidenceScope {
                    agent_id: self.owner.agent_id.clone(),
                    identity_incarnation: self.owner.incarnation,
                })
            {
                return Err(MutationOwnerError::TicketMismatch);
            }
        }
        if let KnownCommit::State {
            tasks: Some(tasks), ..
        } = known
        {
            if tasks.scope()
                != &(InputEvidenceScope {
                    agent_id: self.owner.agent_id.clone(),
                    identity_incarnation: self.owner.incarnation,
                })
            {
                return Err(MutationOwnerError::TicketMismatch);
            }
        }
        if let KnownCommit::State {
            scheduling: Some(domain),
            ..
        } = known
        {
            if domain.scope()
                != &(InputEvidenceScope {
                    agent_id: self.owner.agent_id.clone(),
                    identity_incarnation: self.owner.incarnation,
                })
            {
                return Err(MutationOwnerError::TicketMismatch);
            }
        }
        if let KnownCommit::Metadata(facts) = known {
            if let Some(identity) = &facts.identity {
                if identity.agent_id != self.owner.agent_id
                    || (identity.incarnation != self.owner.incarnation && facts.state.is_some())
                {
                    return Err(MutationOwnerError::TicketMismatch);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn begin_all(
        reservations: &mut [Self],
        operation_id: String,
    ) -> Result<Vec<RecoveryKey>, MutationOwnerError> {
        Self::begin_all_inner(reservations, operation_id, None)
    }

    /// Registers a frozen durable result address in the same critical section
    /// that closes the original operation's publication gate.
    pub(crate) fn begin_all_with_witness(
        reservations: &mut [Self],
        operation_id: String,
        witness: MutationWitness,
    ) -> Result<Vec<RecoveryKey>, MutationOwnerError> {
        Self::begin_all_inner(reservations, operation_id, Some(witness))
    }

    fn begin_all_inner(
        reservations: &mut [Self],
        operation_id: String,
        witness: Option<MutationWitness>,
    ) -> Result<Vec<RecoveryKey>, MutationOwnerError> {
        Self::validate_order(reservations)?;
        let mut publications = reservations
            .iter()
            .map(|r| {
                r.owner
                    .slot
                    .publication
                    .lock()
                    .expect("publication poisoned")
            })
            .collect::<Vec<_>>();
        for (reservation, publication) in reservations.iter().zip(&publications) {
            reservation.owner.validate_publication(publication)?;
            if let Some(recovery) = &publication.recovery {
                return Err(MutationOwnerError::NeedsReconcile(recovery.key.clone()));
            }
        }
        if let Some(witness) = &witness {
            let scopes = reservations
                .iter()
                .map(|reservation| InputEvidenceScope {
                    agent_id: reservation.agent_id().to_owned(),
                    identity_incarnation: reservation.identity_incarnation(),
                })
                .collect::<Vec<_>>();
            if !witness.matches_scope_group(&scopes)
                || witness.operation_id() != operation_id
                || witness.original_attempt_finished()
            {
                return Err(MutationOwnerError::TicketMismatch);
            }
        }
        let keys = reservations
            .iter()
            .map(|reservation| RecoveryKey {
                agent_id: reservation.owner.agent_id.clone(),
                identity_incarnation: reservation.owner.incarnation,
                operation_id: operation_id.clone(),
            })
            .collect::<Vec<_>>();
        let group = Arc::new(RecoveryGroup {
            members: reservations
                .iter()
                .zip(&keys)
                .map(|(reservation, key)| RecoveryMember {
                    key: key.clone(),
                    slot: Arc::downgrade(&reservation.owner.slot),
                })
                .collect(),
        });
        for ((reservation, publication), key) in
            reservations.iter().zip(&mut publications).zip(&keys)
        {
            let original_root_absent = reservation.owner.slot.root.load_full().is_none();
            publication.recovery = Some(RecoveryRecord {
                key: key.clone(),
                phase: RecoveryPhase::Committing,
                committed_state: None,
                committed_runtime: None,
                known: None,
                witness: witness.clone(),
                original_root_absent,
                _original_input_evidence: None,
                group: group.clone(),
            });
        }
        drop(publications);
        for (reservation, key) in reservations.iter_mut().zip(&keys) {
            reservation.ticket = Some(key.clone());
        }
        Ok(keys)
    }

    pub(crate) fn committed_all(
        reservations: &mut [Self],
        known: Vec<KnownCommit>,
    ) -> Result<(), MutationOwnerError> {
        if reservations.len() != known.len() {
            return Err(MutationOwnerError::TicketMismatch);
        }
        Self::validate_order(reservations)?;
        let mut publications = reservations
            .iter()
            .map(|r| {
                r.owner
                    .slot
                    .publication
                    .lock()
                    .expect("publication poisoned")
            })
            .collect::<Vec<_>>();
        Self::validate_complete_groups(reservations, &publications)?;
        for ((reservation, publication), facts) in
            reservations.iter().zip(&publications).zip(&known)
        {
            reservation.check_ticket(publication)?;
            reservation.validate_known(facts)?;
            if publication
                .recovery
                .as_ref()
                .and_then(|r| r.known.as_ref())
                .is_some_and(|saved| !saved.same_facts(facts))
            {
                return Err(MutationOwnerError::TicketMismatch);
            }
        }
        for (publication, facts) in publications.iter_mut().zip(known) {
            publication
                .recovery
                .as_mut()
                .expect("checked ticket")
                .set_known(facts);
        }
        Ok(())
    }

    /// Accept only a coherent Episode receipt lookup made while this complete
    /// single-owner reservation is held. Some is the loader's current exact root,
    /// not the historical command candidate; Metadata is a retired current
    /// identity with explicitly absent state for this slot. None proves the original attempt
    /// did not commit; it preserves the old root and is not an Applied result.
    pub(crate) fn resolve_episode_unknown(
        reservations: &mut [Self],
        key: &RecoveryKey,
        witness: &MutationWitness,
        committed: Option<KnownCommit>,
    ) -> Result<WitnessUnknownResolution, MutationOwnerError> {
        Self::resolve_witness_unknown(
            reservations,
            key,
            witness,
            MutationWitnessKind::EpisodeMemory,
            committed,
        )
    }

    pub(crate) fn resolve_control_unknown(
        reservations: &mut [Self],
        key: &RecoveryKey,
        witness: &MutationWitness,
        committed: Option<KnownCommit>,
    ) -> Result<WitnessUnknownResolution, MutationOwnerError> {
        Self::resolve_witness_unknown(
            reservations,
            key,
            witness,
            MutationWitnessKind::Control,
            committed,
        )
    }

    /// Validate the complete original group without clearing any publication
    /// gate. Negative resolution uses this before fallible pending-ticket
    /// release, while retaining the same reservations through the final clear.
    pub(crate) fn validate_input_unknown(
        reservations: &[Self],
        key: &RecoveryKey,
        witness: &MutationWitness,
    ) -> Result<(), MutationOwnerError> {
        Self::validate_order(reservations)?;
        let publications = reservations
            .iter()
            .map(|reservation| {
                reservation
                    .owner
                    .slot
                    .publication
                    .lock()
                    .expect("publication poisoned")
            })
            .collect::<Vec<_>>();
        Self::validate_input_unknown_locked(reservations, &publications, key, witness).map(|_| ())
    }

    fn validate_input_unknown_locked(
        reservations: &[Self],
        publications: &[std::sync::MutexGuard<'_, Publication>],
        key: &RecoveryKey,
        witness: &MutationWitness,
    ) -> Result<Vec<usize>, MutationOwnerError> {
        Self::validate_complete_groups(reservations, publications)?;
        if witness.kind() != MutationWitnessKind::InputMutation
            || witness.operation_id() != key.operation_id
        {
            return Err(MutationOwnerError::TicketMismatch);
        }
        // reserve_all may contain other ready scopes or complete recovery
        // groups. Only this original attempt's members may be resolved here.
        let selected = publications
            .iter()
            .enumerate()
            .filter_map(|(index, publication)| {
                publication
                    .recovery
                    .as_ref()
                    .filter(|recovery| {
                        recovery.key.operation_id == key.operation_id
                            && recovery
                                .witness
                                .as_ref()
                                .is_some_and(|saved| saved.same_attempt(witness))
                    })
                    .map(|_| index)
            })
            .collect::<Vec<_>>();
        let scopes = selected
            .iter()
            .map(|&index| InputEvidenceScope {
                agent_id: reservations[index].agent_id().to_owned(),
                identity_incarnation: reservations[index].identity_incarnation(),
            })
            .collect::<Vec<_>>();
        if !witness.matches_scope_group(&scopes)
            || !selected.iter().any(|&index| {
                reservations[index].agent_id() == key.agent_id
                    && reservations[index].identity_incarnation() == key.identity_incarnation
            })
        {
            return Err(MutationOwnerError::TicketMismatch);
        }
        for &index in &selected {
            let reservation = &reservations[index];
            let recovery = publications[index]
                .recovery
                .as_ref()
                .expect("selected recovery");
            if recovery.known.is_some()
                || recovery.key.agent_id != reservation.agent_id()
                || recovery.key.identity_incarnation != reservation.identity_incarnation()
                || recovery.group.members.len() != selected.len()
            {
                return Err(MutationOwnerError::TicketMismatch);
            }
        }
        if !witness.original_attempt_finished() {
            return Err(MutationOwnerError::NeedsReconcile(key.clone()));
        }
        Ok(selected)
    }

    /// Promote only the complete original group, after fresh whole-transaction
    /// receipt resolution. Current roots are independent of the original result.
    pub(crate) fn resolve_input_unknown(
        reservations: &mut [Self],
        key: &RecoveryKey,
        witness: &MutationWitness,
        committed: Option<Vec<KnownCommit>>,
    ) -> Result<WitnessUnknownResolution, MutationOwnerError> {
        Self::resolve_input_unknown_inner(reservations, key, witness, committed, false, None)
    }

    /// Only the never-activated exclusive actor may keep an original startup
    /// phase cold. Ordinary same-inc Deleting resolution still needs domains.
    pub(crate) fn resolve_startup_input_unknown(
        reservations: &mut [Self],
        key: &RecoveryKey,
        witness: &MutationWitness,
        committed: Option<Vec<KnownCommit>>,
        capability: &crate::runtime_db::input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery<'_>,
        db: &crate::runtime_db::RuntimeDb,
    ) -> anyhow::Result<WitnessUnknownResolution> {
        capability.validate_database(db)?;
        if let Some(facts) = &committed {
            anyhow::ensure!(facts.iter().all(|facts|matches!(facts,KnownCommit::Metadata(metadata) if metadata.state.is_none())),"startup input recovery cannot install State or domains");
        }
        Ok(Self::resolve_input_unknown_inner(
            reservations,
            key,
            witness,
            committed,
            true,
            None,
        )?)
    }

    pub(in crate::runtime) fn resolve_task_stop_input_unknown(
        reservations: &mut [Self],
        key: &RecoveryKey,
        witness: &MutationWitness,
        committed: Vec<KnownCommit>,
        evidence: OriginalInputRecoveryEvidence,
    ) -> Result<WitnessUnknownResolution, MutationOwnerError> {
        if matches!(
            evidence,
            OriginalInputRecoveryEvidence::Command(_)
                | OriginalInputRecoveryEvidence::TaskStatus(_)
                | OriginalInputRecoveryEvidence::ChildTerminal(_)
                | OriginalInputRecoveryEvidence::ChildBinding(_)
        ) {
            return Err(MutationOwnerError::TicketMismatch);
        }
        Self::resolve_input_unknown_inner(
            reservations,
            key,
            witness,
            Some(committed),
            false,
            Some(Arc::new(evidence)),
        )
    }

    pub(in crate::runtime) fn resolve_original_command_input_unknown(
        reservations: &mut [Self],
        key: &RecoveryKey,
        witness: &MutationWitness,
        committed: Vec<KnownCommit>,
        proof: Arc<crate::runtime_db::command_task_terminal::OriginalCommandTaskSparseRecovery>,
    ) -> Result<WitnessUnknownResolution, MutationOwnerError> {
        Self::resolve_input_unknown_inner(
            reservations,
            key,
            witness,
            Some(committed),
            false,
            Some(Arc::new(OriginalInputRecoveryEvidence::Command(proof))),
        )
    }

    pub(in crate::runtime) fn resolve_task_status_metadata_input_unknown(
        reservations: &mut [Self],
        key: &RecoveryKey,
        witness: &MutationWitness,
        committed: Vec<KnownCommit>,
        proof: Arc<super::input_recovery::TaskStatusMetadataKnown>,
    ) -> Result<WitnessUnknownResolution, MutationOwnerError> {
        Self::resolve_input_unknown_inner(
            reservations,
            key,
            witness,
            Some(committed),
            false,
            Some(Arc::new(OriginalInputRecoveryEvidence::TaskStatus(proof))),
        )
    }

    pub(in crate::runtime) fn resolve_child_terminal_input_unknown(
        reservations: &mut [Self],
        key: &RecoveryKey,
        witness: &MutationWitness,
        proof: Arc<super::input_recovery::ChildTerminalKnown>,
    ) -> Result<WitnessUnknownResolution, MutationOwnerError> {
        Self::resolve_input_unknown_inner(
            reservations,
            key,
            witness,
            Some(proof.known()),
            false,
            Some(Arc::new(OriginalInputRecoveryEvidence::ChildTerminal(
                proof,
            ))),
        )
    }

    pub(in crate::runtime) fn resolve_child_monitor_binding_input_unknown(
        reservations: &mut [Self],
        key: &RecoveryKey,
        witness: &MutationWitness,
        proof: Arc<super::input_recovery::ChildMonitorBindingKnown>,
    ) -> Result<WitnessUnknownResolution, MutationOwnerError> {
        Self::resolve_input_unknown_inner(
            reservations,
            key,
            witness,
            Some(vec![proof.known()]),
            false,
            Some(Arc::new(OriginalInputRecoveryEvidence::ChildBinding(proof))),
        )
    }
    fn resolve_input_unknown_inner(
        reservations: &mut [Self],
        key: &RecoveryKey,
        witness: &MutationWitness,
        committed: Option<Vec<KnownCommit>>,
        startup_cold: bool,
        task_stop_evidence: Option<Arc<OriginalInputRecoveryEvidence>>,
    ) -> Result<WitnessUnknownResolution, MutationOwnerError> {
        Self::validate_order(reservations)?;
        let mut publications = reservations
            .iter()
            .map(|reservation| {
                reservation
                    .owner
                    .slot
                    .publication
                    .lock()
                    .expect("publication poisoned")
            })
            .collect::<Vec<_>>();
        let selected =
            Self::validate_input_unknown_locked(reservations, &publications, key, witness)?;
        if let Some(known) = committed {
            if known.len() != selected.len() {
                return Err(MutationOwnerError::TicketMismatch);
            }
            for (&index, facts) in selected.iter().zip(&known) {
                let reservation = &reservations[index];
                let publication = &publications[index];
                let stop_specific = task_stop_evidence.as_ref().is_some_and(|evidence| {
                    let Some(attempt)=witness.input_attempt() else { return false; };
                    match evidence.as_ref() {
                        OriginalInputRecoveryEvidence::ChildBinding(proof)=> {
                            selected.len()==1 && proof.evidence().matches_original(attempt)
                                && proof.evidence().receipt().prepared.scopes()==attempt.prepared().scopes()
                                && facts.same_facts(&proof.known())
                        }
                        OriginalInputRecoveryEvidence::ChildTerminal(proof)=>{
                            proof.evidence().matches_original(attempt)
                                && proof.evidence().receipt().prepared.scopes()==attempt.prepared().scopes()
                                && proof.known_for_scope(&super::resident_input_evidence::InputEvidenceScope{agent_id:reservation.agent_id().to_owned(),identity_incarnation:reservation.identity_incarnation()})
                                    .is_some_and(|expected|facts.same_facts(expected))
                        }
                        OriginalInputRecoveryEvidence::TaskStatus(proof) => {
                            selected.len()==1 && proof.proof().matches_original(attempt)
                                && proof.proof().receipt().prepared.scopes()==attempt.prepared().scopes()
                                && facts.same_facts(&proof.known())
                        }
                        OriginalInputRecoveryEvidence::Command(proof) => {
                            selected.len()==1 && proof.matches_original(attempt)
                                && proof.mode()!=crate::runtime_db::command_task_terminal::CommandTaskTerminalInstallMode::Current
                                && matches!(facts,KnownCommit::Metadata(metadata) if metadata.state.is_none() && metadata.identity.as_deref()==proof.identity())
                        }
                        OriginalInputRecoveryEvidence::Sparse(proof) => {
                            if selected.len()!=1 || !proof.matches_original(attempt)
                                || proof.receipt().prepared.scopes()!=attempt.prepared().scopes()
                                || proof.mode()==crate::runtime_db::task_stop::TaskStopPhaseInstallMode::Current {
                                return false;
                            }
                            let presence_matches=matches!((proof.mode(),proof.state_presence()),
                                (crate::runtime_db::task_stop::TaskStopPhaseInstallMode::NoState,crate::runtime_db::task_stop::TaskStopSparseStatePresence::Absent)
                                |(crate::runtime_db::task_stop::TaskStopPhaseInstallMode::Incomplete,crate::runtime_db::task_stop::TaskStopSparseStatePresence::PresentUnprepared)
                                |(crate::runtime_db::task_stop::TaskStopPhaseInstallMode::Retired,crate::runtime_db::task_stop::TaskStopSparseStatePresence::NotReadRetired));
                            if !presence_matches { return false; }
                            let cold=reservation.owner.slot.root.load_full().as_ref().is_none_or(|root|
                                root.episodes.is_none() && root.input_evidence.is_none() && root.work.is_none() && root.tasks.is_none());
                            cold && matches!(facts,KnownCommit::Metadata(metadata) if metadata.state.is_none()
                                && metadata.identity.as_deref()==proof.identity())
                        }
                        OriginalInputRecoveryEvidence::Partial(partial) => {
                            let proof=partial.evidence();
                            selected.len()==1 && proof.matches_original(attempt)
                                && proof.mode()==crate::runtime_db::task_stop::TaskStopPhaseInstallMode::Incomplete
                                && reservation.owner.slot.root.load_full().as_ref().is_none_or(|root|root.tasks.is_none())
                                && matches!(facts,KnownCommit::State{tasks:None,..})
                                && facts.same_facts(&partial.known())
                        }
                        OriginalInputRecoveryEvidence::Readonly(proofs) => proofs.iter().any(|proof| {
                            let child=proof.child();
                            if child.target().scope.agent_id!=reservation.agent_id()
                                || child.target().scope.identity_incarnation!=reservation.identity_incarnation()
                                || !proof.matches_original(attempt) { return false; }
                            let Some(original)=witness.initial_task_stop_readonly(reservation.agent_id()) else { return false; };
                            if original.target()!=child.target() { return false; }
                            if child.state_present()==Some(true) && matches!(original.known(),KnownCommit::State{..}) {
                                facts.same_facts(&original.known())
                            } else {
                                matches!(facts,KnownCommit::Metadata(metadata) if metadata.state.is_none()
                                    && metadata.identity.as_ref().is_some_and(|identity|identity.as_ref()==child.identity().as_ref()))
                            }
                        }),
                    }
                });
                if stop_specific {
                    reservation.validate_known(facts)?;
                    continue;
                }
                // A sealed sparse proof cannot fall through to a broader route.
                if matches!(
                    task_stop_evidence.as_deref(),
                    Some(
                        OriginalInputRecoveryEvidence::Sparse(_)
                            | OriginalInputRecoveryEvidence::Partial(_)
                            | OriginalInputRecoveryEvidence::Command(_)
                            | OriginalInputRecoveryEvidence::TaskStatus(_)
                            | OriginalInputRecoveryEvidence::ChildBinding(_)
                    )
                ) {
                    return Err(MutationOwnerError::TicketMismatch);
                }
                match facts {
                    KnownCommit::State {
                        episodes: Some(_),
                        input_evidence: Some(_),
                        ..
                    } => {}
                    KnownCommit::Metadata(metadata) => {
                        let cold_current = metadata.state.is_none()
                            && metadata.identity.as_ref().is_some_and(|identity| {
                                identity.agent_id == reservation.agent_id()
                                    && identity.incarnation == reservation.identity_incarnation()
                                    && (identity.status == AgentRegistryStatus::Active
                                        || (startup_cold
                                            && identity.status == AgentRegistryStatus::Deleting))
                            })
                            && if startup_cold {
                                reservation.owner.slot.root.load_full().as_ref().is_none_or(
                                    |root| {
                                        root.episodes.is_none()
                                            && root.input_evidence.is_none()
                                            && root.work.is_none()
                                            && root.tasks.is_none()
                                    },
                                )
                            } else {
                                publication
                                    .recovery
                                    .as_ref()
                                    .is_some_and(|recovery| recovery.original_root_absent)
                                    && reservation.owner.slot.root.load_full().is_none()
                            };
                        if startup_cold && !cold_current {
                            return Err(MutationOwnerError::TicketMismatch);
                        }
                        if !cold_current {
                            validate_retired_episode_facts(facts, reservation.owner.incarnation)?;
                        }
                    }
                    _ => return Err(MutationOwnerError::TicketMismatch),
                }
                reservation.validate_known(facts)?;
            }
            for (&index, facts) in selected.iter().zip(known) {
                let recovery = publications[index]
                    .recovery
                    .as_mut()
                    .expect("validated group");
                recovery.set_known(facts);
                recovery._original_input_evidence = task_stop_evidence.clone();
                recovery.phase = RecoveryPhase::NeedsReconcile;
            }
            Ok(WitnessUnknownResolution::Known)
        } else {
            for &index in &selected {
                publications[index].recovery = None;
            }
            drop(publications);
            for index in selected {
                reservations[index].ticket = None;
            }
            Ok(WitnessUnknownResolution::NotCommitted)
        }
    }

    fn resolve_witness_unknown(
        reservations: &mut [Self],
        key: &RecoveryKey,
        witness: &MutationWitness,
        expected_family: MutationWitnessKind,
        committed: Option<KnownCommit>,
    ) -> Result<WitnessUnknownResolution, MutationOwnerError> {
        Self::validate_order(reservations)?;
        let mut publications = reservations
            .iter()
            .map(|r| {
                r.owner
                    .slot
                    .publication
                    .lock()
                    .expect("publication poisoned")
            })
            .collect::<Vec<_>>();
        Self::validate_complete_groups(reservations, &publications)?;
        if reservations.len() != 1 {
            return Err(MutationOwnerError::TicketMismatch);
        }
        let recovery = publications[0]
            .recovery
            .as_ref()
            .ok_or(MutationOwnerError::TicketMismatch)?;
        if &recovery.key != key
            || witness.kind() != expected_family
            || recovery.group.members.len() != 1
            || recovery.known.is_some()
            || recovery
                .witness
                .as_ref()
                .is_none_or(|saved| !saved.same_attempt(witness))
            || witness.operation_id() != key.operation_id
            || witness.owner() != (key.agent_id.as_str(), key.identity_incarnation)
        {
            return Err(MutationOwnerError::TicketMismatch);
        }
        if !witness.original_attempt_finished() {
            return Err(MutationOwnerError::NeedsReconcile(key.clone()));
        }
        match committed {
            Some(
                known @ KnownCommit::State {
                    episodes: Some(_), ..
                },
            ) => {
                reservations[0].validate_known(&known)?;
                let recovery = publications[0].recovery.as_mut().expect("checked recovery");
                recovery.set_known(known);
                recovery.phase = RecoveryPhase::NeedsReconcile;
                Ok(WitnessUnknownResolution::Known)
            }
            // Receipt presence can outlive the addressed incarnation. The
            // loader must return only current identity facts, never successor
            // state through this retired owner slot.
            Some(known @ KnownCommit::Metadata(_)) => {
                self::validate_retired_episode_facts(&known, reservations[0].owner.incarnation)?;
                reservations[0].validate_known(&known)?;
                let recovery = publications[0].recovery.as_mut().expect("checked recovery");
                recovery.set_known(known);
                recovery.phase = RecoveryPhase::NeedsReconcile;
                Ok(WitnessUnknownResolution::Known)
            }
            Some(_) => Err(MutationOwnerError::TicketMismatch),
            None => {
                publications[0].recovery = None;
                drop(publications);
                reservations[0].ticket = None;
                Ok(WitnessUnknownResolution::NotCommitted)
            }
        }
    }

    pub(crate) fn definitely_not_committed_all(
        reservations: &mut [Self],
    ) -> Result<(), MutationOwnerError> {
        Self::validate_order(reservations)?;
        let mut publications = reservations
            .iter()
            .map(|r| {
                r.owner
                    .slot
                    .publication
                    .lock()
                    .expect("publication poisoned")
            })
            .collect::<Vec<_>>();
        Self::validate_complete_groups(reservations, &publications)?;
        for (reservation, publication) in reservations.iter().zip(&publications) {
            reservation.check_ticket(publication)?;
            if publication
                .recovery
                .as_ref()
                .is_some_and(|r| r.known.is_some())
            {
                return Err(MutationOwnerError::TicketMismatch);
            }
        }
        for publication in &mut publications {
            publication.recovery = None;
        }
        drop(publications);
        for reservation in reservations {
            reservation.ticket = None;
        }
        Ok(())
    }

    pub(crate) fn validate_group_known(
        reservations: &[Self],
    ) -> Result<Vec<Option<KnownCommit>>, MutationOwnerError> {
        Self::validate_order(reservations)?;
        let publications = reservations
            .iter()
            .map(|r| {
                r.owner
                    .slot
                    .publication
                    .lock()
                    .expect("publication poisoned")
            })
            .collect::<Vec<_>>();
        Self::validate_complete_groups(reservations, &publications)?;
        publications
            .iter()
            .map(|publication| match &publication.recovery {
                None => Ok(None),
                Some(recovery) => recovery
                    .known
                    .clone()
                    .map(Some)
                    .ok_or_else(|| MutationOwnerError::NeedsReconcile(recovery.key.clone())),
            })
            .collect()
    }

    pub(crate) fn reconcile_all(
        reservations: &mut [Self],
        known: Vec<Option<KnownCommit>>,
    ) -> Result<Vec<Option<Arc<PostureObservation>>>, MutationOwnerError> {
        if reservations.len() != known.len() {
            return Err(MutationOwnerError::TicketMismatch);
        }
        Self::validate_order(reservations)?;
        let mut publications = reservations
            .iter()
            .map(|r| {
                r.owner
                    .slot
                    .publication
                    .lock()
                    .expect("publication poisoned")
            })
            .collect::<Vec<_>>();
        Self::validate_complete_groups(reservations, &publications)?;
        for ((reservation, publication), facts) in
            reservations.iter().zip(&publications).zip(&known)
        {
            match (&publication.recovery, facts) {
                (None, None) => (),
                (Some(recovery), Some(facts)) => {
                    let saved = recovery
                        .known
                        .as_ref()
                        .ok_or_else(|| MutationOwnerError::NeedsReconcile(recovery.key.clone()))?;
                    if !saved.same_facts(facts) {
                        return Err(MutationOwnerError::TicketMismatch);
                    }
                    reservation.validate_known(facts)?;
                }
                _ => return Err(MutationOwnerError::TicketMismatch),
            }
        }
        let observations = reservations
            .iter()
            .zip(&mut publications)
            .zip(known)
            .map(|((reservation, publication), known)| {
                known.and_then(|known| reservation.install_known(publication, known))
            })
            .collect();
        drop(publications);
        for reservation in reservations {
            reservation.ticket = None;
        }
        Ok(observations)
    }

    pub(crate) fn publish_all_known(
        reservations: &mut [Self],
    ) -> Result<Vec<Option<Arc<PostureObservation>>>, MutationOwnerError> {
        let known = Self::validate_group_known(reservations)?;
        for (reservation, facts) in reservations.iter().zip(&known) {
            if facts.is_none()
                || reservation.ticket.as_ref()
                    != reservation.recovery()?.as_ref().map(|record| &record.key)
            {
                return Err(MutationOwnerError::TicketMismatch);
            }
        }
        Self::reconcile_all(reservations, known)
    }

    #[cfg(test)]
    pub(crate) fn publish_all(
        reservations: &mut [Self],
        states: Vec<Arc<AgentState>>,
    ) -> Result<Vec<Arc<PostureObservation>>, MutationOwnerError> {
        if reservations.len() != states.len() {
            return Err(MutationOwnerError::TicketMismatch);
        }
        let known = Self::validate_group_known(reservations)?;
        for (facts, state) in known.iter().zip(&states) {
            if facts
                .as_ref()
                .and_then(KnownCommit::state)
                .map(AsRef::as_ref)
                != Some(state.as_ref())
            {
                return Err(MutationOwnerError::TicketMismatch);
            }
        }
        Self::publish_all_known(reservations)?
            .into_iter()
            .map(|observation| observation.ok_or(MutationOwnerError::TicketMismatch))
            .collect()
    }

    pub(crate) fn recovery(&self) -> Result<Option<RecoveryRecord>, MutationOwnerError> {
        let publication = self
            .owner
            .slot
            .publication
            .lock()
            .expect("publication poisoned");
        self.owner.validate_publication(&publication)?;
        Ok(publication.recovery.clone())
    }

    pub(crate) fn begin(
        &mut self,
        operation_id: String,
    ) -> Result<RecoveryKey, MutationOwnerError> {
        Ok(Self::begin_all(std::slice::from_mut(self), operation_id)?.remove(0))
    }

    #[cfg(test)]
    pub(crate) fn definitely_not_committed(&mut self) -> Result<(), MutationOwnerError> {
        Self::definitely_not_committed_all(std::slice::from_mut(self))
    }

    pub(crate) fn committed(
        &self,
        state: Arc<AgentState>,
        tasks: Option<Arc<super::resident_task::ResidentTaskDomain>>,
    ) -> Result<(), MutationOwnerError> {
        self.committed_prepared(state, None, tasks)
    }

    /// Bootstrap and migrated families pass the exact prepared domain with the
    /// acknowledged state. Legacy callers explicitly leave it unprepared.
    pub(crate) fn committed_prepared(
        &self,
        state: Arc<AgentState>,
        episodes: Option<Arc<ResidentEpisodes>>,
        tasks: Option<Arc<super::resident_task::ResidentTaskDomain>>,
    ) -> Result<(), MutationOwnerError> {
        self.committed_prepared_with_inputs(state, episodes, None, tasks)
    }

    pub(crate) fn committed_prepared_with_inputs(
        &self,
        state: Arc<AgentState>,
        episodes: Option<Arc<ResidentEpisodes>>,
        input_evidence: Option<Arc<ResidentInputEvidence>>,
        tasks: Option<Arc<super::resident_task::ResidentTaskDomain>>,
    ) -> Result<(), MutationOwnerError> {
        self.committed_prepared_with_domains(state, episodes, input_evidence, None, tasks)
    }

    pub(crate) fn committed_prepared_with_domains(
        &self,
        state: Arc<AgentState>,
        episodes: Option<Arc<ResidentEpisodes>>,
        input_evidence: Option<Arc<ResidentInputEvidence>>,
        work: Option<Arc<super::resident_work::ResidentWorkDomain>>,
        tasks: Option<Arc<super::resident_task::ResidentTaskDomain>>,
    ) -> Result<(), MutationOwnerError> {
        let scheduling = self
            .owner
            .slot
            .root
            .load_full()
            .and_then(|root| root.scheduling.clone());
        self.committed_prepared_with_scheduling_domains(
            state,
            episodes,
            input_evidence,
            work,
            tasks,
            scheduling,
        )
    }
    pub(crate) fn committed_prepared_with_scheduling_domains(
        &self,
        state: Arc<AgentState>,
        episodes: Option<Arc<ResidentEpisodes>>,
        input_evidence: Option<Arc<ResidentInputEvidence>>,
        work: Option<Arc<super::resident_work::ResidentWorkDomain>>,
        tasks: Option<Arc<super::resident_task::ResidentTaskDomain>>,
        scheduling: Option<Arc<super::resident_scheduling::ResidentSchedulingDomain>>,
    ) -> Result<(), MutationOwnerError> {
        let mut publication = self
            .owner
            .slot
            .publication
            .lock()
            .expect("publication poisoned");
        self.check_ticket(&publication)?;
        let recovery = publication.recovery.as_ref().expect("checked ticket");
        if recovery.group.members.len() != 1 {
            return Err(MutationOwnerError::NeedsReconcile(recovery.key.clone()));
        }
        let known = KnownCommit::State {
            state,
            episodes,
            input_evidence,
            work,
            tasks,
            scheduling,
            runtime: recovery.committed_runtime.clone(),
        };
        self.validate_known(&known)?;
        if recovery
            .known
            .as_ref()
            .is_some_and(|saved| !saved.same_facts(&known))
        {
            return Err(MutationOwnerError::TicketMismatch);
        }
        publication
            .recovery
            .as_mut()
            .expect("checked ticket")
            .set_known(known);
        Ok(())
    }

    pub(crate) fn publish(
        &mut self,
        state: Arc<AgentState>,
    ) -> Result<Arc<PostureObservation>, MutationOwnerError> {
        {
            let publication = self
                .owner
                .slot
                .publication
                .lock()
                .expect("publication poisoned");
            self.check_ticket(&publication)?;
            if publication
                .recovery
                .as_ref()
                .and_then(|record| record.known.as_ref())
                .is_none()
            {
                return Err(MutationOwnerError::TicketMismatch);
            }
        }
        let known = Self::validate_group_known(std::slice::from_ref(self))?;
        if known[0]
            .as_ref()
            .and_then(KnownCommit::state)
            .map(AsRef::as_ref)
            != Some(state.as_ref())
        {
            return Err(MutationOwnerError::TicketMismatch);
        }
        Self::publish_all_known(std::slice::from_mut(self))?
            .remove(0)
            .ok_or(MutationOwnerError::TicketMismatch)
    }

    #[cfg(test)]
    pub(crate) fn reconcile(
        &mut self,
        key: &RecoveryKey,
        state: Arc<AgentState>,
    ) -> Result<Arc<PostureObservation>, MutationOwnerError> {
        let recovery = self.recovery()?.ok_or(MutationOwnerError::TicketMismatch)?;
        if &recovery.key != key || state.id != self.owner.agent_id {
            return Err(MutationOwnerError::TicketMismatch);
        }
        let known = Self::validate_group_known(std::slice::from_ref(self))?;
        if known[0]
            .as_ref()
            .and_then(KnownCommit::state)
            .map(AsRef::as_ref)
            != Some(state.as_ref())
        {
            return Err(MutationOwnerError::TicketMismatch);
        }
        Self::reconcile_all(std::slice::from_mut(self), known)?
            .remove(0)
            .ok_or(MutationOwnerError::TicketMismatch)
    }

    fn check_ticket(&self, publication: &Publication) -> Result<(), MutationOwnerError> {
        self.owner.validate_publication(publication)?;
        if self.ticket.as_ref().is_none()
            || self.ticket.as_ref() != publication.recovery.as_ref().map(|r| &r.key)
        {
            return Err(MutationOwnerError::TicketMismatch);
        }
        Ok(())
    }

    fn install_known(
        &self,
        publication: &mut Publication,
        known: KnownCommit,
    ) -> Option<Arc<PostureObservation>> {
        publication.generation = publication
            .generation
            .checked_add(1)
            .expect("publication generation exhausted");
        let episodes = match &known {
            KnownCommit::State { episodes, .. } => episodes.clone(),
            KnownCommit::Metadata(facts) => facts.identity.as_ref().and_then(|identity| {
                (identity.incarnation == self.owner.incarnation
                    && identity.status == AgentRegistryStatus::Active)
                    .then(|| {
                        self.owner
                            .slot
                            .root
                            .load_full()
                            .and_then(|root| root.episodes.clone())
                    })
                    .flatten()
            }),
        };
        let input_evidence = match &known {
            KnownCommit::State { input_evidence, .. } => input_evidence.clone(),
            KnownCommit::Metadata(facts) => facts.identity.as_ref().and_then(|identity| {
                (identity.incarnation == self.owner.incarnation
                    && identity.status == AgentRegistryStatus::Active)
                    .then(|| {
                        self.owner
                            .slot
                            .root
                            .load_full()
                            .and_then(|root| root.input_evidence.clone())
                    })
                    .flatten()
            }),
        };
        let work = match &known {
            KnownCommit::State { work, .. } => work.clone(),
            KnownCommit::Metadata(facts) => facts.identity.as_ref().and_then(|identity| {
                (identity.incarnation == self.owner.incarnation
                    && identity.status == AgentRegistryStatus::Active)
                    .then(|| {
                        self.owner
                            .slot
                            .root
                            .load_full()
                            .and_then(|root| root.work.clone())
                    })
                    .flatten()
            }),
        };
        let tasks = match &known {
            KnownCommit::State { tasks, .. } => tasks.clone(),
            KnownCommit::Metadata(facts) => facts.identity.as_ref().and_then(|identity| {
                (identity.incarnation == self.owner.incarnation
                    && identity.status == AgentRegistryStatus::Active)
                    .then(|| {
                        self.owner
                            .slot
                            .root
                            .load_full()
                            .and_then(|root| root.tasks.clone())
                    })
                    .flatten()
            }),
        };
        // Metadata-only maintenance cannot attest Scheduling completeness. It
        // keeps the durable responsibility and requires named preparation.
        let scheduling = match &known {
            KnownCommit::State { scheduling, .. } => scheduling.clone(),
            KnownCommit::Metadata(_) => None,
        };
        // Metadata maintenance and domain readback do not attest Current bodies.
        let current = match &known {
            KnownCommit::State { runtime, .. } => {
                runtime.as_ref().and_then(|facts| facts.current.clone())
            }
            KnownCommit::Metadata(_) => None,
        };
        let observation = known.state().map(|state| {
            Arc::new(PostureObservation {
                token: PublicationToken {
                    identity_incarnation: self.owner.incarnation,
                    owner_epoch: self.owner.epoch,
                    generation: publication.generation,
                },
                state: state.clone(),
                episodes,
                input_evidence,
                work,
                tasks,
                scheduling,
                current,
            })
        });
        self.owner.slot.root.store(observation.clone());
        publication.recovery = None;
        observation
    }
}

fn validate_retired_episode_facts(
    known: &KnownCommit,
    incarnation: u64,
) -> Result<(), MutationOwnerError> {
    let KnownCommit::Metadata(facts) = known else {
        return Err(MutationOwnerError::TicketMismatch);
    };
    if facts.state.is_some()
        || facts.identity.as_ref().is_some_and(|identity| {
            identity.incarnation < incarnation
                || (identity.incarnation == incarnation
                    && identity.status == AgentRegistryStatus::Active)
        })
    {
        return Err(MutationOwnerError::TicketMismatch);
    }
    Ok(())
}

impl Drop for MutationReservation {
    fn drop(&mut self) {
        let mut publication = self
            .owner
            .slot
            .publication
            .lock()
            .expect("publication poisoned");
        if publication.epoch == self.owner.epoch {
            if let Some(recovery) = publication.recovery.as_mut() {
                if self.ticket.as_ref() == Some(&recovery.key) {
                    recovery.phase = RecoveryPhase::NeedsReconcile;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "mutation_owner_tests.rs"]
mod tests;
