use super::*;
use crate::runtime::mutation_owner::MutationOwnerRegistry;
use crate::runtime::tests::support::context_config;
use crate::runtime_db::{repositories::WaitConditionWrites, transitions::PostCommitEffects};
use tempfile::TempDir;

async fn fixture() -> Result<(RuntimeHandle, TempDir)> {
    let home = tempfile::tempdir()?;
    let runtime = RuntimeHandle::new(
        "default",
        home.path().to_owned(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(crate::provider::StubProvider::new("unused")),
        "default".into(),
        context_config(),
    )?;
    runtime.await_initial_resources().await;
    Ok((runtime, home))
}

async fn terminal_handle_fixture() -> Result<(RuntimeHandle, TempDir, CurrentRunAbortHandle)> {
    let (runtime, home) = fixture().await?;
    let handle = CurrentRunAbortHandle {
        run_id: "acknowledged-terminal-run".into(),
        token: CancellationToken::new(),
        reason: Arc::new(StdMutex::new("terminal".into())),
    };
    {
        let mut agent = runtime.inner.agent.lock().await;
        assert!(agent.state.current_run_id.is_none());
        assert_eq!(agent.state, agent.last_persisted_state);
        // Terminal ACK installed State; the next mutation owns handle cleanup.
        agent.current_run_abort = Some(handle.clone());
    }
    Ok((runtime, home, handle))
}

#[tokio::test]
async fn durable_readback_preserves_the_same_state_terminal_handle() -> Result<()> {
    let (runtime, _home, handle) = terminal_handle_fixture().await?;
    let state = Arc::new(runtime.inner.agent.lock().await.state.clone());
    runtime.install_exact_durable_readback(&state).await?;
    let agent = runtime.inner.agent.lock().await;
    assert_eq!(&agent.state, state.as_ref());
    assert_eq!(agent.state, agent.last_persisted_state);
    let installed = agent.current_run_abort.as_ref().unwrap();
    assert_eq!(installed.run_id, handle.run_id);
    assert!(Arc::ptr_eq(&installed.reason, &handle.reason));
    assert!(!handle.token.is_cancelled());
    handle.token.cancel();
    assert!(installed.token.is_cancelled());
    Ok(())
}

#[tokio::test]
async fn durable_readback_rejects_changed_state_with_a_conflicting_handle() -> Result<()> {
    let (runtime, _home, handle) = terminal_handle_fixture().await?;
    let before = runtime.inner.agent.lock().await.state.clone();
    let mut changed = before.clone();
    changed.total_input_tokens += 1;
    let error = runtime
        .install_exact_durable_readback(&Arc::new(changed))
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("conflicts with a live run handle"));
    let agent = runtime.inner.agent.lock().await;
    assert_eq!(agent.state, before);
    assert_eq!(agent.last_persisted_state, before);
    assert!(Arc::ptr_eq(
        &agent.current_run_abort.as_ref().unwrap().reason,
        &handle.reason
    ));
    assert!(!handle.token.is_cancelled());
    Ok(())
}

#[tokio::test]
async fn durable_readback_rejects_unpersisted_state_even_when_readback_matches() -> Result<()> {
    let (runtime, _home, handle) = terminal_handle_fixture().await?;
    let (persisted, dirty) = {
        let mut agent = runtime.inner.agent.lock().await;
        let persisted = agent.last_persisted_state.clone();
        agent.state.pending += 1;
        (persisted, Arc::new(agent.state.clone()))
    };
    let error = runtime
        .install_exact_durable_readback(&dirty)
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("cannot discard unpersisted state"));
    let agent = runtime.inner.agent.lock().await;
    assert_eq!(&agent.state, dirty.as_ref());
    assert_eq!(agent.last_persisted_state, persisted);
    assert!(!handle.token.is_cancelled());
    Ok(())
}

