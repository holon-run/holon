//! These tests cover process-local posture observations for the 2A migration.
//! An observation does not authorize legacy runtime decisions or context capture.

use super::*;

fn state(version: usize) -> Arc<AgentState> {
    let mut state = AgentState::new("default");
    state.pending = version;
    state.last_wake_reason = Some(format!("committed posture {version}"));
    Arc::new(state)
}

async fn install_test_observation(
    owner: &MutationOwner,
    operation_id: &str,
    committed: Arc<AgentState>,
) -> Arc<PostureObservation> {
    let mut reservation = owner.reserve().await.unwrap();
    reservation.begin(operation_id.to_owned()).unwrap();
    reservation.committed(committed.clone()).unwrap();
    reservation.publish(committed).unwrap()
}

fn assert_needs_reconcile(owner: &MutationOwner, key: &RecoveryKey) {
    match owner.capture() {
        Err(MutationOwnerError::NeedsReconcile(actual)) => assert_eq!(&actual, key),
        other => panic!("expected recovery fence for {key:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn reservations_keep_prepare_commit_and_publication_in_fifo_order() {
    let owner = MutationOwner::standalone("default", 7);
    let mut first = owner.reserve().await.unwrap();
    let first_key = first.begin("first".into()).unwrap();
    assert_eq!(
        first.recovery().unwrap().unwrap().phase,
        RecoveryPhase::Committing
    );
    let second = owner.reserve();
    let third = owner.reserve();
    tokio::pin!(second, third);

    // Poll explicitly to register both waiters in a deterministic order.
    assert!(futures_util::poll!(&mut second).is_pending());
    assert!(futures_util::poll!(&mut third).is_pending());
    assert_needs_reconcile(&owner, &first_key);
    let first_state = state(1);
    first.committed(first_state.clone()).unwrap();
    let first_observation = first.publish(first_state).unwrap();
    assert_eq!(first_observation.token.generation, 1);
    // SQL completion/publication alone must not release the reservation.
    assert!(futures_util::poll!(&mut second).is_pending());
    assert!(futures_util::poll!(&mut third).is_pending());
    drop(first);

    assert!(futures_util::poll!(&mut third).is_pending());
    let mut second = second.await.unwrap();
    second.begin("second".into()).unwrap();
    let second_state = state(2);
    second.committed(second_state.clone()).unwrap();
    let second_observation = second.publish(second_state).unwrap();
    assert_eq!(second_observation.token.generation, 2);
    assert!(futures_util::poll!(&mut third).is_pending());
    drop(second);

    let mut third = third.await.unwrap();
    third.begin("third".into()).unwrap();
    let third_state = state(3);
    third.committed(third_state.clone()).unwrap();
    let third_observation = third.publish(third_state).unwrap();
    assert_eq!(third_observation.token.generation, 3);
    assert_eq!(owner.capture().unwrap().unwrap().state.pending, 3);
    assert_eq!(first_observation.state.pending, 1);
    assert_eq!(second_observation.state.pending, 2);
}

#[tokio::test]
async fn dropping_a_reservation_after_begin_preserves_the_recovery_fence() {
    let owner = MutationOwner::standalone("default", 7);
    let old = install_test_observation(&owner, "initial", state(1)).await;
    let key = {
        let mut reservation = owner.reserve().await.unwrap();
        reservation.begin("cancelled-request".into()).unwrap()
    };

    assert_needs_reconcile(&owner, &key);
    let mut next = owner.reserve().await.unwrap();
    let pending = next.recovery().unwrap().unwrap();
    assert_eq!(pending.key, key);
    assert_eq!(pending.phase, RecoveryPhase::NeedsReconcile);
    assert!(pending.committed_state.is_none());
    assert!(matches!(
        next.begin("must-not-overtake".into()),
        Err(MutationOwnerError::NeedsReconcile(actual)) if actual == key
    ));
    drop(next);
    assert_needs_reconcile(&owner, &key);
    assert_eq!(old.state.pending, 1);
}

#[tokio::test]
async fn definitely_not_committed_preserves_the_old_observation_and_generation() {
    let owner = MutationOwner::standalone("default", 7);
    let old = install_test_observation(&owner, "initial", state(1)).await;
    let mut reservation = owner.reserve().await.unwrap();
    let key = reservation.begin("validation-rejected".into()).unwrap();
    assert_needs_reconcile(&owner, &key);
    reservation.definitely_not_committed().unwrap();

    let current = owner.capture().unwrap().unwrap();
    assert!(Arc::ptr_eq(&current, &old));
    assert_eq!(current.token, old.token);
    assert!(reservation.recovery().unwrap().is_none());
    reservation.begin("next-success".into()).unwrap();
    let next_state = state(2);
    reservation.committed(next_state.clone()).unwrap();
    let next = reservation.publish(next_state).unwrap();
    assert_eq!(next.token.generation, old.token.generation + 1);
}

#[tokio::test]
async fn known_commit_survives_publication_failure_and_reconciles_without_another_mutation() {
    let owner = MutationOwner::standalone("default", 7);
    let old = install_test_observation(&owner, "initial", state(1)).await;
    let committed = state(2);
    let key = {
        let mut reservation = owner.reserve().await.unwrap();
        let key = reservation
            .begin("committed-before-install-failure".into())
            .unwrap();
        reservation.committed(committed.clone()).unwrap();
        // Simulate cancellation/failure after durable success and before install.
        key
    };
    assert_needs_reconcile(&owner, &key);

    let mut recovery = owner.reserve().await.unwrap();
    let record = recovery.recovery().unwrap().unwrap();
    assert_eq!(record.key, key);
    assert_eq!(record.phase, RecoveryPhase::NeedsReconcile);
    let saved = record.committed_state.unwrap();
    assert!(Arc::ptr_eq(&saved, &committed));
    // Reconcile receives already committed facts, with no business-write callback.
    let restored = recovery.reconcile(&key, saved).unwrap();
    assert!(Arc::ptr_eq(&restored.state, &committed));
    assert_eq!(restored.state.as_ref(), committed.as_ref());
    assert_eq!(restored.token.generation, old.token.generation + 1);
    assert!(recovery.recovery().unwrap().is_none());
    assert!(Arc::ptr_eq(&owner.capture().unwrap().unwrap(), &restored));
    assert_eq!(old.state.pending, 1);
}

#[tokio::test]
async fn successor_claim_reuses_the_slot_and_rejects_old_owner_access() {
    let registry = MutationOwnerRegistry::default();
    let old_owner = registry.claim("default", 7).unwrap();
    let old = install_test_observation(&old_owner, "initial", state(1)).await;
    let successor = registry.claim("default", 7).unwrap();
    assert!(Arc::ptr_eq(&old_owner.slot, &successor.slot));
    assert!(successor.epoch > old_owner.epoch);
    assert!(matches!(
        old_owner.capture(),
        Err(MutationOwnerError::Replaced)
    ));
    assert!(matches!(
        old_owner.reserve().await,
        Err(MutationOwnerError::Replaced)
    ));
    assert!(successor.capture().unwrap().is_none());

    let next = install_test_observation(&successor, "successor", state(2)).await;
    assert_eq!(
        next.token.identity_incarnation,
        old.token.identity_incarnation
    );
    assert_ne!(next.token.owner_epoch, old.token.owner_epoch);
    assert_eq!(next.token.generation, 1);
    assert_eq!(old.state.pending, 1);
}

#[tokio::test]
async fn claim_cannot_replace_an_owner_with_an_active_reservation() {
    let registry = MutationOwnerRegistry::default();
    let owner = registry.claim("default", 7).unwrap();
    let old = install_test_observation(&owner, "initial", state(1)).await;
    let reservation = owner.reserve().await.unwrap();
    let error = registry.claim("default", 7).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<MutationOwnerError>(),
        Some(MutationOwnerError::Busy)
    ));
    assert!(Arc::ptr_eq(&owner.capture().unwrap().unwrap(), &old));
    drop(reservation);
    let successor = registry.claim("default", 7).unwrap();
    assert!(successor.epoch > owner.epoch);
}

