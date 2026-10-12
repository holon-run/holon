//! Process-local serialization for the state migration. Published posture
//! observations are deliberately not an authority for legacy state readers.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::types::{AgentIdentityRecord, AgentState};

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

#[derive(Debug, Clone)]
pub(crate) struct PostureObservation {
    pub token: PublicationToken,
    pub state: Arc<AgentState>,
}

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
                    runtime: ar,
                },
                Self::State {
                    state: b,
                    runtime: br,
                },
            ) => {
                a == b
                    && match (ar, br) {
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

#[derive(Debug, Clone)]
pub(crate) struct RecoveryRecord {
    pub key: RecoveryKey,
    pub phase: RecoveryPhase,
    // Compatibility views only; `known` distinguishes absent rows from unknown.
    pub committed_state: Option<Arc<AgentState>>,
    pub(super) committed_runtime: Option<Arc<super::state_mutation::RuntimeMutationFacts>>,
    pub known: Option<KnownCommit>,
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
    observation: Option<Arc<PostureObservation>>,
    recovery: Option<RecoveryRecord>,
    lifetime: Weak<OwnerLifetime>,
}

#[derive(Debug, Default)]
struct MutationOwnerSlot {
    reservation: Arc<AsyncMutex<()>>,
    publication: Mutex<Publication>,
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
            let mut publication = slot.publication.lock().expect("publication poisoned");
            if publication.epoch == self.epoch {
                // Keep unresolved committed facts for the next owner to recover.
                publication.observation.take()
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
        let (epoch, lifetime) = {
            let mut publication = slot.publication.lock().expect("publication poisoned");
            publication.epoch = publication
                .epoch
                .checked_add(1)
                .expect("owner epoch exhausted");
            publication.generation = 0;
            publication.observation = None;
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
            (publication.epoch, lifetime)
        };
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
        Ok(publication.observation.clone())
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
        for (publication, key) in publications.iter_mut().zip(&keys) {
            publication.recovery = Some(RecoveryRecord {
                key: key.clone(),
                phase: RecoveryPhase::Committing,
                committed_state: None,
                committed_runtime: None,
                known: None,
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

    pub(crate) fn committed(&self, state: Arc<AgentState>) -> Result<(), MutationOwnerError> {
        let mut publication = self
            .owner
            .slot
            .publication
            .lock()
            .expect("publication poisoned");
        self.check_ticket(&publication)?;
        if state.id != self.owner.agent_id {
            return Err(MutationOwnerError::TicketMismatch);
        }
        let recovery = publication.recovery.as_mut().expect("checked ticket");
        if recovery.group.members.len() != 1 {
            return Err(MutationOwnerError::NeedsReconcile(recovery.key.clone()));
        }
        match &recovery.known {
            None => (),
            Some(KnownCommit::State { state: saved, .. }) if saved == &state => (),
            _ => return Err(MutationOwnerError::TicketMismatch),
        }
        let runtime = recovery.committed_runtime.clone();
        recovery.set_known(KnownCommit::State { state, runtime });
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
        let observation = known.state().map(|state| {
            Arc::new(PostureObservation {
                token: PublicationToken {
                    identity_incarnation: self.owner.incarnation,
                    owner_epoch: self.owner.epoch,
                    generation: publication.generation,
                },
                state: state.clone(),
            })
        });
        publication.observation = observation.clone();
        publication.recovery = None;
        observation
    }
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
