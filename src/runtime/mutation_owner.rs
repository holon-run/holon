//! Process-local serialization for the state migration. Published posture
//! observations are deliberately not an authority for legacy state readers.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::types::AgentState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RecoveryKey {
    pub agent_id: String,
    pub identity_incarnation: u64,
    pub operation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PublicationToken {
    pub identity_incarnation: u64,
    pub owner_epoch: u64,
    pub generation: u64,
}

#[derive(Debug, Clone)]
pub(super) struct PostureObservation {
    pub token: PublicationToken,
    pub state: Arc<AgentState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryPhase {
    Committing,
    NeedsReconcile,
}

#[derive(Debug, Clone)]
pub(super) struct RecoveryRecord {
    pub key: RecoveryKey,
    pub phase: RecoveryPhase,
    // None means the original outcome is unknown; a current row cannot prove it.
    pub committed_state: Option<Arc<AgentState>>,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum MutationOwnerError {
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
        let slot = self
            .slots
            .lock()
            .expect("mutation registry poisoned")
            .entry((agent_id.to_owned(), incarnation))
            .or_default()
            .clone();
        // Runtime construction currently happens under the host registry lock.
        // Never wait there for a producer that may need the host to finish.
        let _reservation = slot
            .reservation
            .clone()
            .try_lock_owned()
            .map_err(|_| MutationOwnerError::Busy)?;
        let epoch = {
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
            publication.epoch
        };
        Ok(MutationOwner {
            agent_id: agent_id.to_owned(),
            incarnation,
            epoch,
            _lifetime: Arc::new(OwnerLifetime {
                epoch,
                slot: Arc::downgrade(&slot),
            }),
            slot,
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

    pub(super) fn validate_bootstrap(&self) -> Result<(), MutationOwnerError> {
        let publication = self.slot.publication.lock().expect("publication poisoned");
        self.validate_publication(&publication)?;
        if let Some(recovery) = &publication.recovery {
            if recovery.committed_state.is_none() {
                return Err(MutationOwnerError::NeedsReconcile(recovery.key.clone()));
            }
        }
        Ok(())
    }

    pub(super) async fn reserve(&self) -> Result<MutationReservation, MutationOwnerError> {
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

    pub(super) fn capture(&self) -> Result<Option<Arc<PostureObservation>>, MutationOwnerError> {
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

pub(super) struct MutationReservation {
    owner: MutationOwner,
    _guard: OwnedMutexGuard<()>,
    ticket: Option<RecoveryKey>,
}

impl MutationReservation {
    pub(super) fn recovery(&self) -> Result<Option<RecoveryRecord>, MutationOwnerError> {
        let publication = self
            .owner
            .slot
            .publication
            .lock()
            .expect("publication poisoned");
        self.owner.validate_publication(&publication)?;
        Ok(publication.recovery.clone())
    }

    pub(super) fn begin(
        &mut self,
        operation_id: String,
    ) -> Result<RecoveryKey, MutationOwnerError> {
        let mut publication = self
            .owner
            .slot
            .publication
            .lock()
            .expect("publication poisoned");
        self.owner.validate_publication(&publication)?;
        if let Some(recovery) = &publication.recovery {
            return Err(MutationOwnerError::NeedsReconcile(recovery.key.clone()));
        }
        let key = RecoveryKey {
            agent_id: self.owner.agent_id.clone(),
            identity_incarnation: self.owner.incarnation,
            operation_id,
        };
        // Closed before SQL can commit. Dropping a future/permit never reopens it.
        publication.recovery = Some(RecoveryRecord {
            key: key.clone(),
            phase: RecoveryPhase::Committing,
            committed_state: None,
        });
        self.ticket = Some(key.clone());
        Ok(key)
    }

    pub(super) fn definitely_not_committed(&mut self) -> Result<(), MutationOwnerError> {
        let mut publication = self
            .owner
            .slot
            .publication
            .lock()
            .expect("publication poisoned");
        self.check_ticket(&publication)?;
        if publication
            .recovery
            .as_ref()
            .is_some_and(|r| r.committed_state.is_some())
        {
            return Err(MutationOwnerError::TicketMismatch);
        }
        publication.recovery = None;
        self.ticket = None;
        Ok(())
    }

    pub(super) fn committed(&self, state: Arc<AgentState>) -> Result<(), MutationOwnerError> {
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
        if recovery
            .committed_state
            .as_ref()
            .is_some_and(|saved| saved.as_ref() != state.as_ref())
        {
            return Err(MutationOwnerError::TicketMismatch);
        }
        recovery.committed_state = Some(state);
        Ok(())
    }

    pub(super) fn publish(
        &mut self,
        state: Arc<AgentState>,
    ) -> Result<Arc<PostureObservation>, MutationOwnerError> {
        let mut publication = self
            .owner
            .slot
            .publication
            .lock()
            .expect("publication poisoned");
        self.check_ticket(&publication)?;
        if publication
            .recovery
            .as_ref()
            .and_then(|r| r.committed_state.as_deref())
            != Some(state.as_ref())
        {
            return Err(MutationOwnerError::TicketMismatch);
        }
        Ok(self.install(&mut publication, state))
    }

    // Recovery only installs current committed facts. The caller must retain
    // the distinction between original known/unknown operation outcomes.
    pub(super) fn reconcile(
        &mut self,
        key: &RecoveryKey,
        state: Arc<AgentState>,
    ) -> Result<Arc<PostureObservation>, MutationOwnerError> {
        let mut publication = self
            .owner
            .slot
            .publication
            .lock()
            .expect("publication poisoned");
        self.owner.validate_publication(&publication)?;
        if publication.recovery.as_ref().map(|r| &r.key) != Some(key)
            || state.id != self.owner.agent_id
        {
            return Err(MutationOwnerError::TicketMismatch);
        }
        if publication
            .recovery
            .as_ref()
            .is_some_and(|r| r.committed_state.is_none())
        {
            return Err(MutationOwnerError::NeedsReconcile(key.clone()));
        }
        Ok(self.install(&mut publication, state))
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

    fn install(
        &self,
        publication: &mut Publication,
        state: Arc<AgentState>,
    ) -> Arc<PostureObservation> {
        publication.generation = publication
            .generation
            .checked_add(1)
            .expect("publication generation exhausted");
        let observation = Arc::new(PostureObservation {
            token: PublicationToken {
                identity_incarnation: self.owner.incarnation,
                owner_epoch: self.owner.epoch,
                generation: publication.generation,
            },
            state,
        });
        publication.observation = Some(observation.clone());
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