#[tokio::test]
async fn unknown_and_known_pending_operations_survive_successor_claims() {
    for known_commit in [false, true] {
        let registry = MutationOwnerRegistry::default();
        let owner = registry.claim("default", 7).unwrap();
        let committed = state(2);
        let key = {
            let mut reservation = owner.reserve().await.unwrap();
            let key = reservation.begin("pending-across-handoff".into()).unwrap();
            if known_commit {
                reservation.committed(committed.clone()).unwrap();
            }
            key
        };
        let successor = registry.claim("default", 7).unwrap();
        assert_needs_reconcile(&successor, &key);
        let mut recovery = successor.reserve().await.unwrap();
        let pending = recovery.recovery().unwrap().unwrap();
        assert_eq!(pending.key, key);
        assert_eq!(pending.phase, RecoveryPhase::NeedsReconcile);
        assert_eq!(pending.committed_state.is_some(), known_commit);
        if let Some(saved) = pending.committed_state {
            assert!(Arc::ptr_eq(&saved, &committed));
        }
        assert!(matches!(
            recovery.begin("must-not-replace-pending".into()),
            Err(MutationOwnerError::NeedsReconcile(actual)) if actual == key
        ));
        assert!(matches!(owner.capture(), Err(MutationOwnerError::Replaced)));
    }
}

#[tokio::test]
async fn a_new_identity_incarnation_has_an_independent_slot_and_recovery_namespace() {
    let registry = MutationOwnerRegistry::default();
    let prior = registry.claim("default", 7).unwrap();
    let mut prior_reservation = prior.reserve().await.unwrap();
    let prior_key = prior_reservation.begin("same-operation-id".into()).unwrap();
    // A former incarnation's busy reservation cannot block a new identity slot.
    let current = registry.claim("default", 8).unwrap();
    assert!(!Arc::ptr_eq(&prior.slot, &current.slot));
    let observed = install_test_observation(&current, "same-operation-id", state(2)).await;
    assert_eq!(observed.token.identity_incarnation, 8);
    assert_needs_reconcile(&prior, &prior_key);
    assert_eq!(
        prior_reservation
            .recovery()
            .unwrap()
            .unwrap()
            .key
            .identity_incarnation,
        7
    );
    let mut current_reservation = current.reserve().await.unwrap();
    let current_key = current_reservation
        .begin("same-operation-id".into())
        .unwrap();
    assert_ne!(current_key, prior_key);
    assert_eq!(current_key.identity_incarnation, 8);
    current_reservation.definitely_not_committed().unwrap();
    assert!(Arc::ptr_eq(&current.capture().unwrap().unwrap(), &observed));
    // Durable identity fencing remains the repository's responsibility; creating
    // the new slot does not make this isolated observation an execution authority.
}

