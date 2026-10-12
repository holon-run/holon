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
        state,
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