#[tokio::test]
async fn recovery_install_returns_original_facts_and_preserves_durable_ack() -> Result<()> {
    let (runtime, _home) = fixture().await?;
    {
        let mut agent = runtime.begin_mutation().await?;
        agent.state.status = AgentStatus::AwakeRunning;
        agent.state.current_run_id = Some("original-recovery-run".into());
        agent.persist_state(&runtime.inner.storage).await?;
    }
    runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
    let error = scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
        .reconcile_stale_run_projection(scheduler_executor::StaleRunProjectionBoundary::RunLoopPoll)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("committed but publication"));
    let events = runtime.storage().read_recent_events(100)?;
    let durable = runtime
        .runtime_db()
        .agent_states()
        .latest("default")?
        .unwrap();
    let mut reservations = vec![runtime.inner.mutation_owner.reserve().await?];
    let known = MutationReservation::validate_group_known(&reservations)?;
    let saved = known[0].as_ref().unwrap();
    let KnownCommit::State {
        runtime: Some(original),
        ..
    } = saved
    else {
        panic!("original runtime ACK missing");
    };
    assert!(!original.effects.audit_events.is_empty());
    let weak = Arc::downgrade(original);
    let first = runtime
        .install_saved_mutation(&reservations[0], saved)
        .await?
        .unwrap();
    let last = first.clone();
    assert!(Arc::ptr_eq(&first, original));
    assert!(std::ptr::eq(first.external_effects(), &original.effects));
    assert_eq!(
        first.external_effects().audit_events.as_ptr(),
        original.effects.audit_events.as_ptr()
    );
    MutationReservation::reconcile_all(&mut reservations, known)?;
    drop((reservations, first));
    assert!(weak.upgrade().is_some());
    runtime
        .reissue_mutation_notifications(last.external_effects())
        .await;
    assert_eq!(runtime.storage().read_recent_events(100)?, events);
    assert_eq!(
        runtime.runtime_db().agent_states().latest("default")?,
        Some(durable.clone())
    );
    assert_eq!(runtime.inner.agent.lock().await.state, durable);
    assert!(runtime.take_transition_warnings().is_empty());
    drop(last);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn cold_recovery_borrows_write_set_and_retains_original_plan_until_last_holder() -> Result<()>
{
    let (runtime, _home) = fixture().await?;
    let mut record =
        WorkItemRecord::new("default", "original recovery material", WorkItemState::Open);
    let (_, preparation) = runtime
        .prepare_work_plan_file(&mut record, Some("ORIGINAL_RECOVERY_PLAN_BODY"))
        .await?;
    let material = Arc::downgrade(&preparation.material_for_test());
    let mut mutation = runtime.begin_mutation().await?;
    mutation.attach_plan_preparation(preparation.clone())?;
    let facts = Arc::new(mutation.facts(PostCommitEffects {
        wait_condition_writes: WaitConditionWrites {
            primary_applied: true,
            wait_ids: ["wait:original".into()].into(),
            turn_ids: ["turn:original".into()].into(),
        },
        audit_events: vec![crate::runtime_db::transitions::AuditEventAck::new(
            AuditEvent::legacy(
                "original-recovery-notification",
                serde_json::json!({"body": "ORIGINAL_AUDIT_BODY"}),
            ),
        )?],
        ..Default::default()
    }));
    let ledger = &runtime.inner.input_evidence_budget;
    let before = ledger.report()?.observed_bytes;
    let first = facts.clone();
    let last = first.clone();
    let effects = last.external_effects();
    assert!(std::ptr::eq(effects, &facts.effects));
    assert_eq!(
        effects
            .wait_condition_writes
            .wait_ids
            .first()
            .unwrap()
            .as_ptr(),
        facts
            .effects
            .wait_condition_writes
            .wait_ids
            .first()
            .unwrap()
            .as_ptr()
    );
    assert_eq!(
        effects
            .wait_condition_writes
            .turn_ids
            .first()
            .unwrap()
            .as_ptr(),
        facts
            .effects
            .wait_condition_writes
            .turn_ids
            .first()
            .unwrap()
            .as_ptr()
    );
    assert_eq!(
        effects.audit_events.as_ptr(),
        facts.effects.audit_events.as_ptr()
    );
    assert_eq!(ledger.report()?.observed_bytes, before);
    drop((facts, preparation, mutation));
    let held = ledger.report()?.observed_bytes;
    assert!(material.upgrade().is_some());
    drop(first);
    assert!(material.upgrade().is_some());
    assert_eq!(ledger.report()?.observed_bytes, held);
    drop(last);
    assert!(material.upgrade().is_none());
    assert!(ledger.report()?.observed_bytes < held);
    runtime.join_owned_mutations().await?;
    Ok(())
}