#[tokio::test]
async fn an_unrelated_recovery_key_cannot_clear_the_pending_operation() {
    let owner = MutationOwner::standalone("default", 7);
    let key = {
        let mut reservation = owner.reserve().await.unwrap();
        reservation.begin("pending".into()).unwrap()
    };
    let mut reservation = owner.reserve().await.unwrap();
    let mut unrelated = key.clone();
    unrelated.operation_id = "different-operation".into();
    assert!(matches!(
        reservation.reconcile(&unrelated, state(2)),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert_needs_reconcile(&owner, &key);
    assert!(matches!(
        reservation.publish(state(3)),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert_needs_reconcile(&owner, &key);
}

#[tokio::test]
async fn known_commit_cannot_be_reclassified_or_publish_an_uncommitted_state() {
    let owner = MutationOwner::standalone("default", 7);
    let old = install_test_observation(&owner, "initial", state(1)).await;
    let mut reservation = owner.reserve().await.unwrap();
    let key = reservation
        .begin("must-publish-the-committed-value".into())
        .unwrap();
    assert!(matches!(
        reservation.publish(state(2)),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert_needs_reconcile(&owner, &key);

    let committed = state(2);
    reservation.committed(committed.clone()).unwrap();
    assert!(matches!(
        reservation.definitely_not_committed(),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert!(matches!(
        reservation.publish(state(3)),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert_needs_reconcile(&owner, &key);
    let saved = reservation.recovery().unwrap().unwrap();
    assert_eq!(saved.key, key);
    assert!(Arc::ptr_eq(
        saved.committed_state.as_ref().unwrap(),
        &committed
    ));
    assert_eq!(old.state.pending, 1);

    // Equal committed facts are sufficient; pointer identity is not a revision.
    let equal_state = Arc::new(committed.as_ref().clone());
    assert!(!Arc::ptr_eq(&equal_state, &committed));
    let restored = reservation.publish(equal_state).unwrap();
    assert_eq!(restored.state.as_ref(), committed.as_ref());
    assert_eq!(restored.token.generation, old.token.generation + 1);
}

#[tokio::test]
async fn last_owner_and_reservation_release_published_body_while_registry_lives() {
    let registry = MutationOwnerRegistry::default();
    let owner = registry.claim("default", 7).unwrap();
    let observation = install_test_observation(&owner, "initial", state(1)).await;
    let body = Arc::downgrade(&observation.state);
    let token = observation.token;
    let owner_clone = owner.clone();
    let reservation = owner.reserve().await.unwrap();
    drop(observation);
    drop(owner);
    assert!(body.upgrade().is_some());
    drop(owner_clone);
    assert!(body.upgrade().is_some());
    drop(reservation);
    assert!(body.upgrade().is_none());

    // Slot identity and epochs survive even though the body has been released.
    assert_eq!(registry.slots.lock().unwrap().len(), 1);
    let successor = registry.claim("default", 7).unwrap();
    assert!(successor.epoch > token.owner_epoch);
    assert!(successor.capture().unwrap().is_none());
}

#[tokio::test]
async fn last_owner_release_preserves_pending_committed_facts_for_recovery() {
    let registry = MutationOwnerRegistry::default();
    let owner = registry.claim("default", 7).unwrap();
    let observation = install_test_observation(&owner, "initial", state(1)).await;
    let old_body = Arc::downgrade(&observation.state);
    drop(observation);
    let mut reservation = owner.reserve().await.unwrap();
    let key = reservation
        .begin("committed-before-release".into())
        .unwrap();
    let committed = state(2);
    let pending_body = Arc::downgrade(&committed);
    reservation.committed(committed).unwrap();
    drop(owner);
    drop(reservation);
    assert!(old_body.upgrade().is_none());
    assert!(pending_body.upgrade().is_some());

    let successor = registry.claim("default", 7).unwrap();
    assert_needs_reconcile(&successor, &key);
    let reservation = successor.reserve().await.unwrap();
    let recovery = reservation.recovery().unwrap().unwrap();
    assert_eq!(recovery.key, key);
    assert_eq!(recovery.phase, RecoveryPhase::NeedsReconcile);
    assert!(Arc::ptr_eq(
        recovery.committed_state.as_ref().unwrap(),
        &pending_body.upgrade().unwrap()
    ));
}

#[tokio::test]
async fn replaced_owner_release_cannot_clear_the_successor_observation() {
    let registry = MutationOwnerRegistry::default();
    let old_owner = registry.claim("default", 7).unwrap();
    let old = install_test_observation(&old_owner, "initial", state(1)).await;
    let old_body = Arc::downgrade(&old.state);
    drop(old);
    let successor = registry.claim("default", 7).unwrap();
    assert!(old_body.upgrade().is_none());
    let current = install_test_observation(&successor, "successor", state(2)).await;
    let current_body = Arc::downgrade(&current.state);
    let token = current.token;
    drop(current);
    drop(old_owner);
    assert_eq!(successor.capture().unwrap().unwrap().token, token);
    assert!(current_body.upgrade().is_some());
    drop(successor);
    assert!(current_body.upgrade().is_none());
}

#[tokio::test]
async fn bootstrap_and_reconcile_cannot_clear_an_unknown_operation() {
    let registry = MutationOwnerRegistry::default();
    let owner = registry.claim("default", 1).unwrap();
    let key = {
        let mut reservation = owner.reserve().await.unwrap();
        reservation.begin("unknown".into()).unwrap()
    };
    let successor = registry.claim("default", 1).unwrap();
    assert!(matches!(
        successor.validate_bootstrap(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    let mut recovery = successor.reserve().await.unwrap();
    assert!(matches!(
        recovery.reconcile(&key, state(2)),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert_needs_reconcile(&successor, &key);
}

#[tokio::test]
async fn current_owner_reuses_live_lease_without_replacing_epoch() {
    let registry = MutationOwnerRegistry::default();
    let owner = registry.claim("default", 7).unwrap();
    install_test_observation(&owner, "initial", state(1)).await;
    let sibling = registry.current_owner("default", 7).unwrap();
    assert_eq!(owner.epoch, sibling.epoch);
    assert!(Arc::ptr_eq(&owner._lifetime, &sibling._lifetime));
    drop(sibling);
    assert!(owner.capture().unwrap().is_some());
}

#[tokio::test]
async fn claim_reserved_keeps_handoff_exclusive_through_private_bootstrap() {
    let registry = MutationOwnerRegistry::default();
    let (owner, reservation) = registry.claim_reserved("default", 7).unwrap();
    assert!(registry
        .claim("default", 7)
        .unwrap_err()
        .downcast_ref::<MutationOwnerError>()
        .is_some_and(|error| matches!(error, MutationOwnerError::Busy)));
    let sibling = registry.current_owner("default", 7).unwrap();
    assert!(matches!(
        sibling.try_reserve(),
        Err(MutationOwnerError::Busy)
    ));
    assert_eq!(sibling.epoch, owner.epoch);
    drop(reservation);
    assert!(sibling.try_reserve().is_ok());
}

#[tokio::test]
async fn batch_publication_validates_every_member_before_opening_any_gate() {
    let registry = MutationOwnerRegistry::default();
    let first = registry.current_owner("agent-a", 1).unwrap();
    let second = registry.current_owner("agent-b", 1).unwrap();
    let mut reservations = MutationOwner::reserve_all(vec![second.clone(), first.clone()])
        .await
        .unwrap();
    let states = vec![
        Arc::new(AgentState::new("agent-a")),
        Arc::new(AgentState::new("agent-b")),
    ];
    for (reservation, state) in reservations.iter_mut().zip(&states) {
        reservation.begin("compound".into()).unwrap();
        reservation.committed(state.clone()).unwrap();
    }
    let mut wrong = states.clone();
    wrong[1] = Arc::new(AgentState::new("different-agent"));
    assert!(matches!(
        MutationReservation::publish_all(&mut reservations, wrong),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert!(matches!(
        first.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert!(matches!(
        second.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    let published = MutationReservation::publish_all(&mut reservations, states).unwrap();
    assert_eq!(published.len(), 2);
    assert!(first.capture().unwrap().is_some());
    assert!(second.capture().unwrap().is_some());
}

fn scoped_state(agent_id: &str, pending: usize) -> Arc<AgentState> {
    let mut state = AgentState::new(agent_id);
    state.pending = pending;
    Arc::new(state)
}

fn state_facts(state: Arc<AgentState>) -> KnownCommit {
    KnownCommit::State {
        work: None,
        state,
        episodes: None,
        input_evidence: None,
        runtime: None,
    }
}

fn absent_metadata() -> KnownCommit {
    KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
        identity: None,
        state: None,
    }))
}

#[tokio::test]
async fn known_metadata_absence_releases_old_body_without_becoming_an_unknown_outcome() {
    let registry = MutationOwnerRegistry::default();
    let owner = registry.current_owner("default", 7).unwrap();
    let old = install_test_observation(&owner, "old", state(1)).await;
    let old_body = Arc::downgrade(&old.state);
    drop(old);
    let mut permits = MutationOwner::reserve_all(vec![owner.clone()])
        .await
        .unwrap();
    MutationReservation::begin_all(&mut permits, "metadata-ack".into()).unwrap();
    MutationReservation::committed_all(&mut permits, vec![absent_metadata()]).unwrap();
    let record = permits[0].recovery().unwrap().unwrap();
    assert!(record.committed_state.is_none());
    assert!(matches!(record.known, Some(KnownCommit::Metadata(_))));
    drop(permits);
    assert!(owner.capture().is_err());
    let mut permits = MutationOwner::reserve_all(vec![owner.clone()])
        .await
        .unwrap();
    let known = MutationReservation::validate_group_known(&permits).unwrap();
    let observations = MutationReservation::reconcile_all(&mut permits, known).unwrap();
    assert!(observations[0].is_none());
    assert!(owner.capture().unwrap().is_none());
    assert!(old_body.upgrade().is_none());
}

#[tokio::test]
async fn compound_recovery_expands_members_and_never_opens_only_one_gate() {
    let registry = MutationOwnerRegistry::default();
    let parent = registry.current_owner("agent-a", 1).unwrap();
    let child = registry.current_owner("agent-b", 1).unwrap();
    let mut permits = MutationOwner::reserve_all(vec![child.clone(), parent.clone()])
        .await
        .unwrap();
    let keys = MutationReservation::begin_all(&mut permits, "parent-child".into()).unwrap();
    MutationReservation::committed_all(
        &mut permits,
        vec![state_facts(scoped_state("agent-a", 4)), absent_metadata()],
    )
    .unwrap();
    assert!(matches!(
        permits[0].publish(scoped_state("agent-a", 4)),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert_needs_reconcile(&parent, &keys[0]);
    assert_needs_reconcile(&child, &keys[1]);
    drop(permits);
    let mut recovery = MutationOwner::reserve_all(vec![child.clone()])
        .await
        .unwrap();
    assert_eq!(
        recovery
            .iter()
            .map(MutationReservation::agent_id)
            .collect::<Vec<_>>(),
        vec!["agent-a", "agent-b"]
    );
    let known = MutationReservation::validate_group_known(&recovery).unwrap();
    let mut wrong = known.clone();
    wrong[1] = Some(state_facts(scoped_state("agent-b", 9)));
    assert!(matches!(
        MutationReservation::reconcile_all(&mut recovery, wrong),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert_needs_reconcile(&parent, &keys[0]);
    assert_needs_reconcile(&child, &keys[1]);
    let published = MutationReservation::reconcile_all(&mut recovery, known).unwrap();
    assert_eq!(published[0].as_ref().unwrap().state.pending, 4);
    assert!(published[1].is_none());
    assert!(parent.capture().unwrap().is_some());
    assert!(child.capture().unwrap().is_none());
}

#[tokio::test]
async fn compound_unknown_cannot_be_acknowledged_by_current_rows_or_partial_rollback() {
    let registry = MutationOwnerRegistry::default();
    let a = registry.current_owner("agent-a", 1).unwrap();
    let b = registry.current_owner("agent-b", 1).unwrap();
    let mut permits = MutationOwner::reserve_all(vec![a.clone(), b.clone()])
        .await
        .unwrap();
    let keys = MutationReservation::begin_all(&mut permits, "unknown".into()).unwrap();
    assert!(matches!(
        permits[0].definitely_not_committed(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    drop(permits);
    let mut recovery = MutationOwner::reserve_all(vec![a.clone()]).await.unwrap();
    assert!(matches!(
        MutationReservation::validate_group_known(&recovery),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    let current = vec![
        Some(state_facts(scoped_state("agent-a", 1))),
        Some(absent_metadata()),
    ];
    assert!(matches!(
        MutationReservation::reconcile_all(&mut recovery, current),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert_needs_reconcile(&a, &keys[0]);
    assert_needs_reconcile(&b, &keys[1]);
}

#[tokio::test]
async fn compound_known_validation_failure_records_no_partial_success() {
    let registry = MutationOwnerRegistry::default();
    let a = registry.current_owner("agent-a", 1).unwrap();
    let b = registry.current_owner("agent-b", 1).unwrap();
    let mut permits = MutationOwner::reserve_all(vec![a.clone(), b.clone()])
        .await
        .unwrap();
    MutationReservation::begin_all(&mut permits, "rejected-before-sql".into()).unwrap();
    let wrong = vec![
        state_facts(scoped_state("agent-a", 1)),
        state_facts(scoped_state("wrong", 2)),
    ];
    assert!(matches!(
        MutationReservation::committed_all(&mut permits, wrong),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert!(permits
        .iter()
        .all(|permit| permit.recovery().unwrap().unwrap().known.is_none()));
    MutationReservation::definitely_not_committed_all(&mut permits).unwrap();
    assert!(a.capture().unwrap().is_none());
    assert!(b.capture().unwrap().is_none());
}

#[tokio::test]
async fn queued_single_scope_restarts_sorted_acquisition_when_a_group_appears() {
    let registry = MutationOwnerRegistry::default();
    let a = registry.current_owner("agent-a", 1).unwrap();
    let b = registry.current_owner("agent-b", 1).unwrap();
    let mut first = MutationOwner::reserve_all(vec![a.clone(), b.clone()])
        .await
        .unwrap();
    let waiter = MutationOwner::reserve_all(vec![b.clone()]);
    tokio::pin!(waiter);
    assert!(futures_util::poll!(&mut waiter).is_pending());
    MutationReservation::begin_all(&mut first, "created-after-waiter-queued".into()).unwrap();
    MutationReservation::committed_all(
        &mut first,
        vec![state_facts(scoped_state("agent-a", 3)), absent_metadata()],
    )
    .unwrap();
    drop(first);
    let mut acquired = waiter.await.unwrap();
    assert_eq!(
        acquired
            .iter()
            .map(MutationReservation::agent_id)
            .collect::<Vec<_>>(),
        vec!["agent-a", "agent-b"]
    );
    let known = MutationReservation::validate_group_known(&acquired).unwrap();
    MutationReservation::reconcile_all(&mut acquired, known).unwrap();
    assert!(a.capture().is_ok());
    assert!(b.capture().is_ok());
}

#[tokio::test]
async fn a_successor_recovers_the_whole_group_without_retaining_resolved_bodies() {
    let registry = MutationOwnerRegistry::default();
    let a = registry.current_owner("agent-a", 1).unwrap();
    let b = registry.current_owner("agent-b", 1).unwrap();
    let committed = scoped_state("agent-a", 6);
    let body = Arc::downgrade(&committed);
    let mut first = MutationOwner::reserve_all(vec![a.clone(), b.clone()])
        .await
        .unwrap();
    MutationReservation::begin_all(&mut first, "group-before-successor".into()).unwrap();
    MutationReservation::committed_all(&mut first, vec![state_facts(committed), absent_metadata()])
        .unwrap();
    drop(first);
    let successor = registry.claim("agent-a", 1).unwrap();
    assert!(matches!(
        successor.validate_bootstrap(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    let mut permits = MutationOwner::reserve_all(vec![successor.clone()])
        .await
        .unwrap();
    let known = MutationReservation::validate_group_known(&permits).unwrap();
    let observations = MutationReservation::reconcile_all(&mut permits, known).unwrap();
    assert_eq!(
        observations[0].as_ref().unwrap().token.owner_epoch,
        successor.epoch
    );
    drop(observations);
    drop(permits);
    drop(a);
    drop(b);
    assert!(body.upgrade().is_some());
    drop(successor);
    assert!(body.upgrade().is_none());
}

// Owner protocol tests exercise process-local responsibility;
// coherent receipt/storage qualification remains the typed loader's contract.
fn prepared_episodes(agent_id: &str, incarnation: u64) -> Arc<ResidentEpisodes> {
    Arc::new(ResidentEpisodes::bootstrap(agent_id, incarnation, 8, true, Vec::new()).unwrap())
}

fn episode_witness(operation: &str, agent_id: &str, incarnation: u64) -> MutationWitness {
    use crate::memory::episode_transition::{EpisodeMemoryOutcome, EpisodeMemoryPlan};
    use crate::runtime_db::episode_memory::{
        EpisodeMemoryInvocation, EpisodeMemoryKey, EpisodeMemoryTransitionCommand,
    };
    let mut candidate = AgentState::new(agent_id);
    candidate.turn_index = 1;
    let command = EpisodeMemoryTransitionCommand::new(
        EpisodeMemoryKey {
            agent_id: agent_id.into(),
            identity_incarnation: incarnation,
            actual_dispatch_turn_id: "actual-turn-a".into(),
        },
        EpisodeMemoryInvocation {
            message_id: "input-a".into(),
            turn_index: 1,
        },
        candidate.clone(),
        EpisodeMemoryPlan {
            candidate,
            finalized_episode: None,
            audit_events: Vec::new(),
            outcome: EpisodeMemoryOutcome {
                episode_memory_changed: false,
                business_fields_changed: false,
                boundary_reason: None,
                finalized_episode_id: None,
                previous_builder_id: None,
                active_builder_id: None,
                compression_epoch_before: 0,
                compression_epoch_after: 0,
                archived_episode_count_before: 0,
                archived_episode_count_after: 0,
            },
        },
        chrono::DateTime::from_timestamp(1, 0).unwrap(),
    )
    .unwrap();
    MutationWitness::episode_memory(operation.into(), &command).unwrap()
}

fn prepared_facts(state: Arc<AgentState>, episodes: Arc<ResidentEpisodes>) -> KnownCommit {
    KnownCommit::State {
        work: None,
        state,
        episodes: Some(episodes),
        input_evidence: None,
        runtime: None,
    }
}

#[tokio::test]
async fn prepared_publication_keeps_exact_domain_and_rejects_different_saved_arcs() {
    let owner = MutationOwner::standalone("default", 7);
    let episodes = prepared_episodes("default", 7);
    let mut permit = owner.reserve().await.unwrap();
    permit.begin("prepared-bootstrap".into()).unwrap();
    permit
        .committed_prepared(state(1), Some(episodes.clone()))
        .unwrap();
    assert!(matches!(
        permit.committed_prepared(state(1), Some(prepared_episodes("default", 7))),
        Err(MutationOwnerError::TicketMismatch)
    ));
    permit
        .committed_prepared(state(1), Some(episodes.clone()))
        .unwrap();
    let first = permit.publish(state(1)).unwrap();
    assert!(Arc::ptr_eq(first.episodes.as_ref().unwrap(), &episodes));
    assert!(Arc::ptr_eq(&owner.capture().unwrap().unwrap(), &first));

    permit
        .begin("explicitly-unprepared-legacy-writer".into())
        .unwrap();
    permit.committed(state(2)).unwrap();
    let next = permit.publish(state(2)).unwrap();
    assert!(next.episodes.is_none());
    assert!(Arc::ptr_eq(first.episodes.as_ref().unwrap(), &episodes));
}

#[tokio::test]
async fn prepared_domain_scope_is_validated_before_recording_known_facts() {
    let owner = MutationOwner::standalone("default", 7);
    let mut permit = owner.reserve().await.unwrap();
    let key = permit.begin("bad-domain".into()).unwrap();
    for episodes in [
        prepared_episodes("other", 7),
        prepared_episodes("default", 8),
    ] {
        assert!(matches!(
            permit.committed_prepared(state(1), Some(episodes)),
            Err(MutationOwnerError::TicketMismatch)
        ));
        assert!(permit.recovery().unwrap().unwrap().known.is_none());
        assert_needs_reconcile(&owner, &key);
    }
    permit.definitely_not_committed().unwrap();
}

#[tokio::test]
async fn witness_registration_rejects_foreign_scope_multi_owner_or_finished_attempt() {
    let a = MutationOwner::standalone("agent-a", 1);
    let b = MutationOwner::standalone("agent-b", 1);
    let mut permits = MutationOwner::reserve_all(vec![a.clone(), b.clone()])
        .await
        .unwrap();
    let witness = episode_witness("episode", "agent-a", 1);
    assert!(matches!(
        MutationReservation::begin_all_with_witness(
            &mut permits,
            "episode".into(),
            witness.clone()
        ),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert!(permits.iter().all(|p| p.recovery().unwrap().is_none()));
    drop(permits);
    let mut permits = vec![a.reserve().await.unwrap()];
    for invalid in [
        episode_witness("other-operation", "agent-a", 1),
        episode_witness("episode", "agent-b", 1),
        episode_witness("episode", "agent-a", 2),
    ] {
        assert!(matches!(
            MutationReservation::begin_all_with_witness(&mut permits, "episode".into(), invalid),
            Err(MutationOwnerError::TicketMismatch)
        ));
        assert!(permits[0].recovery().unwrap().is_none());
    }
    witness.finish_original_attempt();
    assert!(matches!(
        MutationReservation::begin_all_with_witness(&mut permits, "episode".into(), witness),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert!(permits[0].recovery().unwrap().is_none());
}

#[tokio::test]
async fn unfinished_or_reconstructed_witness_cannot_clear_an_unknown_fence() {
    let owner = MutationOwner::standalone("default", 7);
    let witness = episode_witness("episode", "default", 7);
    let key = {
        let mut permits = vec![owner.reserve().await.unwrap()];
        MutationReservation::begin_all_with_witness(&mut permits, "episode".into(), witness.clone())
            .unwrap()
            .remove(0)
    };
    let mut permits = vec![owner.reserve().await.unwrap()];
    assert!(permits[0].ticket.is_none());
    assert!(matches!(
        MutationReservation::resolve_episode_unknown(&mut permits, &key, &witness, None),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    let reconstructed = episode_witness("episode", "default", 7);
    reconstructed.finish_original_attempt();
    assert!(matches!(
        MutationReservation::resolve_episode_unknown(&mut permits, &key, &reconstructed, None),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert_needs_reconcile(&owner, &key);
    witness.finish_original_attempt();
    assert_eq!(
        MutationReservation::resolve_episode_unknown(&mut permits, &key, &witness, None).unwrap(),
        WitnessUnknownResolution::NotCommitted
    );
    assert!(owner.capture().unwrap().is_none());
}

#[tokio::test]
async fn completed_absence_preserves_the_exact_old_root_and_generation() {
    let owner = MutationOwner::standalone("default", 7);
    let old = install_test_observation(&owner, "initial", state(1)).await;
    let witness = episode_witness("episode", "default", 7);
    let key = {
        let mut permits = vec![owner.reserve().await.unwrap()];
        let key = MutationReservation::begin_all_with_witness(
            &mut permits,
            "episode".into(),
            witness.clone(),
        )
        .unwrap()
        .remove(0);
        witness.finish_original_attempt();
        key
    };
    let mut permits = vec![owner.reserve().await.unwrap()];
    assert_eq!(
        MutationReservation::resolve_episode_unknown(&mut permits, &key, &witness, None).unwrap(),
        WitnessUnknownResolution::NotCommitted
    );
    let current = owner.capture().unwrap().unwrap();
    assert!(Arc::ptr_eq(&old, &current));
    assert_eq!(old.token, current.token);
    assert!(permits[0].recovery().unwrap().is_none());
}

#[tokio::test]
async fn receipt_presence_promotes_current_loader_facts_but_does_not_publish() {
    let owner = MutationOwner::standalone("default", 7);
    let old = install_test_observation(&owner, "initial", state(1)).await;
    let witness = episode_witness("episode", "default", 7);
    let key = {
        let mut permits = vec![owner.reserve().await.unwrap()];
        let key = MutationReservation::begin_all_with_witness(
            &mut permits,
            "episode".into(),
            witness.clone(),
        )
        .unwrap()
        .remove(0);
        witness.finish_original_attempt();
        key
    };
    let episodes = prepared_episodes("default", 7);
    let mut permits = vec![owner.reserve().await.unwrap()];
    // A receipt's historical candidate is not installed. The coherent loader's
    // later current root (version 9) is the only state handed to this resolver.
    assert_eq!(
        MutationReservation::resolve_episode_unknown(
            &mut permits,
            &key,
            &witness,
            Some(prepared_facts(state(9), episodes.clone()))
        )
        .unwrap(),
        WitnessUnknownResolution::Known
    );
    assert_needs_reconcile(&owner, &key);
    assert!(permits[0].ticket.is_none());
    assert_eq!(
        permits[0]
            .recovery()
            .unwrap()
            .unwrap()
            .committed_state
            .unwrap()
            .pending,
        9
    );
    let known = MutationReservation::validate_group_known(&permits).unwrap();
    let published = MutationReservation::reconcile_all(&mut permits, known)
        .unwrap()
        .remove(0)
        .unwrap();
    assert_eq!(published.state.pending, 9);
    assert_eq!(published.token.generation, old.token.generation + 1);
    assert!(Arc::ptr_eq(published.episodes.as_ref().unwrap(), &episodes));
    assert!(Arc::ptr_eq(&owner.capture().unwrap().unwrap(), &published));
}

#[tokio::test]
async fn witness_survives_epoch_replacement_but_old_owner_access_remains_rejected() {
    let registry = MutationOwnerRegistry::default();
    let old = registry.claim("default", 7).unwrap();
    let witness = episode_witness("episode", "default", 7);
    let key = {
        let mut permits = vec![old.reserve().await.unwrap()];
        let key = MutationReservation::begin_all_with_witness(
            &mut permits,
            "episode".into(),
            witness.clone(),
        )
        .unwrap()
        .remove(0);
        witness.finish_original_attempt();
        key
    };
    let successor = registry.claim("default", 7).unwrap();
    assert!(matches!(old.capture(), Err(MutationOwnerError::Replaced)));
    let mut permits = vec![successor.reserve().await.unwrap()];
    let saved = permits[0].recovery().unwrap().unwrap().witness.unwrap();
    assert!(saved.same_attempt(&witness));
    assert_eq!(
        MutationReservation::resolve_episode_unknown(
            &mut permits,
            &key,
            &saved,
            Some(prepared_facts(state(4), prepared_episodes("default", 7)))
        )
        .unwrap(),
        WitnessUnknownResolution::Known
    );
    let known = MutationReservation::validate_group_known(&permits).unwrap();
    let root = MutationReservation::reconcile_all(&mut permits, known)
        .unwrap()
        .remove(0)
        .unwrap();
    assert_eq!(root.token.owner_epoch, successor.epoch);
    assert_eq!(root.state.pending, 4);
}

#[tokio::test]
async fn episode_resolver_cannot_resolve_unwitnessed_other_family_or_partial_group() {
    let a = MutationOwner::standalone("agent-a", 1);
    let b = MutationOwner::standalone("agent-b", 1);
    let witness = episode_witness("ordinary", "agent-a", 1);
    witness.finish_original_attempt();
    let mut permits = MutationOwner::reserve_all(vec![a.clone(), b.clone()])
        .await
        .unwrap();
    let keys = MutationReservation::begin_all(&mut permits, "ordinary".into()).unwrap();
    assert!(matches!(
        MutationReservation::resolve_episode_unknown(&mut permits, &keys[0], &witness, None),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert!(matches!(
        MutationReservation::resolve_episode_unknown(&mut permits[..1], &keys[0], &witness, None),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert_needs_reconcile(&a, &keys[0]);
    assert_needs_reconcile(&b, &keys[1]);
    MutationReservation::definitely_not_committed_all(&mut permits).unwrap();
    drop(permits);
    let mut permits = vec![a.reserve().await.unwrap()];
    let key = permits[0].begin("ordinary".into()).unwrap();
    assert!(matches!(
        MutationReservation::resolve_episode_unknown(&mut permits, &key, &witness, None),
        Err(MutationOwnerError::TicketMismatch)
    ));
    assert_needs_reconcile(&a, &key);
}

#[tokio::test]
async fn episode_resolution_rejects_unprepared_or_foreign_facts_without_opening_gate() {
    let owner = MutationOwner::standalone("default", 7);
    let witness = episode_witness("episode", "default", 7);
    let mut permits = vec![owner.reserve().await.unwrap()];
    let key = MutationReservation::begin_all_with_witness(
        &mut permits,
        "episode".into(),
        witness.clone(),
    )
    .unwrap()
    .remove(0);
    witness.finish_original_attempt();
    for invalid in [
        state_facts(state(2)),
        prepared_facts(state(2), prepared_episodes("default", 8)),
    ] {
        assert!(matches!(
            MutationReservation::resolve_episode_unknown(
                &mut permits,
                &key,
                &witness,
                Some(invalid)
            ),
            Err(MutationOwnerError::TicketMismatch)
        ));
        let recovery = permits[0].recovery().unwrap().unwrap();
        assert!(recovery.known.is_none());
        assert_needs_reconcile(&owner, &key);
    }
}

#[tokio::test]
async fn prepared_domains_survive_active_metadata_only_for_the_current_incarnation() {
    use crate::types::{AgentKind, AgentVisibility};
    let owner = MutationOwner::standalone("default", 7);
    let episodes = prepared_episodes("default", 7);
    let mut permits = vec![owner.reserve().await.unwrap()];
    permits[0].begin("initial".into()).unwrap();
    permits[0]
        .committed_prepared(state(1), Some(episodes.clone()))
        .unwrap();
    permits[0].publish(state(1)).unwrap();
    for (index, status) in [AgentRegistryStatus::Active, AgentRegistryStatus::Deleting]
        .into_iter()
        .enumerate()
    {
        let timestamp = chrono::DateTime::from_timestamp(1, 0).unwrap();
        let identity = AgentIdentityRecord {
            agent_id: "default".into(),
            name: None,
            kind: AgentKind::Default,
            visibility: AgentVisibility::Public,
            ownership: None,
            profile_preset: None,
            durability: None,
            status,
            revision: index as u64 + 1,
            incarnation: 7,
            parent_agent_id: None,
            lineage_parent_agent_id: None,
            delegated_from_task_id: None,
            created_at: timestamp,
            updated_at: timestamp,
            deleted_at: None,
        };
        MutationReservation::begin_all(&mut permits, format!("metadata-{index}")).unwrap();
        MutationReservation::committed_all(
            &mut permits,
            vec![KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
                identity: Some(Arc::new(identity)),
                state: Some(state(2 + index)),
            }))],
        )
        .unwrap();
        let root = MutationReservation::publish_all_known(&mut permits)
            .unwrap()
            .remove(0)
            .unwrap();
        if status == AgentRegistryStatus::Active {
            assert!(Arc::ptr_eq(root.episodes.as_ref().unwrap(), &episodes));
        } else {
            assert!(root.episodes.is_none());
        }
    }
}

fn episode_identity(incarnation: u64, status: AgentRegistryStatus) -> Arc<AgentIdentityRecord> {
    use crate::types::{AgentKind, AgentVisibility};
    let timestamp = chrono::DateTime::from_timestamp(1, 0).unwrap();
    Arc::new(AgentIdentityRecord {
        agent_id: "default".into(),
        name: None,
        kind: AgentKind::Default,
        visibility: AgentVisibility::Public,
        ownership: None,
        profile_preset: None,
        durability: None,
        status,
        revision: 1,
        incarnation,
        parent_agent_id: None,
        lineage_parent_agent_id: None,
        delegated_from_task_id: None,
        created_at: timestamp,
        updated_at: timestamp,
        deleted_at: None,
    })
}

#[tokio::test]
async fn positive_receipt_for_retired_scope_publishes_absence_without_touching_successor() {
    for identity in [
        None,
        Some(episode_identity(7, AgentRegistryStatus::Deleting)),
        Some(episode_identity(7, AgentRegistryStatus::Deleted)),
        Some(episode_identity(8, AgentRegistryStatus::Active)),
    ] {
        let registry = MutationOwnerRegistry::default();
        let owner = registry.claim("default", 7).unwrap();
        let successor = registry.claim("default", 8).unwrap();
        let next = install_test_observation(&successor, "successor-root", state(20)).await;
        let initial = install_test_observation(&owner, "old-root", state(1)).await;
        let old_state = Arc::downgrade(&initial.state);
        drop(initial);
        let witness = episode_witness("episode", "default", 7);
        let mut permits = vec![owner.reserve().await.unwrap()];
        let key = MutationReservation::begin_all_with_witness(
            &mut permits,
            "episode".into(),
            witness.clone(),
        )
        .unwrap()
        .remove(0);
        witness.finish_original_attempt();
        let facts = KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
            identity,
            state: None,
        }));
        assert_eq!(
            MutationReservation::resolve_episode_unknown(&mut permits, &key, &witness, Some(facts))
                .unwrap(),
            WitnessUnknownResolution::Known
        );
        assert_needs_reconcile(&owner, &key);
        assert!(matches!(
            permits[0].recovery().unwrap().unwrap().known,
            Some(KnownCommit::Metadata(_))
        ));
        let known = MutationReservation::validate_group_known(&permits).unwrap();
        assert!(MutationReservation::reconcile_all(&mut permits, known).unwrap()[0].is_none());
        assert!(owner.capture().unwrap().is_none());
        assert!(old_state.upgrade().is_none());
        assert!(Arc::ptr_eq(&successor.capture().unwrap().unwrap(), &next));
    }
}

#[tokio::test]
async fn retired_receipt_resolution_rejects_current_active_or_any_successor_state() {
    let owner = MutationOwner::standalone("default", 7);
    let witness = episode_witness("episode", "default", 7);
    let mut permits = vec![owner.reserve().await.unwrap()];
    let key = MutationReservation::begin_all_with_witness(
        &mut permits,
        "episode".into(),
        witness.clone(),
    )
    .unwrap()
    .remove(0);
    witness.finish_original_attempt();
    for facts in [
        IdentityMutationFacts {
            identity: Some(episode_identity(7, AgentRegistryStatus::Active)),
            state: None,
        },
        IdentityMutationFacts {
            identity: Some(episode_identity(8, AgentRegistryStatus::Active)),
            state: Some(state(8)),
        },
        IdentityMutationFacts {
            identity: Some(episode_identity(6, AgentRegistryStatus::Deleted)),
            state: None,
        },
    ] {
        assert!(matches!(
            MutationReservation::resolve_episode_unknown(
                &mut permits,
                &key,
                &witness,
                Some(KnownCommit::Metadata(Arc::new(facts)))
            ),
            Err(MutationOwnerError::TicketMismatch)
        ));
        assert!(permits[0].recovery().unwrap().unwrap().known.is_none());
        assert_needs_reconcile(&owner, &key);
    }
}

#[path = "mutation_owner_tests/input_recovery.rs"]
mod input_recovery;

#[path = "mutation_owner_tests/work_publication.rs"]
mod work_publication;
