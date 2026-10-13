//! Process-local whole-group recovery validation, independent of the SQL
//! loader's fresh receipt/absence and coherent-domain proof.

use super::*;
use crate::runtime::{
    input_evidence_budget::{InputEvidenceBudgetLedger, PendingAdmission},
    resident_input_evidence::{InputEvidenceCoverage, InputEvidenceScope, ResidentInputEvidence},
};
use crate::runtime_db::input_mutation_receipts::{
    InputMutationFamily, OriginalInputMutationAttempt, PreparedInputMutation,
};

fn scope(agent_id: &str, incarnation: u64) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: agent_id.into(),
        identity_incarnation: incarnation,
    }
}

fn input_witness(operation: &str, scopes: Vec<InputEvidenceScope>) -> MutationWitness {
    let prepared = PreparedInputMutation::new(
        InputMutationFamily::Queue,
        operation.into(),
        scopes,
        "a".repeat(64),
        chrono::DateTime::from_timestamp(1, 0).unwrap(),
    )
    .unwrap();
    MutationWitness::input_mutation(
        operation.into(),
        OriginalInputMutationAttempt::new(prepared),
        Vec::new(),
    )
    .unwrap()
}

fn inputs(agent_id: &str, incarnation: u64) -> Arc<ResidentInputEvidence> {
    Arc::new(
        ResidentInputEvidence::bootstrap(
            scope(agent_id, incarnation),
            Vec::new(),
            InputEvidenceCoverage {
                selected_completion_brief_ids: Default::default(),
                completion_source_observations: Default::default(),
                recovery_source_absences: Default::default(),
                recovery_lineage_messages: Default::default(),
                execution: Default::default(),
                windows: Vec::new(),
                mandatory_roots: Default::default(),
                turn_transcript_keys: Default::default(),
                message_turn_keys: Default::default(),
                unavailable: Default::default(),
                record_limit: 32,
                byte_limit: 1024,
            },
        )
        .unwrap(),
    )
}

fn ready_facts(agent_id: &str, incarnation: u64, pending: usize) -> KnownCommit {
    KnownCommit::State {
        work: None,
        state: scoped_state(agent_id, pending),
        episodes: Some(prepared_episodes(agent_id, incarnation)),
        input_evidence: Some(inputs(agent_id, incarnation)),
        runtime: None,
    }
}

async fn old_root(owner: &MutationOwner, pending: usize) -> Arc<PostureObservation> {
    install_test_observation(owner, "old-root", scoped_state(owner.agent_id(), pending)).await
}

fn assert_group_unknown(permits: &[MutationReservation]) {
    assert!(permits.iter().all(|permit| {
        let recovery = permit.recovery().unwrap().unwrap();
        recovery.known.is_none() && recovery.committed_state.is_none()
    }));
}

#[tokio::test]
async fn reacquired_input_absence_preserves_old_roots_and_original_generations() {
    let registry = MutationOwnerRegistry::default();
    let a = registry.claim("agent-a", 3).unwrap();
    let b = registry.claim("agent-b", 8).unwrap();
    let old_a = old_root(&a, 1).await;
    let old_b = old_root(&b, 2).await;
    let witness = input_witness(
        "original-group",
        vec![scope("agent-a", 3), scope("agent-b", 8)],
    );
    let keys = {
        let mut permits = MutationOwner::reserve_all(vec![b.clone(), a.clone()])
            .await
            .unwrap();
        let keys = MutationReservation::begin_all_with_witness(
            &mut permits,
            "original-group".into(),
            witness.clone(),
        )
        .unwrap();
        witness.finish_original_attempt();
        keys
    };
    // This models a fresh owner entry after the original API future vanished.
    let mut permits = MutationOwner::reserve_all(vec![a.clone()]).await.unwrap();
    assert_eq!(
        permits.len(),
        2,
        "pending recovery expands the original whole group"
    );
    assert!(permits.iter().all(|permit| permit.ticket.is_none()));
    MutationReservation::validate_input_unknown(&permits, &keys[0], &witness).unwrap();
    assert_needs_reconcile(&a, &keys[0]);
    assert_needs_reconcile(&b, &keys[1]);
    assert_eq!(
        MutationReservation::resolve_input_unknown(&mut permits, &keys[0], &witness, None).unwrap(),
        WitnessUnknownResolution::NotCommitted
    );
    assert!(permits
        .iter()
        .all(|permit| permit.recovery().unwrap().is_none()));
    let current_a = a.capture().unwrap().unwrap();
    let current_b = b.capture().unwrap().unwrap();
    assert!(Arc::ptr_eq(&old_a, &current_a));
    assert!(Arc::ptr_eq(&old_b, &current_b));
    assert_eq!(current_a.token, old_a.token);
    assert_eq!(current_b.token, old_b.token);
}

