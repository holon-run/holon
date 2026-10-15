use super::*;
use crate::runtime::resident_task::{
    ResidentTaskDomain, TaskCaptureRequest, TaskCoverage, TaskHistoryCoverage, TASK_TERMINAL_WINDOW,
};

fn tasks(agent_id: &str, incarnation: u64) -> Arc<ResidentTaskDomain> {
    Arc::new(
        ResidentTaskDomain::bootstrap(
            InputEvidenceScope {
                agent_id: agent_id.into(),
                identity_incarnation: incarnation,
            },
            TaskCoverage {
                revision: 1,
                stop: Default::default(),
                output: Default::default(),
                status: Default::default(),
                query: Default::default(),
                running: Default::default(),
                active_complete: true,
                unsettled_complete: true,
                active_task_ids: Default::default(),
                unsettled_result_ids: Default::default(),
                task_metadata: Default::default(),
                result_metadata: Default::default(),
                result_dependencies: Default::default(),
                task_result_ids: Default::default(),
                activation_result_ids: Default::default(),
                reply_result_ids: Default::default(),
                required_keys: Default::default(),
                observations: Default::default(),
                terminal: TaskHistoryCoverage {
                    limit: TASK_TERMINAL_WINDOW,
                    keys: Vec::new(),
                    is_exhaustive: true,
                },
            },
            Vec::new(),
            64,
            16_384,
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn task_known_ack_rejects_foreign_incarnation_and_conflicting_domain_allocation() {
    let owner = MutationOwner::standalone("default", 7);
    let mut reservation = owner.reserve().await.unwrap();
    let key = reservation.begin("task-known".into()).unwrap();
    for wrong in [tasks("other", 7), tasks("default", 8)] {
        assert!(matches!(
            reservation.committed(state(1), Some(wrong)),
            Err(MutationOwnerError::TicketMismatch)
        ));
        assert!(reservation.recovery().unwrap().unwrap().known.is_none());
        assert_needs_reconcile(&owner, &key);
    }
    let original = tasks("default", 7);
    reservation
        .committed(state(1), Some(original.clone()))
        .unwrap();
    assert!(matches!(
        reservation.committed(state(1), Some(tasks("default", 7))),
        Err(MutationOwnerError::TicketMismatch)
    ));
    let current = reservation.publish(state(1)).unwrap();
    assert!(Arc::ptr_eq(current.tasks.as_ref().unwrap(), &original));
}

#[tokio::test]
async fn state_only_capture_preserves_task_arc_and_known_gate_before_publication() {
    let owner = MutationOwner::standalone("default", 7);
    let original = tasks("default", 7);
    let mut reservation = owner.reserve().await.unwrap();
    reservation.begin("task-initial".into()).unwrap();
    reservation
        .committed(state(1), Some(original.clone()))
        .unwrap();
    let old = reservation.publish(state(1)).unwrap();
    let carry = reservation.prepared_tasks_for_state_only_commit().unwrap();
    let key = reservation.begin("state-only-task-carry".into()).unwrap();
    reservation.committed(state(2), carry).unwrap();
    assert_needs_reconcile(&owner, &key);
    assert!(reservation.prepared_tasks_for_state_only_commit().is_err());
    drop(reservation); // Real interrupted publication; the next owner operation reconciles the saved ACK.
    let mut reservation = owner.reserve().await.unwrap();
    let known = reservation.recovery().unwrap().unwrap().known;
    let current =
        MutationReservation::reconcile_all(std::slice::from_mut(&mut reservation), vec![known])
            .unwrap()
            .remove(0)
            .unwrap();
    assert_eq!(current.state.pending, 2);
    assert!(Arc::ptr_eq(current.tasks.as_ref().unwrap(), &original));
    assert!(Arc::ptr_eq(old.tasks.as_ref().unwrap(), &original));
    assert!(current
        .tasks
        .as_ref()
        .unwrap()
        .capture(&TaskCaptureRequest::current())
        .unwrap()
        .records
        .is_empty());
}

#[tokio::test]
async fn task_metadata_retirement_never_installs_successor_or_old_domain() {
    let owner = MutationOwner::standalone("default", 7);
    let original = tasks("default", 7);
    let mut reservation = owner.reserve().await.unwrap();
    reservation.begin("task-initial".into()).unwrap();
    reservation
        .committed(state(1), Some(original.clone()))
        .unwrap();
    reservation.publish(state(1)).unwrap();
    reservation.begin("active-metadata".into()).unwrap();
    MutationReservation::committed_all(
        std::slice::from_mut(&mut reservation),
        vec![KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
            _identity_budget: None,
            identity: Some(episode_identity(7, AgentRegistryStatus::Active)),
            state: Some(state(2)),
        }))],
    )
    .unwrap();
    let current = MutationReservation::publish_all_known(std::slice::from_mut(&mut reservation))
        .unwrap()
        .remove(0)
        .unwrap();
    assert!(Arc::ptr_eq(current.tasks.as_ref().unwrap(), &original));
    reservation.begin("new-incarnation".into()).unwrap();
    MutationReservation::committed_all(
        std::slice::from_mut(&mut reservation),
        vec![KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
            _identity_budget: None,
            identity: Some(episode_identity(8, AgentRegistryStatus::Active)),
            state: None,
        }))],
    )
    .unwrap();
    assert!(
        MutationReservation::publish_all_known(std::slice::from_mut(&mut reservation)).unwrap()[0]
            .is_none()
    );
    assert!(owner.capture().unwrap().is_none());
    assert!(Arc::ptr_eq(current.tasks.as_ref().unwrap(), &original));
}
