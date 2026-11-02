//! Receipt-based recovery for the Episode mutation family.
//!
//! Callers retain the complete reservation set, install saved
//! known facts, then reconcile it. This helper neither publishes nor replays SQL.

use std::sync::Arc;

use anyhow::{ensure, Context, Result};

use super::{
    mutation_owner::{IdentityMutationFacts, KnownCommit, MutationReservation},
    mutation_witness::MutationWitnessFamily,
    resident_state::ResidentEpisodes,
};
use crate::runtime_db::{
    episode_snapshot::{EpisodeMemoryBootstrapSnapshot, EpisodeMemoryLoadScope},
    RuntimeDb,
};

/// True means the addressed unknown was resolved (known, or proven not committed).
/// Known results remain fenced until the caller installs and reconciles them.
/// False leaves an ineligible/unfinished operation fenced for its own resolver.
pub(crate) fn resolve_episode_recovery(
    db: &RuntimeDb,
    reservations: &mut [MutationReservation],
    recent_limit: usize,
) -> Result<bool> {
    let mut resolved = false;
    for reservation in reservations {
        resolved |=
            resolve_single_episode_recovery(db, std::slice::from_mut(reservation), recent_limit)?;
    }
    Ok(resolved)
}

fn resolve_single_episode_recovery(
    db: &RuntimeDb,
    reservations: &mut [MutationReservation],
    recent_limit: usize,
) -> Result<bool> {
    let Some(recovery) = reservations[0].recovery()? else {
        return Ok(false);
    };
    if recovery.known.is_some() {
        return Ok(false);
    }
    let Some(witness) = recovery.witness.as_ref() else {
        return Ok(false);
    };
    if !witness.original_attempt_finished() {
        return Ok(false);
    }
    let MutationWitnessFamily::EpisodeMemory {
        key,
        invocation,
        command_digest,
    } = witness.family()
    else {
        return Ok(false);
    };
    ensure!(
        witness.operation_id() == recovery.key.operation_id
            && witness.owner()
                == (
                    reservations[0].agent_id(),
                    reservations[0].identity_incarnation(),
                )
            && key.agent_id == recovery.key.agent_id
            && key.identity_incarnation == recovery.key.identity_incarnation,
        "Episode recovery witness does not match its original owner ticket"
    );

    // A fresh Deferred transaction fixes identity, exact digest/invocation receipt,
    // current State, qualified Episode membership and coverage in one snapshot.
    // In particular, this is not a latest-State acknowledgement query.
    let scope = EpisodeMemoryLoadScope {
        agent_id: key.agent_id.clone(),
        identity_incarnation: key.identity_incarnation,
    };
    let snapshot = db.load_episode_memory_snapshot(
        &scope,
        recent_limit,
        Some((key, command_digest, invocation)),
    )?;
    let committed = match snapshot {
        EpisodeMemoryBootstrapSnapshot::Current(current) => {
            if current.receipt.is_none() {
                None
            } else {
                let state = current
                    .state
                    .context("committed Episode receipt has no current canonical AgentState")?;
                let episodes = ResidentEpisodes::bootstrap(
                    &scope.agent_id,
                    scope.identity_incarnation,
                    recent_limit,
                    !current.has_more,
                    current.episodes,
                )?;
                Some(KnownCommit::State {
                    work: None,

                    tasks: None,
                    scheduling: None,
                    state: Arc::new(state),
                    episodes: Some(Arc::new(episodes)),
                    input_evidence: None,
                    runtime: None,
                })
            }
        }
        EpisodeMemoryBootstrapSnapshot::Retired {
            requested_scope,
            current_identity,
            receipt,
        } => {
            ensure!(
                requested_scope == scope,
                "retired Episode recovery scope changed"
            );
            receipt.map(|_| {
                KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                    identity: current_identity.map(Arc::new),
                    // An old scope cannot install either its historical candidate
                    // or the successor incarnation's current State.
                    state: None,
                }))
            })
        }
    };

    // The owner independently validates the same original attempt Arc, finished
    // flag, exact ticket/epoch and complete single-owner group under its gate.
    // Absence clears only that ticket, preserving the old root/generation; it is
    // never a business Applied result. Presence only upgrades unknown to known.
    MutationReservation::resolve_episode_unknown(reservations, &recovery.key, witness, committed)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use rusqlite::params;

    use crate::{
        memory::episode_transition::{EpisodeMemoryOutcome, EpisodeMemoryPlan},
        runtime::{
            mutation_owner::{MutationOwner, MutationOwnerError},
            mutation_witness::MutationWitness,
        },
        runtime_db::episode_memory::{
            EpisodeMemoryCommit, EpisodeMemoryError, EpisodeMemoryFaultPoint,
            EpisodeMemoryInvocation, EpisodeMemoryKey, EpisodeMemoryTransitionCommand,
        },
        types::{
            AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
            AgentVisibility, ContextEpisodeRecord, EpisodeBoundaryReason, TurnRecord,
        },
    };

    struct Fixture {
        _directory: tempfile::TempDir,
        db: RuntimeDb,
        owner: MutationOwner,
        state: AgentState,
    }

    fn time() -> DateTime<Utc> {
        "2026-10-12T00:00:00Z".parse().unwrap()
    }

    fn identity(incarnation: u64) -> AgentIdentityRecord {
        let mut identity = AgentIdentityRecord::new(
            "agent-a",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        identity.incarnation = incarnation;
        identity
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let db = RuntimeDb::open_and_migrate(
                directory.path().join("runtime.sqlite"),
                directory.path().join("runtime.lock"),
            )
            .unwrap();
            db.agent_identities().upsert(&identity(1)).unwrap();
            // Exact fresh binding is fixture setup, not inferred continuity.
            // INSERT OR IGNORE also works with the staged central identity hook.
            db.transaction(|tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO agent_episode_continuity
                     (agent_id, identity_incarnation, policy_version, mode, seed_id, bound_at)
                     VALUES ('agent-a', 1, 1, 'fresh_empty', NULL, ?1)",
                    [time().to_rfc3339()],
                )?;
                Ok(())
            })
            .unwrap();
            let mut state = AgentState::new("agent-a");
            state.turn_index = 1;
            state.current_turn_id = Some("dispatch-1".into());
            db.agent_states().upsert_for_fixture(&state).unwrap();
            let mut turn = TurnRecord::new("agent-a", "dispatch-1", 1);
            turn.created_at = time();
            turn.input_message_ids.push("message-1".into());
            db.turn_records().upsert(&turn).unwrap();
            let owner = MutationOwner::standalone("agent-a", 1);
            let mut reservation = owner.try_reserve().unwrap();
            reservation.begin("initial-root".into()).unwrap();
            let state_arc = Arc::new(state.clone());
            reservation
                .committed_prepared(
                    state_arc.clone(),
                    Some(Arc::new(
                        ResidentEpisodes::bootstrap("agent-a", 1, 12, true, vec![]).unwrap(),
                    )),
                    None,
                )
                .unwrap();
            reservation.publish(state_arc).unwrap();
            drop(reservation);
            Self {
                _directory: directory,
                db,
                owner,
                state,
            }
        }

        fn command(&self, finalize: bool, business_change: bool) -> EpisodeMemoryTransitionCommand {
            let mut candidate = self.state.clone();
            if business_change {
                candidate.pending = 42;
            }
            if finalize {
                candidate.working_memory.archived_episode_count = 1;
            }
            let episode = finalize.then(|| {
                serde_json::from_value::<ContextEpisodeRecord>(serde_json::json!({
                    "id": "episode-a", "agent_id": "agent-a", "created_at": time(),
                    "finalized_at": time(), "start_turn_index": 1, "end_turn_index": 1,
                    "start_message_count": 0, "end_message_count": 1,
                    "boundary_reason": "wait_boundary", "source_turn_ids": ["dispatch-1"],
                    "source_refs": ["turn:dispatch-1"],
                }))
                .unwrap()
            });
            EpisodeMemoryTransitionCommand::new(
                EpisodeMemoryKey {
                    agent_id: "agent-a".into(),
                    identity_incarnation: 1,
                    actual_dispatch_turn_id: "dispatch-1".into(),
                },
                EpisodeMemoryInvocation {
                    message_id: "message-1".into(),
                    turn_index: 1,
                },
                self.state.clone(),
                EpisodeMemoryPlan {
                    candidate,
                    audit_events: vec![],
                    outcome: EpisodeMemoryOutcome {
                        episode_memory_changed: finalize,
                        business_fields_changed: business_change,
                        boundary_reason: finalize.then_some(EpisodeBoundaryReason::WaitBoundary),
                        finalized_episode_id: episode.as_ref().map(|episode| episode.id.clone()),
                        previous_builder_id: None,
                        active_builder_id: None,
                        compression_epoch_before: 0,
                        compression_epoch_after: 0,
                        archived_episode_count_before: 0,
                        archived_episode_count_after: usize::from(finalize),
                    },
                    finalized_episode: episode,
                },
                time(),
            )
            .unwrap()
        }

        fn begin(
            &self,
            command: &EpisodeMemoryTransitionCommand,
        ) -> (MutationReservation, MutationWitness) {
            let witness =
                MutationWitness::episode_memory("episode-operation".into(), command).unwrap();
            let mut reservation = self.owner.try_reserve().unwrap();
            MutationReservation::begin_all_with_witness(
                std::slice::from_mut(&mut reservation),
                "episode-operation".into(),
                witness.clone(),
            )
            .unwrap();
            (reservation, witness)
        }

        fn receipt_count(&self) -> i64 {
            self.db
                .connection()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM episode_memory_results", [], |row| {
                    row.get(0)
                })
                .unwrap()
        }
    }

    fn reconcile(reservation: &mut MutationReservation) {
        let facts =
            MutationReservation::validate_group_known(std::slice::from_ref(reservation)).unwrap();
        MutationReservation::reconcile_all(std::slice::from_mut(reservation), facts).unwrap();
    }

    #[test]
    fn receipt_recovery_in_a_larger_reservation_batch_preserves_unrelated_owner() {
        let fixture = Fixture::new();
        let command = fixture.command(false, true);
        let (reservation, witness) = fixture.begin(&command);
        fixture.db.commit_episode_memory(&command).unwrap();
        witness.finish_original_attempt();
        let unrelated = MutationOwner::standalone("agent-b", 1);
        let mut reservations = vec![reservation, unrelated.try_reserve().unwrap()];
        assert!(resolve_episode_recovery(&fixture.db, &mut reservations, 12).unwrap());
        let known = MutationReservation::validate_group_known(&reservations).unwrap();
        assert!(known[0].is_some());
        assert!(known[1].is_none());
        MutationReservation::reconcile_all(&mut reservations, known).unwrap();
        assert!(fixture.owner.capture().unwrap().is_some());
        assert!(unrelated.capture().unwrap().is_none());
    }

    #[test]
    fn a_real_multi_owner_unknown_group_is_not_partially_reconciled() {
        let fixture = Fixture::new();
        let unrelated = MutationOwner::standalone("agent-b", 1);
        let mut reservations = vec![
            fixture.owner.try_reserve().unwrap(),
            unrelated.try_reserve().unwrap(),
        ];
        MutationReservation::begin_all(&mut reservations, "multi-owner".into()).unwrap();
        assert!(!resolve_episode_recovery(&fixture.db, &mut reservations, 12).unwrap());
        for reservation in &reservations {
            assert!(reservation.recovery().unwrap().unwrap().known.is_none());
        }
        assert!(matches!(
            MutationReservation::validate_group_known(&reservations),
            Err(MutationOwnerError::NeedsReconcile(_))
        ));
    }

    #[test]
    fn lost_ack_recovers_current_root_and_qualified_episodes_without_replaying_original_state() {
        let fixture = Fixture::new();
        let command = fixture.command(true, true);
        let (mut reservation, witness) = fixture.begin(&command);
        let lost_ack = command
            .clone()
            .with_fault(EpisodeMemoryFaultPoint::AfterCommitBeforeAck);
        let error = fixture.db.commit_episode_memory(&lost_ack).unwrap_err();
        assert_eq!(
            crate::runtime_db::persistence_certainty(&error),
            Some(crate::runtime_db::PersistenceCertainty::Indeterminate)
        );
        witness.finish_original_attempt();
        // A later acknowledged canonical write must not be replaced by the old
        // receipt's pending=42 candidate during recovery.
        let mut current = command.candidate().clone();
        current.pending = 99;
        fixture
            .db
            .agent_states()
            .upsert_for_fixture(&current)
            .unwrap();
        // Same textual agent ID is insufficient qualification, even when the
        // unadopted legacy record sorts ahead of the receipt-backed episode.
        let legacy: ContextEpisodeRecord = serde_json::from_value(serde_json::json!({
            "id": "unadopted-legacy", "agent_id": "agent-a", "created_at": time(),
            "finalized_at": time() + chrono::Duration::seconds(10),
            "start_turn_index": 1, "end_turn_index": 1,
            "start_message_count": 0, "end_message_count": 1,
            "boundary_reason": "wait_boundary",
        }))
        .unwrap();
        fixture.db.context_episodes().upsert(&legacy).unwrap();
        assert!(
            resolve_episode_recovery(&fixture.db, std::slice::from_mut(&mut reservation), 12)
                .unwrap()
        );
        assert!(matches!(
            fixture.owner.capture(),
            Err(MutationOwnerError::NeedsReconcile(_))
        ));
        let recovery = reservation.recovery().unwrap().unwrap();
        let Some(KnownCommit::State {
            state,
            episodes: Some(episodes),
            runtime,
            ..
        }) = recovery.known
        else {
            panic!("receipt must promote an exact current prepared root");
        };
        assert_eq!(state.pending, 99);
        let selected = episodes.selected(12).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "episode-a");
        assert!(runtime.is_none());
        assert_eq!(fixture.receipt_count(), 1);
        reconcile(&mut reservation);
        assert_eq!(fixture.owner.capture().unwrap().unwrap().state.pending, 99);
        assert!(matches!(
            fixture.db.commit_episode_memory(&command).unwrap(),
            EpisodeMemoryCommit::Replayed { .. }
        ));
        assert_eq!(
            fixture
                .db
                .agent_states()
                .latest("agent-a")
                .unwrap()
                .unwrap(),
            current
        );
    }

    #[test]
    fn rollback_after_receipt_write_proves_absence_without_publishing_a_candidate() {
        let fixture = Fixture::new();
        let before = fixture.owner.capture().unwrap().unwrap();
        let command = fixture.command(true, true);
        let (mut reservation, witness) = fixture.begin(&command);
        assert!(fixture
            .db
            .commit_episode_memory(&command.with_fault(EpisodeMemoryFaultPoint::AfterReceiptWrite))
            .is_err());
        witness.finish_original_attempt();
        assert!(
            resolve_episode_recovery(&fixture.db, std::slice::from_mut(&mut reservation), 12)
                .unwrap()
        );
        assert!(reservation.recovery().unwrap().is_none());
        assert!(Arc::ptr_eq(
            &before,
            &fixture.owner.capture().unwrap().unwrap()
        ));
        assert_eq!(fixture.receipt_count(), 0);
        assert_eq!(
            fixture
                .db
                .agent_states()
                .latest("agent-a")
                .unwrap()
                .unwrap(),
            fixture.state
        );
    }

    #[test]
    fn unfinished_original_attempt_cannot_use_even_a_real_positive_receipt() {
        let fixture = Fixture::new();
        let command = fixture.command(false, true);
        let (mut reservation, witness) = fixture.begin(&command);
        fixture.db.commit_episode_memory(&command).unwrap();
        assert!(
            !resolve_episode_recovery(&fixture.db, std::slice::from_mut(&mut reservation), 12)
                .unwrap()
        );
        assert!(reservation.recovery().unwrap().unwrap().known.is_none());
        assert!(matches!(
            fixture.owner.capture(),
            Err(MutationOwnerError::NeedsReconcile(_))
        ));
        witness.finish_original_attempt();
        assert!(
            resolve_episode_recovery(&fixture.db, std::slice::from_mut(&mut reservation), 12)
                .unwrap()
        );
    }

    #[test]
    fn business_noop_has_a_positive_receipt_and_never_becomes_absence() {
        let fixture = Fixture::new();
        let command = fixture.command(false, false);
        let (mut reservation, witness) = fixture.begin(&command);
        assert!(fixture
            .db
            .commit_episode_memory(
                &command.with_fault(EpisodeMemoryFaultPoint::AfterCommitBeforeAck)
            )
            .is_err());
        witness.finish_original_attempt();
        assert!(
            resolve_episode_recovery(&fixture.db, std::slice::from_mut(&mut reservation), 0)
                .unwrap()
        );
        let recovery = reservation.recovery().unwrap().unwrap();
        assert!(matches!(recovery.known, Some(KnownCommit::State { .. })));
        assert_eq!(fixture.receipt_count(), 1);
        reconcile(&mut reservation);
        assert_eq!(
            *fixture.owner.capture().unwrap().unwrap().state,
            fixture.state
        );
    }

    #[test]
    fn retired_receipt_recovers_metadata_without_installing_successor_state() {
        let fixture = Fixture::new();
        let command = fixture.command(false, true);
        let (mut reservation, witness) = fixture.begin(&command);
        fixture.db.commit_episode_memory(&command).unwrap();
        witness.finish_original_attempt();
        let successor = identity(2);
        fixture.db.agent_identities().upsert(&successor).unwrap();
        let mut successor_state = AgentState::new("agent-a");
        successor_state.pending = 777;
        fixture
            .db
            .transaction(|tx| {
                // Recreation removes the old incarnation's State before its
                // successor starts again at turn zero.
                tx.execute("DELETE FROM agent_states WHERE agent_id = 'agent-a'", [])?;
                crate::runtime_db::evidence::upsert_agent_state_tx(tx, &successor_state)
            })
            .unwrap();
        assert_eq!(
            fixture
                .db
                .agent_states()
                .latest("agent-a")
                .unwrap()
                .unwrap(),
            successor_state
        );
        assert!(
            resolve_episode_recovery(&fixture.db, std::slice::from_mut(&mut reservation), 12)
                .unwrap()
        );
        let recovery = reservation.recovery().unwrap().unwrap();
        let Some(KnownCommit::Metadata(facts)) = recovery.known else {
            panic!("retired receipt needs metadata");
        };
        assert_eq!(facts.identity.as_ref().unwrap().incarnation, 2);
        assert!(facts.state.is_none());
        reconcile(&mut reservation);
        assert!(fixture.owner.capture().unwrap().is_none());
        assert_eq!(
            fixture
                .db
                .agent_states()
                .latest("agent-a")
                .unwrap()
                .unwrap(),
            successor_state
        );
        assert_eq!(fixture.receipt_count(), 1);
    }

    #[test]
    fn conflicting_digest_and_corrupt_receipt_leave_the_original_gate_unknown() {
        let fixture = Fixture::new();
        let command = fixture.command(false, true);
        fixture.db.commit_episode_memory(&command).unwrap();
        let conflicting = fixture.command(false, false);
        let (mut reservation, witness) = fixture.begin(&conflicting);
        witness.finish_original_attempt();
        let error =
            resolve_episode_recovery(&fixture.db, std::slice::from_mut(&mut reservation), 12)
                .unwrap_err();
        assert_eq!(
            error.downcast_ref::<EpisodeMemoryError>(),
            Some(&EpisodeMemoryError::CommandConflict)
        );
        assert!(reservation.recovery().unwrap().unwrap().known.is_none());
        assert!(matches!(
            fixture.owner.capture(),
            Err(MutationOwnerError::NeedsReconcile(_))
        ));
        drop(reservation);

        let fixture = Fixture::new();
        let command = fixture.command(false, true);
        let (mut reservation, witness) = fixture.begin(&command);
        fixture.db.commit_episode_memory(&command).unwrap();
        witness.finish_original_attempt();
        fixture
            .db
            .transaction(|tx| {
                tx.execute(
                    "UPDATE episode_memory_results SET result_json = ?1",
                    params!["not-json"],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(
            resolve_episode_recovery(&fixture.db, std::slice::from_mut(&mut reservation), 12)
                .is_err()
        );
        assert!(reservation.recovery().unwrap().unwrap().known.is_none());
        assert!(matches!(
            fixture.owner.capture(),
            Err(MutationOwnerError::NeedsReconcile(_))
        ));
    }

    #[test]
    fn missing_current_state_or_unwitnessed_family_is_not_an_acknowledgement() {
        let fixture = Fixture::new();
        let command = fixture.command(false, true);
        let (mut reservation, witness) = fixture.begin(&command);
        fixture.db.commit_episode_memory(&command).unwrap();
        witness.finish_original_attempt();
        fixture
            .db
            .transaction(|tx| {
                tx.execute("DELETE FROM agent_states WHERE agent_id = 'agent-a'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(
            resolve_episode_recovery(&fixture.db, std::slice::from_mut(&mut reservation), 12)
                .is_err()
        );
        assert!(reservation.recovery().unwrap().unwrap().known.is_none());
        drop(reservation);

        let fixture = Fixture::new();
        let mut reservation = fixture.owner.try_reserve().unwrap();
        reservation.begin("legacy-unknown".into()).unwrap();
        assert!(
            !resolve_episode_recovery(&fixture.db, std::slice::from_mut(&mut reservation), 12)
                .unwrap()
        );
        assert!(matches!(
            fixture.owner.capture(),
            Err(MutationOwnerError::NeedsReconcile(_))
        ));
    }
}
