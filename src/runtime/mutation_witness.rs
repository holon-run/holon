//! Durable result addresses for explicitly migrated mutation families.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use anyhow::{ensure, Result};

use crate::runtime_db::episode_memory::{
    EpisodeMemoryInvocation, EpisodeMemoryKey, EpisodeMemoryTransitionCommand,
};

/// The address and command digest are frozen before the original SQL attempt.
/// Finishing the synchronous attempt proves only that it cannot still commit;
/// receipt presence or absence must be read on a fresh coherent transaction.
#[derive(Debug, Clone)]
pub(crate) struct MutationWitness {
    operation_id: String,
    family: MutationWitnessFamily,
    original_attempt_finished: Arc<AtomicBool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MutationWitnessFamily {
    EpisodeMemory {
        key: EpisodeMemoryKey,
        invocation: EpisodeMemoryInvocation,
        command_digest: String,
    },
}

impl MutationWitness {
    pub(crate) fn episode_memory(
        operation_id: String,
        command: &EpisodeMemoryTransitionCommand,
    ) -> Result<Self> {
        ensure!(
            !operation_id.trim().is_empty(),
            "mutation operation is empty"
        );
        Ok(Self {
            operation_id,
            family: MutationWitnessFamily::EpisodeMemory {
                key: command.key().clone(),
                invocation: command.invocation().clone(),
                command_digest: command.command_digest().to_owned(),
            },
            original_attempt_finished: Arc::new(AtomicBool::new(false)),
        })
    }

    pub(crate) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub(crate) fn family(&self) -> &MutationWitnessFamily {
        &self.family
    }

    pub(crate) fn owner(&self) -> (&str, u64) {
        match &self.family {
            MutationWitnessFamily::EpisodeMemory { key, .. } => {
                (&key.agent_id, key.identity_incarnation)
            }
        }
    }

    /// Called immediately when the synchronous storage invocation returns, on
    /// both success and failure, before any async installation can be cancelled.
    pub(crate) fn finish_original_attempt(&self) {
        self.original_attempt_finished
            .store(true, Ordering::Release);
    }

    pub(crate) fn original_attempt_finished(&self) -> bool {
        self.original_attempt_finished.load(Ordering::Acquire)
    }

    pub(crate) fn same_attempt(&self, other: &Self) -> bool {
        self.operation_id == other.operation_id
            && self.family == other.family
            && Arc::ptr_eq(
                &self.original_attempt_finished,
                &other.original_attempt_finished,
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn witness() -> MutationWitness {
        MutationWitness {
            operation_id: "operation-a".into(),
            family: MutationWitnessFamily::EpisodeMemory {
                key: EpisodeMemoryKey {
                    agent_id: "agent-a".into(),
                    identity_incarnation: 2,
                    actual_dispatch_turn_id: "turn-a".into(),
                },
                invocation: EpisodeMemoryInvocation {
                    message_id: "message-a".into(),
                    turn_index: 1,
                },
                command_digest: "digest-a".into(),
            },
            original_attempt_finished: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn an_address_alone_does_not_prove_the_original_attempt_finished() {
        let original = witness();
        let saved = original.clone();
        let same_address_new_attempt = witness();
        assert!(!saved.original_attempt_finished());
        assert!(saved.same_attempt(&original));
        assert!(!saved.same_attempt(&same_address_new_attempt));
        original.finish_original_attempt();
        assert!(saved.original_attempt_finished());
        assert!(!same_address_new_attempt.original_attempt_finished());
        assert_eq!(saved.owner(), ("agent-a", 2));
    }
}