#[tokio::test]
async fn borrowed_recovery_notifications_preserve_faults_without_installing_authority() -> Result<()>
{
    for fault in [
        None,
        Some(TransitionFaultPoint::BeforeCacheUpdate),
        Some(TransitionFaultPoint::BeforeEventPublication),
        Some(TransitionFaultPoint::BeforeSchedulerNotification),
    ] {
        let (runtime, _home) = fixture().await?;
        runtime
            .storage()
            .enable_event_bus(crate::storage::EventBus::new(8))?;
        let mut events = runtime.storage().subscribe_events()?.unwrap();
        let state = runtime.inner.agent.lock().await.state.clone();
        let mut forbidden = state.clone();
        forbidden.pending += 100;
        let effects = PostCommitEffects {
            agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                expected: None,
                record: Box::new(forbidden),
            }),
            audit_events: vec![crate::runtime_db::transitions::AuditEventAck::new(
                AuditEvent::legacy(
                    "borrowed-original-ack",
                    serde_json::json!({"body": "ORIGINAL_NOTIFICATION_BODY"}),
                ),
            )?],
            notify_scheduler: true,
            notify_memory_index: true,
            fault,
            ..Default::default()
        };
        let durable = runtime.storage().read_recent_events(100)?;
        {
            let drain = runtime.inner.notify.notified();
            tokio::pin!(drain);
            if drain.as_mut().enable() {
                drain.await;
            }
        }
        let scheduler = runtime.inner.notify.notified();
        tokio::pin!(scheduler);
        scheduler.as_mut().enable();
        runtime.reissue_mutation_notifications(&effects).await;
        assert_eq!(effects.fault, fault);
        assert!(effects.agent_state.is_some());
        assert_eq!(runtime.inner.agent.lock().await.state, state);
        assert_eq!(runtime.storage().read_recent_events(100)?, durable);
        let warnings = runtime.take_transition_warnings();
        match fault {
            Some(TransitionFaultPoint::BeforeEventPublication) => {
                assert!(events.try_recv().is_err());
                assert_eq!(warnings.len(), 1);
                assert_eq!(warnings[0].effect, "event_publication");
            }
            _ => {
                assert_eq!(events.try_recv()?.event, effects.audit_events[0]);
                assert!(events.try_recv().is_err());
                if fault == Some(TransitionFaultPoint::BeforeSchedulerNotification) {
                    assert_eq!(warnings.len(), 1);
                    assert_eq!(warnings[0].effect, "scheduler_notification");
                } else {
                    assert!(warnings.is_empty());
                }
            }
        }
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_millis(10), scheduler)
                .await
                .is_err(),
            fault == Some(TransitionFaultPoint::BeforeSchedulerNotification),
            "scheduler notification for fault {fault:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn borrowed_recovery_keeps_foreign_owner_and_old_epoch_fences() -> Result<()> {
    let (runtime, _home) = fixture().await?;
    let mutation = runtime.begin_mutation().await?;
    let mut facts = mutation.facts(Default::default());
    facts.owner_epoch += 1;
    let facts = Arc::new(facts);
    let known = facts.task_status_metadata_known();
    let foreign =
        MutationOwnerRegistry::default().claim("default", runtime.instance_incarnation())?;
    let reservation = foreign.reserve().await?;
    assert!(runtime
        .install_saved_mutation(&reservation, &known)
        .await?
        .is_none());
    let weak = Arc::downgrade(&facts);
    assert!(runtime
        .install_saved_mutation(&mutation.reservations[0], &known)
        .await?
        .is_none());
    drop((known, facts));
    assert!(weak.upgrade().is_none());
    assert!(runtime.take_transition_warnings().is_empty());
    Ok(())
}
