use super::*;
use crate::runtime::resident_work::{
    ResidentWorkDomain, WorkCaptureRequest, WorkCoverage, WorkHistoryCoverage, WORK_NONOPEN_WINDOW,
};

fn work_domain(agent_id: &str, incarnation: u64) -> Arc<ResidentWorkDomain> {
    Arc::new(
        ResidentWorkDomain::bootstrap(
            InputEvidenceScope {
                agent_id: agent_id.into(),
                identity_incarnation: incarnation,
            },
            WorkCoverage {
                legacy_delivery_heads: std::collections::BTreeMap::new(),
                task_wait_keys: Default::default(),
                trigger_wait_keys: Default::default(),
                revision: 1,
                live_complete: true,
                live_work_ids: Default::default(),
                unresolved_wait_ids: Default::default(),
                active_frame_ids: Default::default(),
                turn_wait_keys: Default::default(),
                mandatory_roots: Default::default(),
                observations: Default::default(),
                nonopen: WorkHistoryCoverage {
                    limit: WORK_NONOPEN_WINDOW,
                    keys: Vec::new(),
                    is_exhaustive: true,
                },
                latest_results: Default::default(),
            },
            Vec::new(),
            32,
            64 * 1024,
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn work_publication_rejects_foreign_scope_before_known_acknowledgement() {
    let owner = MutationOwner::standalone("default", 7);
    let mut reservation = owner.reserve().await.unwrap();
    let key = reservation.begin("work-scope".into()).unwrap();
    for work in [work_domain("other", 7), work_domain("default", 8)] {
        assert!(matches!(
            reservation.committed_prepared_with_domains(state(1), None, None, Some(work), None),
            Err(MutationOwnerError::TicketMismatch)
        ));
        assert!(reservation.recovery().unwrap().unwrap().known.is_none());
        assert_needs_reconcile(&owner, &key);
    }
    reservation.definitely_not_committed().unwrap();
}

#[tokio::test]
async fn work_publication_keeps_original_known_allocation_and_explicit_readiness() {
    let owner = MutationOwner::standalone("default", 7);
    let work = work_domain("default", 7);
    let mut reservation = owner.reserve().await.unwrap();
    reservation.begin("work-known".into()).unwrap();
    reservation
        .committed_prepared_with_domains(state(1), None, None, Some(work.clone()), None)
        .unwrap();
    assert!(matches!(
        reservation.committed_prepared_with_domains(
            state(1),
            None,
            None,
            Some(work_domain("default", 7)),
            None,
        ),
        Err(MutationOwnerError::TicketMismatch)
    ));
    let first = reservation.publish(state(1)).unwrap();
    assert!(Arc::ptr_eq(first.work.as_ref().unwrap(), &work));
    assert_eq!(
        first
            .work
            .as_ref()
            .unwrap()
            .capture(&WorkCaptureRequest::current())
            .unwrap()
            .revision,
        1
    );
    reservation.begin("unmigrated-writer".into()).unwrap();
    reservation.committed(state(2), None).unwrap();
    let next = reservation.publish(state(2)).unwrap();
    assert!(
        next.work.is_none(),
        "unprepared is not a proved empty Work domain"
    );
    assert!(Arc::ptr_eq(first.work.as_ref().unwrap(), &work));
}

#[tokio::test]
async fn work_publication_preserves_same_incarnation_active_metadata_only() {
    let owner = MutationOwner::standalone("default", 7);
    let work = work_domain("default", 7);
    let mut reservation = owner.reserve().await.unwrap();
    reservation.begin("work-initial".into()).unwrap();
    reservation
        .committed_prepared_with_domains(state(1), None, None, Some(work.clone()), None)
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
    assert!(Arc::ptr_eq(current.work.as_ref().unwrap(), &work));
    reservation
        .begin("retire-original-identity".into())
        .unwrap();
    MutationReservation::committed_all(
        std::slice::from_mut(&mut reservation),
        vec![KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
            _identity_budget: None,
            identity: Some(episode_identity(7, AgentRegistryStatus::Deleted)),
            state: None,
        }))],
    )
    .unwrap();
    assert!(
        MutationReservation::publish_all_known(std::slice::from_mut(&mut reservation)).unwrap()[0]
            .is_none()
    );
    assert!(owner.capture().unwrap().is_none());
    assert!(
        Arc::ptr_eq(current.work.as_ref().unwrap(), &work),
        "prior capture remains immutable"
    );
}