#[tokio::test]
async fn input_absence_rejects_partial_unfinished_reconstructed_and_foreign_attempts() {
    let registry = MutationOwnerRegistry::default();
    let a = registry.claim("agent-a", 3).unwrap();
    let b = registry.claim("agent-b", 8).unwrap();
    let scopes = vec![scope("agent-a", 3), scope("agent-b", 8)];
    let witness = input_witness("original-group", scopes.clone());
    let keys = {
        let mut permits = MutationOwner::reserve_all(vec![a.clone(), b.clone()])
            .await
            .unwrap();
        MutationReservation::begin_all_with_witness(
            &mut permits,
            "original-group".into(),
            witness.clone(),
        )
        .unwrap()
    };
    let mut permits = MutationOwner::reserve_all(vec![a.clone()]).await.unwrap();
    assert!(matches!(
        MutationReservation::validate_input_unknown(&permits[..1], &keys[0], &witness),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert!(matches!(
        MutationReservation::resolve_input_unknown(&mut permits[..1], &keys[0], &witness, None),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert!(matches!(
        MutationReservation::validate_input_unknown(&permits, &keys[0], &witness),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert_group_unknown(&permits);

    let reconstructed = input_witness("original-group", scopes);
    reconstructed.finish_original_attempt();
    assert!(matches!(
        MutationReservation::validate_input_unknown(&permits, &keys[0], &reconstructed),
        Err(MutationOwnerError::TicketMismatch)
    ));
    witness.finish_original_attempt();
    for invalid in [
        input_witness("different", vec![scope("agent-a", 3), scope("agent-b", 8)]),
        input_witness("original-group", vec![scope("agent-a", 3)]),
        input_witness(
            "original-group",
            vec![scope("agent-a", 3), scope("agent-b", 9)],
        ),
        episode_witness("original-group", "agent-a", 3),
    ] {
        invalid.finish_original_attempt();
        assert!(matches!(
            MutationReservation::validate_input_unknown(&permits, &keys[0], &invalid),
            Err(MutationOwnerError::TicketMismatch)
        ));
        assert_group_unknown(&permits);
    }
    let mut wrong_key = keys[0].clone();
    wrong_key.identity_incarnation += 1;
    assert!(matches!(
        MutationReservation::validate_input_unknown(&permits, &wrong_key, &witness),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert_needs_reconcile(&a, &keys[0]);
    assert_needs_reconcile(&b, &keys[1]);
}

#[tokio::test]
async fn input_presence_validates_every_domain_before_promoting_any_group_member() {
    let registry = MutationOwnerRegistry::default();
    let a = registry.claim("agent-a", 3).unwrap();
    let b = registry.claim("agent-b", 8).unwrap();
    let witness = input_witness(
        "original-group",
        vec![scope("agent-a", 3), scope("agent-b", 8)],
    );
    let keys = {
        let mut permits = MutationOwner::reserve_all(vec![a.clone(), b.clone()])
            .await
            .unwrap();
        let keys = MutationReservation::begin_all_with_witness(
            &mut permits,
            "original-group".into(),
            witness.clone(),
        )
        .unwrap();
        witness.finish_original_attempt();
        keys
    };
    let mut permits = MutationOwner::reserve_all(vec![a.clone()]).await.unwrap();
    for invalid_b in [
        state_facts(scoped_state("agent-b", 4)),
        KnownCommit::State {
            work: None,
            state: scoped_state("agent-b", 4),
            episodes: Some(prepared_episodes("agent-b", 8)),
            input_evidence: Some(inputs("agent-b", 9)),
            runtime: None,
        },
        KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
            identity: None,
            state: Some(scoped_state("agent-b", 4)),
        })),
    ] {
        assert!(matches!(
            MutationReservation::resolve_input_unknown(
                &mut permits,
                &keys[0],
                &witness,
                Some(vec![ready_facts("agent-a", 3, 9), invalid_b])
            ),
            Err(MutationOwnerError::TicketMismatch)
        ));
        assert_group_unknown(&permits);
        assert_needs_reconcile(&a, &keys[0]);
        assert_needs_reconcile(&b, &keys[1]);
    }
    assert!(matches!(
        MutationReservation::resolve_input_unknown(
            &mut permits,
            &keys[0],
            &witness,
            Some(vec![ready_facts("agent-a", 3, 9)])
        ),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert_group_unknown(&permits);
}

#[tokio::test]
async fn input_presence_retains_current_domains_and_retired_metadata_until_whole_group_reconcile() {
    let registry = MutationOwnerRegistry::default();
    let a = registry.claim("agent-a", 3).unwrap();
    let b = registry.claim("agent-b", 8).unwrap();
    let old_a = old_root(&a, 1).await;
    let _old_b = old_root(&b, 2).await;
    let witness = input_witness(
        "original-group",
        vec![scope("agent-a", 3), scope("agent-b", 8)],
    );
    let keys = {
        let mut permits = MutationOwner::reserve_all(vec![a.clone(), b.clone()])
            .await
            .unwrap();
        let keys = MutationReservation::begin_all_with_witness(
            &mut permits,
            "original-group".into(),
            witness.clone(),
        )
        .unwrap();
        witness.finish_original_attempt();
        keys
    };
    let current = ready_facts("agent-a", 3, 91);
    let (exact_state, exact_episodes, exact_inputs) = match &current {
        KnownCommit::State {
            state,
            episodes,
            input_evidence,
            ..
        } => (
            state.clone(),
            episodes.clone().unwrap(),
            input_evidence.clone().unwrap(),
        ),
        _ => unreachable!(),
    };
    {
        let mut permits = MutationOwner::reserve_all(vec![b.clone()]).await.unwrap();
        assert_eq!(
            MutationReservation::resolve_input_unknown(
                &mut permits,
                &keys[0],
                &witness,
                Some(vec![current, absent_metadata()])
            )
            .unwrap(),
            WitnessUnknownResolution::Known
        );
        assert_needs_reconcile(&a, &keys[0]);
        assert_needs_reconcile(&b, &keys[1]);
        assert!(
            matches!(
                MutationReservation::validate_input_unknown(&permits, &keys[0], &witness),
                Err(MutationOwnerError::TicketMismatch)
            ),
            "known facts cannot become negative absence"
        );
        assert!(matches!(
            MutationReservation::validate_group_known(&permits[..1]),
            Err(MutationOwnerError::NeedsReconcile(_))
        ));
    }
    // Cancelling the recovering caller cannot discard these saved exact Arcs.
    let mut permits = MutationOwner::reserve_all(vec![a.clone()]).await.unwrap();
    let known = MutationReservation::validate_group_known(&permits).unwrap();
    let roots = MutationReservation::reconcile_all(&mut permits, known).unwrap();
    let restored = roots[0].as_ref().unwrap();
    assert!(Arc::ptr_eq(&restored.state, &exact_state));
    assert!(Arc::ptr_eq(
        restored.episodes.as_ref().unwrap(),
        &exact_episodes
    ));
    assert!(Arc::ptr_eq(
        restored.input_evidence.as_ref().unwrap(),
        &exact_inputs
    ));
    assert_eq!(restored.token.generation, old_a.token.generation + 1);
    assert!(
        roots[1].is_none(),
        "retired metadata never installs a successor State"
    );
    assert!(b.capture().unwrap().is_none());
    assert!(permits
        .iter()
        .all(|permit| permit.recovery().unwrap().is_none()));
}

#[tokio::test]
async fn input_resolution_rejects_wrong_epoch_and_allows_original_attempt_on_successor_owner() {
    let registry = MutationOwnerRegistry::default();
    let old = registry.claim("agent-a", 3).unwrap();
    let witness = input_witness("original", vec![scope("agent-a", 3)]);
    let key = {
        let mut permits = vec![old.reserve().await.unwrap()];
        let key = MutationReservation::begin_all_with_witness(
            &mut permits,
            "original".into(),
            witness.clone(),
        )
        .unwrap()
        .remove(0);
        witness.finish_original_attempt();
        key
    };
    let successor = registry.claim("agent-a", 3).unwrap();
    assert!(matches!(old.capture(), Err(MutationOwnerError::Replaced)));
    let mut permits = vec![successor.reserve().await.unwrap()];
    assert!(permits[0].ticket.is_none());
    let valid_owner = std::mem::replace(&mut permits[0].owner, old.clone());
    assert!(matches!(
        MutationReservation::validate_input_unknown(&permits, &key, &witness),
        Err(MutationOwnerError::Replaced)
    ));
    assert!(matches!(
        MutationReservation::resolve_input_unknown(&mut permits, &key, &witness, None),
        Err(MutationOwnerError::Replaced)
    ));
    permits[0].owner = valid_owner;
    MutationReservation::validate_input_unknown(&permits, &key, &witness).unwrap();
    assert_eq!(
        MutationReservation::resolve_input_unknown(
            &mut permits,
            &key,
            &witness,
            Some(vec![ready_facts("agent-a", 3, 4)])
        )
        .unwrap(),
        WitnessUnknownResolution::Known
    );
    let known = MutationReservation::validate_group_known(&permits).unwrap();
    let root = MutationReservation::reconcile_all(&mut permits, known)
        .unwrap()
        .remove(0)
        .unwrap();
    assert_eq!(root.token.owner_epoch, successor.owner_epoch());
    assert_eq!(root.state.pending, 4);
}

#[tokio::test]
async fn pending_release_failure_keeps_the_validated_input_group_fenced() {
    let registry = MutationOwnerRegistry::default();
    let a = registry.claim("agent-a", 3).unwrap();
    let b = registry.claim("agent-b", 8).unwrap();
    let ledger =
        InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default()).unwrap();
    let mut tickets = Vec::new();
    for (agent, inc) in [("agent-a", 3), ("agent-b", 8)] {
        let PendingAdmission::Reserved(ticket) = ledger
            .reserve_pending(&scope(agent, inc), "pending", 10)
            .unwrap()
        else {
            panic!("fresh fixture ticket must be reserved");
        };
        tickets.push((ledger.clone(), ticket));
    }
    let prepared = PreparedInputMutation::new(
        InputMutationFamily::Queue,
        "original-group".into(),
        vec![scope("agent-a", 3), scope("agent-b", 8)],
        "a".repeat(64),
        chrono::DateTime::from_timestamp(1, 0).unwrap(),
    )
    .unwrap();
    let witness = MutationWitness::input_mutation(
        "original-group".into(),
        OriginalInputMutationAttempt::new(prepared),
        tickets,
    )
    .unwrap();
    let keys = {
        let mut permits = MutationOwner::reserve_all(vec![a.clone(), b.clone()])
            .await
            .unwrap();
        let keys = MutationReservation::begin_all_with_witness(
            &mut permits,
            "original-group".into(),
            witness.clone(),
        )
        .unwrap();
        witness.mark_pending_started().unwrap();
        witness.mark_pending_unknown().unwrap();
        witness.finish_original_attempt();
        keys
    };
    let permits = MutationOwner::reserve_all(vec![a.clone()]).await.unwrap();
    MutationReservation::validate_input_unknown(&permits, &keys[0], &witness).unwrap();
    // A conflicting logical resolution is a fallible ledger operation. It must
    // happen before the owner's final clear, and batch validation releases none.
    witness.mark_pending_accepted().unwrap();
    assert!(witness.release_pending_not_committed(true).is_err());
    assert_eq!(ledger.report().unwrap().pending.count, 2);
    assert_group_unknown(&permits);
    assert_needs_reconcile(&a, &keys[0]);
    assert_needs_reconcile(&b, &keys[1]);
}

#[tokio::test]
async fn validated_whole_group_absence_releases_original_pending_tickets_before_clear() {
    let registry = MutationOwnerRegistry::default();
    let a = registry.claim("agent-a", 3).unwrap();
    let b = registry.claim("agent-b", 8).unwrap();
    let ledger =
        InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default()).unwrap();
    let mut tickets = Vec::new();
    for (agent, inc) in [("agent-a", 3), ("agent-b", 8)] {
        let PendingAdmission::Reserved(ticket) = ledger
            .reserve_pending(&scope(agent, inc), "pending", 10)
            .unwrap()
        else {
            panic!("fresh fixture ticket must be reserved");
        };
        tickets.push((ledger.clone(), ticket));
    }
    let prepared = PreparedInputMutation::new(
        InputMutationFamily::Queue,
        "original-group".into(),
        vec![scope("agent-a", 3), scope("agent-b", 8)],
        "a".repeat(64),
        chrono::DateTime::from_timestamp(1, 0).unwrap(),
    )
    .unwrap();
    let witness = MutationWitness::input_mutation(
        "original-group".into(),
        OriginalInputMutationAttempt::new(prepared),
        tickets,
    )
    .unwrap();
    let keys = {
        let mut permits = MutationOwner::reserve_all(vec![a.clone(), b.clone()])
            .await
            .unwrap();
        let keys = MutationReservation::begin_all_with_witness(
            &mut permits,
            "original-group".into(),
            witness.clone(),
        )
        .unwrap();
        witness.mark_pending_started().unwrap();
        witness.mark_pending_unknown().unwrap();
        witness.finish_original_attempt();
        keys
    };
    let mut permits = MutationOwner::reserve_all(vec![a.clone()]).await.unwrap();
    assert!(
        MutationReservation::validate_input_unknown(&permits[..1], &keys[0], &witness).is_err()
    );
    assert_eq!(ledger.report().unwrap().pending.count, 2);
    assert_eq!(ledger.report().unwrap().pending.unresolved_count, 2);
    MutationReservation::validate_input_unknown(&permits, &keys[0], &witness).unwrap();
    // The real caller has separately obtained fresh same-TX receipt absence.
    // There is no await or owner release in this validate/release/clear span.
    witness.release_pending_not_committed(true).unwrap();
    assert_eq!(ledger.report().unwrap().pending.count, 0);
    assert_needs_reconcile(&a, &keys[0]);
    assert_needs_reconcile(&b, &keys[1]);
    assert_eq!(
        MutationReservation::resolve_input_unknown(&mut permits, &keys[0], &witness, None).unwrap(),
        WitnessUnknownResolution::NotCommitted
    );
    assert!(permits
        .iter()
        .all(|permit| permit.recovery().unwrap().is_none()));
}

fn cold_current_metadata(agent_id: &str, incarnation: u64) -> KnownCommit {
    let mut identity = episode_identity(incarnation, AgentRegistryStatus::Active)
        .as_ref()
        .clone();
    identity.agent_id = agent_id.to_owned();
    KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
        identity: Some(Arc::new(identity)),
        state: None,
    }))
}

#[tokio::test]
async fn input_group_recovers_active_cold_identity_without_inventing_state() {
    let registry = MutationOwnerRegistry::default();
    let a = registry.claim("agent-a", 3).unwrap();
    let b = registry.claim("agent-b", 8).unwrap();
    let _old_a = old_root(&a, 1).await;
    assert!(b.capture().unwrap().is_none());
    let witness = input_witness(
        "original-group",
        vec![scope("agent-a", 3), scope("agent-b", 8)],
    );
    let keys = {
        let mut permits = MutationOwner::reserve_all(vec![a.clone(), b.clone()])
            .await
            .unwrap();
        let keys = MutationReservation::begin_all_with_witness(
            &mut permits,
            "original-group".into(),
            witness.clone(),
        )
        .unwrap();
        witness.finish_original_attempt();
        keys
    };
    let mut permits = MutationOwner::reserve_all(vec![a.clone()]).await.unwrap();
    assert_eq!(
        MutationReservation::resolve_input_unknown(
            &mut permits,
            &keys[0],
            &witness,
            Some(vec![
                ready_facts("agent-a", 3, 4),
                cold_current_metadata("agent-b", 8)
            ])
        )
        .unwrap(),
        WitnessUnknownResolution::Known
    );
    assert_needs_reconcile(&a, &keys[0]);
    assert_needs_reconcile(&b, &keys[1]);
    let known = MutationReservation::validate_group_known(&permits).unwrap();
    let roots = MutationReservation::reconcile_all(&mut permits, known).unwrap();
    assert_eq!(roots[0].as_ref().unwrap().state.pending, 4);
    assert!(roots[1].is_none());
    assert!(b.capture().unwrap().is_none());
}

#[tokio::test]
async fn input_current_metadata_requires_original_cold_scope_even_after_epoch_replacement() {
    let registry = MutationOwnerRegistry::default();
    let old = registry.claim("agent-a", 3).unwrap();
    let _old_root = old_root(&old, 1).await;
    let witness = input_witness("original", vec![scope("agent-a", 3)]);
    let key = {
        let mut permits = vec![old.reserve().await.unwrap()];
        let key = MutationReservation::begin_all_with_witness(
            &mut permits,
            "original".into(),
            witness.clone(),
        )
        .unwrap()
        .remove(0);
        witness.finish_original_attempt();
        key
    };
    let successor = registry.claim("agent-a", 3).unwrap();
    assert!(
        successor.slot.root.load_full().is_none(),
        "claim clears the old instance cache, not original State responsibility"
    );
    let mut permits = vec![successor.reserve().await.unwrap()];
    assert!(matches!(
        MutationReservation::resolve_input_unknown(
            &mut permits,
            &key,
            &witness,
            Some(vec![cold_current_metadata("agent-a", 3)])
        ),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert_group_unknown(&permits);
    assert_needs_reconcile(&successor, &key);
}

#[tokio::test]
async fn input_cold_metadata_cannot_install_state_or_a_foreign_identity() {
    let registry = MutationOwnerRegistry::default();
    let owner = registry.claim("agent-a", 3).unwrap();
    let witness = input_witness("original", vec![scope("agent-a", 3)]);
    let key = {
        let mut permits = vec![owner.reserve().await.unwrap()];
        let key = MutationReservation::begin_all_with_witness(
            &mut permits,
            "original".into(),
            witness.clone(),
        )
        .unwrap()
        .remove(0);
        witness.finish_original_attempt();
        key
    };
    let mut permits = vec![owner.reserve().await.unwrap()];
    for invalid in [
        cold_current_metadata("foreign", 3),
        KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
            identity: match cold_current_metadata("agent-a", 3) {
                KnownCommit::Metadata(facts) => facts.identity.clone(),
                _ => unreachable!(),
            },
            state: Some(scoped_state("agent-a", 999)),
        })),
    ] {
        assert!(matches!(
            MutationReservation::resolve_input_unknown(
                &mut permits,
                &key,
                &witness,
                Some(vec![invalid])
            ),
            Err(MutationOwnerError::TicketMismatch)
        ));
        assert_group_unknown(&permits);
    }
    // A fresh loader may instead prove this original scope retired. Even an
    // Active successor identity must leave this old scope without any State.
    assert_eq!(
        MutationReservation::resolve_input_unknown(
            &mut permits,
            &key,
            &witness,
            Some(vec![cold_current_metadata("agent-a", 4)])
        )
        .unwrap(),
        WitnessUnknownResolution::Known
    );
    let known = MutationReservation::validate_group_known(&permits).unwrap();
    assert!(MutationReservation::reconcile_all(&mut permits, known).unwrap()[0].is_none());
    assert!(owner.capture().unwrap().is_none());
}

struct InterleavedRecovery {
    owners: Vec<MutationOwner>,
    witness: MutationWitness,
    original_keys: Vec<RecoveryKey>,
    other_unknown_keys: Vec<RecoveryKey>,
    other_known_keys: Vec<RecoveryKey>,
    other_known_states: Vec<Arc<AgentState>>,
    ready_root: Arc<PostureObservation>,
}

async fn interleaved_recovery() -> InterleavedRecovery {
    let registry = MutationOwnerRegistry::default();
    let owners = [
        "agent-a", "agent-b", "agent-c", "agent-d", "agent-e", "agent-f", "agent-g",
    ]
    .into_iter()
    .map(|id| registry.claim(id, 3).unwrap())
    .collect::<Vec<_>>();
    let ready_root = old_root(&owners[1], 2).await;
    let witness = input_witness(
        "shared-operation",
        vec![scope("agent-a", 3), scope("agent-c", 3)],
    );
    let original_keys = {
        let mut permits = MutationOwner::reserve_all(vec![owners[0].clone(), owners[2].clone()])
            .await
            .unwrap();
        let keys = MutationReservation::begin_all_with_witness(
            &mut permits,
            "shared-operation".into(),
            witness.clone(),
        )
        .unwrap();
        witness.finish_original_attempt();
        keys
    };
    // The operation text is deliberately equal. Original Arc identity, rather
    // than an operation string alone, must isolate these different groups.
    let other_witness = input_witness(
        "shared-operation",
        vec![scope("agent-d", 3), scope("agent-e", 3)],
    );
    let other_unknown_keys = {
        let mut permits = MutationOwner::reserve_all(vec![owners[3].clone(), owners[4].clone()])
            .await
            .unwrap();
        let keys = MutationReservation::begin_all_with_witness(
            &mut permits,
            "shared-operation".into(),
            other_witness.clone(),
        )
        .unwrap();
        other_witness.finish_original_attempt();
        keys
    };
    let other_known_states = vec![scoped_state("agent-f", 6), scoped_state("agent-g", 7)];
    let other_known_keys = {
        let mut permits = MutationOwner::reserve_all(vec![owners[5].clone(), owners[6].clone()])
            .await
            .unwrap();
        let keys = MutationReservation::begin_all(&mut permits, "other-known".into()).unwrap();
        MutationReservation::committed_all(
            &mut permits,
            other_known_states
                .iter()
                .cloned()
                .map(state_facts)
                .collect(),
        )
        .unwrap();
        keys
    };
    InterleavedRecovery {
        owners,
        witness,
        original_keys,
        other_unknown_keys,
        other_known_keys,
        other_known_states,
        ready_root,
    }
}

fn assert_unrelated_groups_unchanged(
    fixture: &InterleavedRecovery,
    permits: &[MutationReservation],
) {
    let ready = fixture.owners[1].capture().unwrap().unwrap();
    assert!(Arc::ptr_eq(&ready, &fixture.ready_root));
    assert!(permits[1].recovery().unwrap().is_none());
    for index in 3..5 {
        assert_eq!(
            permits[index].recovery().unwrap().unwrap().key,
            fixture.other_unknown_keys[index - 3]
        );
        assert!(permits[index].recovery().unwrap().unwrap().known.is_none());
        assert_needs_reconcile(
            &fixture.owners[index],
            &fixture.other_unknown_keys[index - 3],
        );
    }
    for index in 5..7 {
        let recovery = permits[index].recovery().unwrap().unwrap();
        assert_eq!(recovery.key, fixture.other_known_keys[index - 5]);
        assert!(Arc::ptr_eq(
            recovery.committed_state.as_ref().unwrap(),
            &fixture.other_known_states[index - 5]
        ));
        assert_needs_reconcile(&fixture.owners[index], &fixture.other_known_keys[index - 5]);
    }
}

#[tokio::test]
async fn input_absence_resolves_only_original_interleaved_group_in_reservation_superset() {
    let fixture = interleaved_recovery().await;
    let mut permits = MutationOwner::reserve_all(fixture.owners.clone())
        .await
        .unwrap();
    assert!(permits.iter().all(|permit| permit.ticket.is_none()));
    MutationReservation::validate_input_unknown(
        &permits,
        &fixture.original_keys[0],
        &fixture.witness,
    )
    .unwrap();
    assert_eq!(
        MutationReservation::resolve_input_unknown(
            &mut permits,
            &fixture.original_keys[0],
            &fixture.witness,
            None
        )
        .unwrap(),
        WitnessUnknownResolution::NotCommitted
    );
    assert!(permits[0].recovery().unwrap().is_none());
    assert!(permits[2].recovery().unwrap().is_none());
    assert_unrelated_groups_unchanged(&fixture, &permits);
}

#[tokio::test]
async fn input_presence_promotes_only_original_interleaved_group_and_keeps_other_gates_closed() {
    let fixture = interleaved_recovery().await;
    let mut permits = MutationOwner::reserve_all(fixture.owners.clone())
        .await
        .unwrap();
    let whole_superset = fixture
        .owners
        .iter()
        .map(|owner| ready_facts(owner.agent_id(), 3, 99))
        .collect();
    assert!(matches!(
        MutationReservation::resolve_input_unknown(
            &mut permits,
            &fixture.original_keys[0],
            &fixture.witness,
            Some(whole_superset)
        ),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert!(permits[0].recovery().unwrap().unwrap().known.is_none());
    assert!(permits[2].recovery().unwrap().unwrap().known.is_none());
    assert_eq!(
        MutationReservation::resolve_input_unknown(
            &mut permits,
            &fixture.original_keys[0],
            &fixture.witness,
            Some(vec![
                ready_facts("agent-a", 3, 91),
                ready_facts("agent-c", 3, 93)
            ])
        )
        .unwrap(),
        WitnessUnknownResolution::Known
    );
    assert_needs_reconcile(&fixture.owners[0], &fixture.original_keys[0]);
    assert_needs_reconcile(&fixture.owners[2], &fixture.original_keys[1]);
    assert_eq!(
        permits[0]
            .recovery()
            .unwrap()
            .unwrap()
            .committed_state
            .unwrap()
            .pending,
        91
    );
    assert_eq!(
        permits[2]
            .recovery()
            .unwrap()
            .unwrap()
            .committed_state
            .unwrap()
            .pending,
        93
    );
    assert_unrelated_groups_unchanged(&fixture, &permits);
    // Other unknown work must not prevent this original group's later install,
    // and must not be mistaken for a member of its exact known outcome.
    let mut original = permits
        .into_iter()
        .filter(|permit| matches!(permit.agent_id(), "agent-a" | "agent-c"))
        .collect::<Vec<_>>();
    let known = MutationReservation::validate_group_known(&original).unwrap();
    let roots = MutationReservation::reconcile_all(&mut original, known).unwrap();
    assert_eq!(roots[0].as_ref().unwrap().state.pending, 91);
    assert_eq!(roots[1].as_ref().unwrap().state.pending, 93);
    for (owner_index, key) in [
        (3, &fixture.other_unknown_keys[0]),
        (4, &fixture.other_unknown_keys[1]),
        (5, &fixture.other_known_keys[0]),
        (6, &fixture.other_known_keys[1]),
    ] {
        assert_needs_reconcile(&fixture.owners[owner_index], key);
    }
}

#[tokio::test]
async fn input_superset_validation_rejects_a_missing_original_or_unrelated_group_member() {
    let fixture = interleaved_recovery().await;
    let mut permits = MutationOwner::reserve_all(fixture.owners.clone())
        .await
        .unwrap();
    // A/B omit original C. No original or unrelated gate may be cleared.
    assert!(matches!(
        MutationReservation::resolve_input_unknown(
            &mut permits[..2],
            &fixture.original_keys[0],
            &fixture.witness,
            None
        ),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    // A/B/C/D include the original group, but omit D's own recovery member E.
    assert!(matches!(
        MutationReservation::validate_input_unknown(
            &permits[..4],
            &fixture.original_keys[0],
            &fixture.witness
        ),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert!(permits[0].recovery().unwrap().unwrap().known.is_none());
    assert!(permits[2].recovery().unwrap().unwrap().known.is_none());
    assert_unrelated_groups_unchanged(&fixture, &permits);
}
